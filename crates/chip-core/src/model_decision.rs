//! The smallest contract between a model's text and a [`WorkDecision`] (`chip.work-decision.v1`).
//!
//! The model produces *data*: one JSON object naming a decision. Chip reads it strictly, validates
//! it, and only then does anything follow; the model never produces an instruction that is run.
//! A capability request is still just a request: it passes through the same validation, execution
//! and observation path as any other, and Compute is never told anything the model wrote except a
//! declared capability id and typed input values.
//!
//! ```text
//! (each object may also carry "schema":"chip.work-decision.v1")
//! {"decision":"request_capability","capability":"<id>","inputs":{"<name>":<string|integer|boolean>}}  ("inputs" optional; a capability that declares no inputs accepts none, not even {})
//! {"decision":"complete","summary":"<text>"}
//! {"decision":"escalate","reason":"<text>"}
//! {"decision":"block","reason":"<text>"}
//! ```
//!
//! Strict by design: anything else (prose around the object, an unknown decision, a missing or an
//! extra field, a wrong type, an undeclared or malformed capability, an oversized reply) is an
//! error. There is no repair, no retry and no second model call. A single Markdown code fence
//! around the whole reply is tolerated, because models add one habitually; nothing else is.

use std::collections::BTreeMap;

use chip_inference::ModelResponse;

use crate::{
    Capability, CapabilityAvailability, CapabilityError, CapabilityId, CapabilityRequest,
    DecisionError, ExecutionId, InputValue, WorkDecision, WorkDecisionBoundary,
};

pub const WORK_DECISION_SCHEMA: &str = "chip.work-decision.v1";

/// Largest reply the boundary will read.
pub const MAX_REPLY_BYTES: usize = 4096;
/// Largest text field (a `summary` or a `reason`), in bytes of UTF-8.
pub const MAX_TEXT_BYTES: usize = 1024;
const MAX_INPUTS: usize = 16;
/// A JSON string can cost up to six bytes per byte it carries (`\u00xx`), so a capability that
/// declares a larger text input raises the reply limit by that much and no more.
const ESCAPE_EXPANSION: usize = 6;
const MAX_DESCRIPTION_CHARS: usize = 160;

// ---- the contract as data, and the schema derived from it ---------------------------------------------

/// What a field of a decision holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// A string of at most [`MAX_TEXT_BYTES`] bytes of UTF-8.
    Text,
    /// An object of at most 16 entries whose values are strings, integers or booleans.
    Inputs,
}

/// One field of a decision shape.
#[derive(Debug, Clone, Copy)]
pub struct DecisionField {
    pub name: &'static str,
    pub required: bool,
    pub kind: FieldKind,
}

/// One decision the contract allows: its name and exactly the fields it may carry. `schema` (the
/// optional contract marker) is allowed on every shape and is not listed.
#[derive(Debug, Clone, Copy)]
pub struct DecisionShape {
    pub name: &'static str,
    pub fields: &'static [DecisionField],
    /// Said once to the provider, next to the field.
    pub says: &'static str,
}

const fn field(name: &'static str, required: bool, kind: FieldKind) -> DecisionField {
    DecisionField {
        name,
        required,
        kind,
    }
}

/// The `chip.work-decision.v1` decisions, as the parser in [`ModelDecisionBoundary::interpret`]
/// reads them. A test checks the two against each other in both directions: nothing this table
/// allows is refused for its shape, and nothing the parser accepts is excluded by it.
pub const DECISION_SHAPES: [DecisionShape; 4] = [
    DecisionShape {
        name: "request_capability",
        fields: &[
            field("capability", true, FieldKind::Text),
            field("inputs", false, FieldKind::Inputs),
        ],
        says: "Ask for one capability. Add inputs only if that capability declares inputs.",
    },
    DecisionShape {
        name: "complete",
        fields: &[field("summary", true, FieldKind::Text)],
        says: "Give the final answer.",
    },
    DecisionShape {
        name: "escalate",
        fields: &[field("reason", true, FieldKind::Text)],
        says: "Ask a person for help.",
    },
    DecisionShape {
        name: "block",
        fields: &[field("reason", true, FieldKind::Text)],
        says: "Say why the work cannot proceed.",
    },
];

