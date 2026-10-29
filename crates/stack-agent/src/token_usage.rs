//! Per-call token accounting for `work`: one provider-neutral record per model call, and the rules
//! that turn what each provider reported into comparable numbers without double counting.
//!
//! [`Recording`] wraps the work's model provider (outermost, so it sees exactly what Chip sends and
//! what Chip gets back) and appends one [`CallRecord`] per call to a [`CallLedger`]. After the run,
//! [`work_section`] joins the ledger with the work's own trajectory and writes the report's
//! `token_usage` section (schema [`SCHEMA`]).
//!
//! What a record holds, and what it never holds:
//!
//! * Counts and sizes measured here, on this side of the provider: messages and bytes by role, the
//!   bytes a JSON rendering of the messages takes (a local size, not the provider's wire body), and
//!   how many of a request's messages were byte-identical to a message of the previous request
//!   (compared by a SHA-256 fingerprint kept only in memory).
//! * What the provider reported, field for field: a field it did not report is `null`, never 0.
//! * Timing (start, duration, gap since the previous call ended) and, for a failed call, FX's
//!   failure kind, delivery certainty and HTTP status.
//!
//! No message text, reply text, path, key or header is recorded, and nothing here is an estimate:
//! the report labels its one local measure (`serialized_messages_bytes_local`) as local.
//!
//! # Accounting rules
//!
//! Providers count cached input differently, and adding the wrong fields counts a token twice:
//!
//! * **Anthropic** (`input_semantics: cache_separate`): `input_tokens` excludes
//!   `cache_read_input_tokens` and `cache_creation_input_tokens`. Total input =
//!   input + cache read + cache write; uncached input = `input_tokens`.
//! * **OpenAI-compatible** (`input_semantics: cache_included`): `prompt_tokens` includes
//!   `prompt_tokens_details.cached_tokens`. Total input = `prompt_tokens`; uncached input =
//!   prompt − cached. There is no cache-write count (`null`).
//!
//! A derived number is `null` whenever any field it needs is `null`: a half-known total is not a
//! total. Total tokens = total input + output; reasoning tokens are part of the output and are never
//! added to it.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use chip_core::{ContextReport, WorkEvent, WorkReport};
use chip_inference::{FxError, MessageRole, ModelProvider, ModelRequest, ModelResponse, Usage};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The schema of the report's `token_usage` section.
pub const SCHEMA: &str = "stack-agent.token-usage.v1";
/// `off` leaves the ledger out entirely: no wrapper, no `token_usage` section.
pub const DIAGNOSTICS_VARIABLE: &str = "CHIP_TOKEN_DIAGNOSTICS";
/// `key=value,key=value` labels recorded with the run (for example `course=...,obstacle=...`), so a
/// harness can aggregate runs. Labels are the caller's words, recorded as given after validation.
pub const LABELS_VARIABLE: &str = "CHIP_RUN_LABELS";

const MAX_LABELS: usize = 16;
const MAX_LABEL_KEY: usize = 32;
const MAX_LABEL_VALUE: usize = 128;

/// Whether the ledger is on, from [`DIAGNOSTICS_VARIABLE`]: on unless it says `off`.
pub fn diagnostics_enabled(env: &dyn Fn(&str) -> Option<String>) -> Result<bool, String> {
    match env(DIAGNOSTICS_VARIABLE).as_deref().map(str::trim) {
        None | Some("") | Some("on") => Ok(true),
        Some("off") => Ok(false),
        Some(other) => Err(format!(
            "{DIAGNOSTICS_VARIABLE} takes `on` or `off` (got `{other}`)"
        )),
    }
}

/// Parses `key=value,key=value`. Keys are lower-case letters, digits, `_`, `-` or `.`; values are
/// printable ASCII without `,` or `=`. Anything else is refused by name rather than recorded.
pub fn parse_labels(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut labels = BTreeMap::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| format!("a run label is `key=value` (got `{part}`)"))?;
        let (key, value) = (key.trim(), value.trim());
        let key_ok = !key.is_empty()
            && key.len() <= MAX_LABEL_KEY
            && key.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.')
            });
        let value_ok = !value.is_empty()
            && value.len() <= MAX_LABEL_VALUE
            && value
                .bytes()
                .all(|b| (0x20..0x7f).contains(&b) && b != b',' && b != b'=');
        if !key_ok || !value_ok {
            return Err(format!(
                "run label `{key}` is not accepted: keys are 1-{MAX_LABEL_KEY} of [a-z0-9_.-], values 1-{MAX_LABEL_VALUE} printable ASCII without `,` or `=`"
            ));
        }
        if labels.insert(key.to_string(), value.to_string()).is_some() {
            return Err(format!("run label `{key}` is given twice"));
        }
    }
    if labels.len() > MAX_LABELS {
        return Err(format!("at most {MAX_LABELS} run labels are accepted"));
    }
    Ok(labels)
}

