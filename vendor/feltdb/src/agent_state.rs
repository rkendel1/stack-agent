//! Durable agent state: the production implementation of
//! [`crate::agent_transition_contract`].
//!
//! This module contains **no lifecycle logic**. Every rule about who may do
//! what, when, and on what evidence lives in the contract; this module only
//! performs the five steps the contract's documentation requires of a caller:
//!
//! ```text
//!   1. read the records the operation names, with their versions
//!   2. take the authority's time
//!   3. ask the contract to decide
//!   4. commit its writes and guards in one guarded transaction
//!   5. report success only if that commit succeeded
//! ```
//!
//! Step 4 is what makes step 3 true. A decision was valid for the records read
//! in step 1; the commit carries a precondition for each of them, so if any
//! record changed in between, nothing is written. Lease expiry is judged twice,
//! both times on the authority's clock: by the contract at decision time, and by
//! the transaction (`expected_lease_id`) at commit time.
//!
//! # Retries
//!
//! A lost race is [`AgentStateError::Conflict`], reported, never retried here.
//! A caller that wants to try again calls [`AgentState::apply`] again, which
//! re-reads current state and asks the contract again. The same writes are
//! never resent.

use crate::agent_transition_contract::{
    evaluate, AgentTransitionInput, Decision, ErrorCode, Expect, Guard, Observation, Operation,
    Records, Stored, Write, AGENT_TRANSITION_CONTRACT_ID, AGENT_TRANSITION_CONTRACT_VERSION,
    HANDOFF_COLLECTION, INTENT_COLLECTION, TASK_COLLECTION,
};
use crate::{AtomicMutation, FeltDb, FlowError, PreconditionFailure, RecordPrecondition};
use serde_json::Value;
use std::sync::Arc;

/// The time source decisions are made against.
///
/// Production uses [`FeltDb::authority_time_ms`]. A test may inject its own to
/// drive the contract through a lifecycle without sleeping. The commit-time
/// lease predicate always uses the authority's real clock.
pub trait AuthorityClock: Send + Sync {
    fn now_ms(&self) -> u64;
}

struct DatabaseClock(FeltDb);

impl AuthorityClock for DatabaseClock {
    fn now_ms(&self) -> u64 {
        self.0.authority_time_ms()
    }
}

#[derive(Debug)]
pub enum AgentStateError {
    /// The contract refused the operation. Nothing was written.
    Refused(ErrorCode),
    /// The decision was valid when made, but a record it depended on changed
    /// before the commit. Nothing was written. Re-read and decide again.
    Conflict(PreconditionFailure),
    /// The store failed. Whether anything was written is the store's report.
    Storage(FlowError),
}

impl std::fmt::Display for AgentStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(code) => write!(f, "{}", code.as_str()),
            Self::Conflict(failure) => write!(f, "CONFLICT: {failure}"),
            Self::Storage(error) => write!(f, "STORAGE: {error}"),
        }
    }
}

impl std::error::Error for AgentStateError {}

/// A record the commit wrote, with the version it now has.
#[derive(Debug, Clone, PartialEq)]
pub struct Written {
    pub collection: String,
    pub id: String,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Applied {
    Committed(Vec<Written>),
    /// A recognized repeat: nothing changed, and nothing needed to.
    Noop,
}

/// A decision, with nothing yet committed.
#[derive(Debug, Clone)]
pub struct Prepared {
    decision: Decision,
}

impl Prepared {
    pub fn decision(&self) -> &Decision {
        &self.decision
    }
}

#[derive(Clone)]
pub struct AgentState {
    db: FeltDb,
    clock: Arc<dyn AuthorityClock>,
}

fn key(collection: &str, id: &str) -> String {
    format!("{collection}:{id}")
}

impl AgentState {
    pub fn new(db: FeltDb) -> Self {
        let clock = Arc::new(DatabaseClock(db.clone()));
        Self { db, clock }
    }

    pub fn with_clock(db: FeltDb, clock: Arc<dyn AuthorityClock>) -> Self {
        Self { db, clock }
    }

    /// One record and its authority version, or `None`.
    ///
    /// The version is the document's `__version`, defaulting to 1 where the
    /// record was never written under a version precondition: the same reading
    /// the transaction's precondition makes.
    pub fn read(
        &self,
        collection: &str,
        id: &str,
    ) -> Result<Option<(u64, Value)>, AgentStateError> {
        let Some(mut value) = self
            .db
            .get_value(&key(collection, id))
            .map_err(AgentStateError::Storage)?
        else {
            return Ok(None);
        };
        let version = value.get("__version").and_then(Value::as_u64).unwrap_or(1);
        if let Some(object) = value.as_object_mut() {
            object.remove("__version");
        }
        Ok(Some((version, value)))
    }

    fn stored<T: serde::de::DeserializeOwned>(
        &self,
        collection: &str,
        id: &str,
    ) -> Result<Option<Stored<T>>, AgentStateError> {
        match self.read(collection, id)? {
            None => Ok(None),
            Some((version, value)) => match serde_json::from_value(value) {
                Ok(value) => Ok(Some(Stored { value, version })),
                // Stored state that does not parse is not decided around.
                Err(_) => Err(AgentStateError::Refused(ErrorCode::ContractInputInvalid)),
            },
        }
    }