/// Which projection of the canonical contract to hand a provider. Both say the same thing; they
/// differ in what a provider's structured-output feature can express.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionSchemaForm {
    /// One alternative per decision (`anyOf`): `decision` is required, each decision requires its
    /// own fields and allows no others. Constrains a server that supports `anyOf` (a local
    /// llama.cpp-style grammar does).
    Full,
    /// One flat object with every field optional except `decision`, with each field's description
    /// saying which decision it belongs to. For providers that reject `anyOf`, `oneOf` and `const`
    /// at the root (OpenAI does). It cannot say that `complete` needs `summary`: that stays with
    /// the runtime, which is authoritative in every case.
    Flat,
    /// The full projection restated in the subset of JSON Schema that Anthropic's structured
    /// output enforces: one `anyOf` alternative per decision (so a capability request must name its
    /// capability and a completion must carry a summary), every object closed
    /// (`additionalProperties: false`), no length, count or range bounds, and no `type` beside the
    /// root `anyOf`. `inputs` is closed over the named inputs: only names a capability declares may
    /// appear, each with a string, integer or boolean value. The names are supplied by the host
    /// from the capability declarations, never by the model. The bounds it drops (text length,
    /// input count) and the rule that an input belongs to the requested capability stay with the
    /// runtime, which is authoritative in every case. The flat form is unsuitable there: it cannot
    /// say which decision needs which field, and a constrained decoder then fills optional fields
    /// with arbitrary values (measured live).
    Closed(&'static [&'static str]),
}

fn text_property(says: &str) -> String {
    format!(
        "{{\"type\":\"string\",\"maxLength\":{MAX_TEXT_BYTES},\"description\":\"{says} At most {MAX_TEXT_BYTES} bytes of UTF-8.\"}}"
    )
}

fn closed_text_property(says: &str) -> String {
    format!(
        "{{\"type\":\"string\",\"description\":\"{says} At most {MAX_TEXT_BYTES} bytes of UTF-8.\"}}"
    )
}

fn closed_inputs_property(names: &[&str]) -> String {
    let value = "{\"anyOf\":[{\"type\":\"string\"},{\"type\":\"integer\"},{\"type\":\"boolean\"}]}";
    let props: Vec<String> = names.iter().map(|n| format!("\"{n}\":{value}")).collect();
    format!(
        "{{\"type\":\"object\",\"description\":\"Only with request_capability, and only if the capability declares inputs: its declared input names with string, integer or boolean values.\",\"properties\":{{{}}},\"additionalProperties\":false}}",
        props.join(",")
    )
}

fn inputs_property() -> String {
    format!(
        "{{\"type\":\"object\",\"maxProperties\":{MAX_INPUTS},\"description\":\"Only with request_capability, and only if the capability declares inputs: its declared input names with string, integer or boolean values.\",\"additionalProperties\":{{\"anyOf\":[{{\"type\":\"string\"}},{{\"type\":\"integer\"}},{{\"type\":\"boolean\"}}]}}}}"
    )
}

fn property(shape: &DecisionShape, f: &DecisionField) -> String {
    match f.kind {
        FieldKind::Text => text_property(&format!(
            "Only with decision \\\"{}\\\".{}",
            shape.name,
            if f.name == "capability" {
                " The id of a capability from the list you were given."
            } else {
                ""
            }
        )),
        FieldKind::Inputs => inputs_property(),
    }
}

