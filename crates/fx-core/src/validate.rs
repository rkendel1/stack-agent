//! Deterministic, provider-independent request validation.
//!
//! [`validate_request`] runs before any capability call. It checks structure,
//! syntax and bounds only. It never selects models, resolves credentials, routes,
//! retries, executes tools or applies provider-specific structured-output rules.
//! Checks run in a fixed order and the first failure is returned, so the same
//! request always produces the same error.
//!
//! Errors carry a code and a location, never request content, so they are safe to
//! log (invariant 8).

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::contract::{
    Event, ImageMediaType, Limits, Message, Part, ReasoningEffort, Request, Role, ToolChoice,
    ToolExecution, ToolOutput,
};
use crate::json_check::{self, JsonKind};

/// What was wrong with a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ValidationCode {
    /// The model id is empty.
    EmptyModelId,
    /// The model id has control characters or leading/trailing whitespace.
    InvalidModelId,
    /// The model id exceeds `max_model_id_bytes`.
    ModelIdTooLong,
    /// The request has no messages.
    NoMessages,
    /// More than `max_messages` messages.
    TooManyMessages,
    /// A message has no parts.
    EmptyMessage,
    /// More than `max_parts_per_message` parts.
    TooManyParts,
    /// A part kind is not allowed for the message role.
    PartNotAllowedForRole,
    /// Text, arguments and tool results exceed `max_total_text_bytes`.
    TotalTextTooLarge,
    /// An image has no bytes.
    ImageEmpty,
    /// An image exceeds `max_image_bytes`.
    ImageTooLarge,
    /// More than `max_images` images.
    TooManyImages,
    /// Images exceed `max_total_image_bytes` in total.
    TotalImageBytesTooLarge,
    /// The image bytes are not of the declared media type.
    ImageMediaTypeMismatch,
    /// A tool-call id is empty, has whitespace or control characters, or is too long.
    InvalidToolCallId,
    /// A tool or proposal name is empty, too long or has disallowed characters.
    InvalidToolName,
    /// Proposal arguments exceed `max_tool_arguments_bytes`.
    ToolArgumentsTooLarge,
    /// Proposal arguments are not valid JSON (or repeat a key, or nest too deeply).
    MalformedToolArguments,
    /// Two proposals in one assistant message share an id.
    DuplicateToolCallId,
    /// A tool message does not follow an assistant message with pending proposals.
    OrphanToolResult,
    /// An assistant message's proposals are not all answered by the following tool messages.
    MissingToolResult,
    /// A tool result answers no pending proposal of the preceding assistant message.
    UnmatchedToolResult,
    /// A proposal is answered twice.
    DuplicateToolResult,
    /// A tool result's name differs from the proposal it answers.
    ToolResultNameMismatch,
    /// A tool result exceeds `max_tool_result_bytes`.
    ToolResultTooLarge,
    /// A JSON tool result is not valid JSON.
    MalformedToolResultJson,
    /// More than `max_tools` tool definitions.
    TooManyTools,
    /// Two tool definitions share a name.
    DuplicateToolName,
    /// A tool description exceeds `max_tool_description_bytes`.
    ToolDescriptionTooLarge,
    /// A tool input schema is not a JSON object (or is malformed).
    InvalidToolSchema,
    /// A schema exceeds `max_schema_bytes`.
    SchemaTooLarge,
    /// A client tool carries provider arguments.
    ProviderArgsOnClientTool,
    /// Provider tool arguments are not a JSON object.
    InvalidProviderArgs,
    /// A named tool choice references no defined tool.
    UnknownNamedTool,
    /// `required` or `named` tool choice without any tool.
    ToolChoiceWithoutTools,
    /// `max_output_tokens` is zero.
    InvalidMaxOutputTokens,
    /// `temperature_milli` is above the maximum.
    InvalidTemperature,
    /// A reasoning label is empty, too long or has disallowed characters.
    InvalidReasoningLabel,
    /// A response-format name is empty, too long or has disallowed characters.
    InvalidResponseFormatName,
    /// A response-format schema is not a JSON object (or is malformed).
    InvalidResponseFormatSchema,
    /// A response-format description exceeds `max_tool_description_bytes`.
    ResponseFormatDescriptionTooLarge,
    /// The affinity key is empty, too long or has whitespace or control characters.
    InvalidAffinityKey,
    /// The attempt budget is zero or above `max_attempt_budget`.
    BudgetOutOfRange,
    /// An event field exceeds its bound.
    EventFieldTooLarge,
    /// An event identifier or name is empty.
    EventFieldEmpty,
    /// The request document could not be decoded (reported by
    /// [`crate::executor::execute_document`]; see the `DecodeError` for the category).
    Undecodable,
}