/// How a provider's input count relates to its cache counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputSemantics {
    /// The input count excludes cache reads and writes (Anthropic).
    CacheSeparate,
    /// The input count includes cache reads (OpenAI-compatible `prompt_tokens`).
    CacheIncluded,
    /// Not known for this provider: nothing is derived.
    Unknown,
}

impl InputSemantics {
    pub fn of_provider(provider: &str) -> Self {
        match provider {
            chip_inference::PROVIDER_ANTHROPIC => Self::CacheSeparate,
            chip_inference::PROVIDER_OPENAI_COMPATIBLE => Self::CacheIncluded,
            _ => Self::Unknown,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::CacheSeparate => "cache_separate",
            Self::CacheIncluded => "cache_included",
            Self::Unknown => "unknown",
        }
    }
}

/// One call's usage under the accounting rules. Every field is `None` unless known.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Accounted {
    /// The provider's own input field, as reported (`input_tokens` or `prompt_tokens`).
    pub input_reported: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
    pub reasoning: Option<u64>,
    /// Input processed without the cache (billed at the ordinary input rate).
    pub uncached_input: Option<u64>,
    /// Everything the request carried as input: uncached + cache read + cache write.
    pub total_input: Option<u64>,
    /// Total input + output.
    pub total: Option<u64>,
}

/// Applies the accounting rules to what one call reported.
pub fn account(usage: &Usage, semantics: InputSemantics) -> Accounted {
    let mut a = Accounted {
        input_reported: usage.prompt_tokens,
        output: usage.completion_tokens,
        cache_read: usage.cache_read_tokens,
        cache_write: usage.cache_write_tokens,
        reasoning: usage.reasoning_tokens,
        ..Accounted::default()
    };
    match semantics {
        InputSemantics::CacheSeparate => {
            a.uncached_input = usage.prompt_tokens;
            a.total_input = (|| {
                Some(usage.prompt_tokens? + usage.cache_read_tokens? + usage.cache_write_tokens?)
            })();
        }
        InputSemantics::CacheIncluded => {
            a.total_input = usage.prompt_tokens;
            a.uncached_input = (|| usage.prompt_tokens?.checked_sub(usage.cache_read_tokens?))();
        }
        InputSemantics::Unknown => {}
    }
    a.total = (|| Some(a.total_input? + a.output?))();
    a
}

/// Messages and bytes of one role in one request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoleSize {
    pub messages: usize,
    pub bytes: usize,
}

/// What a failed call's error said, typed; never its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallFailure {
    pub kind: String,
    pub delivery: Option<&'static str>,
    pub status: Option<u16>,
    pub partial_output_bytes: Option<usize>,
}

impl CallFailure {
    fn of(error: &FxError) -> Self {
        let kind = |k: &str| Self {
            kind: k.to_string(),
            delivery: None,
            status: None,
            partial_output_bytes: None,
        };
        match error {
            FxError::Inference(f) => Self {
                kind: f.kind.clone(),
                delivery: Some(f.delivery.as_str()),
                status: f.status,
                partial_output_bytes: f.partial_output.as_ref().map(String::len),
            },
            FxError::InvalidRequest(_) => kind("invalid_request"),
            FxError::Provider(_) => kind("provider"),
            FxError::Configuration(_) => kind("configuration"),
            FxError::Authentication(_) => kind("authentication"),
            FxError::Http(_) => kind("http"),
            FxError::InvalidResponse(_) => kind("invalid_response"),
            FxError::Timeout(_) => kind("timeout"),
            FxError::Serialization(_) => kind("serialization"),
        }
    }
}

/// One model call, as measured around the provider.
#[derive(Debug, Clone, PartialEq)]
pub struct CallRecord {
    /// 1-based, in the order the calls were made.
    pub seq: usize,
    pub started_unix_ms: u64,
    pub elapsed_ms: f64,
    /// From the end of the previous call to the start of this one: capability execution and
    /// runtime work between calls. `None` for the first call.
    pub gap_before_ms: Option<f64>,
    pub model_requested: String,
    pub system: RoleSize,
    pub user: RoleSize,
    pub assistant: RoleSize,
    /// Bytes of a compact JSON rendering of `[{role, content}, ...]`: a local, provider-neutral
    /// size, not the provider's wire body (which adds its own framing and options).
    pub serialized_messages_bytes_local: usize,
    /// Messages byte-identical to a message of the previous request, and their content bytes.
    pub resent_messages: usize,
    pub resent_bytes: usize,
    pub max_tokens: Option<u32>,
    /// `Ok(usage)` or the failure.
    pub outcome: Result<(Usage, usize), CallFailure>,
}