impl ModelDecisionBoundary {
    /// The JSON schema (as text) of the canonical decision contract, derived from
    /// [`DECISION_SHAPES`]. A provider may enforce all, part or none of it; the reply is read by
    /// [`interpret`](WorkDecisionBoundary::interpret) either way and nothing is repaired.
    pub fn response_schema(form: DecisionSchemaForm) -> String {
        let marker = format!("{{\"type\":\"string\",\"enum\":[\"{WORK_DECISION_SCHEMA}\"]}}");
        let names: Vec<String> = DECISION_SHAPES
            .iter()
            .map(|d| format!("\"{}\"", d.name))
            .collect();
        match form {
            DecisionSchemaForm::Full | DecisionSchemaForm::Closed(_) => {
                let closed = match form {
                    DecisionSchemaForm::Closed(names) => Some(names),
                    _ => None,
                };
                let branches: Vec<String> = DECISION_SHAPES
                    .iter()
                    .map(|d| {
                        let mut props = vec![
                            format!("\"schema\":{marker}"),
                            format!(
                                "\"decision\":{{\"type\":\"string\",\"enum\":[\"{}\"],\"description\":\"{}\"}}",
                                d.name, d.says
                            ),
                        ];
                        props.extend(d.fields.iter().map(|f| {
                            let rendered = match (closed, f.kind) {
                                (Some(names), FieldKind::Inputs) => closed_inputs_property(names),
                                (Some(_), FieldKind::Text) => closed_text_property(&format!(
                                    "Only with decision \\\"{}\\\".{}",
                                    d.name,
                                    if f.name == "capability" {
                                        " The id of a capability from the list you were given."
                                    } else {
                                        ""
                                    }
                                )),
                                (None, _) => property(d, f),
                            };
                            format!("\"{}\":{rendered}", f.name)
                        }));
                        let mut required = vec!["\"decision\"".to_string()];
                        required.extend(d.fields.iter().filter(|f| f.required).map(|f| format!("\"{}\"", f.name)));
                        format!(
                            "{{\"type\":\"object\",\"properties\":{{{}}},\"required\":[{}],\"additionalProperties\":false}}",
                            props.join(","),
                            required.join(",")
                        )
                    })
                    .collect();
                if closed.is_some() {
                    // Anthropic rejects a `type` beside `anyOf`: the root is the alternatives alone.
                    format!("{{\"anyOf\":[{}]}}", branches.join(","))
                } else {
                    format!("{{\"type\":\"object\",\"anyOf\":[{}]}}", branches.join(","))
                }
            }
            DecisionSchemaForm::Flat => {
                let says: Vec<String> = DECISION_SHAPES
                    .iter()
                    .map(|d| format!("{}: {}", d.name, d.says))
                    .collect();
                let mut props = vec![
                    format!("\"schema\":{marker}"),
                    format!(
                        "\"decision\":{{\"type\":\"string\",\"enum\":[{}],\"description\":\"The next step. {} request_capability needs capability; complete needs summary; escalate and block need reason. Carry only the fields of your decision.\"}}",
                        names.join(","),
                        says.join(" ")
                    ),
                ];
                let mut seen: Vec<&str> = Vec::new();
                for d in &DECISION_SHAPES {
                    for f in d.fields {
                        if seen.contains(&f.name) {
                            continue;
                        }
                        seen.push(f.name);
                        // `reason` is shared by two decisions; say so.
                        let shape = if f.name == "reason" {
                            DecisionShape {
                                name: "escalate\\\" or \\\"block",
                                ..*d
                            }
                        } else {
                            *d
                        };
                        props.push(format!("\"{}\":{}", f.name, property(&shape, f)));
                    }
                }
                format!(
                    "{{\"type\":\"object\",\"properties\":{{{}}},\"required\":[\"decision\"],\"additionalProperties\":false}}",
                    props.join(",")
                )
            }
        }
    }
}

/// Interprets a model reply under the `chip.work-decision.v1` contract.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModelDecisionBoundary;