/// Where in the request the problem is. Indices are zero-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Location {
    /// Message index.
    pub message: Option<usize>,
    /// Part index within the message.
    pub part: Option<usize>,
    /// Tool definition index.
    pub tool: Option<usize>,
}

/// A request validation failure. Contains no request content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidationError {
    /// What was wrong.
    pub code: ValidationCode,
    /// Where.
    pub location: Location,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid request: {:?}", self.code)?;
        if let Some(m) = self.location.message {
            write!(f, " at message {m}")?;
        }
        if let Some(p) = self.location.part {
            write!(f, " part {p}")?;
        }
        if let Some(t) = self.location.tool {
            write!(f, " at tool {t}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationError {}

fn fail(code: ValidationCode) -> ValidationError {
    ValidationError {
        code,
        location: Location::default(),
    }
}

fn at_part(code: ValidationCode, message: usize, part: usize) -> ValidationError {
    ValidationError {
        code,
        location: Location {
            message: Some(message),
            part: Some(part),
            tool: None,
        },
    }
}

fn at_message(code: ValidationCode, message: usize) -> ValidationError {
    ValidationError {
        code,
        location: Location {
            message: Some(message),
            part: None,
            tool: None,
        },
    }
}

fn at_tool(code: ValidationCode, tool: usize) -> ValidationError {
    ValidationError {
        code,
        location: Location {
            message: None,
            part: None,
            tool: Some(tool),
        },
    }
}

/// Tool, proposal and response-format names: `[A-Za-z0-9_.-]`, 1..=`max` bytes.
pub fn is_valid_tool_name(name: &str, limits: &Limits) -> bool {
    !name.is_empty()
        && name.len() <= limits.max_tool_name_bytes
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
}

/// Tool-call ids: printable ASCII without whitespace, 1..=`max` bytes.
pub fn is_valid_tool_call_id(id: &str, limits: &Limits) -> bool {
    !id.is_empty()
        && id.len() <= limits.max_tool_call_id_bytes
        && id.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

fn is_valid_model_id(id: &str) -> bool {
    id.trim() == id && !id.chars().any(char::is_control)
}

/// Reasoning labels (oracle: `ReasoningEffort.Name.parse`): `[A-Za-z0-9._-]`, 1..=max bytes.
fn is_valid_reasoning_label(label: &str, limits: &Limits) -> bool {
    !label.is_empty()
        && label.len() <= limits.max_reasoning_label_bytes
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

fn is_json_object(text: &str, limits: &Limits) -> bool {
    matches!(
        json_check::check(text, limits.max_json_depth),
        Ok(JsonKind::Object)
    )
}

/// Validates a request against `limits`. Returns the first failure in a fixed order:
/// model id, message count, each message in order (parts, content, history pairing),
/// totals, tools, tool choice, options, correlation, budget.
pub fn validate_request(request: &Request, limits: &Limits) -> Result<(), ValidationError> {
    validate_model(request.model.as_str(), limits)?;
    validate_messages(&request.messages, limits)?;
    validate_tools(request, limits)?;
    validate_tool_choice(request)?;
    validate_options(request, limits)?;
    validate_correlation(request, limits)?;
    validate_budget(request, limits)
}

fn validate_model(id: &str, limits: &Limits) -> Result<(), ValidationError> {
    if id.is_empty() {
        return Err(fail(ValidationCode::EmptyModelId));
    }
    if id.len() > limits.max_model_id_bytes {
        return Err(fail(ValidationCode::ModelIdTooLong));
    }
    if !is_valid_model_id(id) {
        return Err(fail(ValidationCode::InvalidModelId));
    }
    Ok(())
}

fn part_allowed(role: Role, part: &Part) -> bool {
    matches!(
        (role, part),
        (Role::System, Part::Text(_))
            | (Role::User, Part::Text(_) | Part::Image(_))
            | (Role::Assistant, Part::Text(_) | Part::ToolCall(_))
            | (Role::Tool, Part::ToolResult(_))
    )
}

/// Proposals of the most recent assistant message that still await results.
struct Pending {
    assistant_index: usize,
    /// id -> (name, answered)
    calls: HashMap<String, (String, bool)>,
    remaining: usize,
}

fn validate_messages(messages: &[Message], limits: &Limits) -> Result<(), ValidationError> {
    if messages.is_empty() {
        return Err(fail(ValidationCode::NoMessages));
    }
    if messages.len() > limits.max_messages {
        return Err(fail(ValidationCode::TooManyMessages));
    }
    let mut total_text: usize = 0;
    let mut images: usize = 0;
    let mut image_bytes: usize = 0;
    let mut pending: Option<Pending> = None;

    for (mi, message) in messages.iter().enumerate() {
        if message.parts.is_empty() {
            return Err(at_message(ValidationCode::EmptyMessage, mi));
        }
        if message.parts.len() > limits.max_parts_per_message {
            return Err(at_message(ValidationCode::TooManyParts, mi));
        }

        // History pairing (oracle: validateToolMessageHistory, gateway_json.zig:445-501).
        match message.role {
            Role::Tool => {
                if pending.is_none() {
                    return Err(at_message(ValidationCode::OrphanToolResult, mi));
                }
            }
            _ => {
                if pending.is_some() {
                    return Err(at_message(ValidationCode::MissingToolResult, mi));
                }
            }
        }

        let mut assistant_calls: Vec<(String, String)> = Vec::new();
        for (pi, part) in message.parts.iter().enumerate() {
            if !part_allowed(message.role, part) {
                return Err(at_part(ValidationCode::PartNotAllowedForRole, mi, pi));
            }
            match part {
                Part::Text(text) => total_text = total_text.saturating_add(text.len()),
                Part::Image(image) => {
                    images += 1;
                    if images > limits.max_images {
                        return Err(at_part(ValidationCode::TooManyImages, mi, pi));
                    }
                    if image.bytes.is_empty() {
                        return Err(at_part(ValidationCode::ImageEmpty, mi, pi));
                    }
                    if image.bytes.len() > limits.max_image_bytes {
                        return Err(at_part(ValidationCode::ImageTooLarge, mi, pi));
                    }
                    image_bytes = image_bytes.saturating_add(image.bytes.len());
                    if image_bytes > limits.max_total_image_bytes {
                        return Err(at_part(ValidationCode::TotalImageBytesTooLarge, mi, pi));
                    }
                    if ImageMediaType::sniff(&image.bytes) != Some(image.media_type) {
                        return Err(at_part(ValidationCode::ImageMediaTypeMismatch, mi, pi));
                    }
                }
                Part::ToolCall(call) => {
                    if !is_valid_tool_call_id(&call.id, limits) {
                        return Err(at_part(ValidationCode::InvalidToolCallId, mi, pi));
                    }
                    if !is_valid_tool_name(&call.name, limits) {
                        return Err(at_part(ValidationCode::InvalidToolName, mi, pi));
                    }
                    if call.arguments_json.len() > limits.max_tool_arguments_bytes {
                        return Err(at_part(ValidationCode::ToolArgumentsTooLarge, mi, pi));
                    }
                    // Any JSON value is accepted, as in the oracle (scalar roots included).
                    if json_check::check(&call.arguments_json, limits.max_json_depth).is_err() {
                        return Err(at_part(ValidationCode::MalformedToolArguments, mi, pi));
                    }
                    if assistant_calls.iter().any(|(id, _)| id == &call.id) {
                        return Err(at_part(ValidationCode::DuplicateToolCallId, mi, pi));
                    }
                    total_text = total_text.saturating_add(call.arguments_json.len());
                    assistant_calls.push((call.id.clone(), call.name.clone()));
                }
                Part::ToolResult(result) => {
                    if !is_valid_tool_call_id(&result.call_id, limits) {
                        return Err(at_part(ValidationCode::InvalidToolCallId, mi, pi));
                    }
                    if !is_valid_tool_name(&result.name, limits) {
                        return Err(at_part(ValidationCode::InvalidToolName, mi, pi));
                    }
                    let output = match &result.output {
                        ToolOutput::Text(t) | ToolOutput::Json(t) | ToolOutput::Failure(t) => t,
                    };
                    if output.len() > limits.max_tool_result_bytes {
                        return Err(at_part(ValidationCode::ToolResultTooLarge, mi, pi));
                    }
                    if let ToolOutput::Json(json) = &result.output
                        && json_check::check(json, limits.max_json_depth).is_err()
                    {
                        return Err(at_part(ValidationCode::MalformedToolResultJson, mi, pi));
                    }
                    total_text = total_text.saturating_add(output.len());
                    // `pending` is Some for tool messages (checked above).
                    if let Some(state) = pending.as_mut() {
                        match state.calls.get_mut(&result.call_id) {
                            None => {
                                return Err(at_part(ValidationCode::UnmatchedToolResult, mi, pi));
                            }
                            Some((_, true)) => {
                                return Err(at_part(ValidationCode::DuplicateToolResult, mi, pi));
                            }
                            Some((name, answered)) => {
                                if name != &result.name {
                                    return Err(at_part(
                                        ValidationCode::ToolResultNameMismatch,
                                        mi,
                                        pi,
                                    ));
                                }
                                *answered = true;
                                state.remaining -= 1;
                            }
                        }
                    }
                }
            }
            if total_text > limits.max_total_text_bytes {
                return Err(at_part(ValidationCode::TotalTextTooLarge, mi, pi));
            }
        }

        if message.role == Role::Tool {
            if pending.as_ref().is_some_and(|p| p.remaining == 0) {
                pending = None;
            }
        } else if !assistant_calls.is_empty() {
            let remaining = assistant_calls.len();
            pending = Some(Pending {
                assistant_index: mi,
                calls: assistant_calls
                    .into_iter()
                    .map(|(id, name)| (id, (name, false)))
                    .collect(),
                remaining,
            });
        }
    }

    if let Some(state) = pending {
        return Err(at_message(
            ValidationCode::MissingToolResult,
            state.assistant_index,
        ));
    }
    Ok(())
}

fn validate_tools(request: &Request, limits: &Limits) -> Result<(), ValidationError> {
    if request.tools.len() > limits.max_tools {
        return Err(fail(ValidationCode::TooManyTools));
    }
    let mut names: HashSet<&str> = HashSet::new();
    for (ti, tool) in request.tools.iter().enumerate() {
        if !is_valid_tool_name(&tool.name, limits) {
            return Err(at_tool(ValidationCode::InvalidToolName, ti));
        }
        if !names.insert(tool.name.as_str()) {
            return Err(at_tool(ValidationCode::DuplicateToolName, ti));
        }
        if tool.description.len() > limits.max_tool_description_bytes {
            return Err(at_tool(ValidationCode::ToolDescriptionTooLarge, ti));
        }
        if tool.input_schema_json.len() > limits.max_schema_bytes {
            return Err(at_tool(ValidationCode::SchemaTooLarge, ti));
        }
        if !is_json_object(&tool.input_schema_json, limits) {
            return Err(at_tool(ValidationCode::InvalidToolSchema, ti));
        }
        match (&tool.execution, &tool.provider_args_json) {
            (ToolExecution::Client, Some(_)) => {
                return Err(at_tool(ValidationCode::ProviderArgsOnClientTool, ti));
            }
            (ToolExecution::Provider, Some(args)) => {
                if args.len() > limits.max_schema_bytes {
                    return Err(at_tool(ValidationCode::SchemaTooLarge, ti));
                }
                if !is_json_object(args, limits) {
                    return Err(at_tool(ValidationCode::InvalidProviderArgs, ti));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_tool_choice(request: &Request) -> Result<(), ValidationError> {
    match &request.tool_choice {
        ToolChoice::Auto | ToolChoice::None => Ok(()),
        ToolChoice::Required if request.tools.is_empty() => {
            Err(fail(ValidationCode::ToolChoiceWithoutTools))
        }
        ToolChoice::Required => Ok(()),
        ToolChoice::Named(_) if request.tools.is_empty() => {
            Err(fail(ValidationCode::ToolChoiceWithoutTools))
        }
        ToolChoice::Named(name) => {
            if request.tools.iter().any(|t| &t.name == name) {
                Ok(())
            } else {
                Err(fail(ValidationCode::UnknownNamedTool))
            }
        }
    }
}

fn validate_options(request: &Request, limits: &Limits) -> Result<(), ValidationError> {
    let options = &request.options;
    if options.max_output_tokens == Some(0) {
        return Err(fail(ValidationCode::InvalidMaxOutputTokens));
    }
    if options
        .temperature_milli
        .is_some_and(|t| t > crate::contract::GenerationOptions::MAX_TEMPERATURE_MILLI)
    {
        return Err(fail(ValidationCode::InvalidTemperature));
    }
    if let ReasoningEffort::Named(label) = &options.reasoning
        && !is_valid_reasoning_label(label, limits)
    {
        return Err(fail(ValidationCode::InvalidReasoningLabel));
    }
    if let Some(format) = &options.response_format {
        if !is_valid_tool_name(&format.name, limits) {
            return Err(fail(ValidationCode::InvalidResponseFormatName));
        }
        if format.description.len() > limits.max_tool_description_bytes {
            return Err(fail(ValidationCode::ResponseFormatDescriptionTooLarge));
        }
        if format.schema_json.len() > limits.max_schema_bytes {
            return Err(fail(ValidationCode::SchemaTooLarge));
        }
        if !is_json_object(&format.schema_json, limits) {
            return Err(fail(ValidationCode::InvalidResponseFormatSchema));
        }
    }
    Ok(())
}

fn validate_correlation(request: &Request, limits: &Limits) -> Result<(), ValidationError> {
    if let Some(key) = &request.correlation.affinity_key
        && (key.is_empty()
            || key.len() > limits.max_affinity_key_bytes
            || !key.bytes().all(|b| (0x21..=0x7e).contains(&b)))
    {
        return Err(fail(ValidationCode::InvalidAffinityKey));
    }
    Ok(())
}

fn validate_budget(request: &Request, limits: &Limits) -> Result<(), ValidationError> {
    if let Some(budget) = request.budget
        && (budget.0.is_zero() || budget.0 > limits.max_attempt_budget)
    {
        return Err(fail(ValidationCode::BudgetOutOfRange));
    }
    Ok(())
}

/// Checks that every field of a capability event is within its bound.
///
/// This is size validation only. Semantic normalization of events (proposal
/// identity, malformed-argument replacement, ordering, terminal rules) belongs to
/// the executor, so malformed proposal arguments are *not* rejected here.
pub fn validate_event_bounds(event: &Event, limits: &Limits) -> Result<(), ValidationError> {
    let too_large = || Err(fail(ValidationCode::EventFieldTooLarge));
    let empty = || Err(fail(ValidationCode::EventFieldEmpty));
    let id_ok = |id: &str| id.len() <= limits.max_tool_call_id_bytes;
    let name_ok = |name: &str| name.len() <= limits.max_tool_name_bytes;
    match event {
        Event::Metadata(m) => {
            if m.model_reported
                .as_ref()
                .is_some_and(|s| s.len() > limits.max_reported_model_bytes)
                || m.generation_id
                    .as_ref()
                    .is_some_and(|s| s.len() > limits.max_opaque_token_bytes)
            {
                return too_large();
            }
        }
        Event::TextDelta(_) | Event::ReasoningDelta(_) => {}
        Event::ToolInputStart { id, name } => {
            if id.is_empty() || name.is_empty() {
                return empty();
            }
            if !id_ok(id) || !name_ok(name) {
                return too_large();
            }
        }
        Event::ToolInputDelta { id, delta } => {
            if id.is_empty() {
                return empty();
            }
            if !id_ok(id) || delta.len() > limits.max_tool_arguments_bytes {
                return too_large();
            }
        }
        Event::ToolInputEnd { id } => {
            if id.is_empty() {
                return empty();
            }
            if !id_ok(id) {
                return too_large();
            }
        }
        Event::ToolCall(call) => {
            // Empty ids are an identity state the executor records, not a size error.
            if !id_ok(&call.id)
                || !name_ok(&call.name)
                || call.arguments_json.len() > limits.max_tool_arguments_bytes
            {
                return too_large();
            }
        }
        Event::ProviderToolRecord(record) => {
            if !id_ok(&record.call_id)
                || !name_ok(&record.name)
                || record.input_json.len() > limits.max_tool_arguments_bytes
                || record
                    .result_json
                    .as_ref()
                    .is_some_and(|r| r.len() > limits.max_tool_result_bytes)
            {
                return too_large();
            }
        }
        Event::Usage(usage) => {
            if usage
                .cost
                .as_ref()
                .is_some_and(|c| c.len() > limits.max_opaque_token_bytes)
            {
                return too_large();
            }
        }
        Event::Finish { .. } => {}
    }
    Ok(())
}
