//! Versioned JSON encoding of the contract (`"v": 1`).
//!
//! JSON is an interchange format only. Documents are decoded into private boundary
//! types and converted into the semantic types in [`crate::contract`]; nothing
//! outside this module sees JSON shapes. Decoding is strict and deterministic:
//!
//! 1. the byte length is checked against [`Limits`] before any parsing;
//! 2. the bytes must be valid UTF-8;
//! 3. the `"v"` field is read and must equal [`CONTRACT_VERSION`];
//! 4. the document is decoded with unknown fields, unknown tags, invalid enum
//!    values, duplicate fields, missing required fields and trailing data rejected;
//! 5. decoded values are checked against their bounds.
//!
//! Every top-level document carries `"v"`: requests are flat objects with `"v"`;
//! events, outcomes and capability errors are `{"v":1,"<kind>":{...}}` envelopes.
//! Tagged unions use an explicit `"type"` field.
//!
//! [`DecodeError`] carries a category and position only, never input content.

use std::fmt;
use std::time::Duration;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};

use crate::base64;
use crate::contract::{
    AttemptBudget, CONTRACT_VERSION, CacheHint, Cancellation, CapabilityError, Completion,
    Correlation, DeliveryCertainty, Detail, ErrorKind, Event, Failure, FailureOrigin, FinishReason,
    GenerationOptions, ImageData, ImageMediaType, Limits, Message, Metadata, ModelRef, Outcome,
    Part, PartialOutput, ProviderToolRecord, ReasoningEffort, RemoteCancellation, Request,
    RequestId, ResponseFormat, Role, Speed, ToolCallProposal, ToolChoice, ToolDefinition,
    ToolExecution, ToolOutput, ToolResult, Usage,
};
use crate::diagnostics::TerminalCause;
use crate::executor::ExecutionReport;
use crate::normalize::{
    ArgumentIntegrity, ArgumentSource, Disposition, ProposalCheck, ProposalIdentity,
};
use crate::validate::validate_event_bounds;

/// Category of a decoding failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecodeErrorKind {
    /// The document exceeds its byte limit; nothing was parsed.
    TooLarge,
    /// The document is not valid UTF-8.
    InvalidUtf8,
    /// The document is not valid JSON.
    Syntax,
    /// Data follows the JSON document.
    TrailingData,
    /// `"v"` is not the supported contract version.
    UnsupportedVersion,
    /// An unknown `"type"` tag or enum value.
    UnknownVariant,
    /// A field that the contract does not define.
    UnknownField,
    /// A required field is absent.
    MissingField,
    /// A field appears twice.
    DuplicateField,
    /// A field has the wrong JSON type.
    InvalidType,
    /// A field has an invalid value (bad hex, bad base64, out of range).
    InvalidValue,
    /// A decoded value exceeds a contract bound.
    LimitExceeded,
}

/// A decoding failure. Contains no input content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeError {
    /// Category.
    pub kind: DecodeErrorKind,
    /// 1-based line of the failure, when known.
    pub line: usize,
    /// 1-based column of the failure, when known.
    pub column: usize,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "contract decode error: {:?} at line {} column {}",
            self.kind, self.line, self.column
        )
    }
}

impl std::error::Error for DecodeError {}

fn error(kind: DecodeErrorKind) -> DecodeError {
    DecodeError {
        kind,
        line: 0,
        column: 0,
    }
}

/// Maps a serde_json error to a category by its stable message prefix, discarding
/// the message itself (it can quote input).
fn classify(err: &serde_json::Error) -> DecodeError {
    let message = err.to_string();
    let kind = match err.classify() {
        serde_json::error::Category::Syntax | serde_json::error::Category::Eof => {
            if message.starts_with("trailing characters") {
                DecodeErrorKind::TrailingData
            } else {
                DecodeErrorKind::Syntax
            }
        }
        serde_json::error::Category::Io => DecodeErrorKind::Syntax,
        serde_json::error::Category::Data => {
            if message.starts_with("unknown variant") {
                DecodeErrorKind::UnknownVariant
            } else if message.starts_with("unknown field") {
                DecodeErrorKind::UnknownField
            } else if message.starts_with("missing field") {
                DecodeErrorKind::MissingField
            } else if message.starts_with("duplicate field") {
                DecodeErrorKind::DuplicateField
            } else if message.starts_with("invalid type") {
                DecodeErrorKind::InvalidType
            } else {
                DecodeErrorKind::InvalidValue
            }
        }
    };
    DecodeError {
        kind,
        line: err.line(),
        column: err.column(),
    }
}

#[derive(Deserialize)]
struct VersionProbe {
    v: u32,
}

fn decode_document<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
    max_bytes: usize,
) -> Result<T, DecodeError> {
    if bytes.len() > max_bytes {
        return Err(error(DecodeErrorKind::TooLarge));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| error(DecodeErrorKind::InvalidUtf8))?;
    let probe: VersionProbe = serde_json::from_str(text).map_err(|e| classify(&e))?;
    if probe.v != CONTRACT_VERSION {
        return Err(error(DecodeErrorKind::UnsupportedVersion));
    }
    serde_json::from_str(text).map_err(|e| classify(&e))
}