fn invalid(message: impl Into<String>) -> DecisionError {
    DecisionError::InvalidDecision(message.into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Json {
    Str(String),
    Int(i64),
    Bool(bool),
    Object(Vec<(String, Json)>),
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Parser<'a> {
    fn skip_space(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|b| b.is_ascii_whitespace())
        {
            self.at += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.bytes.get(self.at) == Some(&byte) {
            self.at += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at byte {}", byte as char, self.at))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        self.skip_space();
        match self.bytes.get(self.at) {
            Some(b'{') if depth < 2 => self.object(depth),
            Some(b'{') => Err("objects nested too deeply".to_string()),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.word("true", Json::Bool(true)),
            Some(b'f') => self.word("false", Json::Bool(false)),
            Some(b'-' | b'0'..=b'9') => self.integer(),
            Some(b'[') => Err("arrays are not part of the contract".to_string()),
            Some(b'n') => Err("null is not part of the contract".to_string()),
            Some(_) => Err(format!("unexpected character at byte {}", self.at)),
            None => Err("unexpected end of reply".to_string()),
        }
    }

    fn word(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(format!("unexpected word at byte {}", self.at))
        }
    }

    fn integer(&mut self) -> Result<Json, String> {
        let start = self.at;
        if self.bytes.get(self.at) == Some(&b'-') {
            self.at += 1;
        }
        let digits = self.at;
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
        }
        if self.at == digits {
            return Err("a number needs digits".to_string());
        }
        if matches!(self.bytes.get(self.at), Some(b'.' | b'e' | b'E')) {
            return Err("only whole numbers are part of the contract".to_string());
        }
        std::str::from_utf8(&self.bytes[start..self.at])
            .ok()
            .and_then(|t| t.parse::<i64>().ok())
            .map(Json::Int)
            .ok_or_else(|| "number out of range".to_string())
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let Some(&b) = self.bytes.get(self.at) else {
                return Err("unterminated string".to_string());
            };
            self.at += 1;
            match b {
                b'"' => return Ok(out),
                b'\\' => {
                    let Some(&e) = self.bytes.get(self.at) else {
                        return Err("unterminated escape".to_string());
                    };
                    self.at += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hex = self
                                .bytes
                                .get(self.at..self.at + 4)
                                .ok_or("short \\u escape")?;
                            let code = std::str::from_utf8(hex)
                                .ok()
                                .and_then(|h| u32::from_str_radix(h, 16).ok())
                                .ok_or("bad \\u escape")?;
                            self.at += 4;
                            out.push(
                                char::from_u32(code).ok_or("unpaired surrogate in a \\u escape")?,
                            );
                        }
                        _ => return Err("unknown escape".to_string()),
                    }
                }
                b if b < 0x20 => return Err("a control character inside a string".to_string()),
                _ => {
                    // Copy one UTF-8 scalar whole.
                    let start = self.at - 1;
                    let width = match b {
                        0x00..=0x7f => 1,
                        0xc0..=0xdf => 2,
                        0xe0..=0xef => 3,
                        _ => 4,
                    };
                    let end = start + width;
                    let slice = self.bytes.get(start..end).ok_or("truncated UTF-8")?;
                    out.push_str(std::str::from_utf8(slice).map_err(|_| "invalid UTF-8")?);
                    self.at = end;
                }
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, String> {
        self.expect(b'{')?;
        let mut fields: Vec<(String, Json)> = Vec::new();
        self.skip_space();
        if self.bytes.get(self.at) == Some(&b'}') {
            self.at += 1;
            return Ok(Json::Object(fields));
        }
        loop {
            self.skip_space();
            let key = self.string()?;
            if fields.iter().any(|(k, _)| *k == key) {
                return Err(format!("duplicate field \"{key}\""));
            }
            self.skip_space();
            self.expect(b':')?;
            let value = self.value(depth + 1)?;
            fields.push((key, value));
            self.skip_space();
            match self.bytes.get(self.at) {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Json::Object(fields));
                }
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.at)),
            }
        }
    }
}

/// The reply as one object: trimmed, optionally unwrapped from a single code fence, and nothing
/// after the closing brace.
fn parse_reply(output: &str, limit: usize) -> Result<Vec<(String, Json)>, DecisionError> {
    if output.len() > limit {
        return Err(invalid(format!(
            "the reply is {} bytes; the limit is {limit}",
            output.len()
        )));
    }
    let mut text = output.trim();
    if let Some(rest) = text.strip_prefix("```") {
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        let Some(body) = rest.trim_end().strip_suffix("```") else {
            return Err(invalid("an unclosed code fence"));
        };
        text = body.trim();
    }
    let mut parser = Parser {
        bytes: text.as_bytes(),
        at: 0,
    };
    let value = parser
        .value(0)
        .map_err(|e| invalid(format!("not valid JSON: {e}")))?;
    parser.skip_space();
    if parser.at != text.len() {
        return Err(invalid("text after the JSON object"));
    }
    match value {
        Json::Object(fields) => Ok(fields),
        _ => Err(invalid("the reply must be a JSON object")),
    }
}

fn take_string(fields: &mut Vec<(String, Json)>, key: &str) -> Result<String, DecisionError> {
    let position = fields
        .iter()
        .position(|(k, _)| k == key)
        .ok_or_else(|| invalid(format!("missing required field \"{key}\"")))?;
    match fields.remove(position).1 {
        Json::Str(s) if s.len() <= MAX_TEXT_BYTES => Ok(s),
        Json::Str(s) => Err(invalid(format!(
            "\"{key}\" is too long: {} bytes, the limit is {MAX_TEXT_BYTES}",
            s.len()
        ))),
        _ => Err(invalid(format!("\"{key}\" must be a string"))),
    }
}

fn reject_extras(fields: &[(String, Json)]) -> Result<(), DecisionError> {
    match fields.first() {
        Some((k, _)) => Err(invalid(format!("unexpected field \"{k}\""))),
        None => Ok(()),
    }
}

/// The provider's id for a response, kept as correlation metadata: bounded and free of control
/// characters, `None` when the provider gave none. It is never an execution identity.
fn provider_response_id(provider_id: &str) -> Option<String> {
    let cleaned: String = provider_id
        .chars()
        .filter(|c| !c.is_control())
        .take(128)
        .collect();
    (!cleaned.trim().is_empty()).then_some(cleaned)
}

