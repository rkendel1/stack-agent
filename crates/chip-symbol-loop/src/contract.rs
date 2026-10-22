//! The FX contract: what Chip sends ([`TaskPacket`], [`ClassifyPacket`]) and the only replies it
//! accepts ([`Decision`], [`Classification`]). Every reply is parsed strictly and validated against
//! the packet it answers. Nothing in a reply names a file, an offset, a command or a tool, and no
//! reply can declare the task complete: only the acceptance command can.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

pub const PACKET_VERSION: &str = "chip.symbol-repair.packet.v1";
pub const CLASSIFY_VERSION: &str = "chip.symbol-repair.classify.v1";

/// Bounds on what a packet may carry. A packet over any bound is never sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PacketLimits {
    pub max_packet_bytes: usize,
    pub max_goal_bytes: usize,
    pub max_excerpt_bytes: usize,
    pub max_source_bytes: usize,
    pub max_neighbors: usize,
    pub max_neighbor_bytes: usize,
    pub max_body_bytes: usize,
    pub max_error_bytes: usize,
    pub max_reply_bytes: usize,
}

impl Default for PacketLimits {
    fn default() -> Self {
        Self {
            max_packet_bytes: 32 * 1024,
            max_goal_bytes: 400,
            max_excerpt_bytes: 4 * 1024,
            max_source_bytes: 8 * 1024,
            max_neighbors: 12,
            max_neighbor_bytes: 400,
            max_body_bytes: 8 * 1024,
            max_error_bytes: 2 * 1024,
            max_reply_bytes: 12 * 1024,
        }
    }
}

/// `text` cut to at most `max` bytes at a character boundary, keeping its end (where compiler and
/// test output put the decisive lines).
pub fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = text.len() - max;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    text[cut..].to_string()
}

/// `text` cut to at most `max` bytes at a character boundary, keeping its start.
pub fn head(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text[..cut].to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acceptance {
    /// The exact command Chip runs, as argv. Shown to the model; never taken from it.
    pub command: Vec<String>,
    pub exit_status: Option<i32>,
    pub failure_excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolRef {
    pub id: String,
    pub path: String,
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
    pub fingerprint: String,
    /// The symbol's current source, declaration and body.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Neighbor {
    pub id: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rejected {
    pub body_digest: String,
    pub body: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSymbol {
    pub id: String,
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    ReplaceSymbolBody,
    InsertSymbol,
    NeedSymbol,
    NoChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Constraints {
    pub signature_frozen: bool,
    pub allowed_ops: Vec<Op>,
    /// The one place a new symbol may go, by the id of the symbol it follows. `None`: nowhere.
    pub insertion_after: Option<String>,
    pub max_body_bytes: usize,
    pub context_requests_left: u32,
}

/// Everything one decision is made from, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPacket {
    pub version: String,
    pub goal: String,
    pub acceptance: Acceptance,
    pub symbol: SymbolRef,
    pub neighbors: Vec<Neighbor>,
    pub rejected: Vec<Rejected>,
    pub constraints: Constraints,
    pub context: Vec<ContextSymbol>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacketError {
    TooLarge { bytes: usize, max: usize },
    Field(String),
}

impl fmt::Display for PacketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PacketError::TooLarge { bytes, max } => {
                write!(f, "packet is {bytes} bytes; at most {max} are sent")
            }
            PacketError::Field(m) => write!(f, "packet field out of bounds: {m}"),
        }
    }
}

impl TaskPacket {
    /// Checks every bound; the canonical bytes are what is sent and what is digested.
    pub fn checked_bytes(&self, limits: &PacketLimits) -> Result<Vec<u8>, PacketError> {
        let field = |ok: bool, what: &str| {
            if ok {
                Ok(())
            } else {
                Err(PacketError::Field(what.to_string()))
            }
        };
        field(self.version == PACKET_VERSION, "version")?;
        field(
            !self.goal.is_empty()
                && self.goal.len() <= limits.max_goal_bytes
                && !self.goal.contains('\n'),
            "goal",
        )?;
        field(!self.acceptance.command.is_empty(), "acceptance.command")?;
        field(
            self.acceptance.failure_excerpt.len() <= limits.max_excerpt_bytes,
            "acceptance.failure_excerpt",
        )?;
        field(
            self.symbol.source.len() <= limits.max_source_bytes,
            "symbol.source",
        )?;
        field(
            self.neighbors.len() <= limits.max_neighbors
                && self
                    .neighbors
                    .iter()
                    .all(|n| n.signature.len() <= limits.max_neighbor_bytes),
            "neighbors",
        )?;
        field(
            self.rejected.len() <= 2
                && self.rejected.iter().all(|r| {
                    r.body.len() <= limits.max_body_bytes && r.error.len() <= limits.max_error_bytes
                }),
            "rejected",
        )?;
        field(
            self.context.len() <= 2
                && self
                    .context
                    .iter()
                    .all(|c| c.source.len() <= limits.max_source_bytes),
            "context",
        )?;
        let bytes = serde_json::to_vec(self).map_err(|e| PacketError::Field(e.to_string()))?;
        if bytes.len() > limits.max_packet_bytes {
            return Err(PacketError::TooLarge {
                bytes: bytes.len(),
                max: limits.max_packet_bytes,
            });
        }
        Ok(bytes)
    }
}

/// One bounded choice among supplied candidates, for an ambiguous target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClassifyPacket {
    pub version: String,
    pub goal: String,
    pub acceptance: Acceptance,
    pub candidates: Vec<Neighbor>,
}

/// A validated decision. Only [`validate_decision`] makes one from a reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Decision {
    ReplaceSymbolBody { symbol: String, body: String },
    InsertSymbol { after: String, declaration: String },
    NeedSymbol { symbol: String },
    NoChange { reason: String },
}

