//! Durable evaluation of committed state changes into workloads.
//!
//! This module is intentionally only the causal bridge from an already committed
//! operation to `WorkloadStore::create`. It is not a registry, queue, worker, or
//! execution runtime.

use crate::authorization::{SignedGrant, Subject};
use crate::trigger_contract;
use crate::workload::{
    CreateWorkload, WorkloadError, WorkloadRetryPolicy, WorkloadStore, WorkloadTimeouts,
};
use crate::{FeltDb, FlowError, Operation};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

pub const STATE_TRIGGER_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StateTriggerChangeKind {
    Insert,
    Update,
    Delete,
}

impl StateTriggerChangeKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTriggerPredicate {
    pub field: String,
    pub equals: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTrigger {
    pub trigger_id: String,
    pub source_collection: String,
    pub change_kind: StateTriggerChangeKind,
    #[serde(default)]
    pub predicate: Option<StateTriggerPredicate>,
    pub target_capability: String,
    pub tenant_id: String,
    pub application_id: String,
    #[serde(default = "default_environment")]
    pub environment: String,
    #[serde(default = "default_revision")]
    pub definition_revision: String,
    pub requested_by: Subject,
    pub authorization_snapshot: SignedGrant,
    #[serde(default)]
    pub capability_snapshot: BTreeSet<String>,
    #[serde(default)]
    pub connection_scopes: BTreeSet<String>,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub retry_policy: WorkloadRetryPolicy,
    #[serde(default)]
    pub timeouts: WorkloadTimeouts,
    #[serde(default)]
    pub not_before: Option<i64>,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn default_environment() -> String {
    "production".into()
}

fn default_revision() -> String {
    "trigger".into()
}

fn enabled() -> bool {
    true
}

impl StateTrigger {
    /// Whether this trigger matches a committed operation.
    ///
    /// The decision itself lives in [`crate::trigger_contract`], the
    /// runtime-independent deterministic contract. This method is the
    /// projection from FeltDB's durable types onto that contract's input; it
    /// holds no matching logic of its own, so the native and WASM contract
    /// implementations decide production matching identically by construction.
    pub fn matches(&self, operation: &Operation) -> bool {
        trigger_contract::evaluate_trigger(&trigger_contract::TriggerMatchInput::for_operation(
            self, operation,
        ))
        .matched()
    }

    /// The contract input this trigger and operation produce, for provenance.
    pub fn contract_input(&self, operation: &Operation) -> trigger_contract::TriggerMatchInput {
        trigger_contract::TriggerMatchInput::for_operation(self, operation)
    }
}

impl StateTriggerPredicate {
    pub fn matches(&self, value: Option<&Value>) -> bool {
        let Some(value) = value else { return false };
        get_path(value, &self.field) == Some(&self.equals)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggeredWorkload {
    pub trigger_id: String,
    pub source_change_id: String,
    pub workload_id: String,
    pub duplicate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateTriggerStore {
    #[serde(skip)]
    path: PathBuf,
    #[serde(default = "contract_version")]
    pub contract_version: u32,
    #[serde(default)]
    pub triggers: BTreeMap<String, StateTrigger>,
    #[serde(default)]
    pub cursors: HashMap<String, u64>,
}

fn contract_version() -> u32 {
    STATE_TRIGGER_CONTRACT_VERSION
}

impl StateTriggerStore {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, StateTriggerError> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Ok(Self {
                path,
                contract_version: STATE_TRIGGER_CONTRACT_VERSION,
                triggers: BTreeMap::new(),
                cursors: HashMap::new(),
            });
        }
        let mut value: Self = serde_json::from_slice(&fs::read(&path).map_err(io)?).map_err(io)?;
        value.path = path;
        if value.contract_version == 0 {
            value.contract_version = STATE_TRIGGER_CONTRACT_VERSION;
        }
        Ok(value)
    }

    pub fn define(&mut self, trigger: StateTrigger) -> Result<(), StateTriggerError> {
        validate_trigger(&trigger)?;
        self.triggers.insert(trigger.trigger_id.clone(), trigger);
        self.persist()
    }

    pub fn list(&self) -> Vec<StateTrigger> {
        self.triggers.values().cloned().collect()
    }

    pub fn evaluate(
        &mut self,
        db: &FeltDb,
        workloads: &mut WorkloadStore,
        now: i64,
    ) -> Result<Vec<TriggeredWorkload>, StateTriggerError> {
        let mut produced = Vec::new();
        let mut operations = db
            .operations_since(&self.cursors)
            .map_err(StateTriggerError::from)?;
        operations.sort_by(|left, right| {
            left.instance_id
                .cmp(&right.instance_id)
                .then_with(|| left.sequence.cmp(&right.sequence))
        });

        for operation in operations {
            let matches = self
                .triggers
                .values()
                .filter(|trigger| trigger.matches(&operation))
                .cloned()
                .collect::<Vec<_>>();
            for trigger in matches {
                let before = workloads.workloads.len();
                let workload = workloads
                    .create(create_workload(&trigger, &operation, now))
                    .map_err(StateTriggerError::from)?;
                produced.push(TriggeredWorkload {
                    trigger_id: trigger.trigger_id,
                    source_change_id: source_change_id(&operation),
                    workload_id: workload.workload_id,
                    duplicate: workloads.workloads.len() == before,
                });
            }
            self.cursors
                .entry(operation.instance_id.clone())
                .and_modify(|cursor| *cursor = (*cursor).max(operation.sequence))
                .or_insert(operation.sequence);
            self.persist()?;
        }
        Ok(produced)
    }

    pub fn cursor_versions(&self) -> HashMap<String, u64> {
        self.cursors.clone()
    }
}

fn validate_trigger(trigger: &StateTrigger) -> Result<(), StateTriggerError> {
    for (field, value) in [
        ("trigger_id", &trigger.trigger_id),
        ("source_collection", &trigger.source_collection),
        ("target_capability", &trigger.target_capability),
        ("tenant_id", &trigger.tenant_id),
        ("application_id", &trigger.application_id),
    ] {
        if value.trim().is_empty() {
            return Err(StateTriggerError::new(
                "TRIGGER_INVALID",
                format!("{field} is required"),
            ));
        }
    }
    if trigger.authorization_snapshot.grant.subject != trigger.requested_by
        || trigger.authorization_snapshot.grant.tenant_id != trigger.tenant_id
        || trigger.authorization_snapshot.grant.application_id != trigger.application_id
    {
        return Err(StateTriggerError::new(
            "TRIGGER_UNAUTHORIZED",
            "authorization snapshot does not bind the trigger requester",
        ));
    }
    Ok(())
}

fn create_workload(trigger: &StateTrigger, operation: &Operation, now: i64) -> CreateWorkload {
    let source_change_id = source_change_id(operation);
    let kind = trigger.change_kind.as_str();
    let contract_input = trigger.contract_input(operation);
    let capability_snapshot = if trigger.capability_snapshot.is_empty() {
        [trigger.target_capability.clone()].into()
    } else {
        trigger.capability_snapshot.clone()
    };
    CreateWorkload {
        tenant_id: trigger.tenant_id.clone(),
        application_id: trigger.application_id.clone(),
        environment: trigger.environment.clone(),
        definition_id: trigger.target_capability.clone(),
        definition_revision: trigger.definition_revision.clone(),
        trigger: format!("state_change:{}", trigger.trigger_id),
        input: json!({
            "provenance": {
                "trigger_id": trigger.trigger_id,
                "source_change_id": source_change_id,
                "source_collection": operation.capability,
                "source_key": operation.key,
                "change_kind": kind,
                // Which deterministic contract decided this match, at which
                // version, on which runtime, over which exact input. This is a
                // description of the existing decision, not a second record of
                // it: the digest identifies the same committed change the
                // surrounding provenance already names.
                "contract": {
                    "id": trigger_contract::TRIGGER_MATCH_CONTRACT_ID,
                    "version": trigger_contract::TRIGGER_MATCH_CONTRACT_VERSION,
                    "runtime": trigger_contract::ContractRuntime::Native.as_str(),
                    "input_digest": trigger_contract::input_digest(&contract_input),
                },
            },
            "source": {
                "origin": operation.instance_id,
                "sequence": operation.sequence,
                "key": operation.key,
                "collection": operation.capability,
                "content_hash": operation.content_hash,
            },
            "record": operation.value,
        }),
        requested_by: trigger.requested_by.clone(),
        session_id: None,
        authorization_snapshot: trigger.authorization_snapshot.clone(),
        capability_snapshot,
        connection_scopes: trigger.connection_scopes.clone(),
        priority: trigger.priority,
        idempotency_key: format!("state-trigger:{}:{}", trigger.trigger_id, source_change_id),
        retry_policy: trigger.retry_policy.clone(),
        timeouts: trigger.timeouts.clone(),
        dependencies: vec![],
        not_before: trigger.not_before,
        created_at: now,
    }
}

fn source_change_id(operation: &Operation) -> String {
    format!("{}:{}", operation.instance_id, operation.sequence)
}

fn get_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(value);
    }
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

impl StateTriggerStore {
    fn persist(&self) -> Result<(), StateTriggerError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(io)?;
        }
        let temp = self.path.with_extension("tmp");
        fs::write(&temp, serde_json::to_vec_pretty(self).map_err(io)?).map_err(io)?;
        fs::rename(temp, &self.path).map_err(io)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StateTriggerError {
    pub code: String,
    pub message: String,
}

impl StateTriggerError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StateTriggerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for StateTriggerError {}

impl From<FlowError> for StateTriggerError {
    fn from(value: FlowError) -> Self {
        Self::new("TRIGGER_STATE_REPLAY_FAILED", value.to_string())
    }
}

impl From<WorkloadError> for StateTriggerError {
    fn from(value: WorkloadError) -> Self {
        Self::new(&value.code, value.message)
    }
}

fn io(error: impl std::fmt::Display) -> StateTriggerError {
    StateTriggerError::new("TRIGGER_STORAGE_FAILURE", error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorization::{
        Grant, GrantConstraints, GrantSigner, GrantStatus, ResourceScope, ResourceScopeKind,
    };
    use crate::workload::WorkloadState;
    use crate::AtomicMutation;
    use tempfile::tempdir;

    fn grant(subject: &Subject) -> SignedGrant {
        let signer = GrantSigner::new("k", vec![7; 32]).unwrap();
        signer
            .sign(Grant {
                contract_version: 1,
                id: "trigger-grant".into(),
                issuer: Subject::Human("owner".into()),
                subject: subject.clone(),
                tenant_id: "t".into(),
                application_id: "app".into(),
                capabilities: ["workload:execute".into()].into(),
                resources: vec![ResourceScope {
                    kind: ResourceScopeKind::Workload,
                    uri: "flow://app/workloads/*".into(),
                }],
                constraints: GrantConstraints::default(),
                issued_at: 1,
                expires_at: 9999,
                parent_grant: None,
                revision: 1,
                status: GrantStatus::Active,
            })
            .unwrap()
    }

    fn trigger(id: &str, collection: &str, kind: StateTriggerChangeKind) -> StateTrigger {
        let subject = Subject::Agent("trigger-agent".into());
        StateTrigger {
            trigger_id: id.into(),
            source_collection: collection.into(),
            change_kind: kind,
            predicate: None,
            target_capability: "identity".into(),
            tenant_id: "t".into(),
            application_id: "app".into(),
            environment: "production".into(),
            definition_revision: "r1".into(),
            requested_by: subject.clone(),
            authorization_snapshot: grant(&subject),
            capability_snapshot: ["identity".into()].into(),
            connection_scopes: BTreeSet::new(),
            priority: 0,
            retry_policy: Default::default(),
            timeouts: Default::default(),
            not_before: None,
            enabled: true,
        }
    }

    fn stores(name: &str) -> (tempfile::TempDir, FeltDb, StateTriggerStore, WorkloadStore) {
        let dir = tempdir().unwrap();
        let db = FeltDb::open(dir.path().join(format!("{name}.log"))).unwrap();
        let triggers =
            StateTriggerStore::load(dir.path().join(format!("{name}.triggers.json"))).unwrap();
        let workloads =
            WorkloadStore::load(dir.path().join(format!("{name}.workloads.json"))).unwrap();
        (dir, db, triggers, workloads)
    }

    #[test]
    fn matching_change_creates_workload_with_provenance() {
        let (_dir, db, mut triggers, mut workloads) = stores("matching");
        triggers
            .define(trigger(
                "on-order",
                "orders",
                StateTriggerChangeKind::Insert,
            ))
            .unwrap();
        db.insert("orders:1", json!({"status":"new"})).unwrap();

        let created = triggers.evaluate(&db, &mut workloads, 10).unwrap();

        assert_eq!(created.len(), 1);
        let workload = workloads.get(&created[0].workload_id).unwrap();
        assert_eq!(workload.state, WorkloadState::Ready);
        assert_eq!(workload.trigger, "state_change:on-order");
        assert_eq!(
            workload.idempotency_key,
            format!("state-trigger:on-order:{}", created[0].source_change_id)
        );
        assert_eq!(workload.input["provenance"]["trigger_id"], "on-order");
        assert_eq!(workload.input["provenance"]["source_collection"], "orders");
        assert_eq!(workload.input["record"]["status"], "new");
    }

    #[test]
    fn nonmatching_change_creates_nothing() {
        let (_dir, db, mut triggers, mut workloads) = stores("nonmatching");
        triggers
            .define(trigger(
                "on-order",
                "orders",
                StateTriggerChangeKind::Insert,
            ))
            .unwrap();
        db.insert("customers:1", json!({"name":"Ada"})).unwrap();

        assert!(triggers
            .evaluate(&db, &mut workloads, 10)
            .unwrap()
            .is_empty());
        assert!(workloads.list("t", "app").is_empty());
    }

    #[test]
    fn predicate_accepts_and_rejects_deterministically() {
        let (_dir, db, mut triggers, mut workloads) = stores("predicate");
        let mut accepted = trigger("paid", "orders", StateTriggerChangeKind::Insert);
        accepted.predicate = Some(StateTriggerPredicate {
            field: "status".into(),
            equals: json!("paid"),
        });
        triggers.define(accepted).unwrap();
        db.insert("orders:1", json!({"status":"draft"})).unwrap();
        db.insert("orders:2", json!({"status":"paid"})).unwrap();

        let created = triggers.evaluate(&db, &mut workloads, 10).unwrap();

        assert_eq!(created.len(), 1);
        assert_eq!(
            workloads.get(&created[0].workload_id).unwrap().input["record"]["status"],
            "paid"
        );
    }

    #[test]
    fn multiple_triggers_create_independent_workloads() {
        let (_dir, db, mut triggers, mut workloads) = stores("multiple");
        triggers
            .define(trigger("one", "orders", StateTriggerChangeKind::Insert))
            .unwrap();
        triggers
            .define(trigger("two", "orders", StateTriggerChangeKind::Insert))
            .unwrap();
        db.insert("orders:1", json!({"status":"paid"})).unwrap();

        let created = triggers.evaluate(&db, &mut workloads, 10).unwrap();

        assert_eq!(created.len(), 2);
        assert_ne!(created[0].workload_id, created[1].workload_id);
        assert_eq!(workloads.list("t", "app").len(), 2);
    }

    #[test]
    fn duplicate_and_replayed_change_create_exactly_one_workload() {
        let (dir, db, mut triggers, mut workloads) = stores("replay");
        let trigger_path = dir.path().join("replay.triggers.json");
        let workload_path = dir.path().join("replay.workloads.json");
        triggers
            .define(trigger("once", "orders", StateTriggerChangeKind::Insert))
            .unwrap();
        db.insert("orders:1", json!({"status":"paid"})).unwrap();

        let first = triggers.evaluate(&db, &mut workloads, 10).unwrap();
        let again = triggers.evaluate(&db, &mut workloads, 11).unwrap();
        assert_eq!(first.len(), 1);
        assert!(
            again.is_empty(),
            "durable cursor suppresses already evaluated changes"
        );

        let mut cursor_lost = StateTriggerStore::load(&trigger_path).unwrap();
        cursor_lost.cursors.clear();
        let mut reopened_workloads = WorkloadStore::load(&workload_path).unwrap();
        let replayed = cursor_lost
            .evaluate(&db, &mut reopened_workloads, 12)
            .unwrap();
        assert_eq!(replayed.len(), 1, "lost cursor replays the change");
        assert!(
            replayed[0].duplicate,
            "workload idempotency suppresses the duplicate"
        );
        assert_eq!(reopened_workloads.list("t", "app").len(), 1);
    }

    #[test]
    fn restart_after_observing_before_workload_creation_replays() {
        let (dir, db, mut triggers, mut workloads) = stores("restart-before-create");
        let trigger_path = dir.path().join("restart-before-create.triggers.json");
        let workload_path = dir.path().join("restart-before-create.workloads.json");
        triggers
            .define(trigger("recover", "orders", StateTriggerChangeKind::Insert))
            .unwrap();
        db.insert("orders:1", json!({"status":"paid"})).unwrap();
        drop(triggers);
        drop(workloads);

        let mut restarted_triggers = StateTriggerStore::load(trigger_path).unwrap();
        let mut restarted_workloads = WorkloadStore::load(workload_path).unwrap();
        let created = restarted_triggers
            .evaluate(&db, &mut restarted_workloads, 10)
            .unwrap();

        assert_eq!(created.len(), 1);
        assert_eq!(restarted_workloads.list("t", "app").len(), 1);
    }

    #[test]
    fn future_eligibility_and_unsupported_capability_are_preserved() {
        let (_dir, db, mut triggers, mut workloads) = stores("eligibility");
        let mut future = trigger("future", "orders", StateTriggerChangeKind::Insert);
        future.target_capability = "missing-capability".into();
        future.capability_snapshot = ["missing-capability".into()].into();
        future.not_before = Some(1_000);
        triggers.define(future).unwrap();
        db.insert("orders:1", json!({"status":"paid"})).unwrap();

        let created = triggers.evaluate(&db, &mut workloads, 10).unwrap();
        let workload = workloads.get(&created[0].workload_id).unwrap();
        let workload_id = workload.workload_id.clone();
        assert_eq!(workload.next_attempt_at, Some(1_000));
        let caps: BTreeSet<String> = ["identity".into()].into();
        assert_eq!(
            workloads
                .claim(&workload_id, "worker", &caps, 1_001, 30_000)
                .unwrap_err()
                .code,
            "WORKLOAD_CAPABILITY_DENIED"
        );
    }

    #[test]
    fn cancellation_and_retry_use_normal_workload_semantics() {
        let (_dir, db, mut triggers, mut workloads) = stores("lifecycle");
        let mut lifecycle = trigger("life", "orders", StateTriggerChangeKind::Insert);
        lifecycle.retry_policy.max_attempts = 1;
        triggers.define(lifecycle).unwrap();
        db.insert("orders:1", json!({"status":"paid"})).unwrap();
        db.insert("orders:2", json!({"status":"paid"})).unwrap();
        let created = triggers.evaluate(&db, &mut workloads, 10).unwrap();
        assert_eq!(created.len(), 2);

        let cancelled = created[0].workload_id.clone();
        assert_eq!(
            workloads.cancel(&cancelled, "operator", 11).unwrap(),
            WorkloadState::Cancelled
        );
        assert_eq!(
            workloads
                .retry(&cancelled, "operator", 12)
                .unwrap_err()
                .code,
            "WORKLOAD_TRANSITION_DENIED"
        );

        let failed = created[1].workload_id.clone();
        let caps: BTreeSet<String> = ["identity".into()].into();
        let claim = workloads
            .claim(&failed, "worker", &caps, 12, 30_000)
            .unwrap();
        workloads
            .start(&failed, &claim.claim_id, "worker", claim.fencing_token, 13)
            .unwrap();
        assert_eq!(
            workloads
                .fail(
                    &failed,
                    &claim.claim_id,
                    "worker",
                    claim.fencing_token,
                    "worker_failure",
                    "crashed",
                    14
                )
                .unwrap(),
            WorkloadState::DeadLetter
        );
        workloads.retry(&failed, "operator", 15).unwrap();
        assert_eq!(workloads.get(&failed).unwrap().state, WorkloadState::Ready);
    }

    #[test]
    fn atomic_transaction_changes_are_replayed_into_triggers() {
        let (_dir, db, mut triggers, mut workloads) = stores("transaction");
        triggers
            .define(trigger("tx", "orders", StateTriggerChangeKind::Insert))
            .unwrap();
        db.apply_atomic_transaction(
            "tx-1",
            None,
            &[],
            &[AtomicMutation {
                capability: "orders".into(),
                key: "orders:1".into(),
                value: Some(json!({"status":"paid"})),
            }],
            None,
        )
        .unwrap();

        assert_eq!(triggers.evaluate(&db, &mut workloads, 10).unwrap().len(), 1);
    }
}
