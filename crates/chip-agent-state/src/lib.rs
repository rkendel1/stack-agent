//! Chip work persisted through FeltDB's native agent-state contract.
//!
//! FeltDB is the authority for durable agent state. Its `feltdb.agent.transition` contract (v1)
//! decides every transition, and `feltdb::agent_state::AgentState` commits each decision through
//! one guarded transaction. This crate decides nothing: it names the operations Chip's work
//! implies and reports what FeltDB said.
//!
//! ```text
//!   Chip work              FeltDB record (contract v1)
//!   ---------------------  ------------------------------------------------------------
//!   work id                _feltdb.AgentTask  (the task: owner, fence, lease, status)
//!   start / resume         create_task or acquire, then checkpoint -> running
//!   before an execution    renew, record_intent   (the action is not started otherwise)
//!   after an execution     chip.Observation record, then record_outcome citing it
//!   terminal outcome       chip.WorkResult record, then checkpoint -> completed/blocked/cancelled
//!   restart                read everything back; reconcile intents from their observations
//! ```
//!
//! # What is authoritative
//!
//! FeltDB, never process memory. [`begin`] reads the task from the store: a terminal task is
//! returned as its persisted [`History`] and not run again; a task whose lease is still held is
//! refused; an intent whose outcome FeltDB cannot establish blocks the resumption
//! ([`StateError::OutcomeUncertain`]). Nothing falls back to in-memory state.
//!
//! # Failures are never success
//!
//! If the intent cannot be recorded, the wrapped executor is not called and the execution fails.
//! If the outcome cannot be recorded after the action ran, the execution is reported as failed and
//! the intent stays unresolved in FeltDB: the action may have happened, and the store says so.
//! A completion is persisted only with at least one observation record as evidence; otherwise the
//! task is checkpointed `blocked` and [`Finished::completion_refused`] is set.
//!
//! # The trajectory and the verdict
//!
//! Every work event is a `chip.WorkEvent` record, written as the loop records it, with the
//! provenance of its fact: observed by the host, inferred by the model, or decided by Chip
//! (`events` module). The work result (`chip.work_result.v2`) carries the host's verification
//! verdict beside Chip's outcome. Both are Chip's records next to the contract's; the contract
//! decides no transition from them.
//!
//! # Scope
//!
//! Conversational turns of the Zig `fx` keep their existing owner, the Zig-compatible session
//! store (`chip-session`, `chip-turns`); persisting them here would make a second store. A work's
//! own events are not conversation turns, and live here with the rest of the work's durable state.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use std::collections::BTreeMap;

use chip_core::{
    CapabilityBackend, CapabilityId, CapabilityRequest, DecisionSource, Effect, ExecutionError,
    ExecutionEvent, ExecutionId, ExecutionRequest, ExecutionResult, ExecutionStatus, Executor,
    Idempotency, InputValue, RestoredObservation, RestoredResponse, SettledAction, WorkControl,
    WorkEvent, WorkOutcome, WorkReport, validate_request,
};
pub use feltdb;
mod admission;
mod episodes;
mod events;
mod reconcile;
pub use admission::{
    ADMISSION_COLLECTION, ADMISSION_HANDOFF, ADMISSION_LEASE_MS, ADMISSION_SCHEMA, Admission,
    AdmissionOrigin, AdmissionRecord, Ensured, MAX_ADMISSION_GOAL_BYTES, Waiting, admission, admit,
    cancel_admitted, ensure_admitted, lease_is_live, waiting,
};
pub use episodes::{
    EPISODE_COLLECTION, EPISODE_SCHEMA, EPISODE_VALID_MS, EpisodeContext, EpisodeOutcome,
    EpisodeRecord, EpisodeScope, EpisodeWrite, Exclusion, MIN_SIMILARITY_PCT, PROJECT_SCOPE,
    ProcedureStep, Recall, Recalled, classify, intent_terms, select,
};
pub use events::{Provenance, WorkEventRecord, describe as describe_event};
use feltdb::agent_state::{AgentState, AgentStateError, AuthorityClock};
use feltdb::agent_transition_contract::{
    AGENT_TRANSITION_CONTRACT_ID, AGENT_TRANSITION_CONTRACT_VERSION, CHECKPOINT_COLLECTION,
    EvidenceRef, INTENT_COLLECTION, MAX_EVIDENCE, MAX_IDENTIFIER_BYTES, MAX_LEASE_MS,
    MAX_TEXT_BYTES, Operation, TASK_COLLECTION,
};
pub use feltdb::agent_transition_contract::{Citation, ErrorCode, IntentValue, TaskValue};
use feltdb::{AtomicMutation, FeltDb, FlowError, RecordPrecondition};
pub use reconcile::{
    BusinessCheck, DestinationStatus, Finding, OperationRef, RECONCILIATION_COLLECTION,
    RECONCILIATION_EXECUTION_ID, RECONCILIATION_SCHEMA, ReconcileQuery, Reconciled, Reconciler,
    ReconciliationRecord, decide, reconcilability, reconcile,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The contract this crate was written against. Any other is refused, never adapted.
pub const SUPPORTED_CONTRACT_ID: &str = "feltdb.agent.transition";
pub const SUPPORTED_CONTRACT_VERSION: u32 = 1;

/// Chip's own records. Outside FeltDB's `_feltdb.Agent` prefix, so the contract accepts them as
/// evidence; written once, never changed, so a citation's version guard always holds.
pub const OBSERVATION_COLLECTION: &str = "chip.Observation";
pub const RESULT_COLLECTION: &str = "chip.WorkResult";
pub const OBSERVATION_SCHEMA: &str = "chip.observation.v2";
/// Observation records written before the request was recorded with them. Still read, and still
/// reconcile their intent; a trajectory that contains one is not restored.
pub const OBSERVATION_SCHEMA_V1: &str = "chip.observation.v1";
pub const RESULT_SCHEMA: &str = "chip.work_result.v1";
/// The work result with the host's verification verdict. `v1` records are still read.
pub const RESULT_SCHEMA_V2: &str = "chip.work_result.v2";
pub const WORK_EVENT_COLLECTION: &str = "chip.WorkEvent";
pub const WORK_EVENT_SCHEMA: &str = "chip.work_event.v1";
pub const MODEL_RESPONSE_COLLECTION: &str = "chip.ModelResponse";
pub const MODEL_RESPONSE_SCHEMA: &str = "chip.model_response.v1";

/// Fifteen minutes. A restarted process can take the task over only once the lease has expired.
pub const DEFAULT_LEASE_MS: u64 = 15 * 60 * 1000;

/// Why durable agent state could not do what was asked. In every case nothing is reported as done
/// that FeltDB did not commit.
#[derive(Debug, Clone, PartialEq)]
pub enum StateError {
    /// FeltDB's contract refused the operation. Nothing was written.
    Refused {
        operation: &'static str,
        code: ErrorCode,
    },
    /// A record the decision depended on changed before the commit. Nothing was written.
    Conflict {
        operation: &'static str,
        detail: String,
    },
    /// The store failed.
    Storage {
        operation: &'static str,
        detail: String,
    },
    /// The FeltDB build speaks a contract this crate was not written for.
    UnsupportedContract { contract: String, version: u32 },
    /// An identity or text the contract cannot carry.
    InvalidInput(String),
    /// The task exists with another objective.
    ObjectiveMismatch { task_id: String },
    /// Another owner holds the task's unexpired lease.
    LeaseHeld { task_id: String, owner: String },
    /// Intents whose execution FeltDB cannot establish either way. Resuming could repeat them.
    OutcomeUncertain {
        task_id: String,
        intents: Vec<String>,
    },
    /// As [`StateError::OutcomeUncertain`], but the lease this refusal took could not be given
    /// back: the task stays held until it expires.
    UncertainLeaseHeld {
        task_id: String,
        intents: Vec<String>,
        detail: String,
    },
    /// An operator settlement that cannot be recorded. Nothing was written.
    SettlementRefused {
        task_id: String,
        intent: String,
        reason: String,
    },
    /// A reconciliation that cannot be recorded. Nothing it would resolve was written.
    ReconciliationRefused {
        task_id: String,
        intent: String,
        reason: String,
    },
    /// Stored state that does not form a complete, consistent history.
    InvalidHistory { task_id: String, detail: String },
    /// A Chip record written under a schema this build does not know.
    UnsupportedRecord {
        collection: String,
        id: String,
        schema: String,
    },
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused { operation, code } => {
                write!(f, "FeltDB refused {operation}: {}", code.as_str())
            }
            Self::Conflict { operation, detail } => {
                write!(
                    f,
                    "FeltDB {operation} lost a race (nothing written): {detail}"
                )
            }
            Self::Storage { operation, detail } => write!(f, "FeltDB {operation} failed: {detail}"),
            Self::UnsupportedContract { contract, version } => write!(
                f,
                "unsupported agent-state contract {contract} v{version}; this build supports {SUPPORTED_CONTRACT_ID} v{SUPPORTED_CONTRACT_VERSION}"
            ),
            Self::InvalidInput(why) => write!(f, "invalid agent-state input: {why}"),
            Self::ObjectiveMismatch { task_id } => {
                write!(
                    f,
                    "task {task_id} already exists with a different objective"
                )
            }
            Self::LeaseHeld { task_id, owner } => write!(
                f,
                "task {task_id} is held by {owner} under an unexpired lease; it can be resumed once that lease expires"
            ),
            Self::OutcomeUncertain { task_id, intents } => write!(
                f,
                "task {task_id} has actions whose outcome is unknown ({}); they may have run, so the work is not resumed until they are reconciled with evidence",
                intents.join(", ")
            ),
            Self::UncertainLeaseHeld {
                task_id,
                intents,
                detail,
            } => write!(
                f,
                "task {task_id} has actions whose outcome is unknown ({}); they may have run, so the work is not resumed until they are reconciled with evidence. The lease this refusal took could not be released ({detail}), so the task stays held until it expires",
                intents.join(", ")
            ),
            Self::SettlementRefused {
                task_id,
                intent,
                reason,
            } => write!(
                f,
                "intent {intent} of task {task_id} was not settled (nothing was written): {reason}"
            ),
            Self::ReconciliationRefused {
                task_id,
                intent,
                reason,
            } => write!(
                f,
                "intent {intent} of task {task_id} was not reconciled (the intent is unchanged): {reason}"
            ),
            Self::InvalidHistory { task_id, detail } => {
                write!(f, "task {task_id} has invalid durable history: {detail}")
            }
            Self::UnsupportedRecord {
                collection,
                id,
                schema,
            } => write!(
                f,
                "record {collection}:{id} has unsupported schema {schema:?}"
            ),
        }
    }
}

impl std::error::Error for StateError {}

fn from_agent(operation: &'static str, error: AgentStateError) -> StateError {
    match error {
        AgentStateError::Refused(code) => StateError::Refused { operation, code },
        AgentStateError::Conflict(failure) => StateError::Conflict {
            operation,
            detail: failure.to_string(),
        },
        AgentStateError::Storage(error) => StateError::Storage {
            operation,
            detail: error.to_string(),
        },
    }
}

fn from_flow(operation: &'static str, error: FlowError) -> StateError {
    match error {
        FlowError::PreconditionFailed(failure) => StateError::Conflict {
            operation,
            detail: failure.to_string(),
        },
        other => StateError::Storage {
            operation,
            detail: other.to_string(),
        },
    }
}

/// Refuses any contract but the one this crate was written for.
pub fn check_contract(contract: &str, version: u32) -> Result<(), StateError> {
    if contract == SUPPORTED_CONTRACT_ID && version == SUPPORTED_CONTRACT_VERSION {
        Ok(())
    } else {
        Err(StateError::UnsupportedContract {
            contract: contract.to_string(),
            version,
        })
    }
}

/// The contract's identifier rule: 1..=128 bytes of `[A-Za-z0-9_.:@-]`.
pub fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'@' | b'-'))
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn unique(prefix: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{prefix}-{}-{nanos:x}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}

/// The idempotency key of one execution: unique per attempt, so a resumed run that reuses Chip's
/// execution numbering never collides with an earlier attempt's intents.
pub fn intent_key(attempt: u64, execution_id: &str) -> String {
    let key = format!("a{attempt}.{execution_id}");
    if is_identifier(&key) {
        return key;
    }
    let digest = Sha256::digest(execution_id.as_bytes());
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("a{attempt}.h{hex}")
}

/// The idempotency key of one logical action of `task_id`: the action Chip numbers
/// `execution_id` in the task's trajectory.
///
/// It is derived from the task and the execution's identity alone, never from the attempt, the
/// process or a retry count, so every attempt that performs the action again (after a crash, in
/// another process) derives the same key, and every other action of the task derives another one.
/// Within the task, a key is bound to one request ([`binding`]); a request that would reuse it for
/// anything else is refused before it is executed.
pub fn action_key(task_id: &str, execution_id: &str) -> String {
    let mut hasher = Sha256::new();
    for part in ["chip.action-key.v1", task_id, execution_id] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("ak1.{hex}")
}

/// What a key is bound to: the digest of the validated request (its capability and typed inputs),
/// as the observation record writes it.
pub fn binding(request: &ExecutionRequest) -> String {
    binding_of(&RecordedRequest::of(request))
}

fn binding_of(recorded: &RecordedRequest) -> String {
    let canonical = serde_json::to_vec(recorded).expect("a request serializes");
    let digest = Sha256::digest(&canonical);
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("b1.{hex}")
}

/// The logical action an intent records, for a capability that carries the key. Kept in the
/// intent's `action` text, which contract v1 leaves to its caller:
/// `<capability> key=<key> binding=<binding> idempotency=<class>`. An intent for a capability that
/// cannot carry a key keeps the bare capability id it always had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionTag {
    pub capability: String,
    pub key: String,
    pub binding: String,
    pub idempotency: Idempotency,
}

impl ActionTag {
    pub fn to_action(&self) -> String {
        format!(
            "{} key={} binding={} idempotency={}",
            self.capability,
            self.key,
            self.binding,
            self.idempotency.name()
        )
    }

