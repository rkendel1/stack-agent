//! Durable admission: a work has a durable identity from the moment it is accepted, not from the
//! moment it starts.
//!
//! FeltDB's contract already has a status for a task that exists and has not started: `ready`
//! (`create_task` makes one; `ready -> running` and `ready -> cancelled` are its transitions).
//! Admission uses exactly that, so there is one task, one identity and one lifecycle. Nothing here
//! is a queue and nothing here decides a transition: FeltDB's contract does.
//!
//! What a `ready` task cannot hold is the work's *kind* and a goal longer than the contract's
//! 1,024-byte objective. They are in `chip.Admission`, a write-once record under the task's own id,
//! beside Chip's other records (`chip.Observation`, `chip.WorkEvent`, ...). It is written first:
//! an admission record with no task was never acknowledged to anyone and is ignored; a `ready` task
//! with no admission record cannot be made by [`admit`], so finding one is an inconsistency to
//! report, not to repair.
//!
//! The admitting call holds the task's lease for the contract's shortest term, 1 ms, so the next
//! owner can take the task over with `acquire` (what [`crate::begin`] does for any existing task)
//! as soon as it has expired. Starting is therefore the same `ready -> running` checkpoint a
//! `--task-id` resume makes, and two starters cannot both take the task: FeltDB grants one lease.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AgentStore, DurableConfig, ErrorCode, MAX_IDENTIFIER_BYTES, Operation, StateError, TaskValue,
    is_identifier, unique,
};

pub const ADMISSION_COLLECTION: &str = "chip.Admission";
pub const ADMISSION_SCHEMA: &str = "chip.admission.v1";
/// The longest goal an admission record holds. The service's own limit is lower.
pub const MAX_ADMISSION_GOAL_BYTES: usize = 16 * 1024;
/// The contract's shortest lease. An admitted task is free to take over once it has passed.
pub const ADMISSION_LEASE_MS: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionRecord {
    pub schema: String,
    pub task_id: String,
    /// `change`, `verify` or `inspect`: the client's choice, never the model's.
    pub kind: String,
    /// The goal exactly as accepted (the task's objective is this, cut to the contract's limit).
    pub goal: String,
    pub admitted_at_ms: u64,
}

/// What the database holds about a `ready` task's admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    Valid(AdmissionRecord),
    /// No admission record: [`admit`] writes it before it creates the task, so this is damage.
    Missing,
    /// A record that does not parse, names another task, or has another schema.
    Unreadable(String),
}