impl Decision {
    pub fn op(&self) -> Op {
        match self {
            Decision::ReplaceSymbolBody { .. } => Op::ReplaceSymbolBody,
            Decision::InsertSymbol { .. } => Op::InsertSymbol,
            Decision::NeedSymbol { .. } => Op::NeedSymbol,
            Decision::NoChange { .. } => Op::NoChange,
        }
    }
}

/// Why a reply was not accepted as a decision. Each names the rule, never echoes the reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum ReplyError {
    TooLarge(usize),
    NotOneJsonObject(String),
    DuplicateKey(String),
    UnknownOperation(String),
    MissingField(String),
    UnknownField(String),
    WrongType(String),
    OperationNotAllowed(String),
    UnauthorizedSymbol(String),
    OversizedField(String),
    EmptyField(String),
    NotACandidate(String),
}

impl fmt::Display for ReplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplyError::TooLarge(n) => write!(f, "the reply is {n} bytes, over the limit"),
            ReplyError::NotOneJsonObject(m) => {
                write!(f, "the reply is not exactly one JSON object: {m}")
            }
            ReplyError::DuplicateKey(k) => write!(f, "the reply repeats the key {k:?}"),
            ReplyError::UnknownOperation(o) => write!(f, "unknown op {o:?}"),
            ReplyError::MissingField(k) => write!(f, "missing field {k:?}"),
            ReplyError::UnknownField(k) => write!(f, "field {k:?} is not part of this op"),
            ReplyError::WrongType(k) => write!(f, "field {k:?} must be a string"),
            ReplyError::OperationNotAllowed(o) => write!(f, "op {o} is not allowed by this packet"),
            ReplyError::UnauthorizedSymbol(s) => {
                write!(f, "symbol {s:?} is not the one this packet authorizes")
            }
            ReplyError::OversizedField(k) => write!(f, "field {k:?} is over its size limit"),
            ReplyError::EmptyField(k) => write!(f, "field {k:?} is empty"),
            ReplyError::NotACandidate(s) => {
                write!(f, "{s:?} is not one of the supplied candidates")
            }
        }
    }
}

/// A JSON object whose keys are each present once.
struct UniqueObject(BTreeMap<String, Value>);

impl<'de> Deserialize<'de> for UniqueObject {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = UniqueObject;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<UniqueObject, A::Error> {
                let mut out = BTreeMap::new();
                while let Some((k, v)) = map.next_entry::<String, Value>()? {
                    if out.contains_key(&k) {
                        return Err(de::Error::custom(format!("duplicate key {k}")));
                    }
                    out.insert(k, v);
                }
                Ok(UniqueObject(out))
            }
        }
        d.deserialize_map(V)
    }
}

/// The reply's one JSON object: surrounding whitespace and one surrounding Markdown code fence are
/// tolerated (as `chip.work-decision.v1` tolerates them); nothing else is.
fn one_object(reply: &str, max: usize) -> Result<BTreeMap<String, Value>, ReplyError> {
    if reply.len() > max {
        return Err(ReplyError::TooLarge(reply.len()));
    }
    let mut text = reply.trim();
    if let Some(rest) = text.strip_prefix("```") {
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        text = rest
            .strip_suffix("```")
            .ok_or_else(|| ReplyError::NotOneJsonObject("unclosed code fence".into()))?
            .trim();
    }
    if !text.starts_with('{') {
        return Err(ReplyError::NotOneJsonObject(
            "it does not start with `{`".into(),
        ));
    }
    let mut de = serde_json::Deserializer::from_str(text);
    let object = UniqueObject::deserialize(&mut de).map_err(|e| {
        let m = e.to_string();
        if m.contains("duplicate key") {
            ReplyError::DuplicateKey(
                m.split("duplicate key ")
                    .nth(1)
                    .unwrap_or("")
                    .split(' ')
                    .next()
                    .unwrap_or("")
                    .to_string(),
            )
        } else {
            ReplyError::NotOneJsonObject(m)
        }
    })?;
    de.end()
        .map_err(|_| ReplyError::NotOneJsonObject("text follows the object".into()))?;
    Ok(object.0)
}