fn encode_document<T: Serialize>(document: &T) -> Vec<u8> {
    // Boundary types contain only strings, integers, booleans, sequences and
    // structs, which serde_json always serializes.
    serde_json::to_vec(document).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Scalar helpers

fn serialize_request_id<S: Serializer>(id: &[u8; 16], s: S) -> Result<S::Ok, S::Error> {
    let mut hex = String::with_capacity(32);
    for byte in id {
        hex.push_str(&format!("{byte:02x}"));
    }
    s.serialize_str(&hex)
}

fn deserialize_request_id<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 16], D::Error> {
    let text = String::deserialize(d)?;
    let bytes = text.as_bytes();
    if bytes.len() != 32
        || !bytes
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
    {
        return Err(de::Error::invalid_value(
            de::Unexpected::Other("request id"),
            &"32 lowercase hex digits",
        ));
    }
    let mut out = [0u8; 16];
    for (i, slot) in out.iter_mut().enumerate() {
        let pair = &text[i * 2..i * 2 + 2];
        *slot = u8::from_str_radix(pair, 16)
            .map_err(|_| de::Error::invalid_value(de::Unexpected::Other("request id"), &"hex"))?;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Request documents

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestDoc {
    v: u32,
    model: String,
    messages: Vec<MessageDoc>,
    #[serde(default)]
    tools: Vec<ToolDoc>,
    #[serde(default)]
    tool_choice: ToolChoiceDoc,
    #[serde(default)]
    options: OptionsDoc,
    correlation: CorrelationDoc,
    #[serde(default)]
    budget_ms: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum RoleDoc {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Serialize, Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
enum CacheDoc {
    #[default]
    Default,
    NoCache,
    Breakpoint,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageDoc {
    role: RoleDoc,
    #[serde(default)]
    cache: CacheDoc,
    parts: Vec<PartDoc>,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
enum MediaTypeDoc {
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/jpeg")]
    Jpeg,
    #[serde(rename = "image/gif")]
    Gif,
    #[serde(rename = "image/webp")]
    Webp,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum PartDoc {
    Text {
        text: String,
    },
    Image {
        media_type: MediaTypeDoc,
        data_base64: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments_json: String,
    },
    ToolResult {
        call_id: String,
        name: String,
        output: OutputDoc,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum OutputDoc {
    Text { text: String },
    Json { json: String },
    Failure { message: String },
}

#[derive(Serialize, Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
enum ExecutionDoc {
    #[default]
    Client,
    Provider,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolDoc {
    name: String,
    description: String,
    input_schema_json: String,
    #[serde(default)]
    execution: ExecutionDoc,
    #[serde(default)]
    provider_args_json: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ToolChoiceDoc {
    // Empty struct variants, not unit variants: serde ignores unknown fields on
    // internally tagged unit variants, and the contract must reject them.
    Auto {},
    None {},
    Required {},
    Named { name: String },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ReasoningDoc {
    Default {},
    Named { label: String },
}

impl Default for ToolChoiceDoc {
    fn default() -> Self {
        ToolChoiceDoc::Auto {}
    }
}

impl Default for ReasoningDoc {
    fn default() -> Self {
        ReasoningDoc::Default {}
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
enum SpeedDoc {
    #[default]
    Standard,
    Fast,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponseFormatDoc {
    name: String,
    description: String,
    schema_json: String,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct OptionsDoc {
    #[serde(default)]
    max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    temperature_milli: Option<u16>,
    #[serde(default)]
    reasoning: ReasoningDoc,
    #[serde(default)]
    speed: SpeedDoc,
    #[serde(default)]
    response_format: Option<ResponseFormatDoc>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorrelationDoc {
    #[serde(
        serialize_with = "serialize_request_id",
        deserialize_with = "deserialize_request_id"
    )]
    request_id: [u8; 16],
    #[serde(default)]
    affinity_key: Option<String>,
}

/// Decodes a request document. Structural validation is separate: see
/// [`crate::validate::validate_request`] and [`parse_request`].
pub fn decode_request(bytes: &[u8], limits: &Limits) -> Result<Request, DecodeError> {
    let doc: RequestDoc = decode_document(bytes, limits.max_request_bytes)?;
    request_from_doc(doc, limits)
}

/// Encodes a request document in canonical form (every field present).
pub fn encode_request(request: &Request) -> Vec<u8> {
    encode_document(&request_to_doc(request))
}

fn request_from_doc(doc: RequestDoc, limits: &Limits) -> Result<Request, DecodeError> {
    let mut messages = Vec::with_capacity(doc.messages.len().min(limits.max_messages));
    for message in doc.messages {
        let mut parts = Vec::with_capacity(message.parts.len());
        for part in message.parts {
            parts.push(part_from_doc(part, limits)?);
        }
        messages.push(Message {
            role: match message.role {
                RoleDoc::System => Role::System,
                RoleDoc::User => Role::User,
                RoleDoc::Assistant => Role::Assistant,
                RoleDoc::Tool => Role::Tool,
            },
            parts,
            cache: match message.cache {
                CacheDoc::Default => CacheHint::Default,
                CacheDoc::NoCache => CacheHint::NoCache,
                CacheDoc::Breakpoint => CacheHint::Breakpoint,
            },
        });
    }
    let tools = doc
        .tools
        .into_iter()
        .map(|t| ToolDefinition {
            name: t.name,
            description: t.description,
            input_schema_json: t.input_schema_json,
            execution: match t.execution {
                ExecutionDoc::Client => ToolExecution::Client,
                ExecutionDoc::Provider => ToolExecution::Provider,
            },
            provider_args_json: t.provider_args_json,
        })
        .collect();
    let options = doc.options;
    Ok(Request {
        model: ModelRef(doc.model),
        messages,
        tools,
        tool_choice: match doc.tool_choice {
            ToolChoiceDoc::Auto {} => ToolChoice::Auto,
            ToolChoiceDoc::None {} => ToolChoice::None,
            ToolChoiceDoc::Required {} => ToolChoice::Required,
            ToolChoiceDoc::Named { name } => ToolChoice::Named(name),
        },
        options: GenerationOptions {
            max_output_tokens: options.max_output_tokens,
            temperature_milli: options.temperature_milli,
            reasoning: match options.reasoning {
                ReasoningDoc::Default {} => ReasoningEffort::Default,
                ReasoningDoc::Named { label } => ReasoningEffort::Named(label),
            },
            speed: match options.speed {
                SpeedDoc::Standard => Speed::Standard,
                SpeedDoc::Fast => Speed::Fast,
            },
            response_format: options.response_format.map(|f| ResponseFormat {
                name: f.name,
                description: f.description,
                schema_json: f.schema_json,
            }),
        },
        correlation: Correlation {
            request_id: RequestId(doc.correlation.request_id),
            affinity_key: doc.correlation.affinity_key,
        },
        budget: doc
            .budget_ms
            .map(|ms| AttemptBudget(Duration::from_millis(ms))),
    })
}

fn part_from_doc(part: PartDoc, limits: &Limits) -> Result<Part, DecodeError> {
    Ok(match part {
        PartDoc::Text { text } => Part::Text(text),
        PartDoc::Image {
            media_type,
            data_base64,
        } => {
            // Bound the decoded size before allocating it; the exact limit check is
            // repeated by validation with a location.
            let bytes = base64::decode(&data_base64, limits.max_image_bytes).map_err(|e| {
                error(match e {
                    base64::Base64Error::Invalid => DecodeErrorKind::InvalidValue,
                    base64::Base64Error::TooLarge => DecodeErrorKind::LimitExceeded,
                })
            })?;
            Part::Image(ImageData {
                media_type: match media_type {
                    MediaTypeDoc::Png => ImageMediaType::Png,
                    MediaTypeDoc::Jpeg => ImageMediaType::Jpeg,
                    MediaTypeDoc::Gif => ImageMediaType::Gif,
                    MediaTypeDoc::Webp => ImageMediaType::Webp,
                },
                bytes,
            })
        }
        PartDoc::ToolCall {
            id,
            name,
            arguments_json,
        } => Part::ToolCall(ToolCallProposal {
            id,
            name,
            arguments_json,
        }),
        PartDoc::ToolResult {
            call_id,
            name,
            output,
        } => Part::ToolResult(ToolResult {
            call_id,
            name,
            output: match output {
                OutputDoc::Text { text } => ToolOutput::Text(text),
                OutputDoc::Json { json } => ToolOutput::Json(json),
                OutputDoc::Failure { message } => ToolOutput::Failure(message),
            },
        }),
    })
}

fn request_to_doc(request: &Request) -> RequestDoc {
    RequestDoc {
        v: CONTRACT_VERSION,
        model: request.model.0.clone(),
        messages: request
            .messages
            .iter()
            .map(|m| MessageDoc {
                role: match m.role {
                    Role::System => RoleDoc::System,
                    Role::User => RoleDoc::User,
                    Role::Assistant => RoleDoc::Assistant,
                    Role::Tool => RoleDoc::Tool,
                },
                cache: match m.cache {
                    CacheHint::Default => CacheDoc::Default,
                    CacheHint::NoCache => CacheDoc::NoCache,
                    CacheHint::Breakpoint => CacheDoc::Breakpoint,
                },
                parts: m.parts.iter().map(part_to_doc).collect(),
            })
            .collect(),
        tools: request
            .tools
            .iter()
            .map(|t| ToolDoc {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema_json: t.input_schema_json.clone(),
                execution: match t.execution {
                    ToolExecution::Client => ExecutionDoc::Client,
                    ToolExecution::Provider => ExecutionDoc::Provider,
                },
                provider_args_json: t.provider_args_json.clone(),
            })
            .collect(),
        tool_choice: match &request.tool_choice {
            ToolChoice::Auto => ToolChoiceDoc::Auto {},
            ToolChoice::None => ToolChoiceDoc::None {},
            ToolChoice::Required => ToolChoiceDoc::Required {},
            ToolChoice::Named(name) => ToolChoiceDoc::Named { name: name.clone() },
        },
        options: OptionsDoc {
            max_output_tokens: request.options.max_output_tokens,
            temperature_milli: request.options.temperature_milli,
            reasoning: match &request.options.reasoning {
                ReasoningEffort::Default => ReasoningDoc::Default {},
                ReasoningEffort::Named(label) => ReasoningDoc::Named {
                    label: label.clone(),
                },
            },
            speed: match request.options.speed {
                Speed::Standard => SpeedDoc::Standard,
                Speed::Fast => SpeedDoc::Fast,
            },
            response_format: request
                .options
                .response_format
                .as_ref()
                .map(|f| ResponseFormatDoc {
                    name: f.name.clone(),
                    description: f.description.clone(),
                    schema_json: f.schema_json.clone(),
                }),
        },
        correlation: CorrelationDoc {
            request_id: request.correlation.request_id.0,
            affinity_key: request.correlation.affinity_key.clone(),
        },
        // Durations above u64::MAX milliseconds are rejected by validation long
        // before this point; saturate rather than wrap.
        budget_ms: request
            .budget
            .map(|b| u64::try_from(b.0.as_millis()).unwrap_or(u64::MAX)),
    }
}

fn part_to_doc(part: &Part) -> PartDoc {
    match part {
        Part::Text(text) => PartDoc::Text { text: text.clone() },
        Part::Image(image) => PartDoc::Image {
            media_type: match image.media_type {
                ImageMediaType::Png => MediaTypeDoc::Png,
                ImageMediaType::Jpeg => MediaTypeDoc::Jpeg,
                ImageMediaType::Gif => MediaTypeDoc::Gif,
                ImageMediaType::Webp => MediaTypeDoc::Webp,
            },
            data_base64: base64::encode(&image.bytes),
        },
        Part::ToolCall(call) => PartDoc::ToolCall {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments_json: call.arguments_json.clone(),
        },
        Part::ToolResult(result) => PartDoc::ToolResult {
            call_id: result.call_id.clone(),
            name: result.name.clone(),
            output: match &result.output {
                ToolOutput::Text(text) => OutputDoc::Text { text: text.clone() },
                ToolOutput::Json(json) => OutputDoc::Json { json: json.clone() },
                ToolOutput::Failure(message) => OutputDoc::Failure {
                    message: message.clone(),
                },
            },
        },
    }
}

/// Why a request was rejected by [`parse_request`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestRejection {
    /// The document could not be decoded.
    Decode(DecodeError),
    /// The decoded request failed validation.
    Invalid(crate::validate::ValidationError),
}

impl fmt::Display for RequestRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequestRejection::Decode(e) => e.fmt(f),
            RequestRejection::Invalid(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for RequestRejection {}

/// Decodes and validates a request: the single entry point adapters use.
pub fn parse_request(bytes: &[u8], limits: &Limits) -> Result<Request, RequestRejection> {
    let request = decode_request(bytes, limits).map_err(RequestRejection::Decode)?;
    crate::validate::validate_request(&request, limits).map_err(RequestRejection::Invalid)?;
    Ok(request)
}

// ---------------------------------------------------------------------------
// Event documents

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventEnvelope {
    v: u32,
    event: EventDoc,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct MetadataDoc {
    #[serde(default)]
    model_reported: Option<String>,
    #[serde(default)]
    generation_id: Option<String>,
    #[serde(default)]
    provider_timestamp_ms: Option<i64>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct UsageDoc {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_read_tokens: Option<u64>,
    #[serde(default)]
    cache_write_tokens: Option<u64>,
    #[serde(default)]
    reasoning_tokens: Option<u64>,
    #[serde(default)]
    cost: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum FinishReasonDoc {
    Stop,
    Length,
    ContentFilter,
    ToolCalls,
    ProviderError,
    Other,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposalDoc {
    id: String,
    name: String,
    arguments_json: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderRecordDoc {
    call_id: String,
    name: String,
    input_json: String,
    #[serde(default)]
    result_json: Option<String>,
    is_final: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum EventDoc {
    Metadata(MetadataDoc),
    TextDelta { text: String },
    ReasoningDelta { text: String },
    ToolInputStart { id: String, name: String },
    ToolInputDelta { id: String, delta: String },
    ToolInputEnd { id: String },
    ToolCall(ProposalDoc),
    ProviderToolRecord(ProviderRecordDoc),
    Usage(UsageDoc),
    Finish { reason: FinishReasonDoc },
}

fn finish_from(doc: FinishReasonDoc) -> FinishReason {
    match doc {
        FinishReasonDoc::Stop => FinishReason::Stop,
        FinishReasonDoc::Length => FinishReason::Length,
        FinishReasonDoc::ContentFilter => FinishReason::ContentFilter,
        FinishReasonDoc::ToolCalls => FinishReason::ToolCalls,
        FinishReasonDoc::ProviderError => FinishReason::ProviderError,
        FinishReasonDoc::Other => FinishReason::Other,
    }
}

fn finish_to(reason: FinishReason) -> FinishReasonDoc {
    match reason {
        FinishReason::Stop => FinishReasonDoc::Stop,
        FinishReason::Length => FinishReasonDoc::Length,
        FinishReason::ContentFilter => FinishReasonDoc::ContentFilter,
        FinishReason::ToolCalls => FinishReasonDoc::ToolCalls,
        FinishReason::ProviderError => FinishReasonDoc::ProviderError,
        FinishReason::Other => FinishReasonDoc::Other,
    }
}

fn metadata_from(doc: MetadataDoc) -> Metadata {
    Metadata {
        model_reported: doc.model_reported,
        generation_id: doc.generation_id,
        provider_timestamp_ms: doc.provider_timestamp_ms,
    }
}

fn metadata_to(m: &Metadata) -> MetadataDoc {
    MetadataDoc {
        model_reported: m.model_reported.clone(),
        generation_id: m.generation_id.clone(),
        provider_timestamp_ms: m.provider_timestamp_ms,
    }
}

fn usage_from(doc: UsageDoc) -> Usage {
    Usage {
        input_tokens: doc.input_tokens,
        output_tokens: doc.output_tokens,
        cache_read_tokens: doc.cache_read_tokens,
        cache_write_tokens: doc.cache_write_tokens,
        reasoning_tokens: doc.reasoning_tokens,
        cost: doc.cost,
    }
}

fn usage_to(u: &Usage) -> UsageDoc {
    UsageDoc {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cache_read_tokens: u.cache_read_tokens,
        cache_write_tokens: u.cache_write_tokens,
        reasoning_tokens: u.reasoning_tokens,
        cost: u.cost.clone(),
    }
}

fn proposal_from(doc: ProposalDoc) -> ToolCallProposal {
    ToolCallProposal {
        id: doc.id,
        name: doc.name,
        arguments_json: doc.arguments_json,
    }
}

fn proposal_to(p: &ToolCallProposal) -> ProposalDoc {
    ProposalDoc {
        id: p.id.clone(),
        name: p.name.clone(),
        arguments_json: p.arguments_json.clone(),
    }
}

fn record_from(doc: ProviderRecordDoc) -> ProviderToolRecord {
    ProviderToolRecord {
        call_id: doc.call_id,
        name: doc.name,
        input_json: doc.input_json,
        result_json: doc.result_json,
        is_final: doc.is_final,
    }
}

fn record_to(r: &ProviderToolRecord) -> ProviderRecordDoc {
    ProviderRecordDoc {
        call_id: r.call_id.clone(),
        name: r.name.clone(),
        input_json: r.input_json.clone(),
        result_json: r.result_json.clone(),
        is_final: r.is_final,
    }
}

fn bound_error(e: crate::validate::ValidationError) -> DecodeError {
    error(match e.code {
        crate::validate::ValidationCode::EventFieldEmpty => DecodeErrorKind::InvalidValue,
        _ => DecodeErrorKind::LimitExceeded,
    })
}

/// Decodes one event document and checks its field bounds.
pub fn decode_event(bytes: &[u8], limits: &Limits) -> Result<Event, DecodeError> {
    let envelope: EventEnvelope = decode_document(bytes, limits.max_event_bytes)?;
    let event = match envelope.event {
        EventDoc::Metadata(m) => Event::Metadata(metadata_from(m)),
        EventDoc::TextDelta { text } => Event::TextDelta(text),
        EventDoc::ReasoningDelta { text } => Event::ReasoningDelta(text),
        EventDoc::ToolInputStart { id, name } => Event::ToolInputStart { id, name },
        EventDoc::ToolInputDelta { id, delta } => Event::ToolInputDelta { id, delta },
        EventDoc::ToolInputEnd { id } => Event::ToolInputEnd { id },
        EventDoc::ToolCall(p) => Event::ToolCall(proposal_from(p)),
        EventDoc::ProviderToolRecord(r) => Event::ProviderToolRecord(record_from(r)),
        EventDoc::Usage(u) => Event::Usage(usage_from(u)),
        EventDoc::Finish { reason } => Event::Finish {
            reason: finish_from(reason),
        },
    };
    validate_event_bounds(&event, limits).map_err(bound_error)?;
    Ok(event)
}

/// Encodes one event document.
pub fn encode_event(event: &Event) -> Vec<u8> {
    let doc = match event {
        Event::Metadata(m) => EventDoc::Metadata(metadata_to(m)),
        Event::TextDelta(text) => EventDoc::TextDelta { text: text.clone() },
        Event::ReasoningDelta(text) => EventDoc::ReasoningDelta { text: text.clone() },
        Event::ToolInputStart { id, name } => EventDoc::ToolInputStart {
            id: id.clone(),
            name: name.clone(),
        },
        Event::ToolInputDelta { id, delta } => EventDoc::ToolInputDelta {
            id: id.clone(),
            delta: delta.clone(),
        },
        Event::ToolInputEnd { id } => EventDoc::ToolInputEnd { id: id.clone() },
        Event::ToolCall(p) => EventDoc::ToolCall(proposal_to(p)),
        Event::ProviderToolRecord(r) => EventDoc::ProviderToolRecord(record_to(r)),
        Event::Usage(u) => EventDoc::Usage(usage_to(u)),
        Event::Finish { reason } => EventDoc::Finish {
            reason: finish_to(*reason),
        },
    };
    encode_document(&EventEnvelope {
        v: CONTRACT_VERSION,
        event: doc,
    })
}

// ---------------------------------------------------------------------------
// Errors and outcomes

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum DeliveryDoc {
    DefinitelyUnsent,
    PossiblySent,
    ConfirmedResponse,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ErrorKindDoc {
    InvalidRequest,
    CapabilityUnavailable,
    Unauthenticated,
    Forbidden,
    ModelUnavailable,
    RateLimited,
    ProviderUnavailable,
    Transport,
    SystemResumed,
    Timeout,
    Cancelled,
    ProtocolViolation,
    ResourceLimit,
    Internal,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum OriginDoc {
    Executor,
    Capability,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum RemoteDoc {
    NotStarted,
    Unconfirmed,
    Confirmed,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialDoc {
    content: String,
    content_truncated: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityErrorDoc {
    kind: ErrorKindDoc,
    delivery: DeliveryDoc,
    #[serde(default)]
    retry_after_ms: Option<u64>,
    #[serde(default)]
    status_hint: Option<u16>,
    #[serde(default)]
    detail: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilityErrorEnvelope {
    v: u32,
    error: CapabilityErrorDoc,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletionDoc {
    content: String,
    content_truncated: bool,
    reasoning_bytes: u64,
    proposals: Vec<ProposalDoc>,
    provider_records: Vec<ProviderRecordDoc>,
    finish_reason: FinishReasonDoc,
    #[serde(default)]
    usage: Option<UsageDoc>,
    #[serde(default)]
    metadata: MetadataDoc,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FailureDoc {
    origin: OriginDoc,
    kind: ErrorKindDoc,
    delivery: DeliveryDoc,
    #[serde(default)]
    retry_after_ms: Option<u64>,
    #[serde(default)]
    status_hint: Option<u16>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    partial: Option<PartialDoc>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CancellationDoc {
    delivery: DeliveryDoc,
    remote: RemoteDoc,
    #[serde(default)]
    partial: Option<PartialDoc>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum OutcomeDoc {
    Completed(CompletionDoc),
    Failed(FailureDoc),
    Cancelled(CancellationDoc),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutcomeEnvelope {
    v: u32,
    outcome: OutcomeDoc,
}

fn delivery_from(doc: DeliveryDoc) -> DeliveryCertainty {
    match doc {
        DeliveryDoc::DefinitelyUnsent => DeliveryCertainty::DefinitelyUnsent,
        DeliveryDoc::PossiblySent => DeliveryCertainty::PossiblySent,
        DeliveryDoc::ConfirmedResponse => DeliveryCertainty::ConfirmedResponse,
    }
}

fn delivery_to(d: DeliveryCertainty) -> DeliveryDoc {
    match d {
        DeliveryCertainty::DefinitelyUnsent => DeliveryDoc::DefinitelyUnsent,
        DeliveryCertainty::PossiblySent => DeliveryDoc::PossiblySent,
        DeliveryCertainty::ConfirmedResponse => DeliveryDoc::ConfirmedResponse,
    }
}

fn kind_from(doc: ErrorKindDoc) -> ErrorKind {
    match doc {
        ErrorKindDoc::InvalidRequest => ErrorKind::InvalidRequest,
        ErrorKindDoc::CapabilityUnavailable => ErrorKind::CapabilityUnavailable,
        ErrorKindDoc::Unauthenticated => ErrorKind::Unauthenticated,
        ErrorKindDoc::Forbidden => ErrorKind::Forbidden,
        ErrorKindDoc::ModelUnavailable => ErrorKind::ModelUnavailable,
        ErrorKindDoc::RateLimited => ErrorKind::RateLimited,
        ErrorKindDoc::ProviderUnavailable => ErrorKind::ProviderUnavailable,
        ErrorKindDoc::Transport => ErrorKind::Transport,
        ErrorKindDoc::SystemResumed => ErrorKind::SystemResumed,
        ErrorKindDoc::Timeout => ErrorKind::Timeout,
        ErrorKindDoc::Cancelled => ErrorKind::Cancelled,
        ErrorKindDoc::ProtocolViolation => ErrorKind::ProtocolViolation,
        ErrorKindDoc::ResourceLimit => ErrorKind::ResourceLimit,
        ErrorKindDoc::Internal => ErrorKind::Internal,
    }
}

fn kind_to(kind: ErrorKind) -> ErrorKindDoc {
    match kind {
        ErrorKind::InvalidRequest => ErrorKindDoc::InvalidRequest,
        ErrorKind::CapabilityUnavailable => ErrorKindDoc::CapabilityUnavailable,
        ErrorKind::Unauthenticated => ErrorKindDoc::Unauthenticated,
        ErrorKind::Forbidden => ErrorKindDoc::Forbidden,
        ErrorKind::ModelUnavailable => ErrorKindDoc::ModelUnavailable,
        ErrorKind::RateLimited => ErrorKindDoc::RateLimited,
        ErrorKind::ProviderUnavailable => ErrorKindDoc::ProviderUnavailable,
        ErrorKind::Transport => ErrorKindDoc::Transport,
        ErrorKind::SystemResumed => ErrorKindDoc::SystemResumed,
        ErrorKind::Timeout => ErrorKindDoc::Timeout,
        ErrorKind::Cancelled => ErrorKindDoc::Cancelled,
        ErrorKind::ProtocolViolation => ErrorKindDoc::ProtocolViolation,
        ErrorKind::ResourceLimit => ErrorKindDoc::ResourceLimit,
        ErrorKind::Internal => ErrorKindDoc::Internal,
    }
}

fn detail_from(raw: Option<String>, limits: &Limits) -> Result<Option<Detail>, DecodeError> {
    match raw {
        None => Ok(None),
        Some(text) if text.len() > limits.max_detail_bytes => {
            Err(error(DecodeErrorKind::LimitExceeded))
        }
        Some(text) => Ok(Some(Detail::new(&text, limits))),
    }
}

fn partial_from(
    doc: Option<PartialDoc>,
    limits: &Limits,
) -> Result<Option<PartialOutput>, DecodeError> {
    match doc {
        None => Ok(None),
        Some(p) if p.content.len() > limits.max_completion_bytes => {
            Err(error(DecodeErrorKind::LimitExceeded))
        }
        Some(p) => Ok(Some(PartialOutput {
            content: p.content,
            content_truncated: p.content_truncated,
        })),
    }
}

fn partial_to(p: &Option<PartialOutput>) -> Option<PartialDoc> {
    p.as_ref().map(|p| PartialDoc {
        content: p.content.clone(),
        content_truncated: p.content_truncated,
    })
}

fn retry_from(ms: Option<u64>) -> Option<Duration> {
    ms.map(Duration::from_millis)
}

fn retry_to(d: Option<Duration>) -> Option<u64> {
    d.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Decodes a capability-error document.
pub fn decode_capability_error(
    bytes: &[u8],
    limits: &Limits,
) -> Result<CapabilityError, DecodeError> {
    let envelope: CapabilityErrorEnvelope = decode_document(bytes, limits.max_event_bytes)?;
    let doc = envelope.error;
    Ok(CapabilityError {
        kind: kind_from(doc.kind),
        delivery: delivery_from(doc.delivery),
        retry_after: retry_from(doc.retry_after_ms),
        status_hint: doc.status_hint,
        detail: detail_from(doc.detail, limits)?,
    })
}

/// Encodes a capability-error document.
pub fn encode_capability_error(error: &CapabilityError) -> Vec<u8> {
    encode_document(&CapabilityErrorEnvelope {
        v: CONTRACT_VERSION,
        error: CapabilityErrorDoc {
            kind: kind_to(error.kind),
            delivery: delivery_to(error.delivery),
            retry_after_ms: retry_to(error.retry_after),
            status_hint: error.status_hint,
            detail: error.detail.as_ref().map(|d| d.as_str().to_owned()),
        },
    })
}

/// Decodes an outcome document and checks its bounds.
pub fn decode_outcome(bytes: &[u8], limits: &Limits) -> Result<Outcome, DecodeError> {
    let envelope: OutcomeEnvelope = decode_document(bytes, limits.max_event_bytes)?;
    Ok(match envelope.outcome {
        OutcomeDoc::Completed(c) => {
            if c.content.len() > limits.max_completion_bytes
                || c.proposals.len() > limits.max_proposals
                || c.provider_records.len() > limits.max_proposals
            {
                return Err(error(DecodeErrorKind::LimitExceeded));
            }
            let completion = Completion {
                content: c.content,
                content_truncated: c.content_truncated,
                reasoning_bytes: c.reasoning_bytes,
                proposals: c.proposals.into_iter().map(proposal_from).collect(),
                provider_records: c.provider_records.into_iter().map(record_from).collect(),
                finish_reason: finish_from(c.finish_reason),
                usage: c.usage.map(usage_from),
                metadata: metadata_from(c.metadata),
            };
            for proposal in &completion.proposals {
                validate_event_bounds(&Event::ToolCall(proposal.clone()), limits)
                    .map_err(bound_error)?;
            }
            for record in &completion.provider_records {
                validate_event_bounds(&Event::ProviderToolRecord(record.clone()), limits)
                    .map_err(bound_error)?;
            }
            validate_event_bounds(&Event::Metadata(completion.metadata.clone()), limits)
                .map_err(bound_error)?;
            if let Some(usage) = &completion.usage {
                validate_event_bounds(&Event::Usage(usage.clone()), limits).map_err(bound_error)?;
            }
            Outcome::Completed(completion)
        }
        OutcomeDoc::Failed(f) => Outcome::Failed(Failure {
            origin: match f.origin {
                OriginDoc::Executor => FailureOrigin::Executor,
                OriginDoc::Capability => FailureOrigin::Capability,
            },
            kind: kind_from(f.kind),
            delivery: delivery_from(f.delivery),
            retry_after: retry_from(f.retry_after_ms),
            status_hint: f.status_hint,
            detail: detail_from(f.detail, limits)?,
            partial: partial_from(f.partial, limits)?,
        }),
        OutcomeDoc::Cancelled(c) => Outcome::Cancelled(Cancellation {
            delivery: delivery_from(c.delivery),
            remote: match c.remote {
                RemoteDoc::NotStarted => RemoteCancellation::NotStarted,
                RemoteDoc::Unconfirmed => RemoteCancellation::Unconfirmed,
                RemoteDoc::Confirmed => RemoteCancellation::Confirmed,
            },
            partial: partial_from(c.partial, limits)?,
        }),
    })
}

fn outcome_doc(outcome: &Outcome) -> OutcomeDoc {
    match outcome {
        Outcome::Completed(c) => OutcomeDoc::Completed(CompletionDoc {
            content: c.content.clone(),
            content_truncated: c.content_truncated,
            reasoning_bytes: c.reasoning_bytes,
            proposals: c.proposals.iter().map(proposal_to).collect(),
            provider_records: c.provider_records.iter().map(record_to).collect(),
            finish_reason: finish_to(c.finish_reason),
            usage: c.usage.as_ref().map(usage_to),
            metadata: metadata_to(&c.metadata),
        }),
        Outcome::Failed(f) => OutcomeDoc::Failed(FailureDoc {
            origin: match f.origin {
                FailureOrigin::Executor => OriginDoc::Executor,
                FailureOrigin::Capability => OriginDoc::Capability,
            },
            kind: kind_to(f.kind),
            delivery: delivery_to(f.delivery),
            retry_after_ms: retry_to(f.retry_after),
            status_hint: f.status_hint,
            detail: f.detail.as_ref().map(|d| d.as_str().to_owned()),
            partial: partial_to(&f.partial),
        }),
        Outcome::Cancelled(c) => OutcomeDoc::Cancelled(CancellationDoc {
            delivery: delivery_to(c.delivery),
            remote: match c.remote {
                RemoteCancellation::NotStarted => RemoteDoc::NotStarted,
                RemoteCancellation::Unconfirmed => RemoteDoc::Unconfirmed,
                RemoteCancellation::Confirmed => RemoteDoc::Confirmed,
            },
            partial: partial_to(&c.partial),
        }),
    }
}

/// Encodes an outcome as a version-1 JSON document.
pub fn encode_outcome(outcome: &Outcome) -> Vec<u8> {
    encode_document(&OutcomeEnvelope {
        v: CONTRACT_VERSION,
        outcome: outcome_doc(outcome),
    })
}

// ---------------------------------------------------------------------------
// Execution report (output only)

#[derive(Serialize)]
struct CheckDoc {
    identity: &'static str,
    integrity: &'static str,
    source: &'static str,
}

#[derive(Serialize)]
struct TerminalDoc {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Serialize)]
struct ShapeDoc {
    messages_by_role: [u32; 4],
    text_parts: u32,
    image_parts: u32,
    tool_call_parts: u32,
    tool_result_parts: u32,
    tools: u32,
    tool_choice: u8,
    response_format: bool,
    budget: bool,
}

#[derive(Serialize)]
struct DiagnosticsDoc {
    request_id: Option<String>,
    request_shape: Option<ShapeDoc>,
    terminal: TerminalDoc,
    delivery: DeliveryDoc,
    starts: u32,
    polls: u64,
    cancels: u32,
    closes: u32,
    events: u64,
    forwarded: u64,
    anomalies: u32,
    text_bytes: u64,
    reasoning_bytes: u64,
    tool_input_bytes: u64,
}

#[derive(Serialize)]
struct ReportDoc {
    outcome: OutcomeDoc,
    proposal_checks: Vec<CheckDoc>,
    disposition: Option<&'static str>,
    diagnostics: DiagnosticsDoc,
}

#[derive(Serialize)]
struct ReportEnvelope {
    v: u32,
    report: ReportDoc,
}

/// `CamelCase` variant names (from `Debug`, unit variants only) to `snake_case`.
fn snake(debug: &str) -> String {
    let name = debug.split(['(', ' ', '{']).next().unwrap_or(debug);
    let mut out = String::with_capacity(name.len() + 4);
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn terminal_doc(cause: TerminalCause) -> TerminalDoc {
    let (kind, detail) = match cause {
        TerminalCause::Finished => ("finished", None),
        TerminalCause::InvalidRequest(code) => {
            ("invalid_request", Some(snake(&format!("{code:?}"))))
        }
        TerminalCause::CancelledBeforeStart => ("cancelled_before_start", None),
        TerminalCause::StartFailed => ("start_failed", None),
        TerminalCause::PollFailed => ("poll_failed", None),
        TerminalCause::EndedWithoutFinish => ("ended_without_finish", None),
        TerminalCause::BatchTooLarge => ("batch_too_large", None),
        TerminalCause::TooManyPolls => ("too_many_polls", None),
        TerminalCause::Violation(v) => ("violation", Some(snake(&format!("{v:?}")))),
        TerminalCause::Cancelled => ("cancelled", None),
        TerminalCause::Abandoned => ("abandoned", None),
    };
    TerminalDoc { kind, detail }
}

fn check_doc(check: &ProposalCheck) -> CheckDoc {
    CheckDoc {
        identity: match check.identity {
            ProposalIdentity::Valid => "valid",
            ProposalIdentity::EmptyId => "empty_id",
            ProposalIdentity::DuplicateId => "duplicate_id",
            ProposalIdentity::ConflictingStreamedName => "conflicting_streamed_name",
        },
        integrity: match check.integrity {
            ArgumentIntegrity::Valid => "valid",
            ArgumentIntegrity::Replaced => "replaced",
        },
        source: match check.source {
            ArgumentSource::Final => "final",
            ArgumentSource::StreamedFallback => "streamed_fallback",
            ArgumentSource::Missing => "missing",
        },
    }
}

fn disposition_name(disposition: Disposition) -> &'static str {
    match disposition {
        Disposition::Complete => "complete",
        Disposition::ToolProposals => "tool_proposals",
        Disposition::LengthLimited => "length_limited",
        Disposition::ProviderFailure => "provider_failure",
        Disposition::InvalidCompletion => "invalid_completion",
    }
}

/// Encodes an execution report as a version-1 JSON document: the outcome (same shape
/// as [`encode_outcome`]), per-proposal checks, the disposition and content-free
/// diagnostics. Output only; hosts read it, nothing decodes it back.
pub fn encode_report(report: &ExecutionReport) -> Vec<u8> {
    let d = &report.diagnostics;
    let diagnostics = DiagnosticsDoc {
        request_id: d
            .request_id
            .map(|id| id.0.iter().map(|b| format!("{b:02x}")).collect()),
        request_shape: d.request_shape.map(|s| ShapeDoc {
            messages_by_role: s.messages_by_role,
            text_parts: s.text_parts,
            image_parts: s.image_parts,
            tool_call_parts: s.tool_call_parts,
            tool_result_parts: s.tool_result_parts,
            tools: s.tools,
            tool_choice: s.tool_choice,
            response_format: s.response_format,
            budget: s.budget,
        }),
        terminal: terminal_doc(d.terminal),
        delivery: delivery_to(d.delivery),
        starts: d.starts,
        polls: d.polls,
        cancels: d.cancels,
        closes: d.closes,
        events: d.events,
        forwarded: d.forwarded,
        anomalies: d.anomalies,
        text_bytes: d.text_bytes,
        reasoning_bytes: d.reasoning_bytes,
        tool_input_bytes: d.tool_input_bytes,
    };
    encode_document(&ReportEnvelope {
        v: CONTRACT_VERSION,
        report: ReportDoc {
            outcome: outcome_doc(&report.outcome),
            proposal_checks: report.proposal_checks.iter().map(check_doc).collect(),
            disposition: report.disposition.map(disposition_name),
            diagnostics,
        },
    })
}