/// The exact way to request each available capability, written from its declared inputs and from
/// nothing else, so that what the model is told cannot differ from what the runtime accepts:
///
/// * a capability that declares no inputs is requested with no `inputs` field at all, and an
///   `inputs` field, even `{}`, is invalid;
/// * one that declares inputs is requested with an `inputs` object holding only those fields.
fn invocation_forms(capabilities: &[Capability]) -> String {
    let available: Vec<&Capability> = capabilities
        .iter()
        .filter(|c| c.availability == CapabilityAvailability::Available)
        .collect();
    if available.is_empty() {
        return String::new();
    }
    let mut out = String::from("How to request each capability, exactly: ");
    let mut first_without_inputs: Option<String> = None;
    let entries: Vec<String> = available
        .iter()
        .map(|c| {
            let id = c.descriptor.id.to_string();
            let request = |inputs: &str| {
                format!("{{\"decision\":\"request_capability\",\"capability\":\"{id}\"{inputs}}}")
            };
            if c.descriptor.inputs.is_empty() {
                first_without_inputs.get_or_insert_with(|| id.clone());
                format!(
                    "{id} takes no inputs: {} (no \"inputs\" field)",
                    request("")
                )
            } else {
                let fields: Vec<String> = c
                    .descriptor
                    .inputs
                    .iter()
                    .map(|i| format!("\"{}\":<string|integer|boolean>", i.name))
                    .collect();
                let needs: Vec<String> = c
                    .descriptor
                    .inputs
                    .iter()
                    .map(|i| {
                        format!(
                            "{} {}",
                            i.name,
                            if i.required { "required" } else { "optional" }
                        )
                    })
                    .collect();
                let omit = if c.descriptor.inputs.iter().any(|i| i.required) {
                    ""
                } else {
                    "; with no inputs to give, omit the \"inputs\" field"
                };
                format!(
                    "{id} takes inputs ({}): {}{omit}",
                    needs.join(", "),
                    request(&format!(",\"inputs\":{{{}}}", fields.join(",")))
                )
            }
        })
        .collect();
    out.push_str(&entries.join("; "));
    out.push_str(". ");
    if let Some(id) = first_without_inputs {
        out.push_str(&format!(
            "A capability that takes no inputs must not carry an \"inputs\" field at all, not even an empty one: {{\"decision\":\"request_capability\",\"capability\":\"{id}\",\"inputs\":{{}}}} is invalid. "
        ));
    }
    out
}