    /// The tag an intent's `action` records, or `None` for an action without one (a capability
    /// that cannot carry a key, or a record written before keys were).
    pub fn parse(action: &str) -> Option<Self> {
        let mut parts = without_inputs(action).split(' ');
        let capability = parts.next()?.to_string();
        let key = parts.next()?.strip_prefix("key=")?.to_string();
        let binding = parts.next()?.strip_prefix("binding=")?.to_string();
        let idempotency = Idempotency::from_name(parts.next()?.strip_prefix("idempotency=")?)?;
        if parts.next().is_some() || capability.is_empty() {
            return None;
        }
        Some(Self {
            capability,
            key,
            binding,
            idempotency,
        })
    }
}

/// A checkpoint as FeltDB's contract writes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointRecord {
    pub actor: String,
    pub created_at_ms: u64,
    pub evidence: Vec<Citation>,
    pub fence: u64,
    pub status: String,
    pub summary: String,
    pub task_id: String,
    pub version: u64,
}

/// What Chip observed of one execution, written before its outcome is recorded and cited by it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationRecord {
    pub schema: String,
    pub task_id: String,
    pub intent_key: String,
    pub execution_id: String,
    pub attempt: u64,
    /// `success`, `failure`, `cancelled` or `error` (the executor returned an error).
    pub status: String,
    /// Whether the action may have taken effect. Only an error raised before the executor acted
    /// (an invalid request, an unavailable executor) is `false`.
    pub executed: bool,
    pub output: String,
    pub receipt_id: Option<String>,
    /// The validated request the execution performed (`chip.observation.v2`). Absent from a
    /// `v1` record and from one an operator wrote to settle an intent: such a record still
    /// reconciles its intent but cannot be restored into a trajectory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<RecordedRequest>,
}

/// The execution id an operator's settlement record carries. No Chip execution is named so, and no
/// attempt is numbered 0, so a settlement can never be read as an execution.
pub const SETTLEMENT_EXECUTION_ID: &str = "operator";
/// How a settlement's `output` begins: it is an operator's conclusion, and says so.
const SETTLEMENT_OUTPUT: &str =
    "operator settlement (an operator's conclusion, not an execution observation):";

impl ObservationRecord {
    /// Whether this is an operator's settlement of an intent whose outcome was never observed,
    /// not a record of an execution: it names execution `operator` of attempt 0 and carries no
    /// request and no receipt. Its `status` is never `success` when [`settle`] writes it, since no
    /// capability answered; earlier operator records (the durability bench's, `sa-state`'s) used
    /// `success` for an effect that occurred, and are read the same way.
    pub fn is_settlement(&self) -> bool {
        self.execution_id == SETTLEMENT_EXECUTION_ID
            && self.attempt == 0
            && self.request.is_none()
            && self.receipt_id.is_none()
    }
}

/// An intent settled by an operator: what they concluded, who they are and what they gave as the
/// basis, as FeltDB records it. A conclusion, never an observation or a receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settlement {
    pub intent_key: String,
    /// The capability the intent recorded.
    pub capability: String,
    /// The binding of the request the intent recorded ([`binding`]). `None` for an intent written
    /// before every intent recorded one.
    pub binding: Option<String>,
    /// The validated inputs the intent recorded, if it recorded them.
    pub inputs: Option<BTreeMap<String, InputValue>>,
    /// The effect occurred (`true`) or did not (`false`), in the operator's judgment.
    pub executed: bool,
    /// Who decided. `None` for an operator record written before settlements recorded it.
    pub decided_by: Option<String>,
    pub evidence: String,
}

impl Settlement {
    /// The settlement `record` states for `intent`, or `None` if the record is not one.
    fn of(intent: &IntentValue, record: &ObservationRecord) -> Option<Self> {
        if !record.is_settlement() {
            return None;
        }
        let (capability, binding) = action_parts(&intent.action);
        let (decided_by, evidence) = match record
            .output
            .strip_prefix(SETTLEMENT_OUTPUT)
            .and_then(|rest| rest.strip_prefix(" decided_by="))
            .and_then(|rest| rest.split_once(' '))
            .and_then(|(who, rest)| Some((who, rest.split_once(" evidence=")?.1)))
        {
            Some((who, evidence)) => (Some(who.to_string()), evidence.to_string()),
            None => (None, record.output.clone()),
        };
        Some(Self {
            intent_key: intent.key.clone(),
            capability,
            binding,
            inputs: action_inputs(&intent.action),
            executed: record.executed,
            decided_by,
            evidence,
        })
    }

    /// How chip-core is told of it.
    pub fn to_settled_action(&self) -> Option<SettledAction> {
        Some(SettledAction {
            capability: CapabilityId::new(&self.capability).ok()?,
            inputs: self.inputs.clone(),
            executed: self.executed,
            decided_by: self
                .decided_by
                .clone()
                .unwrap_or_else(|| "an unrecorded operator".into()),
            evidence: self.evidence.clone(),
        })
    }
}

/// An intent's `action` without the inputs it may end with (see [`intent_action`]).
fn without_inputs(action: &str) -> &str {
    action
        .split_once(" inputs=")
        .map_or(action, |(head, _)| head)
}

/// The validated inputs an intent's `action` records, as recorded, if it records them.
fn action_recorded_inputs(action: &str) -> Option<BTreeMap<String, Value>> {
    let (_, json) = action.split_once(" inputs=")?;
    serde_json::from_str(json).ok()
}

/// The validated inputs an intent's `action` records, if it records them.
fn action_inputs(action: &str) -> Option<BTreeMap<String, InputValue>> {
    RecordedRequest {
        capability: String::new(),
        inputs: action_recorded_inputs(action)?,
    }
    .typed_inputs()
    .ok()
}

/// The open intents an intent's `action` says it performs again to obtain a fresh observation
/// (`reobserves=<key>,<key>`, between the binding and the inputs). Empty for an ordinary intent.
fn reobserves_of(action: &str) -> Vec<String> {
    without_inputs(action)
        .split(' ')
        .find_map(|p| p.strip_prefix("reobserves="))
        .map(|keys| keys.split(',').map(str::to_string).collect())
        .unwrap_or_default()
}

/// The re-observation an open intent without an observation calls for: its capability declares
/// itself an observation, and the request it recorded can be reconstructed exactly (the inputs
/// were recorded, and they hash to the binding the intent recorded). `None` otherwise, and the
/// intent stays uncertain.
fn plan_reobservation(intent: &IntentValue, config: &DurableConfig) -> Option<Reobservable> {
    let (capability, binding) = action_parts(&intent.action);
    if config.effects.get(&capability) != Some(&Effect::Observation) {
        return None;
    }
    let recorded = RecordedRequest {
        capability: capability.clone(),
        inputs: action_recorded_inputs(&intent.action)?,
    };
    recorded.typed_inputs().ok()?;
    let binding = binding.filter(|b| *b == binding_of(&recorded))?;
    Some(Reobservable {
        intents: vec![intent.key.clone()],
        capability,
        binding,
        request: recorded,
    })
}

/// The observation of an earlier recovery's re-observation of intent `key` that its capability
/// answered, if there is one: the recovery obtained it and was interrupted before it settled `key`.
fn answered_reobservation(
    store: &AgentStore,
    intents: &[(String, IntentValue)],
    key: &str,
) -> Result<Option<String>, StateError> {
    for (id, other) in intents {
        if reobserves_of(&other.action).iter().any(|k| k == key) {
            if let Some((_, record)) = store.observation(id)? {
                if record.status != "error" && record.request.is_some() {
                    return Ok(Some(id.clone()));
                }
            }
        }
    }
    Ok(None)
}

/// What an intent records of the request it is for, in its `action` (free text in contract v1):
/// `head` (the capability, then its binding and any key), then the validated inputs as JSON when
/// they fit in the contract's text limit. A settlement then names exactly the request it settles.
fn intent_action(head: String, request: &ExecutionRequest) -> String {
    let inputs =
        serde_json::to_string(&RecordedRequest::of(request).inputs).expect("inputs serialize");
    let full = format!("{head} inputs={inputs}");
    if full.len() <= MAX_TEXT_BYTES {
        full
    } else {
        head
    }
}

/// The capability an intent's `action` names and the binding it records, if any.
fn action_parts(action: &str) -> (String, Option<String>) {
    let mut parts = without_inputs(action).split(' ');
    let capability = parts.next().unwrap_or_default().to_string();
    let binding = parts
        .find_map(|p| p.strip_prefix("binding="))
        .map(str::to_string);
    (capability, binding)
}

/// A capability request as Chip validated it: the capability and its typed inputs. Text is a JSON
/// string, an integer a JSON integer, a boolean a JSON boolean; nothing else is a valid input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedRequest {
    pub capability: String,
    pub inputs: BTreeMap<String, Value>,
}

impl RecordedRequest {
    fn of(request: &ExecutionRequest) -> Self {
        Self {
            capability: request.intent.clone(),
            inputs: request
                .inputs
                .iter()
                .map(|(name, value)| {
                    let value = match value {
                        InputValue::Text(t) => Value::from(t.clone()),
                        InputValue::Integer(i) => Value::from(*i),
                        InputValue::Bool(b) => Value::from(*b),
                    };
                    (name.clone(), value)
                })
                .collect(),
        }
    }

    /// The request with `execution_id`, or why the record does not describe one.
    fn to_request(&self, execution_id: &str) -> Result<CapabilityRequest, String> {
        let capability = CapabilityId::new(&self.capability)
            .map_err(|e| format!("capability {:?}: {e}", self.capability))?;
        let mut request = CapabilityRequest::new(ExecutionId::new(execution_id), capability);
        request.inputs = self.typed_inputs()?;
        request.inputs_present = !request.inputs.is_empty();
        Ok(request)
    }

    /// The inputs as Chip types them, or why one is not a valid input.
    fn typed_inputs(&self) -> Result<BTreeMap<String, InputValue>, String> {
        let mut typed = BTreeMap::new();
        for (name, value) in &self.inputs {
            let value = match value {
                Value::String(t) => InputValue::Text(t.clone()),
                Value::Bool(b) => InputValue::Bool(*b),
                Value::Number(n) => InputValue::Integer(
                    n.as_i64()
                        .ok_or_else(|| format!("input {name:?} is not an integer"))?,
                ),
                _ => {
                    return Err(format!(
                        "input {name:?} is not text, an integer or a boolean"
                    ));
                }
            };
            typed.insert(name.clone(), value);
        }
        Ok(typed)
    }
}

/// Whether a resumed attempt starts from what earlier attempts recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restoration {
    /// Every observation earlier attempts recorded is restored, this many, in the order they were
    /// executed (none for a new task).
    Restored { observations: usize },
    /// The recorded history is consistent but cannot be restored whole, so none of it is: the
    /// attempt starts from the goal, as before trajectories were restored.
    NotRestored { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionEntry {
    pub turn: usize,
    pub execution_id: String,
    pub intent_key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionEntry {
    pub turn: usize,
    pub source: String,
    pub decision: String,
}

/// One attempt's result: Chip's outcome, the turn of each execution and every decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkResultRecord {
    pub schema: String,
    pub task_id: String,
    pub work_id: String,
    pub attempt: u64,
    pub fence: u64,
    /// Chip's terminal state (`completed`, `blocked`, `failed`, ...).
    pub outcome: String,
    pub detail: String,
    /// The status this attempt asked FeltDB to checkpoint. The task record is authoritative: if
    /// that checkpoint was refused, the task kept its earlier status.
    pub requested_status: String,
    pub turns: usize,
    pub executions: Vec<ExecutionEntry>,
    pub decisions: Vec<DecisionEntry>,
    pub persistence_failures: Vec<String>,
    /// The host's verdict on the attempt (`v2` only; `None` for a `v1` record).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
}

/// What the host established about an attempt, from its own evidence: never from the model's
/// words. `accepted` is what [`DurableWork::finish`] persists a completion on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verification {
    /// The host accepted the outcome (stack-agent: exit status 0).
    pub accepted: bool,
    /// The host's verification passed on evidence it observed (stack-agent: PAX).
    pub verified: bool,
    /// The goal's required condition held at the last evaluation, if the work has one.
    pub goal_satisfied: Option<bool>,
    /// An answer is grounded in observations but there is nothing to verify it against.
    pub grounded: bool,
    /// Why evidence the work relied on was not accepted (a replaced test file, a change after the
    /// passing run), if it was not.
    pub evidence_invalidated: Option<String>,
    /// The process's exit status for this outcome.
    pub exit_status: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IntentEntry {
    pub id: String,
    pub version: u64,
    pub value: IntentValue,
}

/// A task's complete durable history, read from FeltDB and checked for integrity.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    pub task: TaskValue,
    pub task_version: u64,
    /// Every checkpoint, versions 1 to the task's checkpoint version, in order.
    pub checkpoints: Vec<CheckpointRecord>,
    pub intents: Vec<IntentEntry>,
    /// One per finished attempt, in attempt order.
    pub results: Vec<WorkResultRecord>,
    /// Every recorded work event, by attempt and then trajectory position.
    pub events: Vec<WorkEventRecord>,
    /// Every recorded model response, by attempt and then turn.
    pub model_responses: Vec<ModelResponseRecord>,
    /// Every reconciliation query, by intent and then query number.
    pub reconciliations: Vec<ReconciliationRecord>,
}

impl History {
    /// Model calls an attempt had asked for when it stopped without a result, and never saw
    /// return: whether the provider received, answered or charged them is unknown. Each attempt's
    /// last escalation with no `ModelCalled` after it.
    pub fn model_calls_in_flight(&self) -> Vec<InterruptedModelCall> {
        let finished: Vec<u64> = self.results.iter().map(|r| r.attempt).collect();
        let mut attempts: Vec<u64> = self.events.iter().map(|e| e.attempt).collect();
        attempts.dedup();
        attempts
            .into_iter()
            .filter(|a| !finished.contains(a))
            .filter_map(|attempt| {
                let events: Vec<&WorkEventRecord> = self
                    .events
                    .iter()
                    .filter(|e| e.attempt == attempt)
                    .collect();
                in_flight(attempt, &events)
            })
            .collect()
    }

    pub fn status(&self) -> &str {
        &self.task.status
    }

