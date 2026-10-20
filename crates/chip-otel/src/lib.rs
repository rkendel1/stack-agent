//! OpenTelemetry export of Chip work.
//!
//! A work's durable record (its FeltDB task: the `chip.WorkEvent` trajectory of every attempt and
//! the host's verdict) is mapped to one OTLP trace, under the OpenTelemetry GenAI semantic
//! conventions where they apply:
//!
//! ```text
//! invoke_agent stack-agent      one span per attempt   gen_ai.operation.name = invoke_agent
//!   chat <model>                 one per model call     gen_ai.operation.name = chat, gen_ai.usage.*
//!   execute_tool <capability>    one per execution      gen_ai.operation.name = execute_tool
//!   (span events)                every other event      decisions, observations, goal evaluations,
//!                                                       evidence, limits, the terminal state
//! ```
//!
//! # What is exported, and what never is
//!
//! The export is built from an allowlist, not by copying fields and removing some: every attribute
//! is a count, a boolean, an identifier Chip made, a SHA-256, a capability name, a member of a fixed
//! set (an outcome, an observation kind, a limit), or a provider-reported model name or cost that
//! matches a strict pattern. Nothing else is written, so there is no prompt, no model reply, no goal
//! text, no file content, no execution output, no reason text (a reason can quote the model or a
//! provider's error body), no host path and no credential. The durable record does not hold most of
//! these to begin with; the allowlist holds even for a record that does.
//!
//! Every span and span event carries `stack_agent.provenance`: whether its fact was observed by the
//! host, inferred by the model, or decided by Chip.
//!
//! # Independence from the work
//!
//! [`trace`] is a pure function of the record: it runs no work, reads no clock and touches no file,
//! so the same record always gives the same request (span ids are derived from the task, the
//! attempt and the position). Any record gives a request: an attempt that was interrupted ends at its
//! last recorded event and says so. [`export`] delivers it and reports failure; it never decides
//! anything about the work.

pub mod export;

use chip_agent_state::{History, Provenance, Verification, WorkEventRecord};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The instrumentation scope.
pub const SCOPE_NAME: &str = "stack-agent";

/// What the record does not hold about the run, given by whoever exports it: the service, and the
/// provider and model the work asked for. `None` is left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    pub service_name: String,
    pub service_version: String,
    /// The provider as stack-agent names it (`anthropic`, `openai-compatible`, ...).
    pub provider: Option<String>,
    /// The model the work asked for.
    pub request_model: Option<String>,
}

// ---- values that may be exported --------------------------------------------------------------------

/// An identifier-like string: 1 to 128 of `A-Z a-z 0-9 . _ : / @ ~ -`, with single inner spaces
/// (`request project.read`). Anything else is not exported.
fn token(s: &str) -> Option<&str> {
    let ok = !s.is_empty()
        && s.len() <= 128
        && !s.starts_with(' ')
        && !s.ends_with(' ')
        && !s.contains("  ")
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(b, b'.' | b'_' | b':' | b'/' | b'@' | b'~' | b'-' | b' ')
        })
        && !credential_shaped(s);
    ok.then_some(s)
}

/// Whether a value looks like a credential: a segment with a known key prefix (the shapes FX's
/// redaction recognizes: `sk-`, `ghp_`, `glpat-`, `xoxb-`, `AIza`, `AKIA`, a JWT, ...), or a long
/// run of letters and digits in both cases, which is what a random key looks like and what no
/// model, capability or work id is. FX's own redaction is not linked here: only the adapter
/// crates may link FX (crates/chip-fx/tests/architecture.rs).
fn credential_shaped(s: &str) -> bool {
    const PREFIXES: &[(&str, usize)] = &[
        ("sk-", 10),
        ("sk_", 10),
        ("rk_", 10),
        ("pk_live_", 8),
        ("pk_test_", 8),
        ("ghp_", 20),
        ("gho_", 20),
        ("ghu_", 20),
        ("ghs_", 20),
        ("ghr_", 20),
        ("github_pat_", 20),
        ("glpat-", 16),
        ("xoxa-", 10),
        ("xoxb-", 10),
        ("xoxp-", 10),
        ("xoxr-", 10),
        ("xoxs-", 10),
        ("AIza", 30),
        ("npm_", 30),
        ("hf_", 30),
        ("AKIA", 16),
        ("ASIA", 16),
        ("eyJ", 20),
    ];
    let segments = s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    for segment in segments {
        if PREFIXES
            .iter()
            .any(|(p, rest)| segment.starts_with(p) && segment.len() >= p.len() + rest)
        {
            return true;
        }
        // A run of 24 or more letters and digits with upper case, lower case and a digit.
        for run in segment.split(|c: char| !c.is_ascii_alphanumeric()) {
            let mixed = run.bytes().any(|b| b.is_ascii_uppercase())
                && run.bytes().any(|b| b.is_ascii_lowercase())
                && run.bytes().any(|b| b.is_ascii_digit());
            if run.len() >= 24 && mixed {
                return true;
            }
        }
    }
    false
}