impl CallRecord {
    pub fn content_bytes(&self) -> usize {
        self.system.bytes + self.user.bytes + self.assistant.bytes
    }
    pub fn messages(&self) -> usize {
        self.system.messages + self.user.messages + self.assistant.messages
    }
}

#[derive(Debug, Default)]
struct LedgerState {
    calls: Vec<CallRecord>,
    previous: HashSet<[u8; 32]>,
    last_end: Option<Instant>,
}

/// The run's call records, in order.
#[derive(Debug, Default)]
pub struct CallLedger {
    state: Mutex<LedgerState>,
}

impl CallLedger {
    pub fn calls(&self) -> Vec<CallRecord> {
        self.state
            .lock()
            .map(|s| s.calls.clone())
            .unwrap_or_default()
    }
}

fn fingerprint(role: &MessageRole, content: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(match role {
        MessageRole::System => b"s\0",
        MessageRole::User => b"u\0",
        MessageRole::Assistant => b"a\0",
    });
    h.update(content.as_bytes());
    h.finalize().into()
}

fn role_name(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}

/// The model provider that records every call into a [`CallLedger`].
pub struct Recording {
    pub inner: Arc<dyn ModelProvider>,
    pub ledger: Arc<CallLedger>,
}

#[async_trait]
impl ModelProvider for Recording {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, FxError> {
        let (mut system, mut user, mut assistant) = (
            RoleSize::default(),
            RoleSize::default(),
            RoleSize::default(),
        );
        let mut prints = HashSet::new();
        let mut sizes = Vec::with_capacity(request.messages.len());
        for m in &request.messages {
            let slot = match m.role {
                MessageRole::System => &mut system,
                MessageRole::User => &mut user,
                MessageRole::Assistant => &mut assistant,
            };
            slot.messages += 1;
            slot.bytes += m.content.len();
            let print = fingerprint(&m.role, &m.content);
            sizes.push((print, m.content.len()));
            prints.insert(print);
        }
        let serialized = serde_json::to_vec(
            &request
                .messages
                .iter()
                .map(|m| json!({"role": role_name(&m.role), "content": m.content}))
                .collect::<Vec<_>>(),
        )
        .map_or(0, |v| v.len());
        let model_requested = request.model.to_string();
        let max_tokens = request.max_tokens;
        let started_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let started = Instant::now();
        let result = self.inner.complete(request).await;
        let ended = Instant::now();
        let elapsed_ms = ended.duration_since(started).as_secs_f64() * 1000.0;
        if let Ok(mut state) = self.ledger.state.lock() {
            let (resent_messages, resent_bytes) = sizes
                .iter()
                .filter(|(p, _)| state.previous.contains(p))
                .fold((0, 0), |(n, b), (_, len)| (n + 1, b + len));
            let gap_before_ms = state
                .last_end
                .map(|end| started.saturating_duration_since(end).as_secs_f64() * 1000.0);
            let seq = state.calls.len() + 1;
            state.calls.push(CallRecord {
                seq,
                started_unix_ms,
                elapsed_ms,
                gap_before_ms,
                model_requested,
                system,
                user,
                assistant,
                serialized_messages_bytes_local: serialized,
                resent_messages,
                resent_bytes,
                max_tokens,
                outcome: match &result {
                    Ok(r) => Ok((r.usage.clone(), r.output.len())),
                    Err(e) => Err(CallFailure::of(e)),
                },
            });
            state.previous = prints;
            state.last_end = Some(ended);
        }
        result
    }
}

fn round(ms: f64) -> f64 {
    (ms * 1000.0).round() / 1000.0
}

/// What each model call led to, from the work's own events, in call order: the decision's label
/// (`request <capability>`, `complete`, ...), `rejected`, or `none` (the call failed, or the work
/// ended before deciding). Never a reason text, which can quote the reply.
pub fn call_decisions(events: &[WorkEvent]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut open = false;
    for event in events {
        match event {
            WorkEvent::ModelCalled { .. } => {
                out.push("none".to_string());
                open = true;
            }
            WorkEvent::DecisionMade { decision, .. } if open => {
                if let Some(last) = out.last_mut() {
                    *last = decision_label(decision);
                }
                open = false;
            }
            WorkEvent::DecisionRejected { .. } if open => {
                if let Some(last) = out.last_mut() {
                    *last = "rejected".to_string();
                }
                open = false;
            }
            _ => {}
        }
    }
    out
}