    /// A compact, secret-free description for reports.
    pub fn to_json(&self) -> Value {
        json!({
            "task_id": self.task.task_id,
            "status": self.task.status,
            "owner": self.task.owner,
            "fence": self.task.fence,
            "checkpoint_version": self.task.checkpoint_version,
            "unresolved_intents": self.task.unresolved_intents,
            "intents": self.intents.len(),
            "attempts": self.results.len(),
            "events": self.events.len(),
            "last_summary": self.checkpoints.last().map(|c| c.summary.clone()),
        })
    }

    pub fn unresolved_intents(&self) -> Vec<&IntentEntry> {
        self.intents
            .iter()
            .filter(|i| i.value.state != "resolved")
            .collect()
    }
}

/// FeltDB, opened for agent state.
#[derive(Clone)]
pub struct AgentStore {
    db: FeltDb,
    state: AgentState,
}

impl AgentStore {
    /// Opens (or creates) the file-backed database at `path`. A database another process owns,
    /// or one in a format this FeltDB build does not understand, is refused untouched.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, StateError> {
        FeltDb::open(path)
            .map(Self::new)
            .map_err(|e| from_flow("open", e))
    }

    pub fn new(db: FeltDb) -> Self {
        Self {
            state: AgentState::new(db.clone()),
            db,
        }
    }

    /// Decisions are made against `clock`; the commit-time lease predicate still uses the
    /// authority's real clock. For tests that drive leases through time.
    pub fn with_clock(db: FeltDb, clock: Arc<dyn AuthorityClock>) -> Self {
        Self {
            state: AgentState::with_clock(db.clone(), clock),
            db,
        }
    }

    pub fn database(&self) -> &FeltDb {
        &self.db
    }

    fn apply(&self, name: &'static str, operation: Operation) -> Result<(), StateError> {
        self.state
            .apply(&operation)
            .map(|_| ())
            .map_err(|e| from_agent(name, e))
    }

    fn read(&self, collection: &str, id: &str) -> Result<Option<(u64, Value)>, StateError> {
        self.state
            .read(collection, id)
            .map_err(|e| from_agent("read", e))
    }

    fn task(&self, task_id: &str) -> Result<Option<(u64, TaskValue)>, StateError> {
        match self.read(TASK_COLLECTION, task_id)? {
            None => Ok(None),
            Some((version, value)) => serde_json::from_value(value)
                .map(|task| Some((version, task)))
                .map_err(|e| StateError::InvalidHistory {
                    task_id: task_id.to_string(),
                    detail: format!("task record does not parse: {e}"),
                }),
        }
    }

    /// Ids in `collection` that belong to `task_id`. Task ids cannot contain `~`, so the prefix
    /// names exactly one task's records.
    fn ids(&self, collection: &str, task_id: &str) -> Result<Vec<String>, StateError> {
        let prefix = format!("{collection}:{task_id}~");
        let rows = self
            .db
            .list_collection(collection)
            .map_err(|e| from_flow("list", e))?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                row.key
                    .strip_prefix(&prefix)
                    .map(|rest| format!("{task_id}~{rest}"))
            })
            .collect())
    }

    fn observation(&self, id: &str) -> Result<Option<(u64, ObservationRecord)>, StateError> {
        match self.read(OBSERVATION_COLLECTION, id)? {
            None => Ok(None),
            Some((version, value)) => {
                if value.get("schema").and_then(Value::as_str) != Some(OBSERVATION_SCHEMA_V1) {
                    schema_of(OBSERVATION_COLLECTION, id, &value, OBSERVATION_SCHEMA)?;
                }
                serde_json::from_value(value)
                    .map(|record| Some((version, record)))
                    .map_err(|e| StateError::InvalidHistory {
                        task_id: id.split('~').next().unwrap_or_default().to_string(),
                        detail: format!("observation {id} does not parse: {e}"),
                    })
            }
        }
    }

    /// Writes a record that must not exist yet, through the same guarded transaction primitive
    /// the contract's commits use.
    fn create(&self, collection: &str, id: &str, value: Value) -> Result<(), StateError> {
        let key = format!("{collection}:{id}");
        self.db
            .apply_atomic_transaction_guarded(
                &unique("chip-record"),
                None,
                None,
                &[],
                &[RecordPrecondition {
                    capability: collection.to_string(),
                    key: key.clone(),
                    require_absent: true,
                    ..Default::default()
                }],
                &[AtomicMutation {
                    capability: collection.to_string(),
                    key,
                    value: Some(value),
                }],
                None,
            )
            .map(|_| ())
            .map_err(|e| from_flow("record write", e))
    }

    /// Every task in the database, in the order FeltDB lists them.
    pub fn tasks(&self) -> Result<Vec<TaskValue>, StateError> {
        let rows = self
            .db
            .list_collection(TASK_COLLECTION)
            .map_err(|e| from_flow("list", e))?;
        let mut tasks = Vec::new();
        for row in rows {
            let Some(id) = row.key.strip_prefix(&format!("{TASK_COLLECTION}:")) else {
                continue;
            };
            if let Some((_, task)) = self.task(id)? {
                tasks.push(task);
            }
        }
        Ok(tasks)
    }

    /// Every operator settlement recorded for the task's intents (see [`settle`]).
    pub fn settlements(&self, task_id: &str) -> Result<Vec<Settlement>, StateError> {
        let mut intents = Vec::new();
        for id in self.ids(INTENT_COLLECTION, task_id)? {
            if let Some((_, value)) = self.read(INTENT_COLLECTION, &id)? {
                let intent: IntentValue =
                    serde_json::from_value(value).map_err(|e| StateError::InvalidHistory {
                        task_id: task_id.into(),
                        detail: format!("intent {id} does not parse: {e}"),
                    })?;
                intents.push((id, intent));
            }
        }
        settlements(self, &intents)
    }

    /// The task's durable history, or `None` if FeltDB has no such task. Incomplete or
    /// inconsistent history is an error, never a partial answer.
    pub fn history(&self, task_id: &str) -> Result<Option<History>, StateError> {
        let invalid = |detail: String| StateError::InvalidHistory {
            task_id: task_id.to_string(),
            detail,
        };
        let Some((task_version, task)) = self.task(task_id)? else {
            return Ok(None);
        };
        if task.task_id != task_id {
            return Err(invalid(format!("task record names {}", task.task_id)));
        }
        let mut checkpoints = Vec::new();
        for id in self.ids(CHECKPOINT_COLLECTION, task_id)? {
            let Some((_, value)) = self.read(CHECKPOINT_COLLECTION, &id)? else {
                continue;
            };
            let record: CheckpointRecord = serde_json::from_value(value)
                .map_err(|e| invalid(format!("checkpoint {id} does not parse: {e}")))?;
            if record.task_id != task_id || id != format!("{task_id}~v{}", record.version) {
                return Err(invalid(format!(
                    "checkpoint {id} names another task or version"
                )));
            }
            checkpoints.push(record);
        }
        checkpoints.sort_by_key(|c| c.version);
        let versions: Vec<u64> = checkpoints.iter().map(|c| c.version).collect();
        if versions != (1..=task.checkpoint_version).collect::<Vec<u64>>() {
            return Err(invalid(format!(
                "checkpoints {versions:?} do not cover versions 1 to {}",
                task.checkpoint_version
            )));
        }
        let last = checkpoints.last().expect("version 1 always exists");
        if last.status != task.status {
            return Err(invalid(format!(
                "the last checkpoint says {} but the task says {}",
                last.status, task.status
            )));
        }
        let mut intents = Vec::new();
        for id in self.ids(INTENT_COLLECTION, task_id)? {
            let Some((version, value)) = self.read(INTENT_COLLECTION, &id)? else {
                continue;
            };
            let value: IntentValue = serde_json::from_value(value)
                .map_err(|e| invalid(format!("intent {id} does not parse: {e}")))?;
            if value.task_id != task_id {
                return Err(invalid(format!("intent {id} names another task")));
            }
            intents.push(IntentEntry { id, version, value });
        }
        intents
            .sort_by(|a, b| (a.value.intended_at_ms, &a.id).cmp(&(b.value.intended_at_ms, &b.id)));
        let unresolved = intents
            .iter()
            .filter(|i| i.value.state != "resolved")
            .count() as u64;
        if unresolved != task.unresolved_intents {
            return Err(invalid(format!(
                "{unresolved} unresolved intent records but the task counts {}",
                task.unresolved_intents
            )));
        }
        let mut results = Vec::new();
        for id in self.ids(RESULT_COLLECTION, task_id)? {
            let Some((_, value)) = self.read(RESULT_COLLECTION, &id)? else {
                continue;
            };
            if value.get("schema").and_then(Value::as_str) != Some(RESULT_SCHEMA) {
                schema_of(RESULT_COLLECTION, &id, &value, RESULT_SCHEMA_V2)?;
            }
            let record: WorkResultRecord = serde_json::from_value(value)
                .map_err(|e| invalid(format!("work result {id} does not parse: {e}")))?;
            results.push(record);
        }
        results.sort_by_key(|r| r.attempt);
        let mut events = Vec::new();
        for id in self.ids(WORK_EVENT_COLLECTION, task_id)? {
            let Some((_, value)) = self.read(WORK_EVENT_COLLECTION, &id)? else {
                continue;
            };
            schema_of(WORK_EVENT_COLLECTION, &id, &value, WORK_EVENT_SCHEMA)?;
            let record: WorkEventRecord = serde_json::from_value(value)
                .map_err(|e| invalid(format!("work event {id} does not parse: {e}")))?;
            if record.task_id != task_id || id != event_id(task_id, record.attempt, record.index) {
                return Err(invalid(format!(
                    "work event {id} names another task or position"
                )));
            }
            events.push(record);
        }
        events.sort_by_key(|e| (e.attempt, e.index));
        // Each attempt's trajectory is whole from its start: positions 0, 1, 2, ... without a gap.
        // A recording that stopped early ends early; it never skips.
        let mut expected = (0u64, 0u64);
        for e in &events {
            if e.attempt != expected.0 {
                expected = (e.attempt, 0);
            }
            if e.index != expected.1 {
                return Err(invalid(format!(
                    "attempt {} has a work event at position {} where {} was expected",
                    e.attempt, e.index, expected.1
                )));
            }
            expected.1 += 1;
        }
        let model_responses = model_responses(self, task_id)?
            .into_iter()
            .map(|(_, r)| r)
            .collect();
        Ok(Some(History {
            task,
            task_version,
            checkpoints,
            intents,
            results,
            events,
            model_responses,
            reconciliations: reconcile::reconciliations(self, task_id)?,
        }))
    }
}

/// The id of a work event record: the task, the attempt and the position in its trajectory.
fn event_id(task_id: &str, attempt: u64, index: u64) -> String {
    format!("{task_id}~a{attempt}~e{index:06}")
}

fn schema_of(collection: &str, id: &str, value: &Value, expected: &str) -> Result<(), StateError> {
    let schema = value.get("schema").and_then(Value::as_str).unwrap_or("");
    if schema == expected {
        Ok(())
    } else {
        Err(StateError::UnsupportedRecord {
            collection: collection.to_string(),
            id: id.to_string(),
            schema: schema.to_string(),
        })
    }
}

fn record_ref(collection: &str, id: &str) -> EvidenceRef {
    EvidenceRef {
        kind: "record".into(),
        collection: Some(collection.into()),
        id: Some(id.into()),
        uri: None,
    }
}

/// The usage a provider reported for one model call, field for field; `None` is unreported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub cost: Option<String>,
    pub model: Option<String>,
}

impl RecordedUsage {
    fn of(usage: &chip_inference::Usage) -> Self {
        Self {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            cost: usage.cost.clone(),
            model: usage.model.clone(),
        }
    }

    fn to_usage(&self) -> chip_inference::Usage {
        chip_inference::Usage {
            prompt_tokens: self.prompt_tokens,
            completion_tokens: self.completion_tokens,
            reasoning_tokens: self.reasoning_tokens,
            cache_read_tokens: self.cache_read_tokens,
            cache_write_tokens: self.cache_write_tokens,
            cost: self.cost.clone(),
            model: self.model.clone(),
        }
    }
}

/// One `chip.ModelResponse` record: a response a model call returned, written as the loop
/// received it, before it was interpreted or anything it asked for was attempted. Write-once.
///
/// It is the model's judgment, kept so that an attempt interrupted before acting on it can be
/// answered from it instead of a second, paid call. It is never evidence, and nothing reads it to
/// decide whether anything executed: intents and observations say that.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelResponseRecord {
    pub schema: String,
    pub task_id: String,
    pub work_id: String,
    pub attempt: u64,
    pub fence: u64,
    /// The turn it answered, 1-based, as the work events number it.
    pub turn: u64,
    /// The model the work asked.
    pub model: String,
    /// The provider's id for the response.
    pub response_id: String,
    pub output: String,
    pub output_sha256: String,
    pub usage: RecordedUsage,
    /// The executions whose observations the work held when it asked, in order.
    pub observed: Vec<String>,
    /// The intents this attempt had recorded when the response arrived. A later intent of the
    /// attempt means something was attempted after it, so it was acted on.
    pub intents_before: Vec<String>,
    pub recorded_at_ms: u64,
}

fn event_kind(e: &WorkEventRecord) -> &str {
    e.event
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn event_turn(e: &WorkEventRecord) -> Option<u64> {
    e.event.get("turn").and_then(Value::as_u64)
}

/// The model call one attempt's trajectory (in order) asked for last and never saw return: its
/// last `ModelEscalation`, with no `ModelCalled` after it.
fn in_flight(attempt: u64, events: &[&WorkEventRecord]) -> Option<InterruptedModelCall> {
    let escalated = events
        .iter()
        .rposition(|e| event_kind(e) == "ModelEscalation")?;
    let returned = events[escalated..]
        .iter()
        .any(|e| event_kind(e) == "ModelCalled");
    (!returned).then(|| InterruptedModelCall {
        attempt,
        turn: event_turn(events[escalated]).unwrap_or_default(),
    })
}

/// The id of a model response record: the task, the attempt and the turn it answered.
fn model_response_id(task_id: &str, attempt: u64, turn: u64) -> String {
    format!("{task_id}~a{attempt}~m{turn:06}")
}

/// Whether [`begin`] found a recorded model response the attempt may be answered from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseRestoration {
    /// No interrupted attempt left a recorded response behind.
    None,
    /// The last response the interrupted attempt received was never acted on; the work is offered
    /// it, and takes it only if it is still exactly where the response was received.
    Offered { record: String },
    /// The interrupted attempt's last response is not offered, for `reason`; the model is asked.
    NotOffered { record: String, reason: String },
}