/// A work id as stack-agent makes them (`work_` and 16 hex digits), or the task's own id.
fn work_id<'a>(s: &'a str, task_id: &str) -> Option<&'a str> {
    let generated = s.strip_prefix("work_").is_some_and(|h| {
        h.len() == 16 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    });
    (generated || s == task_id).then_some(s).and_then(token)
}

/// An execution id of this work: `<work id>-exec-<n>`, as the work loop names them.
fn execution_id<'a>(s: &'a str, work: Option<&str>) -> Option<&'a str> {
    let n = s.strip_prefix(work?)?.strip_prefix("-exec-")?;
    (!n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).then_some(s)
}

/// The FeltDB key of an execution's observation record: `<task id>~<intent key>`.
fn observation_record<'a>(s: &'a str, task_id: &str) -> Option<&'a str> {
    s.strip_prefix(task_id)?.strip_prefix('~')?;
    token(s).filter(|s| !s.contains(' '))
}

/// A model or provider name: a [`token`] without spaces.
fn name(s: &str) -> Option<&str> {
    token(s).filter(|s| !s.contains(' '))
}

/// A capability id, as `chip_core::CapabilityId` admits them: 1 to 128 of `a-z 0-9 . _ -`.
fn capability(s: &str) -> Option<&str> {
    let ok = !s.is_empty()
        && s.len() <= 128
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
        && !credential_shaped(s);
    ok.then_some(s)
}

/// A decision label (`WorkDecision::label`): `complete`, `escalate`, `block` or
/// `request <capability>`. A summary or a reason is never part of one.
fn decision(s: &str) -> Option<&str> {
    let ok = match s.strip_prefix("request ") {
        Some(c) => capability(c).is_some(),
        None => matches!(s, "complete" | "escalate" | "block"),
    };
    ok.then_some(s)
}

/// A provider-reported cost: a plain decimal number, nothing else.
fn cost(s: &str) -> Option<&str> {
    let (mantissa, exponent) = match s.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (s, None),
    };
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = match mantissa.split_once('.') {
        Some((i, f)) => digits(i) && digits(f),
        None => digits(mantissa),
    };
    let exponent_ok = exponent.is_none_or(|e| digits(e.trim_start_matches(['+', '-'])));
    (s.len() <= 32 && mantissa_ok && exponent_ok).then_some(s)
}

/// A member of a fixed set, or nothing.
fn member<'a>(s: &'a str, set: &[&str]) -> Option<&'a str> {
    set.contains(&s).then_some(s)
}

/// A 64-character lowercase hex digest.
fn digest(s: &str) -> Option<&str> {
    (s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))).then_some(s)
}

const OUTCOMES: &[&str] = &[
    "completed",
    "escalated",
    "blocked",
    "limit_reached",
    "failed",
    "cancelled",
];
const OBSERVATION_KINDS: &[&str] = &[
    "execution.completed",
    "execution.failed",
    "execution.cancelled",
];
const LIMITS: &[&str] = &["turns", "executions", "context"];
const STOP_CAUSES: &[&str] = &["cancelled", "deadline"];
const DELIVERIES: &[&str] = &["definitely_unsent", "possibly_sent", "confirmed_response"];
const REMOTE_CANCELLATIONS: &[&str] = &["not_started", "unconfirmed", "confirmed"];

