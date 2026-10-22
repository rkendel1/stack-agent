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
//! **Who asked.** An admission says where it came from. A `service` admission was acknowledged to a
//! client (a `202`), so a restart is entitled to queue it again. A `cli` admission is the record a
//! `stack-agent work` invocation makes just before it starts its own work: nobody was told it was
//! queued, so a crash before it ran leaves a task that is interpretable (what it was asked, by whom)
//! but is only ever started by an explicit `--task-id` continuation, never by a service starting up.
//! A `ready` task with no record at all can only come from a caller below this layer (or damage);
//! it is reported, never guessed at.
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
/// How long after admitting a task a caller in the same process waits before it starts (or
/// cancels) it, so the 1 ms admission lease has passed. A caller that finds the task already
/// admitted long ago (a recovery, a continuation) does not wait.
pub const ADMISSION_HANDOFF: std::time::Duration = std::time::Duration::from_millis(5);

/// Who made an admission, which decides who may start it again after a crash.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdmissionOrigin {
    /// Acknowledged to a client; a restarted service queues it again. The default, and omitted from
    /// the record, so records written before this field existed read exactly as they were meant.
    #[default]
    Service,
    /// Made by `stack-agent work` for its own run; only an explicit continuation starts it again.
    Cli,
}

impl AdmissionOrigin {
    fn is_service(&self) -> bool {
        matches!(self, Self::Service)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Cli => "cli",
        }
    }
}

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
    #[serde(default, skip_serializing_if = "AdmissionOrigin::is_service")]
    pub origin: AdmissionOrigin,
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

/// Records, durably, that `goal` was accepted as work `task_id` of `kind`, by `origin`. When this
/// returns `Ok`, the task exists in FeltDB as `ready` and survives the process; nothing is started.
///
/// `objective` is the task's objective, which a later [`crate::begin`] must name identically.
///
/// A task that already exists is refused. A record left by a crash before its task was created is
/// not: the same request made again (same task, kind and goal) completes the admission, so a retried
/// `--task-id` run does not trip over its own earlier attempt.
pub fn admit(
    store: &AgentStore,
    task_id: &str,
    kind: &str,
    goal: &str,
    objective: &str,
    origin: AdmissionOrigin,
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
    if store.task(task_id)?.is_some() {
        return Err(StateError::InvalidInput(format!(
            "task {task_id} already exists; it is not admitted again"
        )));
    }
    let record = AdmissionRecord {
        schema: ADMISSION_SCHEMA.into(),
        task_id: task_id.into(),
        kind: kind.into(),
        goal: goal.into(),
        admitted_at_ms: now_ms(),
        origin,
    };
    if let Err(refused) = store.create(
        ADMISSION_COLLECTION,
        task_id,
        serde_json::to_value(&record).expect("an admission record serializes"),
    ) {
        // Only the same request, finishing what a crash interrupted.
        let same = matches!(
            admission(store, task_id),
            Admission::Valid(r) if r.kind == kind && r.goal == goal && r.origin == origin
        );
        if !same {
            return Err(refused);
        }
    }
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

/// What [`ensure_admitted`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ensured {
    /// The task did not exist; it was admitted and is `ready`. The caller starts it with
    /// [`crate::begin`] once the admission lease has passed.
    Admitted,
    /// The task already exists (a continuation): nothing was written, and `begin` resumes it.
    Existing,
}

/// For a caller that is about to start the work itself (`stack-agent work`): admits the task if it
/// does not exist yet, and leaves an existing one alone, so continuing a task never creates a
/// second identity. Either way the caller goes on to [`crate::begin`].
pub fn ensure_admitted(
    store: &AgentStore,
    task_id: &str,
    kind: &str,
    goal: &str,
    objective: &str,
    origin: AdmissionOrigin,
    config: &DurableConfig,
) -> Result<Ensured, StateError> {
    if store.task(task_id)?.is_some() {
        // A continuation is not a way to widen what the work was admitted to do: a task admitted
        // as an inspection is not continued as a change. (A task with no admission record, or one
        // that cannot be read, has no kind to hold the caller to and is left to `begin`.)
        if let Admission::Valid(record) = admission(store, task_id)
            && record.kind != kind
        {
            return Err(StateError::InvalidInput(format!(
                "task {task_id} was admitted as `{}` work and cannot be continued as `{kind}` work",
                record.kind
            )));
        }
        return Ok(Ensured::Existing);
    }
    admit(store, task_id, kind, goal, objective, origin, config).map(|()| Ensured::Admitted)
}

/// Whether another owner holds the task's lease right now. A `ready` task in that state may be in
/// the middle of being started; it is not a candidate to start.
pub fn lease_is_live(task: &TaskValue) -> bool {
    task.lease
        .as_ref()
        .is_some_and(|lease| now_ms() < lease.expires_at)
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