/// What [`begin`] found about the attempt it resumes: a model call that was in flight when it
/// stopped, whose outcome (sent, received, charged) is unknown. Nothing is replayed for it; the
/// resumed attempt asks the model again, and this says so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptedModelCall {
    pub attempt: u64,
    /// 1-based, as the work events number it.
    pub turn: u64,
}

/// Who runs the work, for how long a lease lasts, and what each capability declares: what it does
/// with an idempotency key and what it does to the world.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableConfig {
    pub actor: String,
    pub lease_ms: u64,
    /// Each capability's declared [`Idempotency`], by capability id, as its descriptor states it.
    /// A capability not named here cannot carry a key.
    pub idempotency: BTreeMap<String, Idempotency>,
    /// Each capability's declared [`Effect`], by capability id, as its descriptor states it. A
    /// capability not named here is [`Effect::Unknown`]: only an explicit `Observation` lets an
    /// interrupted execution be performed again to obtain a fresh observation.
    pub effects: BTreeMap<String, Effect>,
}

impl Default for DurableConfig {
    fn default() -> Self {
        Self {
            actor: "stack-agent".into(),
            lease_ms: DEFAULT_LEASE_MS,
            idempotency: BTreeMap::new(),
            effects: BTreeMap::new(),
        }
    }
}

/// An intent an earlier attempt left open, with no observation, for a capability that applies at
/// most one effect per key: the outcome is unknown, but performing the same logical action again
/// with the same key cannot apply its effect a second time. The attempt may perform it again
/// (when the model asks for exactly that request) and settles the intent from what the capability
/// then answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reissuable {
    pub intent_key: String,
    pub tag: ActionTag,
}

/// The executor of a re-observation, asking the work's owner once more at the last moment, as the
/// loop does before an execution: a stop requested after the intent was recorded and before the
/// capability is called is an `ExecutorUnavailable` (nothing ran), not a call.
struct Gated {
    inner: Arc<dyn CapabilityBackend>,
    control: Option<Arc<dyn WorkControl>>,
}

#[async_trait::async_trait]
impl Executor for Gated {
    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        if let Some(cause) = self.control.as_ref().and_then(|c| c.stop_requested()) {
            return Err(ExecutionError::ExecutorUnavailable(format!(
                "{} was requested; nothing was executed",
                cause.name()
            )));
        }
        self.inner.execute(request).await
    }
}

/// Open intents of earlier attempts, with no observation, for a capability that declares itself an
/// [`Effect::Observation`]: whether the request ran is unknown, but running it again changes
/// nothing, so this attempt performs the same validated request again and records what it then
/// observes. That is a fresh observation made during recovery, never the lost response.
///
/// More than one intent when a recovery was itself interrupted: they name one request, and one
/// fresh observation settles all of them.
#[derive(Debug, Clone, PartialEq)]
pub struct Reobservable {
    /// The open intents the fresh observation settles, oldest first.
    pub intents: Vec<String>,
    pub capability: String,
    /// The binding the intents recorded for the request ([`binding`]).
    pub binding: String,
    request: RecordedRequest,
}

/// A request [`DurableWork::reobserve`] performed again, to settle open intents of earlier
/// attempts. The observation is the capability's answer now, not the answer lost with the earlier
/// execution, which is not recoverable and is not claimed to have been.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reobserved {
    /// The open intents this observation settled.
    pub intents: Vec<String>,
    pub capability: String,
    /// The execution that made the fresh observation, and the intent that recorded it.
    pub execution_id: String,
    pub intent_key: String,
    /// `success`, `failure` or `cancelled`: what the capability answered.
    pub status: String,
}

/// What [`begin`] found.
pub enum Begun {
    /// This process owns the task and may run an attempt.
    Ready(DurableWork),
    /// The task is already terminal. Its persisted history is the answer; nothing is run.
    Finished(Box<History>),
}

/// Takes ownership of `task_id` in FeltDB, creating it if it does not exist, and makes it ready
/// for one attempt.
///
/// On a restart this is the recovery path: the task, its lease and its intents are read from
/// FeltDB, an expired lease is taken over through the contract's `acquire` (which advances the
/// fence, so the previous owner can no longer commit anything), and each unresolved intent is
/// reconciled from the observation record its execution left. An intent with no such record is
/// [`StateError::OutcomeUncertain`]: the absence of an observation is not evidence that nothing ran.
pub fn begin(
    store: &AgentStore,
    task_id: &str,
    objective: &str,
    config: &DurableConfig,
) -> Result<Begun, StateError> {
    check_contract(
        AGENT_TRANSITION_CONTRACT_ID,
        AGENT_TRANSITION_CONTRACT_VERSION,
    )?;
    if !is_identifier(task_id) {
        return Err(StateError::InvalidInput(format!(
            "task id {task_id:?} must be 1 to {MAX_IDENTIFIER_BYTES} bytes of A-Z a-z 0-9 _ . : @ -"
        )));
    }
    if !is_identifier(&config.actor) {
        return Err(StateError::InvalidInput(format!(
            "actor {:?} must be 1 to {MAX_IDENTIFIER_BYTES} bytes of A-Z a-z 0-9 _ . : @ -",
            config.actor
        )));
    }
    if objective.is_empty() || objective.len() > MAX_TEXT_BYTES {
        return Err(StateError::InvalidInput(format!(
            "the objective must be 1 to {MAX_TEXT_BYTES} bytes to be persisted"
        )));
    }
    if config.lease_ms == 0 || config.lease_ms > MAX_LEASE_MS {
        return Err(StateError::InvalidInput(format!(
            "the lease must be 1 to {MAX_LEASE_MS} ms"
        )));
    }
    let lease_id = unique("lease");
    let actor = config.actor.clone();
    let held = |task: &TaskValue| StateError::LeaseHeld {
        task_id: task_id.to_string(),
        owner: task.owner.clone(),
    };
    let resumed_from = match store.task(task_id)? {
        None => {
            let created = store.apply(
                "create_task",
                Operation {
                    op: "create_task".into(),
                    task_id: Some(task_id.into()),
                    actor: Some(actor.clone()),
                    objective: Some(objective.into()),
                    lease_id: Some(lease_id.clone()),
                    lease_ms: Some(config.lease_ms),
                    ..Operation::default()
                },
            );
            match created {
                Ok(()) => None,
                // Another process created it between the read and the commit.
                Err(StateError::Refused {
                    code: ErrorCode::AlreadyExists,
                    ..
                })
                | Err(StateError::Conflict { .. }) => {
                    return Err(match store.task(task_id)? {
                        Some((_, task)) if task.objective != objective => {
                            StateError::ObjectiveMismatch {
                                task_id: task_id.into(),
                            }
                        }
                        Some((_, task)) => held(&task),
                        None => StateError::InvalidHistory {
                            task_id: task_id.into(),
                            detail: "the task was neither created nor found".into(),
                        },
                    });
                }
                Err(other) => return Err(other),
            }
        }
        Some((_, task)) => {
            if task.objective != objective {
                return Err(StateError::ObjectiveMismatch {
                    task_id: task_id.into(),
                });
            }
            if matches!(task.status.as_str(), "completed" | "cancelled") {
                let history = store.history(task_id)?.ok_or(StateError::InvalidHistory {
                    task_id: task_id.into(),
                    detail: "the task disappeared while it was read".into(),
                })?;
                return Ok(Begun::Finished(Box::new(history)));
            }
            match store.apply(
                "acquire",
                Operation {
                    op: "acquire".into(),
                    task_id: Some(task_id.into()),
                    actor: Some(actor.clone()),
                    lease_id: Some(lease_id.clone()),
                    lease_ms: Some(config.lease_ms),
                    ..Operation::default()
                },
            ) {
                Ok(()) => Some(task.status.clone()),
                Err(StateError::Refused {
                    code: ErrorCode::LeaseHeld,
                    ..
                })
                | Err(StateError::Conflict { .. }) => {
                    let current = store.task(task_id)?.map(|(_, t)| t).unwrap_or(task);
                    return Err(held(&current));
                }
                Err(other) => return Err(other),
            }
        }
    };
    // What FeltDB now says, not what this process just asked for: a concurrent `create_task` by
    // the same actor is a no-op that leaves the other caller's lease in place.
    let (_, task) = store.task(task_id)?.ok_or(StateError::InvalidHistory {
        task_id: task_id.into(),
        detail: "the task is missing after it was taken".into(),
    })?;
    if task.owner != actor || task.lease.as_ref().map(|l| l.lease_id.as_str()) != Some(&lease_id) {
        return Err(held(&task));
    }
    let fence = task.fence;
    let reconcile = |key: &str, conclusion: &str, observation: &str| {
        store.apply(
            "reconcile",
            Operation {
                op: "reconcile".into(),
                task_id: Some(task_id.into()),
                actor: Some(actor.clone()),
                fence: Some(fence),
                lease_id: Some(lease_id.clone()),
                idempotency_key: Some(key.into()),
                conclusion: Some(conclusion.into()),
                evidence: Some(vec![record_ref(OBSERVATION_COLLECTION, observation)]),
                ..Operation::default()
            },
        )
    };
    let mut intents = Vec::new();
    for id in store.ids(INTENT_COLLECTION, task_id)? {
        let Some((_, value)) = store.read(INTENT_COLLECTION, &id)? else {
            continue;
        };
        let intent: IntentValue =
            serde_json::from_value(value).map_err(|e| StateError::InvalidHistory {
                task_id: task_id.into(),
                detail: format!("intent {id} does not parse: {e}"),
            })?;
        intents.push((id, intent));
    }
    // Every logical key the task has bound, to what, and the observation that answered it.
    let keys = bound_keys(store, task_id, &intents)?;
    // Settle every intent a previous attempt left open, from evidence only.
    let mut uncertain = Vec::new();
    let mut reissuable = Vec::new();
    let mut plans = Vec::new();
    for (id, intent) in &intents {
        if intent.state == "resolved" {
            continue;
        }
        if let Some((_, observed)) = store.observation(id)? {
            reconcile(
                &intent.key,
                if observed.executed {
                    "executed"
                } else {
                    "not_executed"
                },
                id,
            )?;
            continue;
        }
        // An earlier recovery already obtained a fresh observation of it and was interrupted
        // before it settled this intent: that observation settles it, and nothing runs again.
        if let Some(observation) = answered_reobservation(store, &intents, &intent.key)? {
            reconcile(&intent.key, "not_executed", &observation)?;
            continue;
        }
        // No observation: the action may have run. A capability that declares itself an
        // observation is performed again by this attempt, for a fresh one.
        if let Some(plan) = plan_reobservation(intent, config) {
            plans.push((intent.intended_at_ms, reobserves_of(&intent.action), plan));
            continue;
        }
        // Otherwise only a capability that applies at most one effect per key, then and now, can
        // be asked again; anything else stays uncertain.
        let tag = ActionTag::parse(&intent.action).filter(|tag| {
            tag.idempotency == Idempotency::Deduplicated
                && config.idempotency.get(&tag.capability) == Some(&Idempotency::Deduplicated)
        });
        let Some(tag) = tag else {
            uncertain.push(intent.key.clone());
            continue;
        };
        // A later attempt already performed it again and the capability answered for the key.
        match keys.get(&tag.key) {
            Some(Bound {
                answered: Some(observation),
                ..
            }) => reconcile(&intent.key, "executed", observation)?,
            _ => reissuable.push(Reissuable {
                intent_key: intent.key.clone(),
                tag,
            }),
        }
    }
    // Intents of one interrupted recovery name one request, and one fresh observation settles them.
    plans.sort_by(|a, b| (a.0, &a.2.intents[0]).cmp(&(b.0, &b.2.intents[0])));
    let mut reobservable: Vec<Reobservable> = Vec::new();
    for (_, targets, plan) in plans {
        match reobservable
            .iter_mut()
            .find(|g| g.binding == plan.binding && targets.iter().any(|t| g.intents.contains(t)))
        {
            Some(group) => group.intents.push(plan.intents[0].clone()),
            None => reobservable.push(plan),
        }
    }
    if !uncertain.is_empty() {
        // The refusal keeps nothing: the lease this call just took is given back, so the task can
        // be settled and resumed at once instead of after the lease runs out.
        let refused = StateError::OutcomeUncertain {
            task_id: task_id.into(),
            intents: uncertain.clone(),
        };
        return Err(match release(store, task_id, &actor, fence, &lease_id) {
            Ok(()) => refused,
            Err(e) => StateError::UncertainLeaseHeld {
                task_id: task_id.into(),
                intents: uncertain,
                detail: e.to_string(),
            },
        });
    }
    // Every intent is resolved now; what earlier attempts observed is read back from their
    // observation records, the only place it is kept.
    let open: Vec<&str> = reissuable
        .iter()
        .map(|r| r.intent_key.as_str())
        .chain(
            reobservable
                .iter()
                .flat_map(|r| r.intents.iter().map(String::as_str)),
        )
        .collect();
    let (restored, restored_ids, restoration) = restore(store, task_id, &open)?;
    let settled = settlements(store, &intents)?;
    let (restored_response, response_restoration, interrupted_model_call) =
        unconsumed_response(store, task_id, &intents, !open.is_empty())?;
    let (_, task) = store.task(task_id)?.ok_or(StateError::InvalidHistory {
        task_id: task_id.into(),
        detail: "the task is missing after reconciliation".into(),
    })?;
    let attempt = task.checkpoint_version + 1;
    store.apply(
        "checkpoint",
        Operation {
            op: "checkpoint".into(),
            task_id: Some(task_id.into()),
            actor: Some(actor.clone()),
            fence: Some(fence),
            lease_id: Some(lease_id.clone()),
            expected_version: Some(task.checkpoint_version),
            summary: Some(match &resumed_from {
                None => format!("Chip work started (attempt {attempt})."),
                Some(status) => {
                    format!("Chip work resumed from {status} (attempt {attempt}).")
                }
            }),
            evidence: Some(Vec::new()),
            status: Some("running".into()),
            ..Operation::default()
        },
    )?;
    Ok(Begun::Ready(DurableWork(Arc::new(Inner {
        store: store.clone(),
        task_id: task_id.into(),
        actor,
        lease_id,
        lease_ms: config.lease_ms,
        fence,
        attempt,
        resumed_from,
        restored: Mutex::new(restored),
        restored_ids: Mutex::new(restored_ids),
        restoration,
        idempotency: config.idempotency.clone(),
        keys: Mutex::new(
            keys.into_iter()
                .map(|(key, bound)| (key, bound.binding))
                .collect(),
        ),
        reissuable: Mutex::new(reissuable),
        reobservable: Mutex::new(reobservable),
        reobserved: Mutex::new(Vec::new()),
        settled,
        observations: Mutex::new(Vec::new()),
        intents: Mutex::new(Vec::new()),
        restored_response,
        response_restoration,
        interrupted_model_call,
        failures: Mutex::new(Vec::new()),
        next_event: AtomicU64::new(0),
        events_stopped: std::sync::atomic::AtomicBool::new(false),
        recalled: Mutex::new(Vec::new()),
    }))))
}