/// One OTLP attribute list, built only through the typed setters.
#[derive(Default)]
struct Attrs(Vec<Value>);

impl Attrs {
    fn string(&mut self, key: &str, value: Option<&str>) -> &mut Self {
        // A second, independent guard behind the validators: a value that looks like a
        // credential is not exported at all, whatever validator let it through.
        if let Some(v) = value.filter(|v| !credential_shaped(v)) {
            self.0
                .push(json!({"key": key, "value": {"stringValue": v}}));
        }
        self
    }
    fn int(&mut self, key: &str, value: Option<u64>) -> &mut Self {
        if let Some(v) = value {
            // OTLP/JSON writes 64-bit integers as strings.
            self.0
                .push(json!({"key": key, "value": {"intValue": v.to_string()}}));
        }
        self
    }
    fn bool(&mut self, key: &str, value: Option<bool>) -> &mut Self {
        if let Some(v) = value {
            self.0.push(json!({"key": key, "value": {"boolValue": v}}));
        }
        self
    }
    fn provenance(&mut self, p: Provenance) -> &mut Self {
        self.string("stack_agent.provenance", Some(p.as_str()))
    }
    fn done(&mut self) -> Value {
        Value::Array(std::mem::take(&mut self.0))
    }
}

fn field<'a>(event: &'a Value, name: &str) -> Option<&'a Value> {
    event.get(name).filter(|v| !v.is_null())
}
fn text<'a>(event: &'a Value, name: &str) -> Option<&'a str> {
    field(event, name)?.as_str()
}
fn count(event: &Value, name: &str) -> Option<u64> {
    field(event, name)?.as_u64()
}
fn flag(event: &Value, name: &str) -> Option<bool> {
    field(event, name)?.as_bool()
}

// ---- identifiers and time ---------------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn derived(parts: &[&str], len: usize) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(part.as_bytes());
        hash.update([0]);
    }
    hex(&hash.finalize()[..len])
}

/// The task's trace id: 16 bytes derived from the task id, so one task is always one trace.
pub fn trace_id(task_id: &str) -> String {
    derived(&["stack-agent.trace", task_id], 16)
}

fn span_id(task_id: &str, attempt: u64, role: &str, index: u64) -> String {
    derived(
        &[
            "stack-agent.span",
            task_id,
            &attempt.to_string(),
            role,
            &index.to_string(),
        ],
        8,
    )
}

fn nanos(ms: u64) -> String {
    (u128::from(ms) * 1_000_000).to_string()
}

const KIND_INTERNAL: u64 = 1;
const KIND_CLIENT: u64 = 3;
const STATUS_ERROR: u64 = 2;

/// GenAI's provider name, for the providers it names; `None` when stack-agent's name does not map
/// to exactly one (an OpenAI-compatible endpoint can be anyone's).
fn genai_provider(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("anthropic"),
        "openai" => Some("openai"),
        _ => None,
    }
}

// ---- the mapping --------------------------------------------------------------------------------

struct Span {
    span_id: String,
    parent: Option<String>,
    name: String,
    kind: u64,
    start_ms: u64,
    end_ms: u64,
    attributes: Value,
    events: Vec<Value>,
    error: Option<&'static str>,
}

impl Span {
    fn to_json(&self, trace: &str) -> Value {
        let mut v = json!({
            "traceId": trace,
            "spanId": self.span_id,
            "name": self.name,
            "kind": self.kind,
            "startTimeUnixNano": nanos(self.start_ms),
            "endTimeUnixNano": nanos(self.end_ms.max(self.start_ms)),
            "attributes": self.attributes,
            "events": self.events,
            "status": match self.error {
                Some(message) => json!({"code": STATUS_ERROR, "message": message}),
                None => json!({}),
            },
        });
        if let Some(parent) = &self.parent {
            v["parentSpanId"] = parent.as_str().into();
        }
        v
    }
}