fn string(object: &BTreeMap<String, Value>, key: &str, max: usize) -> Result<String, ReplyError> {
    match object.get(key) {
        None => Err(ReplyError::MissingField(key.into())),
        Some(Value::String(s)) if s.trim().is_empty() => Err(ReplyError::EmptyField(key.into())),
        Some(Value::String(s)) if s.len() > max => Err(ReplyError::OversizedField(key.into())),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(ReplyError::WrongType(key.into())),
    }
}

fn only(object: &BTreeMap<String, Value>, allowed: &[&str]) -> Result<(), ReplyError> {
    match object.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(ReplyError::UnknownField(k.clone())),
        None => Ok(()),
    }
}

/// The reply as a decision this packet authorizes, or why not.
pub fn validate_decision(
    reply: &str,
    packet: &TaskPacket,
    limits: &PacketLimits,
) -> Result<Decision, ReplyError> {
    let object = one_object(reply, limits.max_reply_bytes)?;
    let op = string(&object, "op", 64)?;
    let decision = match op.as_str() {
        "replace_symbol_body" => {
            only(&object, &["op", "symbol", "body"])?;
            Decision::ReplaceSymbolBody {
                symbol: string(&object, "symbol", 512)?,
                body: string(&object, "body", packet.constraints.max_body_bytes)?,
            }
        }
        "insert_symbol" => {
            only(&object, &["op", "after", "declaration"])?;
            Decision::InsertSymbol {
                after: string(&object, "after", 512)?,
                declaration: string(&object, "declaration", packet.constraints.max_body_bytes)?,
            }
        }
        "need_symbol" => {
            only(&object, &["op", "symbol"])?;
            Decision::NeedSymbol {
                symbol: string(&object, "symbol", 512)?,
            }
        }
        "no_change" => {
            only(&object, &["op", "reason"])?;
            Decision::NoChange {
                reason: string(&object, "reason", 512)?,
            }
        }
        other => return Err(ReplyError::UnknownOperation(head(other, 40))),
    };
    if !packet.constraints.allowed_ops.contains(&decision.op()) {
        return Err(ReplyError::OperationNotAllowed(op));
    }
    match &decision {
        Decision::ReplaceSymbolBody { symbol, .. } if *symbol != packet.symbol.id => {
            Err(ReplyError::UnauthorizedSymbol(head(symbol, 120)))
        }
        Decision::InsertSymbol { after, .. }
            if packet.constraints.insertion_after.as_deref() != Some(after.as_str()) =>
        {
            Err(ReplyError::UnauthorizedSymbol(head(after, 120)))
        }
        Decision::NeedSymbol { .. } if packet.constraints.context_requests_left == 0 => Err(
            ReplyError::OperationNotAllowed("need_symbol (no context requests left)".into()),
        ),
        _ => Ok(decision),
    }
}

/// The candidate the reply chose, which must be one of the supplied ids.
pub fn validate_classification(
    reply: &str,
    packet: &ClassifyPacket,
    limits: &PacketLimits,
) -> Result<String, ReplyError> {
    let object = one_object(reply, limits.max_reply_bytes)?;
    only(&object, &["op", "symbol"])?;
    let op = string(&object, "op", 64)?;
    if op != "classify" {
        return Err(ReplyError::UnknownOperation(head(&op, 40)));
    }
    let symbol = string(&object, "symbol", 512)?;
    if packet.candidates.iter().any(|c| c.id == symbol) {
        Ok(symbol)
    } else {
        Err(ReplyError::NotACandidate(head(&symbol, 120)))
    }
}

/// The JSON schema of a decision for this packet, for a server that enforces one (grammar
/// decoding). Chip validates the reply in full whatever the server enforced.
pub fn decision_schema(packet: &TaskPacket) -> Value {
    let alt = |op: &str, fields: &[&str]| {
        let mut props = serde_json::Map::new();
        props.insert("op".into(), serde_json::json!({"const": op}));
        for f in fields {
            props.insert((*f).into(), serde_json::json!({"type": "string"}));
        }
        let mut required = vec!["op".to_string()];
        required.extend(fields.iter().map(|f| f.to_string()));
        serde_json::json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
    };
    let alts: Vec<Value> = packet
        .constraints
        .allowed_ops
        .iter()
        .map(|op| match op {
            Op::ReplaceSymbolBody => alt("replace_symbol_body", &["symbol", "body"]),
            Op::InsertSymbol => alt("insert_symbol", &["after", "declaration"]),
            Op::NeedSymbol => alt("need_symbol", &["symbol"]),
            Op::NoChange => alt("no_change", &["reason"]),
        })
        .collect();
    serde_json::json!({"anyOf": alts})
}
