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
//! # Scope
//!
//! Sessions and conversational turns keep their existing owner, the Zig-compatible session store
//! (`chip-session`, `chip-turns`); persisting them here would make a second store. Contract v1 has
//! no record for them. Turn identity is preserved where work meets it: each execution's decision
//! turn is kept in the work's result record.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chip_core::{
    DecisionSource, ExecutionError, ExecutionEvent, ExecutionRequest, ExecutionResult,
    ExecutionStatus, Executor, WorkEvent, WorkOutcome, WorkReport,
};
pub use feltdb;
use feltdb::agent_state::{AgentState, AgentStateError, AuthorityClock};
use feltdb::agent_transition_contract::{
    AGENT_TRANSITION_CONTRACT_ID, AGENT_TRANSITION_CONTRACT_VERSION, CHECKPOINT_COLLECTION,
    EvidenceRef, INTENT_COLLECTION, MAX_EVIDENCE, MAX_IDENTIFIER_BYTES, MAX_LEASE_MS,
    MAX_TEXT_BYTES, Operation, TASK_COLLECTION,
};
pub use feltdb::agent_transition_contract::{Citation, ErrorCode, IntentValue, TaskValue};
use feltdb::{AtomicMutation, FeltDb, FlowError, RecordPrecondition};
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
pub const OBSERVATION_SCHEMA: &str = "chip.observation.v1";
pub const RESULT_SCHEMA: &str = "chip.work_result.v1";

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
}

impl History {
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
                schema_of(OBSERVATION_COLLECTION, id, &value, OBSERVATION_SCHEMA)?;
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
            schema_of(RESULT_COLLECTION, &id, &value, RESULT_SCHEMA)?;
            let record: WorkResultRecord = serde_json::from_value(value)
                .map_err(|e| invalid(format!("work result {id} does not parse: {e}")))?;
            results.push(record);
        }
        results.sort_by_key(|r| r.attempt);
        Ok(Some(History {
            task,
            task_version,
            checkpoints,
            intents,
            results,
        }))
    }
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

/// Who runs the work and for how long a lease lasts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableConfig {
    pub actor: String,
    pub lease_ms: u64,
}

impl Default for DurableConfig {
    fn default() -> Self {
        Self {
            actor: "stack-agent".into(),
            lease_ms: DEFAULT_LEASE_MS,
        }
    }
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
    // Settle every intent a previous attempt left open, from evidence only.
    let mut uncertain = Vec::new();
    for id in store.ids(INTENT_COLLECTION, task_id)? {
        let Some((_, value)) = store.read(INTENT_COLLECTION, &id)? else {
            continue;
        };
        let intent: IntentValue =
            serde_json::from_value(value).map_err(|e| StateError::InvalidHistory {
                task_id: task_id.into(),
                detail: format!("intent {id} does not parse: {e}"),
            })?;
        if intent.state == "resolved" {
            continue;
        }
        match store.observation(&id)? {
            Some((_, observed)) => store.apply(
                "reconcile",
                Operation {
                    op: "reconcile".into(),
                    task_id: Some(task_id.into()),
                    actor: Some(actor.clone()),
                    fence: Some(fence),
                    lease_id: Some(lease_id.clone()),
                    idempotency_key: Some(intent.key.clone()),
                    conclusion: Some(
                        if observed.executed {
                            "executed"
                        } else {
                            "not_executed"
                        }
                        .into(),
                    ),
                    evidence: Some(vec![record_ref(OBSERVATION_COLLECTION, &id)]),
                    ..Operation::default()
                },
            )?,
            None => uncertain.push(intent.key.clone()),
        }
    }
    if !uncertain.is_empty() {
        return Err(StateError::OutcomeUncertain {
            task_id: task_id.into(),
            intents: uncertain,
        });
    }
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
        observations: Mutex::new(Vec::new()),
        failures: Mutex::new(Vec::new()),
    }))))
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
    /// Observation record ids written by this attempt, in order.
    observations: Mutex<Vec<String>>,
    failures: Mutex<Vec<StateError>>,
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
        )
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

    /// Records the attempt's result and its terminal checkpoint.
    ///
    /// Chip's outcome maps to a contract status: completed to `completed` (only if the host
    /// `accepted` it, with observed evidence, and with no persistence failure), cancelled to
    /// `cancelled`, everything else to `blocked`, which a later [`begin`] can resume. `accepted` is
    /// the host's own verdict on a completion (stack-agent: its verification); a completion the host
    /// did not accept is never persisted as complete.
    pub fn finish(&self, report: &WorkReport, accepted: bool) -> Result<Finished, StateError> {
        let work = &self.0;
        let failures: Vec<String> = self.failures().iter().map(|e| e.to_string()).collect();
        let observations = work.observations.lock().unwrap().clone();
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
        let unbacked = status == "completed" && observations.is_empty();
        let completion_refused = status == "completed" && (unbacked || !accepted);
        if completion_refused {
            status = "blocked";
        }
        let (executions, decisions) = trajectory(report, work.attempt);
        let result_id = format!("{}~a{}", work.task_id, work.attempt);
        let record = WorkResultRecord {
            schema: RESULT_SCHEMA.into(),
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
        };
        work.store.create(
            RESULT_COLLECTION,
            &result_id,
            serde_json::to_value(&record).expect("work result serializes"),
        )?;
        let (_, task) = work
            .store
            .task(&work.task_id)?
            .ok_or(StateError::InvalidHistory {
                task_id: work.task_id.clone(),
                detail: "the task is missing at finish".into(),
            })?;
        let mut evidence = vec![record_ref(RESULT_COLLECTION, &result_id)];
        let skip = observations.len().saturating_sub(MAX_EVIDENCE - 1);
        evidence.extend(
            observations[skip..]
                .iter()
                .map(|id| record_ref(OBSERVATION_COLLECTION, id)),
        );
        let summary = if unbacked {
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
        let key = intent_key(self.work.attempt, &request.id.0);
        if let Err(e) = self.work.before(&key, &request.intent) {
            let message = format!(
                "durable agent state did not record the intent, so nothing was executed: {e}"
            );
            self.work.fail(e);
            return Err(ExecutionError::ExecutionFailed(message));
        }
        let result = self.inner.execute(request.clone()).await;
        if let Err(e) = self.work.after(&key, &request, &result) {
            let message = format!(
                "the action ran but its outcome could not be made durable; FeltDB keeps intent {key} unresolved: {e}"
            );
            self.work.fail(e);
            return Err(ExecutionError::ExecutionFailed(message));
        }
        result
    }
}