/// A span event for one recorded work event, with only the fields its kind may export.
fn span_event(record: &WorkEventRecord, kind: &str, work: Option<&str>) -> Value {
    let e = &record.event;
    let mut a = Attrs::default();
    a.provenance(record.provenance)
        .int("stack_agent.event.index", Some(record.index))
        .int("stack_agent.turn", count(e, "turn"));
    match kind {
        "WorkStarted" => {
            a.string(
                "stack_agent.goal.sha256",
                text(e, "goal_sha256").and_then(digest),
            )
            .int("stack_agent.goal.bytes", count(e, "goal_bytes"))
            .int("stack_agent.limits.max_turns", count(e, "max_turns"))
            .int(
                "stack_agent.limits.max_executions",
                count(e, "max_executions"),
            );
        }
        "CapabilitiesAvailable" => {
            a.int("stack_agent.capabilities", count(e, "count"));
        }
        "DecisionMade" | "LocalDecision" => {
            a.string(
                "stack_agent.decision",
                text(e, "decision").and_then(decision),
            );
        }
        "CapabilityRequested" | "EvidenceRecorded" | "EvidenceReused" => {
            a.string(
                "gen_ai.tool.name",
                text(e, "capability").and_then(capability),
            );
        }
        "ObservationRecorded" | "ObservationRestored" => {
            a.string(
                "gen_ai.tool.call.id",
                text(e, "execution_id").and_then(|id| execution_id(id, work)),
            )
            .string(
                "stack_agent.observation",
                text(e, "observation").and_then(|o| member(o, OBSERVATION_KINDS)),
            )
            .string(
                "gen_ai.tool.name",
                text(e, "capability").and_then(capability),
            )
            .int("stack_agent.remaining", count(e, "remaining"))
            .bool("stack_agent.reobserved", flag(e, "reobserved"));
        }
        "GoalEvaluated" => {
            a.bool("stack_agent.goal.satisfied", flag(e, "satisfied"))
                .int("stack_agent.goal.remaining", count(e, "remaining"));
        }
        "ContextLimit" => {
            a.int("stack_agent.request_bytes", count(e, "request_bytes"))
                .int("stack_agent.budget_bytes", count(e, "budget_bytes"));
        }
        "WorkLimitReached" => {
            a.string(
                "stack_agent.limit",
                text(e, "limit").and_then(|l| member(l, LIMITS)),
            );
        }
        "WorkFailed" | "WorkCancelled" => {
            a.string(
                "stack_agent.delivery",
                text(e, "delivery").and_then(|d| member(d, DELIVERIES)),
            )
            .string(
                "stack_agent.stop_cause",
                text(e, "cause").and_then(|c| member(c, STOP_CAUSES)),
            )
            .string(
                "stack_agent.remote_cancellation",
                text(e, "remote_cancellation").and_then(|r| member(r, REMOTE_CANCELLATIONS)),
            );
        }
        _ => {}
    }
    json!({
        "timeUnixNano": nanos(record.recorded_at_ms),
        "name": kind,
        "attributes": a.done(),
    })
}

fn usage_attrs(a: &mut Attrs, e: &Value) {
    a.int("gen_ai.usage.input_tokens", count(e, "prompt_tokens"))
        .int("gen_ai.usage.output_tokens", count(e, "completion_tokens"))
        .int(
            "gen_ai.usage.cache_read.input_tokens",
            count(e, "cache_read_tokens"),
        )
        .int(
            "gen_ai.usage.cache_creation.input_tokens",
            count(e, "cache_write_tokens"),
        )
        .int(
            "stack_agent.usage.reasoning_tokens",
            count(e, "reasoning_tokens"),
        )
        .string("stack_agent.usage.cost", text(e, "cost").and_then(cost))
        .string(
            "gen_ai.response.model",
            text(e, "model_reported").and_then(name),
        );
}

/// Sums of the usage fields over an attempt's model calls (`None` where no call reported one).
#[derive(Default)]
struct Totals {
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    reasoning: Option<u64>,
}

impl Totals {
    fn add(&mut self, e: &Value) {
        let add = |slot: &mut Option<u64>, n: Option<u64>| {
            if let Some(n) = n {
                *slot = Some(slot.unwrap_or(0) + n);
            }
        };
        add(&mut self.input, count(e, "prompt_tokens"));
        add(&mut self.output, count(e, "completion_tokens"));
        add(&mut self.cache_read, count(e, "cache_read_tokens"));
        add(&mut self.cache_write, count(e, "cache_write_tokens"));
        add(&mut self.reasoning, count(e, "reasoning_tokens"));
    }
}