    /// Step 1: read exactly the records the operation names.
    fn gather(&self, op: &Operation) -> Result<Records, AgentStateError> {
        let mut records = Records::default();
        if let Some(handoff_id) = &op.handoff_id {
            records.handoff = self.stored(HANDOFF_COLLECTION, handoff_id)?;
        }
        let task_id = op
            .task_id
            .clone()
            .or_else(|| records.handoff.as_ref().map(|h| h.value.task_id.clone()));
        if let Some(task_id) = &task_id {
            records.task = self.stored(TASK_COLLECTION, task_id)?;
            if let Some(key_) = &op.idempotency_key {
                records.intent = self.stored(INTENT_COLLECTION, &format!("{task_id}~{key_}"))?;
            }
        }
        if matches!(
            op.op.as_str(),
            "checkpoint" | "record_outcome" | "reconcile"
        ) {
            let mut seen = Vec::new();
            for reference in op.evidence.iter().flatten().filter(|r| r.kind == "record") {
                let (collection, id) = (
                    reference.collection.clone().unwrap_or_default(),
                    reference.id.clone().unwrap_or_default(),
                );
                if let Some((version, _)) = self.read(&collection, &id)? {
                    // A record cited twice is observed once; the contract refuses
                    // the duplicate citation.
                    if !seen
                        .iter()
                        .any(|o: &Observation| o.collection == collection && o.id == id)
                    {
                        seen.push(Observation {
                            collection,
                            id,
                            version,
                        });
                    }
                }
            }
            records.evidence = Some(seen);
        }
        Ok(records)
    }

    /// Steps 1 to 3. Reads, takes the authority's time and decides. Writes
    /// nothing.
    pub fn prepare(&self, operation: &Operation) -> Result<Prepared, AgentStateError> {
        let records = self.gather(operation)?;
        let input = AgentTransitionInput {
            contract: AGENT_TRANSITION_CONTRACT_ID.to_string(),
            now_ms: self.clock.now_ms(),
            operation: operation.clone(),
            records,
            version: AGENT_TRANSITION_CONTRACT_VERSION,
        };
        evaluate(&input)
            .map(|decision| Prepared { decision })
            .map_err(AgentStateError::Refused)
    }

    /// Steps 4 and 5: one guarded transaction carrying every write and guard
    /// the contract returned, each with the precondition it attached.
    ///
    /// Nothing is retried. A fresh transaction id is used per call, so a failed
    /// precondition never consumes an id and a repeated call is a new decision's
    /// commit, not a replay.
    pub fn commit(&self, prepared: &Prepared) -> Result<Applied, AgentStateError> {
        let (writes, guards) = match &prepared.decision {
            Decision::Noop => return Ok(Applied::Noop),
            Decision::Commit { writes, guards } => (writes, guards),
        };
        let mut preconditions: Vec<RecordPrecondition> = Vec::new();
        let mut mutations: Vec<AtomicMutation> = Vec::new();
        for Write {
            collection,
            id,
            expect,
            value,
        } in writes
        {
            preconditions.push(precondition(collection, id, expect));
            mutations.push(AtomicMutation {
                capability: (*collection).to_string(),
                key: key(collection, id),
                value: Some(value.clone()),
            });
        }
        for Guard {
            collection,
            id,
            version,
            lease_id,
        } in guards
        {
            preconditions.push(precondition(
                collection,
                id,
                &Expect::Version {
                    version: *version,
                    lease_id: lease_id.clone(),
                },
            ));
        }
        let transaction_id = format!("agent-state-{:032x}", rand::random::<u128>());
        match self.db.apply_atomic_transaction_guarded(
            &transaction_id,
            None,
            None,
            &[],
            &preconditions,
            &mutations,
            None,
        ) {
            Ok(_) => Ok(Applied::Committed(
                writes
                    .iter()
                    .map(|w| Written {
                        collection: w.collection.to_string(),
                        id: w.id.clone(),
                        version: match &w.expect {
                            Expect::Absent => 1,
                            Expect::Version { version, .. } => version + 1,
                        },
                    })
                    .collect(),
            )),
            Err(FlowError::PreconditionFailed(failure)) => Err(AgentStateError::Conflict(*failure)),
            Err(other) => Err(AgentStateError::Storage(other)),
        }
    }

    /// Decide and commit.
    pub fn apply(&self, operation: &Operation) -> Result<Applied, AgentStateError> {
        self.commit(&self.prepare(operation)?)
    }
}

fn precondition(collection: &str, id: &str, expect: &Expect) -> RecordPrecondition {
    let base = RecordPrecondition {
        capability: collection.to_string(),
        key: key(collection, id),
        ..Default::default()
    };
    match expect {
        Expect::Absent => RecordPrecondition {
            require_absent: true,
            ..base
        },
        Expect::Version { version, lease_id } => RecordPrecondition {
            expected_version: Some(*version),
            expected_lease_id: lease_id.clone(),
            ..base
        },
    }
}