impl WorkDecisionBoundary for ModelDecisionBoundary {
    fn interpret(
        &self,
        response: &ModelResponse,
        capabilities: &[Capability],
    ) -> Result<WorkDecision, DecisionError> {
        let reply_limit = capabilities
            .iter()
            .filter_map(|c| c.descriptor.max_input_bytes)
            .max()
            .map_or(MAX_REPLY_BYTES, |most| {
                MAX_REPLY_BYTES + ESCAPE_EXPANSION * most
            });
        let mut fields = parse_reply(&response.output, reply_limit)?;
        // The schema marker is optional (models echo it when told the schema) but, if present,
        // it must name this contract.
        if fields.iter().any(|(k, _)| k == "schema") {
            let schema = take_string(&mut fields, "schema")?;
            if schema != WORK_DECISION_SCHEMA {
                return Err(invalid(format!("unsupported schema \"{schema}\"")));
            }
        }
        let decision = take_string(&mut fields, "decision")?;
        match decision.as_str() {
            "request_capability" => {
                let capability = take_string(&mut fields, "capability")?;
                let inputs_present = fields.iter().any(|(k, _)| k == "inputs");
                // The size of a text input is the capability's to declare, not the requester's.
                let input_limit = capabilities
                    .iter()
                    .find(|c| c.descriptor.id.as_str() == capability)
                    .and_then(|c| c.descriptor.max_input_bytes)
                    .unwrap_or(MAX_TEXT_BYTES);
                let inputs = match fields.iter().position(|(k, _)| k == "inputs") {
                    None => BTreeMap::new(),
                    Some(i) => match fields.remove(i).1 {
                        Json::Object(entries) if entries.len() <= MAX_INPUTS => entries
                            .into_iter()
                            .map(|(name, value)| match value {
                                Json::Str(s) if s.len() <= input_limit => {
                                    Ok((name, InputValue::Text(s)))
                                }
                                Json::Str(_) => Err(invalid(format!(
                                    "input \"{name}\" is longer than the {input_limit} bytes this capability accepts"
                                ))),
                                Json::Int(n) => Ok((name, InputValue::Integer(n))),
                                Json::Bool(b) => Ok((name, InputValue::Bool(b))),
                                _ => Err(invalid(format!(
                                    "input \"{name}\" must be a string, an integer or a boolean"
                                ))),
                            })
                            .collect::<Result<BTreeMap<_, _>, _>>()?,
                        Json::Object(_) => return Err(invalid("too many inputs")),
                        _ => return Err(invalid("\"inputs\" must be an object")),
                    },
                };
                reject_extras(&fields)?;
                let id = CapabilityId::new(capability)?;
                // The model may only ask for what has been declared and can be used.
                match capabilities.iter().find(|c| c.descriptor.id == id) {
                    None => return Err(CapabilityError::Unknown(id.to_string()).into()),
                    Some(c) if c.availability != CapabilityAvailability::Available => {
                        return Err(
                            CapabilityError::Unavailable(format!("{id} is not available")).into(),
                        );
                    }
                    Some(_) => {}
                }
                Ok(WorkDecision::RequestCapability(CapabilityRequest {
                    // A model's request names no execution. Chip assigns the identity when the work
                    // loop authorizes it; the provider's response id is metadata and nothing more.
                    execution_id: ExecutionId::unassigned(),
                    provider_response_id: provider_response_id(&response.id),
                    capability_id: id,
                    inputs,
                    inputs_present,
                }))
            }
            "complete" => {
                let summary = take_string(&mut fields, "summary")?;
                reject_extras(&fields)?;
                Ok(WorkDecision::Complete { summary })
            }
            "escalate" => {
                let reason = take_string(&mut fields, "reason")?;
                reject_extras(&fields)?;
                Ok(WorkDecision::Escalate { reason })
            }
            "block" => {
                let reason = take_string(&mut fields, "reason")?;
                reject_extras(&fields)?;
                Ok(WorkDecision::Block { reason })
            }
            other => Err(invalid(format!(
                "unknown decision \"{}\"",
                other.chars().take(40).collect::<String>()
            ))),
        }
    }

    /// Capabilities are listed in the order the provider declared them.
    fn question(&self, capabilities: &[Capability]) -> String {
        let available: Vec<String> = capabilities
            .iter()
            .filter(|c| c.availability == CapabilityAvailability::Available)
            .map(|c| {
                let inputs: Vec<&str> = c
                    .descriptor
                    .inputs
                    .iter()
                    .map(|i| i.name.as_str())
                    .collect();
                let head = if inputs.is_empty() {
                    c.descriptor.id.to_string()
                } else {
                    format!("{} (inputs: {})", c.descriptor.id, inputs.join(", "))
                };
                // One line of Chip-declared text, so the model can tell capabilities apart.
                let description: String = c
                    .descriptor
                    .description
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(MAX_DESCRIPTION_CHARS)
                    .collect();
                if description.is_empty() {
                    head
                } else {
                    format!("{head} - {description}")
                }
            })
            .collect();
        let forms = invocation_forms(capabilities);
        format!(
            "Decide the next step. Reply with exactly one JSON object and nothing else; it is read as data and never run. The rules of the reply: it is one JSON object that matches this contract; include every required field and never leave out \"decision\"; no Markdown code fence, and no commentary or other text before or after the object; never a partial object; a \"summary\" or \"reason\" is at most {MAX_TEXT_BYTES} bytes of UTF-8 (bytes, not words or tokens), and a longer one makes the whole reply invalid; request only the capabilities listed below, each in its exact form, and give inputs only to a capability that declares them; do not say an operation succeeded unless its result is among the observations you were shown, and do not treat your own statements as evidence. A reply that breaks a rule is rejected, not repaired. Every object carries \"schema\":\"{WORK_DECISION_SCHEMA}\" (optional) and one of: \
a request for one capability, in the exact form its entry below gives, {{\"decision\":\"complete\",\"summary\":\"<text>\"}}, {{\"decision\":\"escalate\",\"reason\":\"<text>\"}} or {{\"decision\":\"block\",\"reason\":\"<text>\"}}. \
{forms}\
Available capabilities: {}.",
            if available.is_empty() {
                "none".to_string()
            } else {
                available.join("; ")
            }
        )
    }
}