/// The most bytes of evidence a settlement takes. Bounded so that the checkpoint recording it is
/// never clipped by the contract's text limit.
pub const MAX_SETTLEMENT_EVIDENCE_BYTES: usize = 512;
/// How long a settlement holds the task while it records itself; it releases the lease when done.
const SETTLEMENT_LEASE_MS: u64 = 60_000;

/// What [`settle`] recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct Settled {
    pub settlement: Settlement,
    /// The `chip.Observation` id of the settlement record, which the intent now cites.
    pub record_id: String,
    pub history: History,
}

/// Records an operator's settlement of an intent whose outcome was never observed: their
/// conclusion that its effect did (`executed`) or did not occur, who they are (`decided_by`) and
/// the basis they give (`evidence`).
///
/// It performs nothing and makes up no receipt. The settlement is written as a `chip.Observation`
/// that names execution `operator` of attempt 0, carries no request and no receipt, and whose
/// status is `error`, because no capability answered (see [`ObservationRecord::is_settlement`]).
/// The intent is reconciled citing it, and a checkpoint by `decided_by` says what was concluded.
/// The intent itself, and its history, stay as they were.
///
/// Refused, with nothing written, when:
/// * an input is malformed;
/// * the task or the intent does not exist;
/// * the task is finished;
/// * the intent is resolved already;
/// * the intent has an observation of its own (the next start reconciles it from that);
/// * a worker holds the task.
///
/// The settlement record is created only if absent, so two operators settling the same intent
/// cannot both be recorded.
pub fn settle(
    store: &AgentStore,
    task_id: &str,
    intent_key: &str,
    executed: bool,
    decided_by: &str,
    evidence: &str,
) -> Result<Settled, StateError> {
    check_contract(
        AGENT_TRANSITION_CONTRACT_ID,
        AGENT_TRANSITION_CONTRACT_VERSION,
    )?;
    let refused = |reason: String| StateError::SettlementRefused {
        task_id: task_id.into(),
        intent: intent_key.into(),
        reason,
    };
    for (what, value) in [
        ("task id", task_id),
        ("intent", intent_key),
        ("decided-by", decided_by),
    ] {
        if !is_identifier(value) {
            return Err(refused(format!(
                "the {what} {value:?} must be 1 to {MAX_IDENTIFIER_BYTES} bytes of A-Z a-z 0-9 _ . : @ -"
            )));
        }
    }
    let evidence = evidence.trim();
    if evidence.is_empty()
        || evidence.len() > MAX_SETTLEMENT_EVIDENCE_BYTES
        || evidence.chars().any(char::is_control)
    {
        return Err(refused(format!(
            "the evidence must be 1 to {MAX_SETTLEMENT_EVIDENCE_BYTES} bytes of text on one line, saying what the conclusion rests on"
        )));
    }
    let id = format!("{task_id}~{intent_key}");
    // Everything that can refuse is checked before anything is written, and again once the task
    // is held, so a settlement is recorded whole or not at all.
    let check = |store: &AgentStore| -> Result<(TaskValue, IntentValue), StateError> {
        let Some((_, task)) = store.task(task_id)? else {
            return Err(refused("there is no such task".into()));
        };
        if matches!(task.status.as_str(), "completed" | "cancelled") {
            return Err(refused(format!("the task is {}", task.status)));
        }
        let Some((_, value)) = store.read(INTENT_COLLECTION, &id)? else {
            return Err(refused("the task has no such intent".into()));
        };
        let intent: IntentValue =
            serde_json::from_value(value).map_err(|e| StateError::InvalidHistory {
                task_id: task_id.into(),
                detail: format!("intent {id} does not parse: {e}"),
            })?;
        if intent.state == "resolved" {
            return Err(refused(format!(
                "it is resolved already (conclusion {})",
                intent.conclusion.as_deref().unwrap_or("from its outcome")
            )));
        }
        if store.observation(&id)?.is_some() {
            return Err(refused(
                "it has an observation record; the next start reconciles it from that".into(),
            ));
        }
        Ok((task, intent))
    };
    check(store)?;
    let lease_id = unique("settle");
    store
        .apply(
            "acquire",
            Operation {
                op: "acquire".into(),
                task_id: Some(task_id.into()),
                actor: Some(decided_by.into()),
                lease_id: Some(lease_id.clone()),
                lease_ms: Some(SETTLEMENT_LEASE_MS),
                ..Operation::default()
            },
        )
        .map_err(|e| match e {
            StateError::Refused {
                code: ErrorCode::LeaseHeld,
                ..
            }
            | StateError::Conflict { .. } => refused(
                "a worker holds the task's lease; settle once it has stopped and its lease is released or expired".into(),
            ),
            other => other,
        })?;
    let (task, intent) = match check(store) {
        Ok(found) => found,
        Err(e) => {
            let _ = release(
                store,
                task_id,
                decided_by,
                fence_of(store, task_id)?,
                &lease_id,
            );
            return Err(e);
        }
    };
    let fence = task.fence;
    let record = ObservationRecord {
        schema: OBSERVATION_SCHEMA.into(),
        task_id: task_id.into(),
        intent_key: intent_key.into(),
        execution_id: SETTLEMENT_EXECUTION_ID.into(),
        attempt: 0,
        status: "error".into(),
        executed,
        output: format!(
            "{SETTLEMENT_OUTPUT} decided_by={decided_by} executed={executed} evidence={evidence}"
        ),
        receipt_id: None,
        request: None,
    };
    if let Err(e) = store.create(
        OBSERVATION_COLLECTION,
        &id,
        serde_json::to_value(&record).expect("a settlement serializes"),
    ) {
        let _ = release(store, task_id, decided_by, fence, &lease_id);
        return Err(match e {
            StateError::Conflict { .. } => {
                refused("another settlement or observation was recorded first".into())
            }
            other => other,
        });
    }
    // The settlement is recorded; from here a failure leaves it for the next start to reconcile.
    let owner = |op: &str| Operation {
        op: op.into(),
        task_id: Some(task_id.into()),
        actor: Some(decided_by.into()),
        fence: Some(fence),
        lease_id: Some(lease_id.clone()),
        ..Operation::default()
    };
    let conclusion = if executed { "executed" } else { "not_executed" };
    store.apply(
        "reconcile",
        Operation {
            idempotency_key: Some(intent_key.into()),
            conclusion: Some(conclusion.into()),
            evidence: Some(vec![record_ref(OBSERVATION_COLLECTION, &id)]),
            ..owner("reconcile")
        },
    )?;
    let (_, task) = store.task(task_id)?.ok_or(StateError::InvalidHistory {
        task_id: task_id.into(),
        detail: "the task is missing after settlement".into(),
    })?;
    store.apply(
        "checkpoint",
        Operation {
            expected_version: Some(task.checkpoint_version),
            summary: Some(clip(
                &format!(
                    "Operator {decided_by} settled intent {intent_key} ({}): in their judgment the effect {}. A conclusion, not an execution observation. Evidence: {evidence}",
                    action_parts(&intent.action).0,
                    if executed { "occurred" } else { "did not occur" },
                ),
                MAX_TEXT_BYTES,
            )),
            evidence: Some(vec![record_ref(OBSERVATION_COLLECTION, &id)]),
            status: Some(task.status.clone()),
            ..owner("checkpoint")
        },
    )?;
    release(store, task_id, decided_by, fence, &lease_id)?;
    let settlement = Settlement::of(&intent, &record).expect("a settlement record is one");
    let history = store.history(task_id)?.ok_or(StateError::InvalidHistory {
        task_id: task_id.into(),
        detail: "the task is missing after settlement".into(),
    })?;
    Ok(Settled {
        settlement,
        record_id: id,
        history,
    })
}

fn fence_of(store: &AgentStore, task_id: &str) -> Result<u64, StateError> {
    Ok(store
        .task(task_id)?
        .map(|(_, t)| t.fence)
        .unwrap_or_default())
}

/// The shortest lease contract v1 grants. It has no release operation: an owner releases its lease
/// by renewing it to this, under the same owner, fence and lease checks as every other renewal.
const RELEASED_LEASE_MS: u64 = 1;

/// Gives back the lease `lease_id` that `actor` holds on `task_id` at `fence`. FeltDB checks all
/// three, so a lease anyone else has taken since (a later owner, at a later fence) is untouched
/// and this fails.
fn release(
    store: &AgentStore,
    task_id: &str,
    actor: &str,
    fence: u64,
    lease_id: &str,
) -> Result<(), StateError> {
    store.apply(
        "release",
        Operation {
            op: "renew".into(),
            task_id: Some(task_id.into()),
            actor: Some(actor.into()),
            fence: Some(fence),
            lease_id: Some(lease_id.into()),
            lease_ms: Some(RELEASED_LEASE_MS),
            ..Operation::default()
        },
    )
}

/// Every settlement an operator recorded for the task's intents, in intent order.
fn settlements(
    store: &AgentStore,
    intents: &[(String, IntentValue)],
) -> Result<Vec<Settlement>, StateError> {
    let mut settled = Vec::new();
    for (id, intent) in intents {
        if let Some((_, record)) = store.observation(id)? {
            settled.extend(Settlement::of(intent, &record));
        }
    }
    Ok(settled)
}

/// What one logical key of a task is bound to.
struct Bound {
    binding: String,
    /// The observation of an execution that performed it and that its capability answered (any
    /// status but an error), if one did.
    answered: Option<String>,
}

/// Every logical key the task's intents recorded. Two intents that bind one key to different
/// requests contradict the rule every intent was recorded under, and are invalid history.
fn bound_keys(
    store: &AgentStore,
    task_id: &str,
    intents: &[(String, IntentValue)],
) -> Result<BTreeMap<String, Bound>, StateError> {
    let mut keys: BTreeMap<String, Bound> = BTreeMap::new();
    for (id, intent) in intents {
        let Some(tag) = ActionTag::parse(&intent.action) else {
            continue;
        };
        let answered = match store.observation(id)? {
            Some((_, record)) if record.status != "error" && record.request.is_some() => {
                Some(id.clone())
            }
            _ => None,
        };
        match keys.get_mut(&tag.key) {
            Some(bound) if bound.binding != tag.binding => {
                return Err(StateError::InvalidHistory {
                    task_id: task_id.into(),
                    detail: format!(
                        "intent {id} binds key {} to another request than an earlier intent did",
                        tag.key
                    ),
                });
            }
            Some(bound) => {
                if bound.answered.is_none() {
                    bound.answered = answered;
                }
            }
            None => {
                keys.insert(
                    tag.key,
                    Bound {
                        binding: tag.binding,
                        answered,
                    },
                );
            }
        }
    }
    Ok(keys)
}

/// The trajectory earlier attempts of `task_id` executed, from FeltDB's records: each resolved
/// intent's observation record, in execution order (attempt, then the time FeltDB recorded the
/// intent, then Chip's execution sequence). An execution whose executor returned an error, or that
/// never ran, was never an observation and is not one now.
///
/// A record that contradicts its intent is [`StateError::InvalidHistory`]. A consistent history
/// that cannot be restored whole (a record without its request, an execution identity recorded
/// twice) is [`Restoration::NotRestored`] and nothing is restored.
#[allow(clippy::type_complexity)]
fn restore(
    store: &AgentStore,
    task_id: &str,
    reissuable: &[&str],
) -> Result<(Vec<RestoredObservation>, Vec<String>, Restoration), StateError> {
    let invalid = |detail: String| StateError::InvalidHistory {
        task_id: task_id.to_string(),
        detail,
    };
    let mut intents = BTreeMap::new();
    for id in store.ids(INTENT_COLLECTION, task_id)? {
        let Some((_, value)) = store.read(INTENT_COLLECTION, &id)? else {
            continue;
        };
        let intent: IntentValue = serde_json::from_value(value)
            .map_err(|e| invalid(format!("intent {id} does not parse: {e}")))?;
        intents.insert(id, intent);
    }
    let mut found = Vec::new();
    for (id, intent) in &intents {
        if intent.state != "resolved" {
            // Left open for the same logical action to be performed again: nothing was observed,
            // so there is nothing to restore.
            if reissuable.contains(&intent.key.as_str()) {
                continue;
            }
            return Err(invalid(format!("intent {id} is still open")));
        }
        let Some((_, record)) = store.observation(id)? else {
            // Settled by a later execution of the same logical action, whose observation is
            // restored in its own place.
            if superseded(intent, &intents) {
                continue;
            }
            return Err(invalid(format!(
                "intent {id} is resolved but has no observation record"
            )));
        };
        if record.task_id != task_id || record.intent_key != intent.key {
            return Err(invalid(format!(
                "observation {id} does not describe intent {}",
                intent.key
            )));
        }
        found.push((intent.intended_at_ms, id.clone(), record));
    }
    let sequence = |execution_id: &str| {
        execution_id
            .rsplit_once("-exec-")
            .and_then(|(_, n)| n.parse::<u64>().ok())
            .unwrap_or(u64::MAX)
    };
    found.sort_by(|a, b| {
        (a.2.attempt, a.0, sequence(&a.2.execution_id), &a.1).cmp(&(
            b.2.attempt,
            b.0,
            sequence(&b.2.execution_id),
            &b.1,
        ))
    });
    let not_restored =
        |reason: String| Ok((Vec::new(), Vec::new(), Restoration::NotRestored { reason }));
    let (mut restored, mut ids) = (Vec::new(), Vec::new());
    for (_, id, record) in found {
        // An operator's conclusion is not an observation; it is read by [`settlements`].
        if record.is_settlement() {
            continue;
        }
        let status = match record.status.as_str() {
            "success" => ExecutionStatus::Success,
            "failure" => ExecutionStatus::Failure,
            "cancelled" => ExecutionStatus::Cancelled,
            "error" => continue,
            other => return Err(invalid(format!("observation {id} has status {other:?}"))),
        };
        // Only a record Chip wrote for an execution carries the request; an earlier version's
        // record does not, and is not an execution to restore.
        let Some(recorded) = &record.request else {
            return not_restored(format!(
                "observation {id} was recorded without its request (an earlier version)"
            ));
        };
        if !record.executed || intent_key(record.attempt, &record.execution_id) != record.intent_key
        {
            return Err(invalid(format!(
                "observation {id} names execution {} of attempt {} but describes intent {}",
                record.execution_id, record.attempt, record.intent_key
            )));
        }
        if restored
            .iter()
            .any(|r: &RestoredObservation| r.result.id.0 == record.execution_id)
        {
            return not_restored(format!(
                "execution {} was recorded by more than one attempt",
                record.execution_id
            ));
        }
        let request = recorded
            .to_request(&record.execution_id)
            .map_err(|why| invalid(format!("observation {id}: {why}")))?;
        restored.push(RestoredObservation {
            request,
            result: ExecutionResult {
                id: ExecutionId::new(&record.execution_id),
                status,
                output: record.output.clone(),
                receipt_id: record.receipt_id.clone(),
                evidence: None,
            },
            // A recorded observation, however it was made: this attempt performed nothing for it.
            reobserved: false,
        });
        ids.push(id);
    }
    let observations = restored.len();
    Ok((restored, ids, Restoration::Restored { observations }))
}