/// The spans of one attempt: its root and its children.
fn attempt_spans(
    task_id: &str,
    attempt: u64,
    records: &[&WorkEventRecord],
    verification: Option<&Verification>,
    outcome: Option<&str>,
    context: &Context,
) -> Vec<Span> {
    let root_id = span_id(task_id, attempt, "invoke_agent", 0);
    let work = records.first().and_then(|r| work_id(&r.work_id, task_id));
    let first = records.first().map_or(0, |r| r.recorded_at_ms);
    let last = records.last().map_or(first, |r| r.recorded_at_ms);
    let mut children = Vec::new();
    let mut events = Vec::new();
    let mut totals = Totals::default();
    let (mut model_calls, mut executions) = (0u64, 0u64);
    let mut terminal = None;
    // An open model call (its escalation event) and open executions (by id), until their ends.
    let mut open_call: Option<(&WorkEventRecord, u64)> = None;
    let mut context_bytes = None;
    let mut open_exec: Vec<(String, &WorkEventRecord, Option<String>, u64)> = Vec::new();
    let provider = context.provider.as_deref().and_then(name);
    let request_model = context.request_model.as_deref().and_then(name);
    for record in records {
        let e = &record.event;
        let kind = text(e, "kind").unwrap_or_default();
        match kind {
            "ModelEscalation" => {
                model_calls += 1;
                open_call = Some((record, model_calls));
                context_bytes = count(e, "context_bytes");
            }
            "ModelCalled" => {
                let (start, n) = open_call
                    .take()
                    .map_or((record.recorded_at_ms, model_calls), |(r, n)| {
                        (r.recorded_at_ms, n)
                    });
                totals.add(e);
                let mut a = Attrs::default();
                a.string("gen_ai.operation.name", Some("chat"))
                    .string("gen_ai.provider.name", provider.and_then(genai_provider))
                    .string("stack_agent.provider", provider)
                    .string("gen_ai.request.model", request_model)
                    .int("stack_agent.turn", count(e, "turn"))
                    .int("stack_agent.context_bytes", context_bytes.take())
                    .bool("stack_agent.usage.reported", flag(e, "usage_reported"))
                    .provenance(record.provenance);
                usage_attrs(&mut a, e);
                let succeeded = flag(e, "succeeded").unwrap_or(false);
                children.push(Span {
                    span_id: span_id(task_id, attempt, "chat", n),
                    parent: Some(root_id.clone()),
                    name: match request_model {
                        Some(m) => format!("chat {m}"),
                        None => "chat".into(),
                    },
                    kind: KIND_CLIENT,
                    start_ms: start,
                    end_ms: record.recorded_at_ms,
                    attributes: a.done(),
                    events: Vec::new(),
                    error: (!succeeded).then_some("the model call failed"),
                });
            }
            "ExecutionRequested" => {
                executions += 1;
                let id = text(e, "execution_id").unwrap_or_default().to_string();
                let capability = text(e, "capability")
                    .and_then(capability)
                    .map(str::to_string);
                open_exec.push((id, record, capability, executions));
            }
            "ExecutionStarted" => {}
            "ExecutionCompleted" | "ExecutionFailed" => {
                let id = text(e, "execution_id").unwrap_or_default();
                let opened = open_exec
                    .iter()
                    .position(|(open, ..)| open == id)
                    .map(|at| open_exec.remove(at));
                let (start, capability, n) = match opened {
                    Some((_, r, capability, n)) => (r.recorded_at_ms, capability, n),
                    None => (record.recorded_at_ms, None, executions),
                };
                let mut a = Attrs::default();
                a.string("gen_ai.operation.name", Some("execute_tool"))
                    .string("gen_ai.tool.name", capability.as_deref())
                    .string("gen_ai.tool.call.id", execution_id(id, work))
                    .int("stack_agent.output_bytes", count(e, "output_bytes"))
                    .string(
                        "stack_agent.output_sha256",
                        text(e, "output_sha256").and_then(digest),
                    )
                    .string(
                        "stack_agent.observation_record",
                        text(e, "observation_record").and_then(|o| observation_record(o, task_id)),
                    )
                    .provenance(record.provenance);
                let failed = kind == "ExecutionFailed";
                if failed {
                    a.string("error.type", Some("execution_failed"));
                }
                children.push(Span {
                    span_id: span_id(task_id, attempt, "execute_tool", n),
                    parent: Some(root_id.clone()),
                    name: match &capability {
                        Some(c) => format!("execute_tool {c}"),
                        None => "execute_tool".into(),
                    },
                    kind: KIND_INTERNAL,
                    start_ms: start,
                    end_ms: record.recorded_at_ms,
                    attributes: a.done(),
                    events: Vec::new(),
                    error: failed.then_some("the execution failed"),
                });
            }
            other => {
                if other.starts_with("Work") && other != "WorkStarted" {
                    terminal = Some(other);
                }
                if !other.is_empty() {
                    events.push(span_event(record, other, work));
                }
            }
        }
    }
    // What was still in flight when the record ends: it was interrupted there.
    if let Some((r, n)) = open_call {
        let mut a = Attrs::default();
        a.string("gen_ai.operation.name", Some("chat"))
            .string("gen_ai.provider.name", provider.and_then(genai_provider))
            .string("gen_ai.request.model", request_model)
            .bool("stack_agent.interrupted", Some(true))
            .provenance(r.provenance);
        children.push(Span {
            span_id: span_id(task_id, attempt, "chat", n),
            parent: Some(root_id.clone()),
            name: "chat".into(),
            kind: KIND_CLIENT,
            start_ms: r.recorded_at_ms,
            end_ms: last,
            attributes: a.done(),
            events: Vec::new(),
            error: Some("interrupted: the record ends before the model call returned"),
        });
    }
    for (id, r, capability, n) in open_exec {
        let mut a = Attrs::default();
        a.string("gen_ai.operation.name", Some("execute_tool"))
            .string("gen_ai.tool.name", capability.as_deref())
            .string("gen_ai.tool.call.id", execution_id(&id, work))
            .bool("stack_agent.interrupted", Some(true))
            .provenance(r.provenance);
        children.push(Span {
            span_id: span_id(task_id, attempt, "execute_tool", n),
            parent: Some(root_id.clone()),
            name: "execute_tool".into(),
            kind: KIND_INTERNAL,
            start_ms: r.recorded_at_ms,
            end_ms: last,
            attributes: a.done(),
            events: Vec::new(),
            error: Some("interrupted: the record ends before the execution's outcome"),
        });
    }
    let interrupted = terminal.is_none();
    let mut a = Attrs::default();
    a.string("gen_ai.operation.name", Some("invoke_agent"))
        .string("gen_ai.agent.name", Some("stack-agent"))
        .string("gen_ai.agent.id", name(task_id))
        .string("gen_ai.conversation.id", name(task_id))
        .string("gen_ai.provider.name", provider.and_then(genai_provider))
        .string("stack_agent.provider", provider)
        .string("gen_ai.request.model", request_model)
        .int("stack_agent.attempt", Some(attempt))
        .string("stack_agent.work_id", work)
        .string(
            "stack_agent.outcome",
            outcome.and_then(|o| member(o, OUTCOMES)),
        )
        .bool("stack_agent.interrupted", Some(interrupted))
        .int("stack_agent.model_calls", Some(model_calls))
        .int("stack_agent.executions", Some(executions))
        .int("gen_ai.usage.input_tokens", totals.input)
        .int("gen_ai.usage.output_tokens", totals.output)
        .int("gen_ai.usage.cache_read.input_tokens", totals.cache_read)
        .int(
            "gen_ai.usage.cache_creation.input_tokens",
            totals.cache_write,
        )
        .int("stack_agent.usage.reasoning_tokens", totals.reasoning)
        .provenance(Provenance::RuntimeDecided);
    if let Some(v) = verification {
        a.bool("stack_agent.verified", Some(v.verified))
            .bool("stack_agent.accepted", Some(v.accepted))
            .bool("stack_agent.goal.satisfied", v.goal_satisfied)
            .bool("stack_agent.grounded", Some(v.grounded))
            .bool(
                "stack_agent.evidence_invalidated",
                Some(v.evidence_invalidated.is_some()),
            )
            .int("stack_agent.exit_status", u64::try_from(v.exit_status).ok());
    }
    let error = if interrupted {
        Some("interrupted: the record ends before the work did")
    } else if terminal == Some("WorkFailed") {
        a.string("error.type", Some("work_failed"));
        Some("the work failed")
    } else {
        None
    };
    let root = Span {
        span_id: root_id,
        parent: None,
        name: "invoke_agent stack-agent".into(),
        kind: KIND_INTERNAL,
        start_ms: first,
        end_ms: last,
        attributes: a.done(),
        events,
        error,
    };
    std::iter::once(root).chain(children).collect()
}