/// What each model call's request was assembled as, from the work's own events, in call order: the
/// context policy, the context profile and the rule that chose it (for a policy with profiles), the
/// previous profile, the sections added, how many observations travelled in full, condensed to
/// facts or omitted as repeats, the bytes condensation withheld, the evidence items, and how long
/// assembly took. Chip's own words and counts; never message text.
pub fn call_contexts(report: &WorkReport) -> Vec<Value> {
    let mut out = Vec::new();
    let mut pending: Option<Value> = None;
    let mut escalation = 0usize;
    for event in &report.events {
        match event {
            WorkEvent::ModelEscalation {
                context_policy,
                context,
                profile,
                ..
            } => {
                let assembly_ms = report
                    .context_assembly
                    .get(escalation)
                    .map(|d| round(d.as_secs_f64() * 1000.0));
                escalation += 1;
                pending = Some(json!({
                    "policy": context_policy,
                    "profile": profile.as_ref().map(|p| p.profile.name()),
                    "reason": profile.as_ref().map(|p| p.reason.as_str()),
                    "previous_profile": profile.as_ref().and_then(|p| p.previous.map(|q| q.name())),
                    "changed": profile.as_ref().map(|p| p.previous.is_some_and(|q| q != p.profile)),
                    "sections": profile.as_ref().map(|p| p.sections.clone()),
                    "observations_in_full": context.observations,
                    "observations_condensed": context.condensed_observations,
                    "observations_omitted_as_repeats": context.omitted_observations,
                    "evidence_items": context.evidence_items,
                    "request_bytes": context.bytes,
                    "assembly_ms": assembly_ms,
                }));
            }
            WorkEvent::ModelCalled { .. } => {
                if let Some(value) = pending.take() {
                    out.push(value);
                }
            }
            _ => {}
        }
    }
    out
}

/// Calls per context profile, and each profile change in call order.
fn profile_summary(contexts: &[Value]) -> Value {
    let mut calls: BTreeMap<String, usize> = BTreeMap::new();
    let mut changes = Vec::new();
    for (i, c) in contexts.iter().enumerate() {
        let name = c["profile"].as_str().unwrap_or("none").to_string();
        *calls.entry(name).or_default() += 1;
        if c["changed"] == true {
            changes.push(json!({
                "call": i + 1,
                "from": c["previous_profile"],
                "to": c["profile"],
                "reason": c["reason"],
            }));
        }
    }
    json!({"calls_by_profile": calls, "changes": changes})
}