/// Whether `intent`, resolved without an observation of its own, was settled by an observation of
/// another execution: it cites one, and that one's intent either records the same key bound to the
/// same request (the same logical action performed again), or says it performs `intent` again to
/// obtain a fresh observation (`reobserves=`) of the same request.
/// The task's model response records, in (attempt, turn) order. A record of another schema, or
/// one that does not name its own place, is invalid history.
fn model_responses(
    store: &AgentStore,
    task_id: &str,
) -> Result<Vec<(String, ModelResponseRecord)>, StateError> {
    let mut records = Vec::new();
    for id in store.ids(MODEL_RESPONSE_COLLECTION, task_id)? {
        let Some((_, value)) = store.read(MODEL_RESPONSE_COLLECTION, &id)? else {
            continue;
        };
        schema_of(
            MODEL_RESPONSE_COLLECTION,
            &id,
            &value,
            MODEL_RESPONSE_SCHEMA,
        )?;
        let record: ModelResponseRecord =
            serde_json::from_value(value).map_err(|e| StateError::InvalidHistory {
                task_id: task_id.into(),
                detail: format!("model response {id} does not parse: {e}"),
            })?;
        if record.task_id != task_id
            || id != model_response_id(task_id, record.attempt, record.turn)
            || record.output_sha256 != sha256_hex(&record.output)
        {
            return Err(StateError::InvalidHistory {
                task_id: task_id.into(),
                detail: format!("model response {id} names another task, place or output"),
            });
        }
        records.push((id, record));
    }
    records.sort_by_key(|(_, r)| (r.attempt, r.turn));
    Ok(records)
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The response the last attempt received and never acted on, if there is one, and a model call
/// that was in flight when that attempt stopped, if there was one.
///
/// The last attempt is the latest one that recorded any work event. Its last recorded response is
/// offered only when every record agrees that nothing was attempted after it:
///
/// * the attempt has no work result (it was interrupted, not finished);
/// * its last `ModelCalled` event is that response's turn and succeeded, and every event after it
///   is the decision and request the response made (no execution, reuse, new turn or end);
/// * its intents are exactly the ones it had recorded when the response arrived, and no later
///   attempt recorded any;
/// * no intent of the task is still open (an open intent is recovery's to settle first).
///
/// Anything else is not offered, with the reason. The work then checks, before taking it, that it
/// is the same model and the same observations (`chip_core::RestoredResponse`).
fn unconsumed_response(
    store: &AgentStore,
    task_id: &str,
    intents: &[(String, IntentValue)],
    open: bool,
) -> Result<
    (
        Option<RestoredResponse>,
        ResponseRestoration,
        Option<InterruptedModelCall>,
    ),
    StateError,
> {
    let mut events: Vec<WorkEventRecord> = Vec::new();
    for id in store.ids(WORK_EVENT_COLLECTION, task_id)? {
        let Some((_, value)) = store.read(WORK_EVENT_COLLECTION, &id)? else {
            continue;
        };
        schema_of(WORK_EVENT_COLLECTION, &id, &value, WORK_EVENT_SCHEMA)?;
        let record: WorkEventRecord =
            serde_json::from_value(value).map_err(|e| StateError::InvalidHistory {
                task_id: task_id.into(),
                detail: format!("work event {id} does not parse: {e}"),
            })?;
        events.push(record);
    }
    let Some(last) = events.iter().map(|e| e.attempt).max() else {
        return Ok((None, ResponseRestoration::None, None));
    };
    let mut events: Vec<&WorkEventRecord> = events.iter().filter(|e| e.attempt == last).collect();
    events.sort_by_key(|e| e.index);
    let (kind, turn_of) = (event_kind, event_turn);
    let finished = store
        .read(RESULT_COLLECTION, &format!("{task_id}~a{last}"))?
        .is_some();
    // A model call that was asked for and never returned: the attempt's last escalation has no
    // `ModelCalled` after it.
    let interrupted = (!finished).then(|| in_flight(last, &events)).flatten();
    let Some((record_id, record)) = model_responses(store, task_id)?
        .into_iter()
        .rfind(|(_, r)| r.attempt == last)
    else {
        return Ok((None, ResponseRestoration::None, interrupted));
    };
    if finished {
        return Ok((None, ResponseRestoration::None, interrupted));
    }
    let not_offered = |reason: &str| {
        Ok((
            None,
            ResponseRestoration::NotOffered {
                record: record_id.clone(),
                reason: reason.into(),
            },
            interrupted.clone(),
        ))
    };
    let Some(called) = events.iter().rposition(|e| kind(e) == "ModelCalled") else {
        return not_offered("the attempt recorded no model call for it");
    };
    if turn_of(events[called]) != Some(record.turn)
        || events[called].event.get("succeeded") != Some(&Value::Bool(true))
    {
        return not_offered("it is not the attempt's last model call");
    }
    if let Some(after) = events[called + 1..]
        .iter()
        .map(|e| kind(e))
        .find(|k| *k != "DecisionMade" && *k != "CapabilityRequested")
    {
        return not_offered(&format!("the attempt recorded {after} after it"));
    }
    let prefix = format!("a{last}.");
    let mut attempted: Vec<&str> = intents
        .iter()
        .map(|(_, i)| i.key.as_str())
        .filter(|k| k.starts_with(&prefix))
        .collect();
    let mut before: Vec<&str> = record.intents_before.iter().map(String::as_str).collect();
    attempted.sort_unstable();
    before.sort_unstable();
    if attempted != before {
        return not_offered("the attempt recorded an intent after it");
    }
    // An attempt after it that recorded intents but no events did something no event shows.
    let later = intents.iter().any(|(_, i)| {
        i.key
            .strip_prefix('a')
            .and_then(|rest| rest.split('.').next())
            .and_then(|n| n.parse::<u64>().ok())
            .is_some_and(|n| n > last)
    });
    if later {
        return not_offered("a later attempt recorded an intent");
    }
    if open {
        return not_offered("intents of the task are still open");
    }
    let response = chip_inference::ModelResponse::new(
        record.response_id.clone(),
        record.output.clone(),
        record.usage.to_usage(),
    );
    Ok((
        Some(RestoredResponse {
            record: record_id.clone(),
            model: record.model.clone(),
            response,
            observed: record.observed.iter().map(ExecutionId::new).collect(),
        }),
        ResponseRestoration::Offered { record: record_id },
        interrupted,
    ))
}

fn superseded(intent: &IntentValue, intents: &BTreeMap<String, IntentValue>) -> bool {
    let tag = ActionTag::parse(&intent.action);
    let (_, binding) = action_parts(&intent.action);
    intent.evidence.iter().any(|citation| {
        if citation.collection.as_deref() != Some(OBSERVATION_COLLECTION) {
            return false;
        }
        let Some(other) = citation.id.as_ref().and_then(|id| intents.get(id)) else {
            return false;
        };
        let same_action = tag.as_ref().is_some_and(|tag| {
            ActionTag::parse(&other.action)
                .is_some_and(|other| other.key == tag.key && other.binding == tag.binding)
        });
        let observed_again = binding.is_some()
            && action_parts(&other.action).1 == binding
            && reobserves_of(&other.action).contains(&intent.key);
        same_action || observed_again
    })
}

struct Inner {
    store: AgentStore,
    task_id: String,
    actor: String,
    lease_id: String,
    lease_ms: u64,
    fence: u64,
    attempt: u64,
    resumed_from: Option<String>,
    /// What earlier attempts executed, to start this attempt from, and their record ids.
    restored: Mutex<Vec<RestoredObservation>>,
    restored_ids: Mutex<Vec<String>>,
    restoration: Restoration,
    idempotency: BTreeMap<String, Idempotency>,
    /// Every logical key the task has bound, to its request's binding.
    keys: Mutex<BTreeMap<String, String>>,
    /// Open intents this attempt may settle by performing their action again.
    reissuable: Mutex<Vec<Reissuable>>,
    /// Open intents of an observation capability this attempt has yet to settle by observing again.
    reobservable: Mutex<Vec<Reobservable>>,
    /// What [`DurableWork::reobserve`] has observed again.
    reobserved: Mutex<Vec<Reobserved>>,
    /// Intents an operator settled.
    settled: Vec<Settlement>,
    /// Observation record ids written by this attempt, in order.
    observations: Mutex<Vec<String>>,
    /// Intent keys this attempt has recorded, in order.
    intents: Mutex<Vec<String>>,
    /// The recorded response an interrupted earlier attempt never acted on, and what was decided
    /// about it.
    restored_response: Option<RestoredResponse>,
    response_restoration: ResponseRestoration,
    interrupted_model_call: Option<InterruptedModelCall>,
    failures: Mutex<Vec<StateError>>,
    /// The next work event's position, and whether recording stopped at a failure (so the
    /// recorded trajectory stays whole: it may end early, it never has a gap).
    next_event: AtomicU64,
    events_stopped: std::sync::atomic::AtomicBool,
    /// The episodes [`DurableWork::recall`] recalled for this attempt.
    recalled: Mutex<Vec<Recalled>>,
}

impl Inner {
    fn owner_op(&self, op: &str) -> Operation {
        Operation {
            op: op.into(),
            task_id: Some(self.task_id.clone()),
            actor: Some(self.actor.clone()),
            fence: Some(self.fence),
            lease_id: Some(self.lease_id.clone()),
            ..Operation::default()
        }
    }

    fn fail(&self, error: StateError) {
        self.failures.lock().unwrap().push(error);
    }

    /// Renews the lease and records the intent. Until this commits, the action does not start.
    fn before(&self, key: &str, action: &str) -> Result<(), StateError> {
        self.store.apply(
            "renew",
            Operation {
                lease_ms: Some(self.lease_ms),
                ..self.owner_op("renew")
            },
        )?;
        let action = clip(action.trim(), MAX_TEXT_BYTES);
        self.store.apply(
            "record_intent",
            Operation {
                idempotency_key: Some(key.into()),
                action: Some(if action.is_empty() {
                    "execute".into()
                } else {
                    action
                }),
                ..self.owner_op("record_intent")
            },
        )?;
        self.intents.lock().unwrap().push(key.into());
        Ok(())
    }

    /// Writes what was observed, then records the outcome citing it.
    fn after(
        &self,
        key: &str,
        request: &ExecutionRequest,
        result: &Result<ExecutionResult, ExecutionError>,
    ) -> Result<(), StateError> {
        let id = format!("{}~{key}", self.task_id);
        let (status, executed, output, receipt_id) = match result {
            Ok(r) => (
                match r.status {
                    ExecutionStatus::Success => "success",
                    ExecutionStatus::Failure => "failure",
                    ExecutionStatus::Cancelled => "cancelled",
                },
                true,
                r.output.clone(),
                r.receipt_id.clone(),
            ),
            Err(e) => (
                "error",
                !matches!(
                    e,
                    ExecutionError::InvalidRequest(_) | ExecutionError::ExecutorUnavailable(_)
                ),
                e.to_string(),
                None,
            ),
        };
        let record = ObservationRecord {
            schema: OBSERVATION_SCHEMA.into(),
            task_id: self.task_id.clone(),
            intent_key: key.into(),
            execution_id: request.id.0.clone(),
            attempt: self.attempt,
            status: status.into(),
            executed,
            output,
            receipt_id,
            request: Some(RecordedRequest::of(request)),
        };
        self.store.create(
            OBSERVATION_COLLECTION,
            &id,
            serde_json::to_value(&record).expect("observation serializes"),
        )?;
        self.store.apply(
            "record_outcome",
            Operation {
                idempotency_key: Some(key.into()),
                result: Some(
                    if status == "success" {
                        "succeeded"
                    } else {
                        "failed"
                    }
                    .into(),
                ),
                evidence: Some(vec![record_ref(OBSERVATION_COLLECTION, &id)]),
                ..self.owner_op("record_outcome")
            },
        )?;
        self.observations.lock().unwrap().push(id);
        Ok(())
    }

    /// The key and binding the request is performed under, and the open intents performing it
    /// settles, or why it must not be performed. `None` for a capability that cannot carry a key.
    fn claim(&self, request: &ExecutionRequest) -> Result<Option<Claim>, String> {
        let idempotency = self
            .idempotency
            .get(&request.intent)
            .copied()
            .unwrap_or_default();
        if idempotency == Idempotency::NotCarried {
            return Ok(None);
        }
        let binding = binding(request);
        let reissuable = self.reissuable.lock().unwrap();
        // The same request as an open intent's is that logical action performed again, under its
        // key, wherever the trajectory now numbers it.
        let again: Vec<&Reissuable> = reissuable
            .iter()
            .filter(|r| r.tag.capability == request.intent && r.tag.binding == binding)
            .collect();
        let key = match again.first() {
            Some(r) => r.tag.key.clone(),
            None => action_key(&self.task_id, &request.id.0),
        };
        let mut keys = self.keys.lock().unwrap();
        if let Some(bound) = keys.get(&key) {
            if *bound != binding {
                return Err(format!(
                    "idempotency key {key} already names another request of task {}; reusing it for this one is refused, and nothing was executed",
                    self.task_id
                ));
            }
        }
        keys.insert(key.clone(), binding.clone());
        Ok(Some(Claim {
            tag: ActionTag {
                capability: request.intent.clone(),
                key,
                binding,
                idempotency,
            },
            settles: again.iter().map(|r| r.intent_key.clone()).collect(),
        }))
    }

    /// Settles the open intents a performed-again action answered for, citing its observation.
    fn settle_again(&self, key: &str, settles: &[String]) -> Result<(), StateError> {
        let observation = format!("{}~{key}", self.task_id);
        for intent in settles {
            self.store.apply(
                "reconcile",
                Operation {
                    idempotency_key: Some(intent.clone()),
                    // Its effect may have been applied by it or by the execution that performed it
                    // again; the capability guarantees one of them at most, and says which outcome
                    // the key has.
                    conclusion: Some("executed".into()),
                    evidence: Some(vec![record_ref(OBSERVATION_COLLECTION, &observation)]),
                    ..self.owner_op("reconcile")
                },
            )?;
            self.reissuable
                .lock()
                .unwrap()
                .retain(|r| r.intent_key != *intent);
        }
        Ok(())
    }
}

impl Inner {
    /// Performs one planned request again and settles the intents it stands for. `Err` is why the
    /// request was not performed, or its intents not settled.
    async fn reobserve_one(
        &self,
        durable: &DurableExecutor,
        backend: &dyn CapabilityBackend,
        plan: &Reobservable,
        n: usize,
    ) -> Result<Reobserved, String> {
        let execution_id = format!("{}-reobs-{}-{n}", self.task_id, self.attempt);
        let request = plan.request.to_request(&execution_id)?;
        let (descriptor, execution) = validate_request(backend, &request)
            .await
            .map_err(|e| format!("the recorded request is no longer valid: {e}"))?;
        // The declaration the validated descriptor makes now, not the one the intent was recorded
        // under: a capability that no longer declares an observation is not performed again.
        if descriptor.effect != Effect::Observation {
            return Err(format!(
                "{} declares its effect as {}, not an observation",
                plan.capability,
                descriptor.effect.name()
            ));
        }
        // What was validated is what the intent recorded: the capability and typed inputs it is
        // bound to, unchanged.
        if execution.intent != plan.capability || binding(&execution) != plan.binding {
            return Err("the validated request is not the one the intent recorded".into());
        }
        let result = durable
            .perform(execution.clone(), &plan.intents)
            .await
            .map_err(|e| e.to_string())?;
        let key = intent_key(self.attempt, &execution_id);
        let observation = format!("{}~{key}", self.task_id);
        for intent in &plan.intents {
            // The capability answered, so the observation exists and the intent is accounted for
            // by it. Contract v1 can only say `executed` or `not_executed`, and neither is known
            // of the lost execution; `not_executed` is the reading that does not credit it with an
            // effect, which an observation does not have. The new intent's `reobserves` and this
            // intent's citation of a different execution's observation are what say what happened.
            self.store
                .apply(
                    "reconcile",
                    Operation {
                        idempotency_key: Some(intent.clone()),
                        conclusion: Some("not_executed".into()),
                        evidence: Some(vec![record_ref(OBSERVATION_COLLECTION, &observation)]),
                        ..self.owner_op("reconcile")
                    },
                )
                .map_err(|e| {
                    format!("the fresh observation was recorded, but intent {intent} was not settled: {e}")
                })?;
        }
        // Its record is already among this attempt's observations (the executor wrote it), which
        // is what backs a completion; only the trajectory needs it.
        if matches!(self.restoration, Restoration::Restored { .. }) {
            self.restored.lock().unwrap().push(RestoredObservation {
                request,
                result: ExecutionResult {
                    id: result.id.clone(),
                    status: result.status,
                    output: result.output.clone(),
                    receipt_id: result.receipt_id.clone(),
                    evidence: None,
                },
                reobserved: true,
            });
        }
        let reobserved = Reobserved {
            intents: plan.intents.clone(),
            capability: plan.capability.clone(),
            execution_id,
            intent_key: key,
            status: match result.status {
                ExecutionStatus::Success => "success",
                ExecutionStatus::Failure => "failure",
                ExecutionStatus::Cancelled => "cancelled",
            }
            .into(),
        };
        self.reobserved.lock().unwrap().push(reobserved.clone());
        Ok(reobserved)
    }

    /// Refuses the attempt: `intents` stay unresolved and the lease this attempt took is given
    /// back, exactly as a refusal at [`begin`].
    fn refuse(&self, intents: Vec<String>, why: String) -> StateError {
        self.fail(StateError::InvalidInput(format!(
            "an observation could not be performed again: {why}"
        )));
        match release(
            &self.store,
            &self.task_id,
            &self.actor,
            self.fence,
            &self.lease_id,
        ) {
            Ok(()) => StateError::OutcomeUncertain {
                task_id: self.task_id.clone(),
                intents,
            },
            Err(e) => StateError::UncertainLeaseHeld {
                task_id: self.task_id.clone(),
                intents,
                detail: e.to_string(),
            },
        }
    }
}

/// How one execution is performed under a logical key.
struct Claim {
    tag: ActionTag,
    /// Open intents of earlier attempts this execution performs again.
    settles: Vec<String>,
}

/// One attempt of a task this process owns in FeltDB.
#[derive(Clone)]
pub struct DurableWork(Arc<Inner>);

/// What [`DurableWork::finish`] committed.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    /// The task status FeltDB now holds.
    pub status: String,
    /// Chip reported completion, but no observation backed it or the host did not accept it, so
    /// the task was checkpointed `blocked` instead. The work must not be reported as completed.
    pub completion_refused: bool,
    pub history: History,
}

