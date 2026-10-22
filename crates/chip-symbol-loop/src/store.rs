//! The loop's checkpoints, in FeltDB (the store `chip-agent-state` uses), through the same guarded
//! atomic transactions. One transition is one commit: the run's state document, fenced on its
//! version, and a create-only event record for that transition's sequence number. A transition
//! that was already committed is recognized by its event record and is never applied twice.
//!
//! What FeltDB cannot make atomic with a commit is a file write or an inference call; the machine
//! records an intent before each and reconciles it on resume (see `machine.rs`).

use std::path::Path;

use feltdb::{AtomicMutation, FeltDb, RecordPrecondition};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::machine::RunState;

pub const RUN_COLLECTION: &str = "chip_symbol_loop_run";
pub const EVENT_COLLECTION: &str = "chip_symbol_loop_event";
pub const EVENT_SCHEMA: &str = "chip.symbol-loop.event.v1";

/// One recorded transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub schema: String,
    pub run_id: String,
    pub seq: u64,
    pub from: String,
    pub to: String,
    pub detail: Value,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    Storage(String),
    /// The run's state is not the version this transition was computed from, and the transition
    /// is not one already committed.
    Conflict(String),
    Corrupt(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Storage(m) => write!(f, "checkpoint store: {m}"),
            StoreError::Conflict(m) => write!(f, "checkpoint conflict: {m}"),
            StoreError::Corrupt(m) => write!(f, "checkpoint record does not parse: {m}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Commit {
    Committed,
    /// The identical transition was already committed; nothing was written.
    Replayed,
}

/// A namespace of runs in one FeltDB database. Run and namespace ids are plain identifiers.
pub struct LoopStore {
    db: FeltDb,
    namespace: String,
}

fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl LoopStore {
    pub fn open(path: impl AsRef<Path>, namespace: &str) -> Result<Self, StoreError> {
        if !identifier(namespace) {
            return Err(StoreError::Storage(format!(
                "namespace {namespace:?} is not an identifier"
            )));
        }
        let db = FeltDb::open(path).map_err(|e| StoreError::Storage(e.to_string()))?;
        Ok(Self {
            db,
            namespace: namespace.to_string(),
        })
    }

    fn run_key(&self, run_id: &str) -> String {
        format!("{RUN_COLLECTION}:{}~{run_id}", self.namespace)
    }

    fn event_key(&self, run_id: &str, seq: u64) -> String {
        format!("{EVENT_COLLECTION}:{}~{run_id}~{seq:08}", self.namespace)
    }

    /// The run's state and its FeltDB version, if it exists.
    pub fn load(&self, run_id: &str) -> Result<Option<(u64, RunState)>, StoreError> {
        let Some(mut value) = self
            .db
            .get_value(&self.run_key(run_id))
            .map_err(|e| StoreError::Storage(e.to_string()))?
        else {
            return Ok(None);
        };
        let version = value.get("__version").and_then(Value::as_u64).unwrap_or(1);
        if let Some(o) = value.as_object_mut() {
            o.remove("__version");
        }
        let state: RunState =
            serde_json::from_value(value).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        Ok(Some((version, state)))
    }

    /// Every recorded transition of the run, in order.
    pub fn events(&self, run_id: &str) -> Result<Vec<Event>, StoreError> {
        let prefix = format!("{}~{run_id}~", self.namespace);
        let mut rows: Vec<(String, Value)> = self
            .db
            .list_collection(EVENT_COLLECTION)
            .map_err(|e| StoreError::Storage(e.to_string()))?
            .into_iter()
            .filter(|r| {
                r.key
                    .strip_prefix(&format!("{EVENT_COLLECTION}:"))
                    .unwrap_or(&r.key)
                    .starts_with(&prefix)
            })
            .map(|r| (r.key, r.value))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows.into_iter()
            .map(|(_, mut v)| {
                if let Some(o) = v.as_object_mut() {
                    o.remove("__version");
                }
                serde_json::from_value(v).map_err(|e| StoreError::Corrupt(e.to_string()))
            })
            .collect()
    }

    /// Commits `state` with `event`: creates the run when `expected` is `None`, otherwise updates
    /// it only from version `expected`. Both records are written together or not at all.
    pub fn commit(
        &self,
        expected: Option<u64>,
        state: &RunState,
        event: &Event,
    ) -> Result<Commit, StoreError> {
        let run_key = self.run_key(&state.run_id);
        let event_key = self.event_key(&state.run_id, event.seq);
        let state_value =
            serde_json::to_value(state).map_err(|e| StoreError::Storage(e.to_string()))?;
        let event_value =
            serde_json::to_value(event).map_err(|e| StoreError::Storage(e.to_string()))?;
        // The transaction id names the transition; the payload hash binds it to its exact content.
        // FeltDB answers a repeat of the same id and content as a duplicate (nothing written) and
        // refuses the same id with different content.
        let payload = crate::symbols::sha256(
            serde_json::to_string(&serde_json::json!([&state_value, &event_value]))
                .map_err(|e| StoreError::Storage(e.to_string()))?
                .as_bytes(),
        );
        let result = self.db.apply_atomic_transaction_guarded(
            &format!(
                "chip-symbol-loop:{}:{}:{}",
                self.namespace, state.run_id, event.seq
            ),
            Some(&payload),
            None,
            &[],
            &[
                RecordPrecondition {
                    capability: RUN_COLLECTION.to_string(),
                    key: run_key.clone(),
                    require_absent: expected.is_none(),
                    expected_version: expected,
                    ..Default::default()
                },
                RecordPrecondition {
                    capability: EVENT_COLLECTION.to_string(),
                    key: event_key.clone(),
                    require_absent: true,
                    ..Default::default()
                },
            ],
            &[
                AtomicMutation {
                    capability: RUN_COLLECTION.to_string(),
                    key: run_key,
                    value: Some(state_value),
                },
                AtomicMutation {
                    capability: EVENT_COLLECTION.to_string(),
                    key: event_key.clone(),
                    value: Some(event_value),
                },
            ],
            None,
        );
        match result {
            Ok(commit) if commit.duplicate => Ok(Commit::Replayed),
            Ok(_) => Ok(Commit::Committed),
            Err(e @ feltdb::FlowError::TransactionIdPayloadMismatch { .. }) => {
                Err(StoreError::Conflict(e.to_string()))
            }
            Err(e) => {
                // Already committed, identically: a replay, and nothing more is done.
                if let Ok(Some(mut existing)) = self.db.get_value(&event_key) {
                    if let Some(o) = existing.as_object_mut() {
                        o.remove("__version");
                    }
                    if serde_json::from_value::<Event>(existing).ok().as_ref() == Some(event) {
                        return Ok(Commit::Replayed);
                    }
                }
                Err(StoreError::Conflict(e.to_string()))
            }
        }
    }
}