/// The decision label as recorded, kept only when it is one of the known shapes.
fn decision_label(decision: &str) -> String {
    match decision {
        "complete" | "escalate" | "block" => decision.to_string(),
        d if d.starts_with("request ")
            && d[8..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')) =>
        {
            d.to_string()
        }
        _ => "other".to_string(),
    }
}

/// One call's usage fields, raw and accounted.
pub fn usage_json(usage: &Usage, semantics: InputSemantics) -> Value {
    let a = account(usage, semantics);
    json!({
        "source": if usage.is_reported() { "provider" } else { "unavailable" },
        "reported": {
            "input_tokens": a.input_reported,
            "output_tokens": a.output,
            "cache_read_tokens": a.cache_read,
            "cache_write_tokens": a.cache_write,
            "reasoning_tokens": a.reasoning,
            "cost": usage.cost,
            "model": usage.model,
        },
        "uncached_input_tokens": a.uncached_input,
        "total_input_tokens": a.total_input,
        "total_tokens": a.total,
    })
}

#[derive(Default)]
struct Sum {
    value: u64,
    known: usize,
    unknown: usize,
}

impl Sum {
    fn add(&mut self, v: Option<u64>) {
        match v {
            Some(n) => {
                self.value += n;
                self.known += 1;
            }
            None => self.unknown += 1,
        }
    }
    fn json(&self) -> Value {
        json!({
            "sum": (self.known > 0).then_some(self.value),
            "calls_known": self.known,
            "calls_unknown": self.unknown,
        })
    }
}

/// Totals over `calls` (each a call's usage, `None` for a failed call). Each field is summed over
/// the calls that reported it, with how many did and did not: a total over every call is only
/// what `calls_unknown: 0` says it is.
pub fn totals_json(usages: &[Option<&Usage>], semantics: InputSemantics) -> Value {
    let mut fields: BTreeMap<&str, Sum> = BTreeMap::new();
    for u in usages {
        let a = u.map(|u| account(u, semantics)).unwrap_or_default();
        for (name, v) in [
            ("input_tokens", a.input_reported),
            ("output_tokens", a.output),
            ("cache_read_tokens", a.cache_read),
            ("cache_write_tokens", a.cache_write),
            ("reasoning_tokens", a.reasoning),
            ("uncached_input_tokens", a.uncached_input),
            ("total_input_tokens", a.total_input),
            ("total_tokens", a.total),
        ] {
            fields.entry(name).or_default().add(v);
        }
    }
    let mut out = serde_json::Map::new();
    for (name, sum) in fields {
        out.insert(name.to_string(), sum.json());
    }
    Value::Object(out)
}

/// Context-growth diagnostics from the recorded observations: sizes, the largest outputs, repeated
/// reads of one file, identical outputs and output lines already seen in an earlier observation.
/// Files and outputs are compared by fingerprint; no path, query or output text is reported.
pub fn observation_diagnostics(report: &WorkReport, context: &ContextReport) -> Value {
    use std::collections::HashMap;
    let observations = &report.observations;
    let origins = &report.origins;
    let mut by_capability: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut outputs_seen: HashMap<[u8; 32], usize> = HashMap::new();
    let mut identical = (0usize, 0usize);
    let mut lines_seen: HashSet<[u8; 32]> = HashSet::new();
    let mut overlap_bytes = 0usize;
    let mut reads_by_file: HashMap<[u8; 32], usize> = HashMap::new();
    let mut invocations: HashMap<&str, usize> = HashMap::new();
    let mut largest: Vec<(usize, usize)> = Vec::new();
    let mut total = 0usize;
    for (i, o) in observations.iter().enumerate() {
        let capability = origins
            .get(i)
            .map_or_else(|| "unknown".to_string(), |r| r.capability.to_string());
        let output = o.output.as_deref().unwrap_or("");
        total += output.len();
        let entry = by_capability.entry(capability.clone()).or_default();
        entry.0 += 1;
        entry.1 += output.len();
        largest.push((i, output.len()));
        if !output.is_empty() {
            let print: [u8; 32] = Sha256::digest(output.as_bytes()).into();
            let n = outputs_seen.entry(print).or_default();
            if *n > 0 {
                identical.0 += 1;
                identical.1 += output.len();
            }
            *n += 1;
        }
        // Lines of 16 bytes or more that an earlier observation already returned.
        for line in output.lines().filter(|l| l.trim().len() >= 16) {
            let print: [u8; 32] = Sha256::digest(line.as_bytes()).into();
            if !lines_seen.insert(print) {
                overlap_bytes += line.len() + 1;
            }
        }
        if capability == chip_project::PROJECT_READ {
            if let Some(path) = crate::software_work::observation_header(o)
                .and_then(|h| h.get("path").and_then(Value::as_str).map(str::to_owned))
            {
                *reads_by_file
                    .entry(Sha256::digest(path.as_bytes()).into())
                    .or_default() += 1;
            }
        }
        if let Some(origin) = origins.get(i) {
            *invocations.entry(origin.invocation.as_str()).or_default() += 1;
        }
    }
    largest.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    // A call carries every observation recorded before it was built, unless the context policy
    // left it out (counted per call in `context.calls[].omitted_observations`).
    let carried = |i: usize| {
        context
            .calls
            .iter()
            .filter(|c| c.observations_known > i)
            .count()
    };
    let read_files = reads_by_file.len();
    let reads: usize = reads_by_file.values().sum();
    json!({
        "observations": observations.len(),
        "output_bytes": total,
        "by_capability": by_capability.iter().map(|(k, (n, b))| (k.clone(), json!({"observations": n, "output_bytes": b}))).collect::<serde_json::Map<_, _>>(),
        "largest_outputs": largest.iter().take(5).map(|(i, bytes)| json!({
            "observation": i + 1,
            "capability": origins.get(*i).map(|r| r.capability.to_string()),
            "status": match observations[*i].status {
                chip_core::ExecutionStatus::Success => "success",
                chip_core::ExecutionStatus::Failure => "failure",
                chip_core::ExecutionStatus::Cancelled => "cancelled",
            },
            "output_bytes": bytes,
            "calls_after_recorded": carried(*i),
        })).collect::<Vec<_>>(),
        "reads": {
            "successful_reads_with_path": reads,
            "distinct_files": read_files,
            "repeated_reads_of_a_file": reads.saturating_sub(read_files),
        },
        "repeated_invocations": invocations.values().map(|n| n.saturating_sub(1)).sum::<usize>(),
        "identical_outputs": {"observations": identical.0, "bytes": identical.1},
        "overlapping_output_line_bytes": overlap_bytes,
        "method": "sha256 fingerprints of outputs, lines (16+ bytes) and read paths, compared in memory; no text is kept",
    })
}

/// The report's `token_usage` section.
pub fn work_section(
    ledger: &CallLedger,
    report: &WorkReport,
    context: &ContextReport,
    provider: &str,
    labels: &BTreeMap<String, String>,
) -> Value {
    let calls = ledger.calls();
    let semantics = InputSemantics::of_provider(provider);
    let decisions = call_decisions(&report.events);
    let correlated = decisions.len() == calls.len();
    let contexts = call_contexts(report);
    let mut warnings: Vec<String> = Vec::new();
    if !correlated {
        warnings.push(format!(
            "{} provider calls were recorded and the work recorded {} model calls; decisions are not attributed to calls",
            calls.len(),
            decisions.len()
        ));
    }
    if semantics == InputSemantics::Unknown {
        warnings.push(format!(
            "the input semantics of provider `{provider}` are not known; no total or uncached input is derived"
        ));
    }
    let usages: Vec<Option<&Usage>> = calls
        .iter()
        .map(|c| c.outcome.as_ref().ok().map(|(u, _)| u))
        .collect();
    let unreported = usages
        .iter()
        .filter(|u| u.is_none_or(|u| !u.is_reported()))
        .count();
    if unreported > 0 {
        warnings.push(format!(
            "{unreported} of {} calls have no provider-reported usage (failed, or the provider sent none); their tokens are unknown, not 0",
            calls.len()
        ));
    }
    let mut by_model: BTreeMap<String, Vec<Option<&Usage>>> = BTreeMap::new();
    for (c, u) in calls.iter().zip(&usages) {
        let model = u
            .and_then(|u| u.model.clone())
            .unwrap_or_else(|| c.model_requested.clone());
        by_model.entry(model).or_default().push(*u);
    }
    let inference_ms: f64 = calls.iter().map(|c| c.elapsed_ms).sum();
    let between_ms: f64 = calls.iter().filter_map(|c| c.gap_before_ms).sum();
    json!({
        "schema": SCHEMA,
        "accounting": {
            "provider": provider,
            "input_semantics": semantics.name(),
            "rules": "anthropic: input_tokens excludes cache_read and cache_write, total_input = input + cache_read + cache_write; openai-compatible: prompt_tokens includes cached_tokens (cache_read), total_input = prompt_tokens, uncached = prompt - cached, no cache_write; a derived value is null when any field it needs is null; reasoning tokens are part of output and never added",
        },
        "run": {
            "work_id": report.work_id.to_string(),
            "labels": labels,
        },
        "calls": calls.iter().enumerate().map(|(i, c)| json!({
            "call": c.seq,
            "started_unix_ms": c.started_unix_ms,
            "elapsed_ms": round(c.elapsed_ms),
            "gap_before_ms": c.gap_before_ms.map(round),
            "model_requested": c.model_requested,
            "max_output_tokens": c.max_tokens,
            "outcome": if c.outcome.is_ok() { "response" } else { "failed" },
            "failure": c.outcome.as_ref().err().map(|f| json!({
                "kind": f.kind, "delivery": f.delivery, "status": f.status,
                "partial_output_bytes": f.partial_output_bytes,
            })),
            "decision": if correlated { Some(decisions[i].as_str()) } else { None },
            "context": if correlated { contexts.get(i).cloned() } else { None },
            "request": {
                "messages": c.messages(),
                "by_role": {
                    "system": {"messages": c.system.messages, "bytes": c.system.bytes},
                    "user": {"messages": c.user.messages, "bytes": c.user.bytes},
                    "assistant": {"messages": c.assistant.messages, "bytes": c.assistant.bytes},
                },
                "content_bytes": c.content_bytes(),
                "serialized_messages_bytes_local": c.serialized_messages_bytes_local,
                "resent_from_previous_call": {"messages": c.resent_messages, "bytes": c.resent_bytes},
                "new_content_bytes": c.content_bytes() - c.resent_bytes,
            },
            "response_output_bytes": c.outcome.as_ref().ok().map(|(_, n)| *n),
            "usage": usages[i].map(|u| usage_json(u, semantics)),
        })).collect::<Vec<_>>(),
        "totals": totals_json(&usages, semantics),
        "by_model": by_model.iter().map(|(m, u)| (m.clone(), json!({
            "calls": u.len(),
            "totals": totals_json(u, semantics),
        }))).collect::<serde_json::Map<_, _>>(),
        "context": {
            "content_bytes_sent": calls.iter().map(CallRecord::content_bytes).sum::<usize>(),
            "resent_bytes": calls.iter().map(|c| c.resent_bytes).sum::<usize>(),
            "new_content_bytes": calls.iter().map(|c| c.content_bytes() - c.resent_bytes).sum::<usize>(),
            "note": "bytes are Chip's own measure of message content; resent bytes are messages byte-identical to one in the previous request. Neither is a token count, and neither says what a provider cache served",
        },
        "context_profiles": profile_summary(&contexts),
        "observations": observation_diagnostics(report, context),
        "time": {
            "inference_ms": round(inference_ms),
            "between_calls_ms": round(between_ms),
            "note": "between_calls_ms is wall-clock between one call's end and the next call's start: capability execution (tools, tests) and runtime work",
        },
        "data_quality": warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chip_inference::{Message, ModelResponse};

    fn usage(
        input: Option<u64>,
        output: Option<u64>,
        read: Option<u64>,
        write: Option<u64>,
    ) -> Usage {
        Usage {
            prompt_tokens: input,
            completion_tokens: output,
            cache_read_tokens: read,
            cache_write_tokens: write,
            ..Usage::default()
        }
    }

    #[test]
    fn anthropic_input_excludes_cache_reads_and_writes_and_they_are_added_once() {
        let a = account(
            &usage(Some(100), Some(20), Some(5000), Some(300)),
            InputSemantics::CacheSeparate,
        );
        assert_eq!(a.uncached_input, Some(100));
        assert_eq!(a.total_input, Some(5400));
        assert_eq!(a.total, Some(5420));
        assert_eq!(a.input_reported, Some(100));
    }

    #[test]
    fn openai_prompt_tokens_include_cached_tokens_which_are_never_added_again() {
        let a = account(
            &usage(Some(5400), Some(20), Some(5000), None),
            InputSemantics::CacheIncluded,
        );
        assert_eq!(a.total_input, Some(5400));
        assert_eq!(a.uncached_input, Some(400));
        assert_eq!(a.cache_write, None);
        assert_eq!(a.total, Some(5420));
    }

    #[test]
    fn a_missing_field_makes_every_number_that_needs_it_unknown_never_zero() {
        // Anthropic without the cache-write count (what builds before this change recorded).
        let a = account(
            &usage(Some(100), Some(20), Some(0), None),
            InputSemantics::CacheSeparate,
        );
        assert_eq!(a.uncached_input, Some(100));
        assert_eq!(a.total_input, None);
        assert_eq!(a.total, None);
        // OpenAI without cached_tokens: the total is known, the uncached part is not.
        let o = account(
            &usage(Some(100), None, None, None),
            InputSemantics::CacheIncluded,
        );
        assert_eq!(o.total_input, Some(100));
        assert_eq!(o.uncached_input, None);
        assert_eq!(o.total, None);
        // Nothing reported at all.
        assert_eq!(
            account(&Usage::unreported(), InputSemantics::CacheSeparate),
            Accounted::default()
        );
        // An unknown provider derives nothing.
        let u = account(
            &usage(Some(1), Some(1), Some(1), Some(1)),
            InputSemantics::Unknown,
        );
        assert_eq!(
            (u.total_input, u.uncached_input, u.total),
            (None, None, None)
        );
    }

    #[test]
    fn totals_sum_only_known_values_and_say_how_many_calls_were_unknown() {
        let a = usage(Some(100), Some(10), Some(900), Some(50));
        let b = usage(Some(200), Some(20), Some(1000), None);
        let t = totals_json(&[Some(&a), Some(&b), None], InputSemantics::CacheSeparate);
        assert_eq!(t["input_tokens"]["sum"], 300);
        assert_eq!(t["input_tokens"]["calls_unknown"], 1);
        assert_eq!(t["cache_read_tokens"]["sum"], 1900);
        assert_eq!(t["cache_write_tokens"]["sum"], 50);
        assert_eq!(t["cache_write_tokens"]["calls_known"], 1);
        assert_eq!(t["total_input_tokens"]["sum"], 1050);
        assert_eq!(t["total_input_tokens"]["calls_known"], 1);
        let none = totals_json(&[None], InputSemantics::CacheSeparate);
        assert_eq!(none["input_tokens"]["sum"], Value::Null);
    }

    #[test]
    fn labels_are_validated_not_recorded_blindly() {
        let l = parse_labels("course=aoc-t4, obstacle=transient-test-failure").unwrap();
        assert_eq!(l["course"], "aoc-t4");
        assert_eq!(l["obstacle"], "transient-test-failure");
        assert!(parse_labels("").unwrap().is_empty());
        for bad in [
            "course", "Course=x", "k=a,b=", "k=x\u{7}", "k=1,k=2", "k=a=b",
        ] {
            assert!(parse_labels(bad).is_err(), "{bad:?}");
        }
        assert!(diagnostics_enabled(&|_| None).unwrap());
        assert!(!diagnostics_enabled(&|_| Some("off".into())).unwrap());
        assert!(diagnostics_enabled(&|_| Some("maybe".into())).is_err());
    }

    struct Scripted(Mutex<Vec<Result<ModelResponse, FxError>>>);

    #[async_trait]
    impl ModelProvider for Scripted {
        async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
            self.0.lock().unwrap().remove(0)
        }
    }

    const SENTINEL: &str = "SENTINEL-sk-ant-api03-never-in-a-record";

    #[tokio::test]
    async fn every_call_is_recorded_with_sizes_resent_content_and_failures_and_no_text() {
        let failure = FxError::Inference(Box::new(chip_inference::InferenceFailure {
            kind: "transport".into(),
            delivery: chip_inference::Delivery::PossiblySent,
            status: Some(502),
            retry_after_ms: None,
            detail: Some(SENTINEL.into()),
            partial_output: Some("partial".into()),
            remote_cancellation: None,
        }));
        let ledger = Arc::new(CallLedger::default());
        let provider = Recording {
            inner: Arc::new(Scripted(Mutex::new(vec![
                Ok(ModelResponse::new("1", SENTINEL, Usage::new(10, 2))),
                Err(failure),
                Ok(ModelResponse::new("3", "{}", Usage::unreported())),
            ]))),
            ledger: ledger.clone(),
        };
        let goal = Message::new(MessageRole::System, format!("Goal {SENTINEL}"));
        let obs = Message::new(MessageRole::System, "Observation: one".to_string());
        let turn = |n: usize| Message::new(MessageRole::User, format!("turn {n}"));
        provider
            .complete(ModelRequest::new("m", vec![goal.clone(), turn(1)]))
            .await
            .unwrap();
        assert!(
            provider
                .complete(ModelRequest::new(
                    "m",
                    vec![goal.clone(), obs.clone(), turn(2)]
                ))
                .await
                .is_err()
        );
        provider
            .complete(ModelRequest::new(
                "m",
                vec![goal.clone(), obs.clone(), turn(3)],
            ))
            .await
            .unwrap();
        let calls = ledger.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls.iter().map(|c| c.seq).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(calls[0].gap_before_ms, None);
        assert!(calls[1].gap_before_ms.is_some());
        assert_eq!((calls[0].system.messages, calls[0].user.messages), (1, 1));
        assert_eq!(calls[0].resent_messages, 0);
        // The goal is resent on call 2; the observation and the goal on call 3.
        assert_eq!(calls[1].resent_messages, 1);
        assert_eq!(calls[1].resent_bytes, goal.content.len());
        assert_eq!(calls[2].resent_messages, 2);
        assert_eq!(
            calls[2].resent_bytes,
            goal.content.len() + obs.content.len()
        );
        let f = calls[1].outcome.as_ref().unwrap_err();
        assert_eq!(
            (
                f.kind.as_str(),
                f.delivery,
                f.status,
                f.partial_output_bytes
            ),
            ("transport", Some("possibly_sent"), Some(502), Some(7))
        );
        // Privacy: nothing the request or the reply said is in a record.
        let debug = format!("{calls:?}");
        assert!(!debug.contains("SENTINEL"), "{debug}");
        assert!(!debug.contains("Observation: one"), "{debug}");
    }

    #[test]
    fn decisions_are_attributed_to_the_call_that_produced_them() {
        use chip_core::WorkId;
        let id = WorkId::new("w");
        let called = |ok: bool| WorkEvent::ModelCalled {
            work_id: id.clone(),
            turn: 0,
            usage: None,
            succeeded: ok,
        };
        let events = vec![
            called(true),
            WorkEvent::DecisionMade {
                work_id: id.clone(),
                turn: 0,
                decision: "request project.read".into(),
            },
            called(true),
            WorkEvent::DecisionRejected {
                work_id: id.clone(),
                turn: 1,
                reason: format!("quoted {SENTINEL}"),
                consecutive: 1,
                limit: 2,
                recovering: true,
            },
            called(false),
            called(true),
            WorkEvent::DecisionMade {
                work_id: id.clone(),
                turn: 3,
                decision: format!("request {SENTINEL} with spaces"),
            },
        ];
        assert_eq!(
            call_decisions(&events),
            ["request project.read", "rejected", "none", "other"]
        );
    }
}