impl DurableWork {
    pub fn task_id(&self) -> &str {
        &self.0.task_id
    }

    /// The checkpoint version this attempt started at; unique per attempt.
    pub fn attempt(&self) -> u64 {
        self.0.attempt
    }

    pub fn fence(&self) -> u64 {
        self.0.fence
    }

    /// The status the task had when a previous owner's attempt was taken over, if it was.
    pub fn resumed_from(&self) -> Option<&str> {
        self.0.resumed_from.as_deref()
    }

    /// What earlier attempts executed and observed, in order: the trajectory this attempt starts
    /// from. Empty for a new task and when [`restoration`](Self::restoration) says it was not
    /// restored.
    pub fn restored(&self) -> Vec<RestoredObservation> {
        self.0.restored.lock().unwrap().clone()
    }

    pub fn restoration(&self) -> &Restoration {
        &self.0.restoration
    }

    /// Open intents of earlier attempts that this attempt may still settle by performing the same
    /// logical action again. The task cannot complete while any remains.
    pub fn reissuable(&self) -> Vec<Reissuable> {
        self.0.reissuable.lock().unwrap().clone()
    }

    /// Intents of earlier attempts an operator settled. One concluded to have taken effect is not
    /// performed again by this attempt: its executor refuses the same request.
    pub fn settled(&self) -> Vec<Settlement> {
        self.0.settled.clone()
    }

    /// The settlements as chip-core is told of them (`WorkSpec::with_settled`).
    pub fn settled_actions(&self) -> Vec<SettledAction> {
        self.0
            .settled
            .iter()
            .filter_map(Settlement::to_settled_action)
            .collect()
    }

    /// Open intents of earlier attempts whose capability declares itself an observation, which
    /// [`reobserve`](Self::reobserve) performs again. The task cannot complete while any remains.
    pub fn reobservable(&self) -> Vec<Reobservable> {
        self.0.reobservable.lock().unwrap().clone()
    }

    /// What [`reobserve`](Self::reobserve) has performed again so far.
    pub fn reobserved(&self) -> Vec<Reobserved> {
        self.0.reobserved.lock().unwrap().clone()
    }

    /// Performs again, to obtain a fresh observation, each request an earlier attempt started
    /// without recording its outcome, whose capability declares itself an [`Effect::Observation`].
    /// Call it once, after [`begin`] and before the work starts; with nothing to do it does nothing.
    ///
    /// Each request is the one the intent recorded, reconstructed exactly (its inputs must hash to
    /// the binding the intent recorded), validated again against what `backend` declares now (the
    /// capability must still be declared, available, declare an observation, and accept the
    /// inputs), and executed through the same durable executor as any execution of this attempt: an
    /// intent first, then the outcome. The new intent says it re-observes the old one. The old
    /// intent is then reconciled citing the new observation; the lost response is not recovered
    /// and nothing says it was.
    ///
    /// If any step fails, nothing is repeated: the lease this attempt took is given back and the
    /// intents stay unresolved, as [`StateError::OutcomeUncertain`]. It does not retry.
    pub async fn reobserve(
        &self,
        backend: Arc<dyn CapabilityBackend>,
        control: Option<Arc<dyn WorkControl>>,
    ) -> Result<Vec<Reobserved>, StateError> {
        let plans = std::mem::take(&mut *self.0.reobservable.lock().unwrap());
        let durable = DurableExecutor {
            inner: Arc::new(Gated {
                inner: backend.clone(),
                control: control.clone(),
            }),
            work: self.0.clone(),
        };
        let mut done = Vec::new();
        for (n, plan) in plans.iter().enumerate() {
            let stop = || control.as_ref().and_then(|c| c.stop_requested());
            // The same check the work loop makes before every execution: a request to stop, or an
            // expired deadline, ends recovery before another intent or execution starts.
            let result = match stop() {
                Some(cause) => Err(format!(
                    "{} was requested before it was performed again",
                    cause.name()
                )),
                None => self.0.reobserve_one(&durable, &*backend, plan, n).await,
            };
            match result {
                Ok(reobserved) => done.push(reobserved),
                Err(why) => {
                    let intents = plans[n..].iter().flat_map(|p| p.intents.clone()).collect();
                    return Err(self.0.refuse(intents, why));
                }
            }
        }
        Ok(done)
    }

    /// The response an interrupted earlier attempt received and never acted on, for
    /// [`chip_core::WorkSpec::with_restored_response`]; `None` when [`begin`] found none to offer.
    pub fn restored_response(&self) -> Option<RestoredResponse> {
        self.0.restored_response.clone()
    }

    /// What [`begin`] decided about an earlier attempt's recorded model response.
    pub fn response_restoration(&self) -> &ResponseRestoration {
        &self.0.response_restoration
    }

    /// A model call the resumed attempt had in flight when it stopped, if it had one.
    pub fn interrupted_model_call(&self) -> Option<&InterruptedModelCall> {
        self.0.interrupted_model_call.as_ref()
    }

    /// Records one model response, as the loop received it (call it from the run's
    /// [`WorkControl::on_model_response`]), before it is interpreted or acted on. A record that
    /// cannot be written is a persistence failure, like an event that cannot be: the attempt is
    /// not persisted as complete.
    pub fn record_model_response(
        &self,
        work_id: &str,
        turn: usize,
        model: &str,
        response: &chip_inference::ModelResponse,
        observed: &[ExecutionId],
    ) {
        let work = &self.0;
        let turn = turn as u64 + 1;
        let record = ModelResponseRecord {
            schema: MODEL_RESPONSE_SCHEMA.into(),
            task_id: work.task_id.clone(),
            work_id: work_id.into(),
            attempt: work.attempt,
            fence: work.fence,
            turn,
            model: model.into(),
            response_id: response.id.clone(),
            output: response.output.clone(),
            output_sha256: sha256_hex(&response.output),
            usage: RecordedUsage::of(&response.usage),
            observed: observed.iter().map(|e| e.0.clone()).collect(),
            intents_before: work.intents.lock().unwrap().clone(),
            recorded_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
        };
        if let Err(e) = work.store.create(
            MODEL_RESPONSE_COLLECTION,
            &model_response_id(&work.task_id, work.attempt, turn),
            serde_json::to_value(&record).expect("a model response serializes"),
        ) {
            work.fail(StateError::InvalidInput(format!(
                "the model response of turn {turn} was not recorded: {e}"
            )));
        }
    }

    /// Records one work event, as the loop records it (call it from the run's
    /// [`WorkControl::on_event`]). A record that cannot be written is a persistence failure: it
    /// stops recording (the trajectory ends there rather than having a gap) and, like every
    /// persistence failure, keeps the attempt from being persisted as complete.
    pub fn record_event(&self, work_id: &str, event: &WorkEvent) {
        let work = &self.0;
        if work.events_stopped.load(Ordering::SeqCst) {
            return;
        }
        let index = work.next_event.fetch_add(1, Ordering::SeqCst);
        let observation = |execution_id: &str| {
            format!(
                "{}~{}",
                work.task_id,
                intent_key(work.attempt, execution_id)
            )
        };
        let (provenance, event) = describe_event(event, &observation);
        let record = WorkEventRecord {
            schema: WORK_EVENT_SCHEMA.into(),
            task_id: work.task_id.clone(),
            work_id: work_id.into(),
            attempt: work.attempt,
            fence: work.fence,
            index,
            recorded_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
            provenance,
            event,
        };
        let written = work.store.create(
            WORK_EVENT_COLLECTION,
            &event_id(&work.task_id, work.attempt, index),
            serde_json::to_value(&record).expect("a work event serializes"),
        );
        if let Err(e) = written {
            work.events_stopped.store(true, Ordering::SeqCst);
            work.fail(StateError::InvalidInput(format!(
                "work event {index} was not recorded, and none after it will be: {e}"
            )));
        }
    }