/// The task's record as one OTLP/JSON `ExportTraceServiceRequest`: one trace, a root span per
/// attempt that recorded events. Pure: the same record and context always give the same request.
pub fn trace(history: &History, context: &Context) -> Value {
    let task_id = history.task.task_id.as_str();
    let trace = trace_id(task_id);
    let mut attempts: Vec<u64> = history.events.iter().map(|e| e.attempt).collect();
    attempts.dedup();
    let mut spans = Vec::new();
    for attempt in attempts {
        let records: Vec<&WorkEventRecord> = history
            .events
            .iter()
            .filter(|e| e.attempt == attempt)
            .collect();
        let result = history.results.iter().find(|r| r.attempt == attempt);
        spans.extend(
            attempt_spans(
                task_id,
                attempt,
                &records,
                result.and_then(|r| r.verification.as_ref()),
                result.map(|r| r.outcome.as_str()),
                context,
            )
            .iter()
            .map(|s| s.to_json(&trace)),
        );
    }
    let mut resource = Attrs::default();
    resource
        .string(
            "service.name",
            Some(name(&context.service_name).unwrap_or("stack-agent")),
        )
        .string("service.version", name(&context.service_version))
        .string("telemetry.sdk.name", Some("stack-agent"))
        .string("telemetry.sdk.language", Some("rust"));
    json!({
        "resourceSpans": [{
            "resource": {"attributes": resource.done()},
            "scopeSpans": [{
                "scope": {"name": SCOPE_NAME, "version": name(&context.service_version)},
                "spans": spans,
            }],
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_strict_values_pass() {
        assert_eq!(token("request project.read"), Some("request project.read"));
        assert_eq!(
            token("work_0123~a2.work_0123-exec-1"),
            Some("work_0123~a2.work_0123-exec-1")
        );
        for bad in [
            "",
            "a\nb",
            "two  spaces",
            " lead",
            "sk-\"x\"",
            "path\\x",
            &"x".repeat(129),
        ] {
            assert_eq!(token(bad), None, "{bad:?}");
        }
        for good in ["0.00123", "12", "1.5e-3", "2E+4"] {
            assert_eq!(cost(good), Some(good));
        }
        for bad in ["$0.01", "0.01 USD", "", "1.", ".5", "token=abc"] {
            assert_eq!(cost(bad), None, "{bad:?}");
        }
        // Names that are not credentials stay exportable.
        for good in [
            "claude-sonnet-4-5-20250929",
            "gpt-4o-mini-2024-07-18",
            "accounts/fireworks/models/llama-v3p1-70b-instruct",
            "work_0123456789abcdef",
            "project.read",
            "sk-short",
        ] {
            assert!(!credential_shaped(good), "{good}");
        }
        let shaped = |parts: &[&str]| parts.concat();
        for bad in [
            shaped(&["sk-ant-", "api03-abcdefghij"]),
            shaped(&["sk-proj-", "abcdefghijkl"]),
            shaped(&["model/", "ghp_", "abcdefghijklmnopqrstuvwxyz"]),
            shaped(&["AKIA", "IOSFODNN7EXAMPLE"]),
            shaped(&["eyJhbGciOiJ", "IUzI1NiJ9abc"]),
            shaped(&["a1B2c3D4e5F6", "g7H8i9J0kLmN"]),
        ] {
            assert!(credential_shaped(&bad), "{bad}");
            assert_eq!(token(&bad), None, "{bad}");
        }
        assert_eq!(capability(&shaped(&["sk-proj-", "abcdefghijkl"])), None);
    }
}