/// A task waiting to start, and what is known of its admission.
#[derive(Debug, Clone)]
pub struct Waiting {
    pub task: TaskValue,
    pub admission: Admission,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Records, durably, that `goal` was accepted as work `task_id` of `kind`. When this returns
/// `Ok`, the task exists in FeltDB as `ready` and survives the process; nothing is started.
///
/// `objective` is the task's objective, which a later [`crate::begin`] must name identically.
pub fn admit(
    store: &AgentStore,
    task_id: &str,
    kind: &str,
    goal: &str,
    objective: &str,
    config: &DurableConfig,
) -> Result<(), StateError> {
    if !is_identifier(task_id) {
        return Err(StateError::InvalidInput(format!(
            "task id {task_id:?} must be 1 to {MAX_IDENTIFIER_BYTES} bytes of A-Z a-z 0-9 _ . : @ -"
        )));
    }
    if goal.trim().is_empty() || goal.len() > MAX_ADMISSION_GOAL_BYTES {
        return Err(StateError::InvalidInput(format!(
            "the goal must be 1 to {MAX_ADMISSION_GOAL_BYTES} bytes to be admitted"
        )));
    }
    if !["change", "verify", "inspect"].contains(&kind) {
        return Err(StateError::InvalidInput(format!(
            "{kind:?} is not a kind of work"
        )));
    }
    let record = AdmissionRecord {
        schema: ADMISSION_SCHEMA.into(),
        task_id: task_id.into(),
        kind: kind.into(),
        goal: goal.into(),
        admitted_at_ms: now_ms(),
    };
    store.create(
        ADMISSION_COLLECTION,
        task_id,
        serde_json::to_value(&record).expect("an admission record serializes"),
    )?;
    store.apply(
        "create_task",
        Operation {
            op: "create_task".into(),
            task_id: Some(task_id.into()),
            actor: Some(config.actor.clone()),
            objective: Some(objective.into()),
            lease_id: Some(unique("lease")),
            lease_ms: Some(ADMISSION_LEASE_MS),
            ..Operation::default()
        },
    )
}

/// The admission record of `task_id`, if there is one.
pub fn admission(store: &AgentStore, task_id: &str) -> Admission {
    let value: Value = match store.read(ADMISSION_COLLECTION, task_id) {
        Ok(Some((_, value))) => value,
        Ok(None) => return Admission::Missing,
        Err(why) => return Admission::Unreadable(why.to_string()),
    };
    match serde_json::from_value::<AdmissionRecord>(value) {
        Ok(r) if r.schema == ADMISSION_SCHEMA && r.task_id == task_id => Admission::Valid(r),
        Ok(r) => Admission::Unreadable(format!(
            "the record says schema {:?} for task {:?}",
            r.schema, r.task_id
        )),
        Err(why) => Admission::Unreadable(why.to_string()),
    }
}

/// Every task still waiting to start, oldest admission first (then by id, so the order is the
/// same on every call). Reads; changes nothing.
pub fn waiting(store: &AgentStore) -> Result<Vec<Waiting>, StateError> {
    let mut found: Vec<Waiting> = store
        .tasks()?
        .into_iter()
        .filter(|t| t.status == "ready")
        .map(|task| {
            let admission = admission(store, &task.task_id);
            Waiting { task, admission }
        })
        .collect();
    found.sort_by(|a, b| {
        let at = |w: &Waiting| match &w.admission {
            Admission::Valid(r) => r.admitted_at_ms,
            _ => w.task.created_at_ms,
        };
        (at(a), &a.task.task_id).cmp(&(at(b), &b.task.task_id))
    });
    Ok(found)
}

/// Cancels a task that has not started: `ready -> cancelled`, durably, through the contract's own
/// `acquire` and `checkpoint`. A task in any other status is refused and left as it is. Returns
/// once FeltDB has committed the cancellation; the caller reports it only then.
pub fn cancel_admitted(
    store: &AgentStore,
    task_id: &str,
    config: &DurableConfig,
) -> Result<(), StateError> {
    let (_, task) = store
        .task(task_id)?
        .ok_or_else(|| StateError::InvalidHistory {
            task_id: task_id.into(),
            detail: "the task to cancel does not exist".into(),
        })?;
    if task.status != "ready" {
        return Err(StateError::InvalidInput(format!(
            "task {task_id} is {}, not waiting to start; nothing was changed",
            task.status
        )));
    }
    let lease_id = unique("lease");
    store
        .apply(
            "acquire",
            Operation {
                op: "acquire".into(),
                task_id: Some(task_id.into()),
                actor: Some(config.actor.clone()),
                lease_id: Some(lease_id.clone()),
                lease_ms: Some(config.lease_ms),
                ..Operation::default()
            },
        )
        .map_err(|e| match e {
            StateError::Refused {
                code: ErrorCode::LeaseHeld,
                ..
            } => StateError::LeaseHeld {
                task_id: task_id.into(),
                owner: task.owner.clone(),
            },
            other => other,
        })?;
    let (_, task) = store
        .task(task_id)?
        .ok_or_else(|| StateError::InvalidHistory {
            task_id: task_id.into(),
            detail: "the task is missing after it was taken".into(),
        })?;
    store.apply(
        "checkpoint",
        Operation {
            op: "checkpoint".into(),
            task_id: Some(task_id.into()),
            actor: Some(config.actor.clone()),
            fence: Some(task.fence),
            lease_id: Some(lease_id),
            expected_version: Some(task.checkpoint_version),
            summary: Some("Cancelled before it started.".into()),
            evidence: Some(Vec::new()),
            status: Some("cancelled".into()),
            ..Operation::default()
        },
    )
}