    /// How many work events this attempt has recorded.
    pub fn events_recorded(&self) -> u64 {
        let next = self.0.next_event.load(Ordering::SeqCst);
        if self.0.events_stopped.load(Ordering::SeqCst) {
            next.saturating_sub(1)
        } else {
            next
        }
    }

    /// Persistence failures this attempt has met so far. Each one also failed its execution.
    pub fn failures(&self) -> Vec<StateError> {
        self.0.failures.lock().unwrap().clone()
    }

    /// `inner`, with every execution bracketed by a durable intent and outcome.
    pub fn executor(&self, inner: Arc<dyn Executor>) -> Arc<dyn Executor> {
        Arc::new(DurableExecutor {
            inner,
            work: self.0.clone(),
        })
    }

    /// Recalls, for this attempt, verified episodes of other works that bear on it (see
    /// [`AgentStore::recall`]), and keeps them for [`recalled`](Self::recalled). Call it once,
    /// after [`begin`] and before the work starts. It never fails the work: when the episodes
    /// cannot be read, the answer says so, nothing is recalled, and the work runs as without it.
    pub fn recall(&self, context: &EpisodeContext) -> Recall {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let recall = self
            .0
            .store
            .recall(
                &self.0.task_id,
                context,
                now_ms,
                chip_core::MAX_RECALLED_EPISODES,
            )
            .unwrap_or_else(|e| Recall::failed(e.to_string()));
        *self.0.recalled.lock().unwrap() = recall.recalled.clone();
        recall
    }

    /// What [`recall`](Self::recall) recalled, as the work is told of it
    /// ([`chip_core::WorkSpec::with_recalled`]).
    pub fn recalled(&self) -> Vec<chip_core::RecalledEpisode> {
        self.0
            .recalled
            .lock()
            .unwrap()
            .iter()
            .map(Recalled::to_recalled_episode)
            .collect()
    }

    /// Records this attempt's episode from what [`finish`](Self::finish) committed. It changes
    /// nothing about the task: the outcome was decided and checkpointed before, and an episode that
    /// cannot be written is reported, never a reason to change it.
    pub fn record_episode(
        &self,
        finished: &Finished,
        context: &EpisodeContext,
    ) -> Result<(EpisodeRecord, EpisodeWrite), StateError> {
        let recalled: Vec<String> = self
            .0
            .recalled
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.episode.id.clone())
            .collect();
        let episode =
            self.0
                .store
                .episode_of(&finished.history, self.0.attempt, context, &recalled)?;
        let written = self.0.store.record_episode(&episode)?;
        Ok((episode, written))
    }

    /// Records the attempt's result and its terminal checkpoint.
    ///
    /// Chip's outcome maps to a contract status: completed to `completed` (only if the host
    /// `accepted` it, with observed evidence, and with no persistence failure), cancelled to
    /// `cancelled`, everything else to `blocked`, which a later [`begin`] can resume. `accepted` is
    /// the host's own verdict on a completion (stack-agent: its verification); a completion the host
    /// did not accept is never persisted as complete.
    pub fn finish(&self, report: &WorkReport, accepted: bool) -> Result<Finished, StateError> {
        self.finish_with(report, accepted, None)
    }

    /// [`finish`](Self::finish), recording the host's verdict in the result (`chip.work_result.v2`).
    /// The completion is persisted on `verification.accepted`.
    pub fn finish_verified(
        &self,
        report: &WorkReport,
        verification: Verification,
    ) -> Result<Finished, StateError> {
        self.finish_with(report, verification.accepted, Some(verification))
    }

    fn finish_with(
        &self,
        report: &WorkReport,
        accepted: bool,
        verification: Option<Verification>,
    ) -> Result<Finished, StateError> {
        let work = &self.0;
        let failures: Vec<String> = self.failures().iter().map(|e| e.to_string()).collect();
        // Every observation the trajectory holds, restored or made by this attempt, is a record
        // that can back a completion.
        let observations: Vec<String> = work
            .restored_ids
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .chain(work.observations.lock().unwrap().iter().cloned())
            .collect();
        let (mut status, detail) = match &report.outcome {
            WorkOutcome::Completed { summary } => ("completed", summary.clone()),
            WorkOutcome::Cancelled { reason, .. } => ("cancelled", reason.clone()),
            WorkOutcome::Escalated { reason } | WorkOutcome::Blocked { reason } => {
                ("blocked", reason.clone())
            }
            WorkOutcome::LimitReached { limit } => {
                ("blocked", format!("the {} limit was reached", limit.name()))
            }
            WorkOutcome::Failed { reason, .. } => ("blocked", reason.clone()),
        };
        if !failures.is_empty() && status != "cancelled" {
            status = "blocked";
        }
        let (_, task) = work
            .store
            .task(&work.task_id)?
            .ok_or(StateError::InvalidHistory {
                task_id: work.task_id.clone(),
                detail: "the task is missing at finish".into(),
            })?;
        let unbacked = status == "completed" && observations.is_empty();
        // An intent still open (one left for the same logical action to be performed again, which
        // was not) means an action may or may not have run: the task is not complete.
        let open = if status == "completed" {
            task.unresolved_intents
        } else {
            0
        };
        let completion_refused = status == "completed" && (unbacked || open > 0 || !accepted);
        if completion_refused {
            status = "blocked";
        }
        let (executions, decisions) = trajectory(report, work.attempt);
        let result_id = format!("{}~a{}", work.task_id, work.attempt);
        let record = WorkResultRecord {
            schema: if verification.is_some() {
                RESULT_SCHEMA_V2
            } else {
                RESULT_SCHEMA
            }
            .into(),
            task_id: work.task_id.clone(),
            work_id: report.work_id.to_string(),
            attempt: work.attempt,
            fence: work.fence,
            outcome: report.outcome.terminal_state().name().into(),
            detail: detail.clone(),
            requested_status: status.into(),
            turns: report.summary.turns,
            executions,
            decisions,
            persistence_failures: failures,
            verification,
        };
        work.store.create(
            RESULT_COLLECTION,
            &result_id,
            serde_json::to_value(&record).expect("work result serializes"),
        )?;
        let mut evidence = vec![record_ref(RESULT_COLLECTION, &result_id)];
        let skip = observations.len().saturating_sub(MAX_EVIDENCE - 1);
        evidence.extend(
            observations[skip..]
                .iter()
                .map(|id| record_ref(OBSERVATION_COLLECTION, id)),
        );
        let summary = if open > 0 {
            format!(
                "Chip reported completion, but {open} intent(s) are still unresolved, so it is not complete: {detail}"
            )
        } else if unbacked {
            format!(
                "Chip reported completion with no observed execution behind it; not accepted as complete: {detail}"
            )
        } else if completion_refused {
            format!("Chip reported completion but the host did not verify it: {detail}")
        } else {
            format!("{}: {detail}", report.outcome.terminal_state().name())
        };
        work.store.apply(
            "checkpoint",
            Operation {
                expected_version: Some(task.checkpoint_version),
                summary: Some(clip(&summary, MAX_TEXT_BYTES)),
                evidence: Some(evidence),
                status: Some(status.into()),
                ..work.owner_op("checkpoint")
            },
        )?;
        let history = work
            .store
            .history(&work.task_id)?
            .ok_or(StateError::InvalidHistory {
                task_id: work.task_id.clone(),
                detail: "the task is missing after finish".into(),
            })?;
        Ok(Finished {
            status: history.task.status.clone(),
            completion_refused,
            history,
        })
    }
}

/// Each execution with the decision turn that asked for it, and every decision.
fn trajectory(report: &WorkReport, attempt: u64) -> (Vec<ExecutionEntry>, Vec<DecisionEntry>) {
    let mut turn = 0;
    let mut executions = Vec::new();
    for event in &report.events {
        match event {
            WorkEvent::DecisionStarted { turn: t, .. } => turn = *t,
            WorkEvent::Execution(ExecutionEvent::ExecutionRequested { id, .. }) => {
                executions.push(ExecutionEntry {
                    turn,
                    execution_id: id.0.clone(),
                    intent_key: intent_key(attempt, &id.0),
                });
            }
            _ => {}
        }
    }
    let decisions = report
        .decisions
        .iter()
        .map(|d| DecisionEntry {
            turn: d.turn,
            source: match d.source {
                DecisionSource::Local => "local",
                DecisionSource::Model => "model",
            }
            .into(),
            decision: d.decision.label(),
        })
        .collect();
    (executions, decisions)
}

struct DurableExecutor {
    inner: Arc<dyn Executor>,
    work: Arc<Inner>,
}

#[async_trait::async_trait]
impl Executor for DurableExecutor {
    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.perform(request, &[]).await
    }
}

impl DurableExecutor {
    /// One execution bracketed by a durable intent and outcome. `reobserves` is empty for a
    /// request the work asked for. Otherwise it names the open intents of earlier attempts this
    /// execution performs again for a fresh observation, and says so in its own intent: it takes
    /// no idempotency key, since an observation has no effect for a key to protect.
    async fn perform(
        &self,
        mut request: ExecutionRequest,
        reobserves: &[String],
    ) -> Result<ExecutionResult, ExecutionError> {
        let key = intent_key(self.work.attempt, &request.id.0);
        // An operator concluded this action took effect, and nothing observed how: performing it
        // again could repeat the effect, so it is not performed.
        let bound = binding(&request);
        if let Some(settled) = self.work.settled.iter().find(|s| {
            s.executed
                && s.capability == request.intent
                && s.binding.as_ref().is_none_or(|b| *b == bound)
        }) {
            // Unavailable, not invalid: the request is well formed, but this work may not perform
            // it, so the work is blocked (pending a person) rather than failed.
            return Err(ExecutionError::ExecutorUnavailable(format!(
                "{} was settled by {} as having taken effect (intent {}); its outcome was never observed, so it is not performed again, and nothing was executed",
                request.intent,
                settled
                    .decided_by
                    .as_deref()
                    .unwrap_or("an unrecorded operator"),
                settled.intent_key
            )));
        }
        let claim = if reobserves.is_empty() {
            self.work
                .claim(&request)
                .map_err(ExecutionError::InvalidRequest)?
        } else {
            None
        };
        // The binding and inputs, so a later operator settlement names exactly this request.
        let action = match &claim {
            Some(claim) => {
                request.idempotency_key = Some(claim.tag.key.clone());
                intent_action(claim.tag.to_action(), &request)
            }
            None if reobserves.is_empty() => {
                intent_action(format!("{} binding={bound}", request.intent), &request)
            }
            None => intent_action(
                format!(
                    "{} binding={bound} reobserves={}",
                    request.intent,
                    reobserves.join(",")
                ),
                &request,
            ),
        };
        if let Err(e) = self.work.before(&key, &action) {
            let message = format!(
                "durable agent state did not record the intent, so nothing was executed: {e}"
            );
            self.work.fail(e);
            return Err(ExecutionError::ExecutionFailed(message));
        }
        let result = self.inner.execute(request.clone()).await;
        // The executor started the action and cannot say how it ended. Recording an outcome would
        // claim one; the intent stays unresolved, exactly as if the process had stopped here, so a
        // later `begin` refuses it as uncertain until an operator settles it.
        if let Err(ExecutionError::OutcomeUnknown(_)) = &result {
            return result;
        }
        if let Err(e) = self.work.after(&key, &request, &result) {
            let message = format!(
                "the action ran but its outcome could not be made durable; FeltDB keeps intent {key} unresolved: {e}"
            );
            self.work.fail(e);
            return Err(ExecutionError::ExecutionFailed(message));
        }
        // The capability answered for the key: the intents it performed again are settled. An
        // error is no answer, and they stay open.
        if let (Some(claim), Ok(_)) = (&claim, &result) {
            if let Err(e) = self.work.settle_again(&key, &claim.settles) {
                let message = format!(
                    "the action was performed again and observed, but the intents it settles could not be: {e}"
                );
                self.work.fail(e);
                return Err(ExecutionError::ExecutionFailed(message));
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    #[derive(Default)]
    struct Clock(AtomicU64);

    impl AuthorityClock for Clock {
        fn now_ms(&self) -> u64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64
                + self.0.load(Ordering::SeqCst)
        }
    }

    fn ready(begun: Result<Begun, StateError>) -> DurableWork {
        match begun {
            Ok(Begun::Ready(work)) => work,
            _ => panic!("expected a runnable task"),
        }
    }

    #[test]
    fn an_earlier_owner_cannot_release_the_lease_a_later_owner_holds() {
        let dir = std::env::temp_dir().join(format!("chip-release-{}", unique("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let clock = Arc::new(Clock::default());
        let store = AgentStore::with_clock(
            FeltDb::open(dir.join("agent-state.log")).unwrap(),
            clock.clone(),
        );
        let config = |actor: &str| DurableConfig {
            actor: actor.into(),
            lease_ms: 60_000,
            ..DurableConfig::default()
        };
        let first = ready(begin(&store, "task", "objective", &config("chip-a")));
        clock.0.fetch_add(60_001, Ordering::SeqCst);
        let second = ready(begin(&store, "task", "objective", &config("chip-b")));
        let held = store.task("task").unwrap().unwrap().1;
        assert_eq!((held.owner.as_str(), held.fence), ("chip-b", 2));

        // The first owner, at its own fence and lease, cannot give back what it no longer holds.
        let (a, b) = (&first.0, &second.0);
        assert_eq!(
            release(&store, "task", &a.actor, a.fence, &a.lease_id),
            Err(StateError::Refused {
                operation: "release",
                code: ErrorCode::FenceStale,
            })
        );
        // Nor by naming the current fence without the current owner and lease.
        assert!(release(&store, "task", &a.actor, b.fence, &a.lease_id).is_err());
        assert!(release(&store, "task", &b.actor, b.fence, &a.lease_id).is_err());
        assert_eq!(store.task("task").unwrap().unwrap().1, held);
        // The owner that holds it can.
        release(&store, "task", &b.actor, b.fence, &b.lease_id).unwrap();
        let released = store.task("task").unwrap().unwrap().1;
        assert!(released.lease.unwrap().expires_at <= clock.now_ms() + RELEASED_LEASE_MS);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
