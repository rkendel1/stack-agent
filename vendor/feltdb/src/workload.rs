//! Durable workload authority. Workers execute; this state machine decides.
use crate::{
    authorization::{SignedGrant, Subject},
    AtomicMutation, FeltDb, FlowError, RecordPrecondition,
};
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
pub const WORKLOAD_CONTRACT_VERSION: u32 = 1;
pub const DURABLE_WORK_COLLECTION: &str = "system_durable_work";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkloadState {
    Created,
    Ready,
    Claimed,
    Running,
    Checkpointed,
    Recovering,
    CancellationRequested,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    DeadLetter,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackoffPolicy {
    Fixed,
    Exponential,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadRetryPolicy {
    pub max_attempts: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub backoff: BackoffPolicy,
    #[serde(default)]
    pub retry_on: BTreeSet<String>,
}
impl Default for WorkloadRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_delay_ms: 1000,
            max_delay_ms: 300_000,
            backoff: BackoffPolicy::Exponential,
            retry_on: [
                "timeout".into(),
                "worker_failure".into(),
                "transient_connection".into(),
            ]
            .into(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkloadTimeouts {
    pub queue_ms: Option<u64>,
    pub claim_ms: Option<u64>,
    pub execution_ms: Option<u64>,
    pub heartbeat_ms: Option<u64>,
    pub overall_ms: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadDependency {
    pub workload_id: String,
    #[serde(default = "succeeded_state")]
    pub required_state: WorkloadState,
}
fn succeeded_state() -> WorkloadState {
    WorkloadState::Succeeded
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadClaim {
    pub claim_id: String,
    pub workload_id: String,
    pub worker_id: String,
    pub lease_id: String,
    pub fencing_token: u64,
    pub capability_snapshot: BTreeSet<String>,
    pub claimed_at: i64,
    pub expires_at: i64,
    pub last_heartbeat: i64,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AttemptStatus {
    Claimed,
    Running,
    Checkpointed,
    Succeeded,
    Failed,
    TimedOut,
    Cancelled,
    WorkerLost,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadAttempt {
    pub attempt_id: String,
    pub workload_id: String,
    pub worker_id: String,
    pub claim_id: String,
    pub fencing_token: u64,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub input_hash: String,
    pub result_hash: Option<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub resource_usage: BTreeMap<String, u64>,
    pub status: AttemptStatus,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadCheckpoint {
    pub checkpoint_id: String,
    pub workload_id: String,
    pub attempt_id: String,
    pub sequence: u64,
    pub state: Value,
    pub state_hash: String,
    pub created_at: i64,
    pub produced_by: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadResult {
    pub result_id: String,
    pub workload_id: String,
    pub attempt_id: String,
    pub status: String,
    pub output: Value,
    pub output_hash: String,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub provenance: BTreeMap<String, String>,
    pub produced_by: String,
    pub created_at: i64,
    pub schema_version: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadExecutionContext {
    pub workload_id: String,
    pub attempt_id: String,
    pub worker_id: String,
    pub capabilities: BTreeSet<String>,
    pub connection_scopes: BTreeSet<String>,
    pub secret_references: Vec<String>,
    pub expires_at: i64,
    pub fencing_token: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadEvent {
    pub sequence: u64,
    pub event_type: String,
    pub from: Option<WorkloadState>,
    pub to: WorkloadState,
    pub actor: String,
    pub at: i64,
    #[serde(default)]
    pub evidence: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workload {
    pub contract_version: u32,
    pub workload_id: String,
    pub tenant_id: String,
    pub application_id: String,
    pub environment: String,
    pub definition_id: String,
    pub definition_revision: String,
    pub trigger: String,
    pub input: Value,
    pub input_hash: String,
    pub requested_by: Subject,
    #[serde(default)]
    pub session_id: Option<String>,
    pub authorization_snapshot: SignedGrant,
    pub capability_snapshot: BTreeSet<String>,
    pub connection_scopes: BTreeSet<String>,
    pub priority: i32,
    pub idempotency_key: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub state: WorkloadState,
    pub execution_history: Vec<WorkloadEvent>,
    pub attempts: Vec<WorkloadAttempt>,
    pub checkpoints: Vec<WorkloadCheckpoint>,
    pub result: Option<WorkloadResult>,
    pub retry_policy: WorkloadRetryPolicy,
    pub timeouts: WorkloadTimeouts,
    pub attempt_count: u32,
    pub retry_count: u32,
    /// The instant, in unix seconds, before which this workload must not be
    /// claimed. `None` means "eligible as soon as it is `Ready`".
    ///
    /// This is the single temporal eligibility concept in the workload model.
    /// It is what retry backoff sets after a failure, what a timeout sets before
    /// re-arming, and what [`CreateWorkload::not_before`] sets at creation, so a
    /// workload that is not yet executable is always not-yet-executable for the
    /// same reason and is refused by the same check in [`WorkloadStore::claim`].
    pub next_attempt_at: Option<i64>,
    pub last_failure: Option<String>,
    pub current_claim: Option<WorkloadClaim>,
    pub fencing_token: u64,
    #[serde(default)]
    pub dependencies: Vec<WorkloadDependency>,
}

impl Workload {
    /// Has this workload's eligibility instant arrived?
    ///
    /// Eligibility is only half of claimability — the state must also be
    /// `Ready`, and [`WorkloadStore::claim`] checks both. This answers the
    /// temporal half alone so a scheduler can skip a workload that is merely
    /// early without attempting a claim it knows will be refused.
    pub fn is_eligible_at(&self, now: i64) -> bool {
        self.next_attempt_at.is_none_or(|instant| instant <= now)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateWorkload {
    pub tenant_id: String,
    pub application_id: String,
    pub environment: String,
    pub definition_id: String,
    pub definition_revision: String,
    pub trigger: String,
    pub input: Value,
    pub requested_by: Subject,
    #[serde(default)]
    pub session_id: Option<String>,
    pub authorization_snapshot: SignedGrant,
    pub capability_snapshot: BTreeSet<String>,
    #[serde(default)]
    pub connection_scopes: BTreeSet<String>,
    #[serde(default)]
    pub priority: i32,
    pub idempotency_key: String,
    #[serde(default)]
    pub retry_policy: WorkloadRetryPolicy,
    #[serde(default)]
    pub timeouts: WorkloadTimeouts,
    #[serde(default)]
    pub dependencies: Vec<WorkloadDependency>,
    /// The instant, in unix seconds, before which this workload must not be
    /// claimed. Omitted or `None` means it is claimable as soon as it is
    /// `Ready`, which is what every workload did before this field existed.
    ///
    /// A workload created with `not_before` is durable immediately and
    /// executable later: it reaches `Ready`, survives restart, and is refused to
    /// every worker until the instant arrives. It is *not* a schedule — there is
    /// no recurrence, no expression, and no component that re-arms it. Creating
    /// the next occurrence is the caller's business.
    ///
    /// A `not_before` already in the past is not an error. It simply describes a
    /// workload that is eligible now, which is the same thing as omitting it.
    #[serde(default)]
    pub not_before: Option<i64>,
    pub created_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkloadError {
    pub code: String,
    pub message: String,
}
impl WorkloadError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl std::fmt::Display for WorkloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for WorkloadError {}

#[derive(Clone)]
pub struct WorkloadStore {
    db: FeltDb,
    pub workloads: BTreeMap<String, Workload>,
    idempotency: BTreeMap<String, String>,
    result_ids: BTreeMap<String, String>,
    /// Rebuildable cache of FeltDB-owned document versions. This is never
    /// serialized and never advances independently of a canonical commit.
    document_versions: BTreeMap<String, u64>,
}

#[derive(Serialize, Deserialize)]
struct LegacyWorkloadStore {
    #[serde(default)]
    workloads: BTreeMap<String, Workload>,
    #[serde(default)]
    idempotency: BTreeMap<String, String>,
    #[serde(default)]
    result_ids: BTreeMap<String, String>,
    #[serde(default)]
    event_sequence: u64,
}

impl WorkloadStore {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, WorkloadError> {
        Self::from_db(FeltDb::open(path).map_err(io)?)
    }

    pub fn from_db(db: FeltDb) -> Result<Self, WorkloadError> {
        let mut workloads = BTreeMap::new();
        let mut document_versions = BTreeMap::new();
        for row in db.list_collection(DURABLE_WORK_COLLECTION).map_err(io)? {
            let version = row
                .value
                .get("__version")
                .and_then(Value::as_u64)
                // Canonical FeltDB CAS defines a pre-versioned document as
                // version 1. The next guarded mutation writes version 2, which
                // upgrades rows created before Work adopted RecordPrecondition.
                .unwrap_or(1);
            let workload: Workload = serde_json::from_value(row.value).map_err(io)?;
            document_versions.insert(workload.workload_id.clone(), version);
            workloads.insert(workload.workload_id.clone(), workload);
        }
        let mut idempotency = BTreeMap::new();
        let mut result_ids = BTreeMap::new();
        for workload in workloads.values() {
            idempotency.insert(
                format!(
                    "{}:{}:{}",
                    workload.tenant_id, workload.application_id, workload.idempotency_key
                ),
                workload.workload_id.clone(),
            );
            if let Some(result) = &workload.result {
                result_ids.insert(result.result_id.clone(), workload.workload_id.clone());
            }
        }
        Ok(Self {
            db,
            workloads,
            idempotency,
            result_ids,
            document_versions,
        })
    }

    pub fn migrate_legacy_file(db: FeltDb, path: &Path) -> Result<Self, WorkloadError> {
        let bytes = std::fs::read(path).map_err(io)?;
        let legacy: LegacyWorkloadStore = serde_json::from_slice(&bytes).map_err(io)?;
        let mut store = Self::from_db(db)?;
        for (id, workload) in legacy.workloads {
            if id != workload.workload_id {
                return Err(WorkloadError::new(
                    "WORKLOAD_MIGRATION_CONFLICT",
                    format!(
                        "legacy key {id} does not match workload identity {}",
                        workload.workload_id
                    ),
                ));
            }
            if let Some(existing) = store.workloads.get(&id) {
                if serde_json::to_value(existing).map_err(io)?
                    != serde_json::to_value(&workload).map_err(io)?
                {
                    return Err(WorkloadError::new(
                        "WORKLOAD_MIGRATION_CONFLICT",
                        format!("canonical workload {id} differs from legacy source"),
                    ));
                }
                continue;
            }
            store.workloads.insert(id.clone(), workload);
            store.persist_workload(&id, "workload_migration")?;
        }
        // The maps are verification evidence, not migrated authorities.
        let rebuilt = Self::from_db(store.db.clone())?;
        if legacy.idempotency != rebuilt.idempotency
            || legacy.result_ids != rebuilt.result_ids
            || legacy.event_sequence != derived_event_sequence(&rebuilt.workloads)
        {
            return Err(WorkloadError::new(
                "WORKLOAD_MIGRATION_CONFLICT",
                "legacy derived indexes do not match canonical work aggregates",
            ));
        }
        Ok(rebuilt)
    }

    fn persist_workload(&mut self, id: &str, action: &str) -> Result<(), WorkloadError> {
        let workload = self
            .workloads
            .get(id)
            .ok_or_else(|| WorkloadError::new("WORKLOAD_NOT_FOUND", "workload not found"))?;
        let key = work_key(workload);
        let expected_version = self.document_versions.get(id).copied();
        let transaction_id = runtime_id("work_tx");
        let aggregate_digest = hash(&serde_json::to_value(workload).map_err(io)?);
        let commit = self
            .db
            .apply_atomic_transaction_guarded(
                &transaction_id,
                None,
                None,
                &[],
                &[RecordPrecondition {
                    capability: DURABLE_WORK_COLLECTION.into(),
                    key: key.clone(),
                    require_absent: expected_version.is_none(),
                    expected_version,
                    expected_epoch: None,
                    expected_lease_id: None,
                }],
                &[AtomicMutation {
                    capability: DURABLE_WORK_COLLECTION.into(),
                    key,
                    value: Some(serde_json::to_value(workload).map_err(io)?),
                }],
                Some(serde_json::json!({
                    "kind": action,
                    "subject": workload.requested_by.id(),
                    "principal_id": workload.requested_by.id(),
                    "agent_id": (workload.requested_by.kind() == "agent").then(|| workload.requested_by.id()),
                    "session_id": workload.session_id,
                    "delegator_id": workload.authorization_snapshot.grant.issuer.id(),
                    "authorization_decision_id": workload.authorization_snapshot.grant.id,
                    "tenant": workload.tenant_id,
                    "application": workload.application_id,
                    "work_id": workload.workload_id,
                    "operation_id": transaction_id,
                    "attempt_id": workload.attempts.last().map(|attempt| attempt.attempt_id.as_str()),
                    "worker_id": workload.current_claim.as_ref().map(|claim| claim.worker_id.as_str()),
                    "result_id": workload.result.as_ref().map(|result| result.result_id.as_str()),
                    "source_kind": (action == "workload_migration").then_some("WorkloadStore"),
                    "source_digest": aggregate_digest,
                    "migration_outcome": (action == "workload_migration").then_some("imported"),
                })),
            )
            .map_err(workload_storage_error)?;
        let version = commit
            .rows
            .first()
            .and_then(|row| row.value.get("__version"))
            .and_then(Value::as_u64)
            .ok_or_else(|| io("canonical work commit has no document version"))?;
        self.document_versions.insert(id.into(), version);
        Ok(())
    }
    pub fn create(&mut self, input: CreateWorkload) -> Result<Workload, WorkloadError> {
        ensure_safe(&input.input)?;
        if input.idempotency_key.trim().is_empty() {
            return Err(WorkloadError::new(
                "WORKLOAD_INVALID",
                "idempotency key is required",
            ));
        }
        let idempotency = format!(
            "{}:{}:{}",
            input.tenant_id, input.application_id, input.idempotency_key
        );
        if let Some(id) = self.idempotency.get(&idempotency) {
            return Ok(self.workloads[id].clone());
        }
        if input.authorization_snapshot.grant.subject != input.requested_by
            || input.authorization_snapshot.grant.tenant_id != input.tenant_id
            || input.authorization_snapshot.grant.application_id != input.application_id
        {
            return Err(WorkloadError::new(
                "WORKLOAD_UNAUTHORIZED",
                "authorization snapshot does not bind the requester",
            ));
        }
        // The scoped idempotency identity is the record identity. Competing
        // admissions therefore contend on one canonical require-absent CAS
        // even when their payloads differ; no separate durable uniqueness
        // index is needed.
        let id = format!("workload_{}", &hex(&Sha256::digest(&idempotency))[..24]);
        let input_hash = hash(&input.input);
        let mut workload = Workload {
            contract_version: 1,
            workload_id: id.clone(),
            tenant_id: input.tenant_id,
            application_id: input.application_id,
            environment: input.environment,
            definition_id: input.definition_id,
            definition_revision: input.definition_revision,
            trigger: input.trigger,
            input: input.input,
            input_hash,
            requested_by: input.requested_by,
            session_id: input.session_id,
            authorization_snapshot: input.authorization_snapshot,
            capability_snapshot: input.capability_snapshot,
            connection_scopes: input.connection_scopes,
            priority: input.priority,
            idempotency_key: input.idempotency_key,
            created_at: input.created_at,
            updated_at: input.created_at,
            state: WorkloadState::Created,
            execution_history: vec![],
            attempts: vec![],
            checkpoints: vec![],
            result: None,
            retry_policy: input.retry_policy,
            timeouts: input.timeouts,
            attempt_count: 0,
            retry_count: 0,
            // Creation-time eligibility and retry backoff are the same thing, so
            // they are the same field. A workload with `not_before` is created
            // exactly as any other and is simply not yet claimable.
            next_attempt_at: input.not_before,
            last_failure: None,
            current_claim: None,
            fencing_token: 0,
            dependencies: input.dependencies,
        };
        // The durable history records the eligibility the workload was created
        // with, so "why was this Ready and unclaimed for an hour" is answerable
        // from the workload itself rather than from the caller's intent.
        let evidence = match workload.next_attempt_at {
            Some(instant) => [("not_before".into(), instant.to_string())].into(),
            None => BTreeMap::new(),
        };
        self.transition(
            &mut workload,
            WorkloadState::Ready,
            "rust",
            input.created_at,
            evidence,
        )?;
        self.idempotency.insert(idempotency.clone(), id.clone());
        self.workloads.insert(id.clone(), workload.clone());
        if let Err(error) = self.persist_workload(&id, "workload.created") {
            self.workloads.remove(&id);
            self.idempotency.remove(&idempotency);
            return Err(error);
        }
        Ok(workload)
    }
    pub fn get(&self, id: &str) -> Result<&Workload, WorkloadError> {
        self.workloads
            .get(id)
            .ok_or_else(|| WorkloadError::new("WORKLOAD_NOT_FOUND", "workload not found"))
    }
    pub fn list(&self, tenant: &str, application: &str) -> Vec<Workload> {
        let mut values = self
            .workloads
            .values()
            .filter(|v| v.tenant_id == tenant && v.application_id == application)
            .cloned()
            .collect::<Vec<_>>();
        values.sort_by_key(|v| (std::cmp::Reverse(v.priority), v.created_at));
        values
    }
    fn edit<R>(
        &mut self,
        id: &str,
        action: impl FnOnce(&mut Self, &mut Workload) -> Result<R, WorkloadError>,
    ) -> Result<R, WorkloadError> {
        let workload = self
            .workloads
            .get(id)
            .cloned()
            .ok_or_else(|| WorkloadError::new("WORKLOAD_NOT_FOUND", "workload not found"))?;
        let mut edited = workload.clone();
        let result = action(self, &mut edited)?;
        self.workloads.insert(id.into(), edited);
        if let Err(error) = self.persist_workload(id, "workload.mutated") {
            self.workloads.insert(id.into(), workload);
            return Err(error);
        }
        Ok(result)
    }
    fn transition(
        &mut self,
        w: &mut Workload,
        to: WorkloadState,
        actor: &str,
        at: i64,
        evidence: BTreeMap<String, String>,
    ) -> Result<(), WorkloadError> {
        if !legal(w.state, to) {
            return Err(WorkloadError::new(
                "WORKLOAD_TRANSITION_DENIED",
                format!("{:?} cannot transition to {:?}", w.state, to),
            ));
        }
        let from = w.state;
        w.state = to;
        w.updated_at = at;
        let sequence = w
            .execution_history
            .last()
            .map_or(1, |event| event.sequence + 1);
        w.execution_history.push(WorkloadEvent {
            sequence,
            event_type: event_name(to).into(),
            from: Some(from),
            to,
            actor: actor.into(),
            at,
            evidence,
        });
        Ok(())
    }
    pub fn claim(
        &mut self,
        id: &str,
        worker: &str,
        capabilities: &BTreeSet<String>,
        now: i64,
        lease_ms: u64,
    ) -> Result<WorkloadClaim, WorkloadError> {
        self.edit(id, |store, w| {
            if w.state != WorkloadState::Ready {
                return Err(WorkloadError::new(
                    "WORKLOAD_NOT_READY",
                    "workload is not eligible",
                ));
            }
            // Separated from the state check because the two mean opposite
            // things to a worker: a workload in the wrong state may never become
            // claimable, while one that is merely early certainly will, at an
            // instant the error names. Collapsing them into one code left a
            // polling worker unable to tell "come back" from "give up".
            if !w.is_eligible_at(now) {
                return Err(WorkloadError::new(
                    "WORKLOAD_NOT_YET_ELIGIBLE",
                    format!(
                        "workload is not claimable until {}",
                        w.next_attempt_at.unwrap_or(now)
                    ),
                ));
            }
            if !w.capability_snapshot.is_subset(capabilities) {
                return Err(WorkloadError::new(
                    "WORKLOAD_CAPABILITY_DENIED",
                    "worker lacks required capability snapshot",
                ));
            }
            w.fencing_token += 1;
            w.attempt_count += 1;
            let seed = format!("{}:{}:{}", w.workload_id, w.fencing_token, worker);
            let claim = WorkloadClaim {
                claim_id: format!("claim_{}", &hex(&Sha256::digest(seed.as_bytes()))[..20]),
                workload_id: w.workload_id.clone(),
                worker_id: worker.into(),
                lease_id: format!("lease_{}", w.fencing_token),
                fencing_token: w.fencing_token,
                capability_snapshot: w.capability_snapshot.clone(),
                claimed_at: now,
                expires_at: now + (lease_ms / 1000) as i64,
                last_heartbeat: now,
            };
            w.attempts.push(WorkloadAttempt {
                attempt_id: format!("attempt_{}_{}", w.workload_id, w.attempt_count),
                workload_id: w.workload_id.clone(),
                worker_id: worker.into(),
                claim_id: claim.claim_id.clone(),
                fencing_token: claim.fencing_token,
                started_at: None,
                ended_at: None,
                input_hash: w.input_hash.clone(),
                result_hash: None,
                error: None,
                resource_usage: BTreeMap::new(),
                status: AttemptStatus::Claimed,
            });
            w.current_claim = Some(claim.clone());
            store.transition(
                w,
                WorkloadState::Claimed,
                worker,
                now,
                [("fencing_token".into(), claim.fencing_token.to_string())].into(),
            )?;
            Ok(claim)
        })
    }
    fn fence(
        w: &Workload,
        claim: &str,
        worker: &str,
        token: u64,
        now: i64,
    ) -> Result<(), WorkloadError> {
        let current = w
            .current_claim
            .as_ref()
            .ok_or_else(|| WorkloadError::new("WORKLOAD_CLAIM_INVALID", "no active claim"))?;
        if current.claim_id != claim
            || current.worker_id != worker
            || current.fencing_token != token
        {
            return Err(WorkloadError::new(
                "WORKLOAD_STALE_FENCE",
                "claim or fencing token is stale",
            ));
        }
        if current.expires_at <= now {
            return Err(WorkloadError::new(
                "WORKLOAD_LEASE_EXPIRED",
                "claim lease expired",
            ));
        }
        Ok(())
    }
    pub fn start(
        &mut self,
        id: &str,
        claim: &str,
        worker: &str,
        token: u64,
        now: i64,
    ) -> Result<(), WorkloadError> {
        self.edit(id, |store, w| {
            Self::fence(w, claim, worker, token, now)?;
            let attempt = w.attempts.last_mut().unwrap();
            attempt.status = AttemptStatus::Running;
            attempt.started_at = Some(now);
            store.transition(w, WorkloadState::Running, worker, now, BTreeMap::new())
        })
    }
    pub fn heartbeat(
        &mut self,
        id: &str,
        claim: &str,
        worker: &str,
        token: u64,
        now: i64,
        extend_ms: u64,
    ) -> Result<WorkloadClaim, WorkloadError> {
        self.edit(id, |_, w| {
            Self::fence(w, claim, worker, token, now)?;
            let current = w.current_claim.as_mut().unwrap();
            current.last_heartbeat = now;
            current.expires_at = now + (extend_ms / 1000) as i64;
            Ok(current.clone())
        })
    }
    pub fn execution_context(
        &self,
        id: &str,
        claim: &str,
        worker: &str,
        token: u64,
        now: i64,
        secret_references: Vec<String>,
    ) -> Result<WorkloadExecutionContext, WorkloadError> {
        let workload = self.get(id)?;
        Self::fence(workload, claim, worker, token, now)?;
        if secret_references
            .iter()
            .any(|v| !v.starts_with("secret://"))
        {
            return Err(WorkloadError::new(
                "WORKLOAD_SECRET_REJECTED",
                "execution context accepts secret references only",
            ));
        }
        let current = workload.current_claim.as_ref().unwrap();
        Ok(WorkloadExecutionContext {
            workload_id: id.into(),
            attempt_id: workload.attempts.last().unwrap().attempt_id.clone(),
            worker_id: worker.into(),
            capabilities: current.capability_snapshot.clone(),
            connection_scopes: workload.connection_scopes.clone(),
            secret_references,
            expires_at: current.expires_at,
            fencing_token: token,
        })
    }
    pub fn checkpoint(
        &mut self,
        id: &str,
        claim: &str,
        worker: &str,
        token: u64,
        state: Value,
        now: i64,
    ) -> Result<WorkloadCheckpoint, WorkloadError> {
        ensure_safe(&state)?;
        self.edit(id, |store, w| {
            Self::fence(w, claim, worker, token, now)?;
            if !matches!(
                w.state,
                WorkloadState::Running | WorkloadState::Checkpointed
            ) {
                return Err(WorkloadError::new(
                    "WORKLOAD_TRANSITION_DENIED",
                    "checkpoint requires a running workload",
                ));
            }
            let attempt = w.attempts.last_mut().unwrap();
            attempt.status = AttemptStatus::Checkpointed;
            let sequence = w.checkpoints.len() as u64 + 1;
            let checkpoint = WorkloadCheckpoint {
                checkpoint_id: format!("checkpoint_{}_{}", w.workload_id, sequence),
                workload_id: w.workload_id.clone(),
                attempt_id: attempt.attempt_id.clone(),
                sequence,
                state_hash: hash(&state),
                state,
                created_at: now,
                produced_by: worker.into(),
            };
            w.checkpoints.push(checkpoint.clone());
            if w.state == WorkloadState::Running {
                store.transition(w, WorkloadState::Checkpointed, worker, now, BTreeMap::new())?;
            }
            Ok(checkpoint)
        })
    }
    pub fn complete(
        &mut self,
        id: &str,
        claim: &str,
        worker: &str,
        token: u64,
        result_id: &str,
        output: Value,
        artifacts: Vec<String>,
        schema_version: u64,
        now: i64,
    ) -> Result<WorkloadResult, WorkloadError> {
        ensure_safe(&output)?;
        if let Some(workload_id) = self.result_ids.get(result_id) {
            if workload_id == id {
                return Ok(self.get(id)?.result.clone().unwrap());
            }
            return Err(WorkloadError::new(
                "WORKLOAD_RESULT_DUPLICATE",
                "result id belongs to another workload",
            ));
        }
        let result = self.edit(id, |store, w| {
            Self::fence(w, claim, worker, token, now)?;
            if !matches!(
                w.state,
                WorkloadState::Running | WorkloadState::Checkpointed
            ) {
                return Err(WorkloadError::new(
                    "WORKLOAD_TRANSITION_DENIED",
                    "completion requires running state",
                ));
            }
            let attempt = w.attempts.last_mut().unwrap();
            let output_hash = hash(&output);
            attempt.status = AttemptStatus::Succeeded;
            attempt.ended_at = Some(now);
            attempt.result_hash = Some(output_hash.clone());
            let result = WorkloadResult {
                result_id: result_id.into(),
                workload_id: w.workload_id.clone(),
                attempt_id: attempt.attempt_id.clone(),
                status: "succeeded".into(),
                output,
                output_hash,
                artifacts,
                provenance: [
                    ("claim_id".into(), claim.into()),
                    ("fencing_token".into(), token.to_string()),
                ]
                .into(),
                produced_by: worker.into(),
                created_at: now,
                schema_version,
            };
            w.result = Some(result.clone());
            w.current_claim = None;
            store.transition(w, WorkloadState::Succeeded, worker, now, BTreeMap::new())?;
            Ok(result)
        })?;
        self.result_ids.insert(result_id.into(), id.into());
        Ok(result)
    }
    pub fn fail(
        &mut self,
        id: &str,
        claim: &str,
        worker: &str,
        token: u64,
        kind: &str,
        error: &str,
        now: i64,
    ) -> Result<WorkloadState, WorkloadError> {
        if sensitive_text(error) {
            return Err(WorkloadError::new(
                "WORKLOAD_SECRET_REJECTED",
                "errors may not contain secret material",
            ));
        }
        self.edit(id, |store, w| {
            Self::fence(w, claim, worker, token, now)?;
            let attempt = w.attempts.last_mut().unwrap();
            attempt.status = AttemptStatus::Failed;
            attempt.ended_at = Some(now);
            attempt.error = Some(error.into());
            w.last_failure = Some(format!("{kind}: {error}"));
            w.current_claim = None;
            store.transition(
                w,
                WorkloadState::Failed,
                worker,
                now,
                [("failure_kind".into(), kind.into())].into(),
            )?;
            let retryable = w.retry_policy.retry_on.contains(kind)
                && w.attempt_count < w.retry_policy.max_attempts;
            if retryable {
                w.retry_count += 1;
                let multiplier = if w.retry_policy.backoff == BackoffPolicy::Exponential {
                    2u64.saturating_pow(w.retry_count.saturating_sub(1))
                } else {
                    1
                };
                let delay = w
                    .retry_policy
                    .initial_delay_ms
                    .saturating_mul(multiplier)
                    .min(w.retry_policy.max_delay_ms);
                w.next_attempt_at = Some(now + (delay / 1000) as i64);
                store.transition(w, WorkloadState::Ready, "rust.retry", now, BTreeMap::new())?;
            } else {
                store.transition(
                    w,
                    WorkloadState::DeadLetter,
                    "rust.retry",
                    now,
                    BTreeMap::new(),
                )?;
            }
            Ok(w.state)
        })
    }
    pub fn cancel(
        &mut self,
        id: &str,
        actor: &str,
        now: i64,
    ) -> Result<WorkloadState, WorkloadError> {
        self.edit(id, |store, w| {
            match w.state {
                WorkloadState::Ready
                | WorkloadState::Created
                | WorkloadState::Failed
                | WorkloadState::DeadLetter => {
                    w.current_claim = None;
                    store.transition(w, WorkloadState::Cancelled, actor, now, BTreeMap::new())?
                }
                WorkloadState::Claimed | WorkloadState::Running | WorkloadState::Checkpointed => {
                    store.transition(
                        w,
                        WorkloadState::CancellationRequested,
                        actor,
                        now,
                        BTreeMap::new(),
                    )?
                }
                WorkloadState::CancellationRequested => {}
                _ => {
                    return Err(WorkloadError::new(
                        "WORKLOAD_TRANSITION_DENIED",
                        "terminal workload cannot be cancelled",
                    ))
                }
            }
            Ok(w.state)
        })
    }
    pub fn confirm_cancel(
        &mut self,
        id: &str,
        claim: &str,
        worker: &str,
        token: u64,
        now: i64,
    ) -> Result<(), WorkloadError> {
        self.edit(id, |store, w| {
            Self::fence(w, claim, worker, token, now)?;
            if w.state != WorkloadState::CancellationRequested {
                return Err(WorkloadError::new(
                    "WORKLOAD_TRANSITION_DENIED",
                    "cancellation was not requested",
                ));
            }
            if let Some(attempt) = w.attempts.last_mut() {
                attempt.status = AttemptStatus::Cancelled;
                attempt.ended_at = Some(now);
            }
            w.current_claim = None;
            store.transition(w, WorkloadState::Cancelled, worker, now, BTreeMap::new())
        })
    }
    pub fn timeout(
        &mut self,
        id: &str,
        actor: &str,
        now: i64,
    ) -> Result<WorkloadState, WorkloadError> {
        self.edit(id, |store, w| {
            if !matches!(
                w.state,
                WorkloadState::Ready
                    | WorkloadState::Claimed
                    | WorkloadState::Running
                    | WorkloadState::Checkpointed
            ) {
                return Err(WorkloadError::new(
                    "WORKLOAD_TRANSITION_DENIED",
                    "workload cannot time out in its current state",
                ));
            }
            if let Some(attempt) = w.attempts.last_mut() {
                attempt.status = AttemptStatus::TimedOut;
                attempt.ended_at = Some(now);
            }
            w.current_claim = None;
            store.transition(w, WorkloadState::TimedOut, actor, now, BTreeMap::new())?;
            if w.attempt_count < w.retry_policy.max_attempts
                && w.retry_policy.retry_on.contains("timeout")
            {
                w.retry_count += 1;
                w.next_attempt_at = Some(now + (w.retry_policy.initial_delay_ms / 1000) as i64);
                store.transition(w, WorkloadState::Ready, "rust.retry", now, BTreeMap::new())?;
            } else {
                store.transition(
                    w,
                    WorkloadState::DeadLetter,
                    "rust.timeout",
                    now,
                    BTreeMap::new(),
                )?;
            }
            Ok(w.state)
        })
    }
    pub fn retry(&mut self, id: &str, actor: &str, now: i64) -> Result<(), WorkloadError> {
        self.edit(id, |store, w| {
            if !matches!(
                w.state,
                WorkloadState::Failed | WorkloadState::DeadLetter | WorkloadState::TimedOut
            ) {
                return Err(WorkloadError::new(
                    "WORKLOAD_TRANSITION_DENIED",
                    "workload is not retryable",
                ));
            }
            w.next_attempt_at = Some(now);
            w.current_claim = None;
            store.transition(w, WorkloadState::Ready, actor, now, BTreeMap::new())
        })
    }
    pub fn recover_expired(&mut self, now: i64) -> Result<Vec<String>, WorkloadError> {
        let ids = self
            .workloads
            .values()
            .filter(|w| {
                w.current_claim
                    .as_ref()
                    .is_some_and(|c| c.expires_at <= now)
                    && matches!(
                        w.state,
                        WorkloadState::Claimed
                            | WorkloadState::Running
                            | WorkloadState::Checkpointed
                            | WorkloadState::CancellationRequested
                    )
            })
            .map(|w| w.workload_id.clone())
            .collect::<Vec<_>>();
        for id in &ids {
            self.edit(id, |store, w| {
                if let Some(attempt) = w.attempts.last_mut() {
                    attempt.status = AttemptStatus::WorkerLost;
                    attempt.ended_at = Some(now);
                }
                w.current_claim = None;
                if w.state == WorkloadState::CancellationRequested {
                    store.transition(
                        w,
                        WorkloadState::Cancelled,
                        "rust.recovery",
                        now,
                        BTreeMap::new(),
                    )?;
                } else {
                    store.transition(
                        w,
                        WorkloadState::Recovering,
                        "rust.recovery",
                        now,
                        BTreeMap::new(),
                    )?;
                    store.transition(
                        w,
                        WorkloadState::Ready,
                        "rust.recovery",
                        now,
                        BTreeMap::new(),
                    )?;
                }
                Ok(())
            })?;
        }
        Ok(ids)
    }
}

pub trait WorkScheduler {
    fn enqueue(&mut self, workload_id: &str);
    fn claim(
        &mut self,
        store: &mut WorkloadStore,
        tenant: &str,
        application: &str,
        worker: &str,
        capabilities: &BTreeSet<String>,
        now: i64,
        lease_ms: u64,
    ) -> Result<Option<WorkloadClaim>, WorkloadError>;
    fn release(&mut self, claim_id: &str);
}
#[derive(Default)]
pub struct LocalDurableScheduler {
    queue: BTreeSet<String>,
}
impl WorkScheduler for LocalDurableScheduler {
    fn enqueue(&mut self, id: &str) {
        self.queue.insert(id.into());
    }
    fn claim(
        &mut self,
        store: &mut WorkloadStore,
        tenant: &str,
        application: &str,
        worker: &str,
        capabilities: &BTreeSet<String>,
        now: i64,
        lease_ms: u64,
    ) -> Result<Option<WorkloadClaim>, WorkloadError> {
        let selected = store
            .list(tenant, application)
            .into_iter()
            .filter(|w| {
                // Eligibility is part of selection, not a reason to fail. A
                // workload that is merely early is skipped so the scheduler can
                // hand out work that is ready now; without this it would pick an
                // ineligible workload, have `store.claim` refuse it, and return
                // an error while claimable work sat behind it.
                w.state == WorkloadState::Ready
                    && w.is_eligible_at(now)
                    && w.capability_snapshot.is_subset(capabilities)
            })
            .find(|w| {
                w.dependencies.iter().all(|d| {
                    store
                        .workloads
                        .get(&d.workload_id)
                        .is_some_and(|dependency| dependency.state == d.required_state)
                })
            })
            .map(|w| w.workload_id);
        match selected {
            Some(id) => store
                .claim(&id, worker, capabilities, now, lease_ms)
                .map(Some),
            None => Ok(None),
        }
    }
    fn release(&mut self, _: &str) {}
}
fn legal(from: WorkloadState, to: WorkloadState) -> bool {
    use WorkloadState::*;
    matches!(
        (from, to),
        (Created, Ready)
            | (Ready, Claimed)
            | (Claimed, Running)
            | (Running, Checkpointed)
            | (Checkpointed, Checkpointed)
            | (Running, Succeeded)
            | (Checkpointed, Succeeded)
            | (Claimed, CancellationRequested)
            | (Running, CancellationRequested)
            | (Checkpointed, CancellationRequested)
            | (CancellationRequested, Cancelled)
            | (Ready, Cancelled)
            | (Created, Cancelled)
            | (Failed, Cancelled)
            | (DeadLetter, Cancelled)
            | (Running, Failed)
            | (Checkpointed, Failed)
            | (Claimed, Recovering)
            | (Running, Recovering)
            | (Checkpointed, Recovering)
            | (Recovering, Ready)
            | (Failed, Ready)
            | (TimedOut, Ready)
            | (DeadLetter, Ready)
            | (Failed, DeadLetter)
            | (Running, TimedOut)
            | (Claimed, TimedOut)
            | (Checkpointed, TimedOut)
            | (Ready, TimedOut)
            | (TimedOut, DeadLetter)
    )
}
pub fn can_transition(from: WorkloadState, to: WorkloadState) -> bool {
    legal(from, to)
}
fn event_name(state: WorkloadState) -> &'static str {
    use WorkloadState::*;
    match state {
        Created => "workload.created",
        Ready => "workload.ready",
        Claimed => "workload.claimed",
        Running => "workload.started",
        Checkpointed => "workload.checkpointed",
        Recovering => "workload.recovered",
        CancellationRequested => "workload.cancellation_requested",
        Succeeded => "workload.completed",
        Failed => "workload.failed",
        Cancelled => "workload.cancelled",
        TimedOut => "workload.timed_out",
        DeadLetter => "workload.dead_lettered",
    }
}
fn ensure_safe(value: &Value) -> Result<(), WorkloadError> {
    fn walk(value: &Value) -> bool {
        match value {
            Value::Object(map) => map.iter().any(|(k, v)| {
                matches!(
                    k.to_ascii_lowercase().as_str(),
                    "password" | "secret" | "token" | "api_key" | "private_key"
                ) || walk(v)
            }),
            Value::Array(values) => values.iter().any(walk),
            Value::String(v) => sensitive_text(v),
            _ => false,
        }
    }
    if walk(value) {
        Err(WorkloadError::new(
            "WORKLOAD_SECRET_REJECTED",
            "plaintext secrets are forbidden in workload state",
        ))
    } else {
        Ok(())
    }
}
fn sensitive_text(v: &str) -> bool {
    v.contains("BEGIN PRIVATE KEY") || v.starts_with("sk-") || v.starts_with("fdb_live_")
}
fn work_key(workload: &Workload) -> String {
    format!(
        "{}/{}/{}",
        workload.tenant_id, workload.application_id, workload.workload_id
    )
}
fn derived_event_sequence(workloads: &BTreeMap<String, Workload>) -> u64 {
    workloads
        .values()
        .flat_map(|workload| workload.execution_history.iter())
        .map(|event| event.sequence)
        .max()
        .unwrap_or_default()
}
fn runtime_id(prefix: &str) -> String {
    let value: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(24)
        .map(char::from)
        .collect();
    format!("{prefix}_{value}")
}
fn io(e: impl std::fmt::Display) -> WorkloadError {
    WorkloadError::new("WORKLOAD_STORAGE_FAILURE", e.to_string())
}
fn workload_storage_error(error: FlowError) -> WorkloadError {
    match error {
        FlowError::PreconditionFailed(_) | FlowError::RevisionConflict { .. } => {
            WorkloadError::new("WORKLOAD_CONFLICT", error.to_string())
        }
        FlowError::CapabilityError(ref message) if message.starts_with("PRECONDITION_FAILED:") => {
            WorkloadError::new("WORKLOAD_CONFLICT", error.to_string())
        }
        _ => io(error),
    }
}
fn hash(value: &Value) -> String {
    hex(&Sha256::digest(
        serde_json::to_vec(value).unwrap_or_default(),
    ))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorization::{
        Grant, GrantConstraints, GrantSigner, GrantStatus, ResourceScope, ResourceScopeKind,
    };
    use std::{fs, path::PathBuf};
    fn create() -> CreateWorkload {
        let signer = GrantSigner::new("k", vec![1; 32]).unwrap();
        let subject = Subject::Agent("a1".into());
        let grant = Grant {
            contract_version: 1,
            id: "g".into(),
            issuer: Subject::Human("h".into()),
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
        };
        CreateWorkload {
            tenant_id: "t".into(),
            application_id: "app".into(),
            environment: "production".into(),
            definition_id: "send".into(),
            definition_revision: "r1".into(),
            trigger: "manual".into(),
            input: serde_json::json!({"id":1}),
            requested_by: subject,
            session_id: Some("session-1".into()),
            authorization_snapshot: signer.sign(grant).unwrap(),
            capability_snapshot: ["email:send".into()].into(),
            connection_scopes: BTreeSet::new(),
            priority: 0,
            idempotency_key: "one".into(),
            retry_policy: Default::default(),
            timeouts: Default::default(),
            dependencies: vec![],
            not_before: None,
            created_at: 10,
        }
    }
    fn store(name: &str) -> (PathBuf, WorkloadStore) {
        let path =
            std::env::temp_dir().join(format!("workload-{name}-{}.json", std::process::id()));
        let store = WorkloadStore::load(&path).unwrap();
        (path, store)
    }
    #[test]
    fn legacy_migration_is_idempotent_conflict_detecting_and_source_independent() {
        let (source_db, mut source) = store("legacy-source-builder");
        let workload = source.create(create()).unwrap();
        let legacy = LegacyWorkloadStore {
            workloads: source.workloads.clone(),
            idempotency: source.idempotency.clone(),
            result_ids: source.result_ids.clone(),
            event_sequence: derived_event_sequence(&source.workloads),
        };
        let legacy_path = source_db.with_extension("legacy-workloads.json");
        fs::write(&legacy_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let canonical_path = source_db.with_extension("canonical.log");
        let db = FeltDb::open(&canonical_path).unwrap();
        let migrated = WorkloadStore::migrate_legacy_file(db.clone(), &legacy_path).unwrap();
        assert_eq!(
            migrated.get(&workload.workload_id).unwrap().input_hash,
            workload.input_hash
        );
        WorkloadStore::migrate_legacy_file(db.clone(), &legacy_path).unwrap();
        fs::remove_file(&legacy_path).unwrap();
        let reopened = WorkloadStore::from_db(FeltDb::open(&canonical_path).unwrap()).unwrap();
        assert_eq!(
            reopened.get(&workload.workload_id).unwrap().workload_id,
            workload.workload_id
        );

        let mut conflicting = legacy;
        conflicting
            .workloads
            .get_mut(&workload.workload_id)
            .unwrap()
            .priority += 1;
        fs::write(&legacy_path, serde_json::to_vec(&conflicting).unwrap()).unwrap();
        assert_eq!(
            WorkloadStore::migrate_legacy_file(db, &legacy_path)
                .err()
                .unwrap()
                .code,
            "WORKLOAD_MIGRATION_CONFLICT"
        );
        let _ = fs::remove_file(source_db);
        let _ = fs::remove_file(canonical_path);
        let _ = fs::remove_file(legacy_path);
    }

    #[test]
    fn canonical_history_reconstructs_work_identity_provenance() {
        let (path, mut store) = store("provenance");
        let workload = store.create(create()).unwrap();
        let transaction = store
            .db
            .committed_transactions_after(0, usize::MAX)
            .unwrap()
            .into_iter()
            .last()
            .unwrap();
        let audit = transaction.audit.unwrap();
        assert_eq!(audit["work_id"], workload.workload_id);
        assert_eq!(audit["session_id"], workload.session_id.as_deref().unwrap());
        assert_eq!(audit["agent_id"], workload.requested_by.id());
        assert_eq!(
            audit["delegator_id"],
            workload.authorization_snapshot.grant.issuer.id()
        );
        assert!(transaction.commit_revision > transaction.base_revision);
        assert_eq!(transaction.work_id.as_deref(), Some(workload.workload_id.as_str()));
        assert_eq!(transaction.agent_id.as_deref(), Some(workload.requested_by.id()));
        assert_eq!(transaction.session_id, workload.session_id);
        assert_eq!(
            transaction.delegator_id.as_deref(),
            Some(workload.authorization_snapshot.grant.issuer.id())
        );
        assert_eq!(transaction.operation_ids.len(), 1);
        assert_ne!(transaction.operation_ids[0], transaction.transaction_id);
        drop(store);
        let reopened = WorkloadStore::load(&path).unwrap();
        assert_eq!(
            reopened.get(&workload.workload_id).unwrap().workload_id,
            workload.workload_id
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn competing_writers_use_canonical_document_cas_before_and_after_restart() {
        let (path, mut seed) = store("canonical-cas");
        let workload = seed.create(create()).unwrap();
        let id = workload.workload_id.clone();
        let db = seed.db.clone();
        let caps = ["email:send".into()].into();

        let mut winner = WorkloadStore::from_db(db.clone()).unwrap();
        let mut loser = WorkloadStore::from_db(db.clone()).unwrap();
        winner.claim(&id, "worker-a", &caps, 11, 1_000).unwrap();
        let conflict = loser.claim(&id, "worker-b", &caps, 11, 1_000).unwrap_err();
        assert_eq!(conflict.code, "WORKLOAD_CONFLICT");
        assert_eq!(loser.get(&id).unwrap().state, WorkloadState::Ready);

        let row = db
            .list_collection(DURABLE_WORK_COLLECTION)
            .unwrap()
            .into_iter()
            .find(|row| row.key.ends_with(&id))
            .unwrap();
        assert_eq!(row.value["__version"], 2);

        drop((seed, winner, loser, db));
        let restarted_db = FeltDb::open(&path).unwrap();
        let mut recovery = WorkloadStore::from_db(restarted_db.clone()).unwrap();
        recovery.recover_expired(13).unwrap();
        let mut restarted_winner = WorkloadStore::from_db(restarted_db.clone()).unwrap();
        let mut restarted_loser = WorkloadStore::from_db(restarted_db.clone()).unwrap();
        restarted_winner
            .claim(&id, "worker-c", &caps, 13, 10_000)
            .unwrap();
        let conflict = restarted_loser
            .claim(&id, "worker-d", &caps, 13, 10_000)
            .unwrap_err();
        assert_eq!(conflict.code, "WORKLOAD_CONFLICT");
        assert_eq!(
            restarted_loser.get(&id).unwrap().state,
            WorkloadState::Ready
        );
        drop((recovery, restarted_winner, restarted_loser, restarted_db));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn competing_admissions_share_one_canonical_idempotency_identity() {
        let path = std::env::temp_dir().join(format!(
            "workload-idempotency-cas-{}.log",
            std::process::id()
        ));
        let db = FeltDb::open(&path).unwrap();
        let mut first = WorkloadStore::from_db(db.clone()).unwrap();
        let mut second = WorkloadStore::from_db(db.clone()).unwrap();
        let accepted = first.create(create()).unwrap();
        let mut competing = create();
        competing.input = serde_json::json!({"id": 2});
        let conflict = second.create(competing).unwrap_err();
        assert_eq!(conflict.code, "WORKLOAD_CONFLICT");
        assert!(
            second.workloads.is_empty(),
            "failed projection was rolled back"
        );

        drop((first, second, db));
        let restarted = WorkloadStore::from_db(FeltDb::open(&path).unwrap()).unwrap();
        assert_eq!(restarted.workloads.len(), 1);
        assert_eq!(restarted.idempotency["t:app:one"], accepted.workload_id);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn pre_versioned_canonical_rows_upgrade_through_the_standard_cas_contract() {
        let (path, mut store) = store("pre-versioned-upgrade");
        let workload = store.create(create()).unwrap();
        let db = store.db.clone();
        db.apply_atomic_transaction(
            "fixture:pre-versioned-work",
            None,
            &[],
            &[AtomicMutation {
                capability: DURABLE_WORK_COLLECTION.into(),
                key: work_key(&workload),
                value: Some(serde_json::to_value(&workload).unwrap()),
            }],
            None,
        )
        .unwrap();

        let mut upgraded = WorkloadStore::from_db(db.clone()).unwrap();
        upgraded
            .claim(
                &workload.workload_id,
                "worker",
                &["email:send".into()].into(),
                11,
                10_000,
            )
            .unwrap();
        let row = db
            .list_collection(DURABLE_WORK_COLLECTION)
            .unwrap()
            .into_iter()
            .find(|row| row.key.ends_with(&workload.workload_id))
            .unwrap();
        assert_eq!(row.value["__version"], 2);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn derived_indexes_rebuild_exclusively_from_canonical_work() {
        let (path, mut store) = store("derived-index-rebuild");
        let workload = store.create(create()).unwrap();
        let caps = ["email:send".into()].into();
        let claim = store
            .claim(&workload.workload_id, "worker", &caps, 11, 10_000)
            .unwrap();
        store
            .start(
                &workload.workload_id,
                &claim.claim_id,
                "worker",
                claim.fencing_token,
                12,
            )
            .unwrap();
        store
            .complete(
                &workload.workload_id,
                &claim.claim_id,
                "worker",
                claim.fencing_token,
                "result-rebuilt",
                serde_json::json!({"ok": true}),
                vec![],
                1,
                13,
            )
            .unwrap();

        store.idempotency.clear();
        store.result_ids.clear();
        store.document_versions.clear();
        let rebuilt = WorkloadStore::from_db(store.db.clone()).unwrap();
        assert_eq!(
            rebuilt.idempotency.get("t:app:one"),
            Some(&workload.workload_id)
        );
        assert_eq!(
            rebuilt.result_ids.get("result-rebuilt"),
            Some(&workload.workload_id)
        );
        assert!(rebuilt.document_versions[&workload.workload_id] > 1);
        assert_eq!(
            rebuilt
                .get(&workload.workload_id)
                .unwrap()
                .execution_history
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        let duplicate = WorkloadStore::from_db(store.db.clone())
            .unwrap()
            .create(create())
            .unwrap();
        assert_eq!(duplicate.workload_id, workload.workload_id);
        let _ = fs::remove_file(path);
    }
    #[test]
    fn idempotent_create_and_legal_lifecycle() {
        let (path, mut store) = store("lifecycle");
        let one = store.create(create()).unwrap();
        let two = store.create(create()).unwrap();
        assert_eq!(one.workload_id, two.workload_id);
        let caps = ["email:send".into()].into();
        let claim = store
            .claim(&one.workload_id, "worker", &caps, 11, 10_000)
            .unwrap();
        store
            .start(
                &one.workload_id,
                &claim.claim_id,
                "worker",
                claim.fencing_token,
                12,
            )
            .unwrap();
        let result = store
            .complete(
                &one.workload_id,
                &claim.claim_id,
                "worker",
                claim.fencing_token,
                "result-1",
                serde_json::json!({"ok":true}),
                vec![],
                1,
                13,
            )
            .unwrap();
        assert_eq!(result.result_id, "result-1");
        assert!(store
            .start(
                &one.workload_id,
                &claim.claim_id,
                "worker",
                claim.fencing_token,
                14
            )
            .is_err());
        let recovered = WorkloadStore::load(&path).unwrap();
        assert_eq!(
            recovered.get(&one.workload_id).unwrap().state,
            WorkloadState::Succeeded
        );
        let _ = fs::remove_file(path);
    }
    #[test]
    fn stale_fence_and_lease_recovery() {
        let (path, mut store) = store("fence");
        let w = store.create(create()).unwrap();
        let caps = ["email:send".into()].into();
        let old = store.claim(&w.workload_id, "a", &caps, 11, 1000).unwrap();
        store
            .start(&w.workload_id, &old.claim_id, "a", old.fencing_token, 11)
            .unwrap();
        store.recover_expired(13).unwrap();
        let new = store.claim(&w.workload_id, "b", &caps, 13, 10_000).unwrap();
        assert!(new.fencing_token > old.fencing_token);
        assert_eq!(
            store
                .complete(
                    &w.workload_id,
                    &old.claim_id,
                    "a",
                    old.fencing_token,
                    "stale",
                    Value::Null,
                    vec![],
                    1,
                    14
                )
                .unwrap_err()
                .code,
            "WORKLOAD_STALE_FENCE"
        );
        let _ = fs::remove_file(path);
    }
    #[test]
    fn retry_dead_letter_checkpoint_and_secret_safety() {
        let (path, mut store) = store("retry");
        let mut input = create();
        input.retry_policy.max_attempts = 1;
        let w = store.create(input).unwrap();
        let caps = ["email:send".into()].into();
        let claim = store.claim(&w.workload_id, "a", &caps, 11, 10_000).unwrap();
        store
            .start(
                &w.workload_id,
                &claim.claim_id,
                "a",
                claim.fencing_token,
                12,
            )
            .unwrap();
        assert!(store
            .checkpoint(
                &w.workload_id,
                &claim.claim_id,
                "a",
                claim.fencing_token,
                serde_json::json!({"password":"bad"}),
                13
            )
            .is_err());
        assert_eq!(
            store
                .fail(
                    &w.workload_id,
                    &claim.claim_id,
                    "a",
                    claim.fencing_token,
                    "worker_failure",
                    "crashed",
                    13
                )
                .unwrap(),
            WorkloadState::DeadLetter
        );
        let _ = fs::remove_file(path);
    }
    #[test]
    fn cancellation_race_and_duplicate_completion_are_deterministic() {
        let (path, mut store) = store("cancel");
        let w = store.create(create()).unwrap();
        let caps = ["email:send".into()].into();
        let claim = store.claim(&w.workload_id, "a", &caps, 11, 10_000).unwrap();
        store
            .start(
                &w.workload_id,
                &claim.claim_id,
                "a",
                claim.fencing_token,
                12,
            )
            .unwrap();
        assert_eq!(
            store.cancel(&w.workload_id, "user", 13).unwrap(),
            WorkloadState::CancellationRequested
        );
        assert_eq!(
            store
                .complete(
                    &w.workload_id,
                    &claim.claim_id,
                    "a",
                    claim.fencing_token,
                    "r",
                    Value::Null,
                    vec![],
                    1,
                    14
                )
                .unwrap_err()
                .code,
            "WORKLOAD_TRANSITION_DENIED"
        );
        store
            .confirm_cancel(
                &w.workload_id,
                &claim.claim_id,
                "a",
                claim.fencing_token,
                14,
            )
            .unwrap();
        assert_eq!(
            store.get(&w.workload_id).unwrap().state,
            WorkloadState::Cancelled
        );
        let _ = fs::remove_file(path);
    }
    #[test]
    fn scheduler_respects_dependencies_and_tenant_application_isolation() {
        let (path, mut store) = store("dependency");
        let parent = store.create(create()).unwrap();
        let mut child = create();
        child.idempotency_key = "child".into();
        child.dependencies = vec![WorkloadDependency {
            workload_id: parent.workload_id.clone(),
            required_state: WorkloadState::Succeeded,
        }];
        let child = store.create(child).unwrap();
        assert_eq!(store.list("other", "app").len(), 0);
        let caps = ["email:send".into()].into();
        let mut scheduler = LocalDurableScheduler::default();
        let first = scheduler
            .claim(&mut store, "t", "app", "worker", &caps, 11, 10_000)
            .unwrap()
            .unwrap();
        assert_eq!(first.workload_id, parent.workload_id);
        assert_ne!(first.workload_id, child.workload_id);
        let _ = fs::remove_file(path);
    }

    // ------------------------------------------------------------------
    // Temporal work eligibility.
    //
    // The invariant under test: a workload may exist durably before it is
    // executable. Every test below drives the real store — creating, claiming,
    // reloading from disk — rather than asserting that a field or a function
    // name exists.
    // ------------------------------------------------------------------

    /// A store on a path unique to this test, cleared first so a recycled
    /// process id cannot hand a test a previous run's file.
    fn temporal_store(name: &str) -> (PathBuf, WorkloadStore) {
        let path = std::env::temp_dir().join(format!(
            "workload-temporal-{name}-{}-{}.json",
            std::process::id(),
            rand::random::<u64>()
        ));
        let _ = fs::remove_file(&path);
        (path.clone(), WorkloadStore::load(&path).unwrap())
    }

    fn scheduled(not_before: i64) -> CreateWorkload {
        let mut input = create();
        input.not_before = Some(not_before);
        input
    }

    fn caps() -> BTreeSet<String> {
        ["email:send".into()].into()
    }

    #[test]
    fn scheduled_work_is_not_claimable_before_not_before() {
        let (path, mut store) = temporal_store("before");
        let workload = store.create(scheduled(2_000)).unwrap();

        // Durable and Ready immediately: existing before it is executable is
        // the whole point.
        assert_eq!(workload.state, WorkloadState::Ready);
        assert_eq!(workload.next_attempt_at, Some(2_000));

        let refused = store
            .claim(&workload.workload_id, "worker", &caps(), 1_999, 10_000)
            .unwrap_err();
        assert_eq!(refused.code, "WORKLOAD_NOT_YET_ELIGIBLE");
        assert!(
            refused.message.contains("2000"),
            "the refusal names the instant the worker should return at: {}",
            refused.message
        );

        // Refusing a claim is not a state change. Nothing executed.
        let after = store.get(&workload.workload_id).unwrap();
        assert_eq!(after.state, WorkloadState::Ready);
        assert_eq!(after.attempt_count, 0);
        assert!(after.attempts.is_empty());
        assert!(after.current_claim.is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn scheduled_work_becomes_claimable_after_not_before() {
        let (path, mut store) = temporal_store("after");
        let workload = store.create(scheduled(2_000)).unwrap();

        // The boundary belongs to the eligible side: at exactly `not_before`
        // the workload is claimable, so a worker polling on the second does not
        // have to wait a further tick.
        let claim = store
            .claim(&workload.workload_id, "worker", &caps(), 2_000, 10_000)
            .expect("eligible at the instant itself");
        assert_eq!(claim.worker_id, "worker");
        assert_eq!(
            store.get(&workload.workload_id).unwrap().state,
            WorkloadState::Claimed
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn scheduled_work_survives_restart_before_eligibility() {
        let (path, mut store) = temporal_store("restart-before");
        let id = store.create(scheduled(2_000)).unwrap().workload_id;
        drop(store);

        // Restart the authority. The workload is still here, still Ready, still
        // carrying its eligibility, and still refused.
        let mut reopened = WorkloadStore::load(&path).unwrap();
        let recovered = reopened.get(&id).unwrap();
        assert_eq!(recovered.state, WorkloadState::Ready);
        assert_eq!(recovered.next_attempt_at, Some(2_000));

        assert_eq!(
            reopened
                .claim(&id, "worker", &caps(), 1_999, 10_000)
                .unwrap_err()
                .code,
            "WORKLOAD_NOT_YET_ELIGIBLE"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn scheduled_work_executes_after_restart() {
        let (path, mut store) = temporal_store("restart-after");
        let id = store.create(scheduled(2_000)).unwrap().workload_id;
        drop(store);

        let mut reopened = WorkloadStore::load(&path).unwrap();
        let claim = reopened
            .claim(&id, "worker", &caps(), 2_001, 10_000)
            .expect("claimable after restart once eligible");
        reopened
            .start(&id, &claim.claim_id, "worker", claim.fencing_token, 2_002)
            .unwrap();
        reopened
            .complete(
                &id,
                &claim.claim_id,
                "worker",
                claim.fencing_token,
                "result-scheduled",
                serde_json::json!({"ok": true}),
                vec![],
                1,
                2_003,
            )
            .unwrap();

        // And the outcome is durable across a second restart.
        let again = WorkloadStore::load(&path).unwrap();
        let finished = again.get(&id).unwrap();
        assert_eq!(finished.state, WorkloadState::Succeeded);
        assert_eq!(
            finished.result.as_ref().map(|r| r.result_id.as_str()),
            Some("result-scheduled")
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn immediate_work_behavior_is_unchanged() {
        let (path, mut store) = temporal_store("immediate");
        // `create()` sets no `not_before`, which is what every caller did
        // before this field existed.
        let workload = store.create(create()).unwrap();
        assert_eq!(workload.next_attempt_at, None);
        assert!(workload.is_eligible_at(workload.created_at));

        // Claimable at its own creation instant, with no waiting.
        store
            .claim(
                &workload.workload_id,
                "worker",
                &caps(),
                workload.created_at,
                10_000,
            )
            .expect("work with no eligibility instant is claimable immediately");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn multiple_workers_claim_temporally_eligible_work_once() {
        let (path, mut store) = temporal_store("contention");
        let id = store.create(scheduled(2_000)).unwrap().workload_id;

        // Both workers are polling and both see eligibility open at the same
        // instant. Exactly one wins.
        let winner = store
            .claim(&id, "worker-a", &caps(), 2_000, 10_000)
            .unwrap();
        let loser = store
            .claim(&id, "worker-b", &caps(), 2_000, 10_000)
            .unwrap_err();
        assert_eq!(
            loser.code, "WORKLOAD_NOT_READY",
            "the second worker is refused on state, not on time: the workload is claimed"
        );

        // And the loser cannot complete work it never owned.
        assert_eq!(
            store
                .complete(
                    &id,
                    &winner.claim_id,
                    "worker-b",
                    winner.fencing_token,
                    "duplicate",
                    Value::Null,
                    vec![],
                    1,
                    2_001,
                )
                .unwrap_err()
                .code,
            "WORKLOAD_STALE_FENCE"
        );
        assert_eq!(store.get(&id).unwrap().attempt_count, 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn retry_sets_next_eligibility_without_duplicate_execution() {
        let (path, mut store) = temporal_store("retry");
        let id = store.create(scheduled(2_000)).unwrap().workload_id;

        let first = store
            .claim(&id, "worker-a", &caps(), 2_000, 10_000)
            .unwrap();
        store
            .start(&id, &first.claim_id, "worker-a", first.fencing_token, 2_001)
            .unwrap();
        let state = store
            .fail(
                &id,
                &first.claim_id,
                "worker-a",
                first.fencing_token,
                "timeout",
                "upstream did not answer",
                2_002,
            )
            .unwrap();

        // Retry re-arms the same eligibility field creation used, so there is
        // one temporal concept rather than two competing ones.
        assert_eq!(state, WorkloadState::Ready);
        let backing_off = store.get(&id).unwrap();
        let next = backing_off.next_attempt_at.expect("retry set eligibility");
        assert!(
            next > 2_002,
            "backoff pushed eligibility into the future: {next}"
        );

        assert_eq!(
            store
                .claim(&id, "worker-b", &caps(), next - 1, 10_000)
                .unwrap_err()
                .code,
            "WORKLOAD_NOT_YET_ELIGIBLE"
        );

        let second = store.claim(&id, "worker-b", &caps(), next, 10_000).unwrap();
        assert!(second.fencing_token > first.fencing_token);

        // The first attempt cannot land a result after the retry was handed on.
        assert_eq!(
            store
                .complete(
                    &id,
                    &first.claim_id,
                    "worker-a",
                    first.fencing_token,
                    "stale",
                    Value::Null,
                    vec![],
                    1,
                    next,
                )
                .unwrap_err()
                .code,
            "WORKLOAD_STALE_FENCE"
        );
        assert_eq!(store.get(&id).unwrap().attempt_count, 2);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn cancellation_before_not_before_prevents_execution() {
        let (path, mut store) = temporal_store("cancel");
        let id = store.create(scheduled(2_000)).unwrap().workload_id;

        // Cancelling work that is durable but not yet executable is the whole
        // reason to be able to see it before it runs.
        assert_eq!(
            store.cancel(&id, "operator", 1_500).unwrap(),
            WorkloadState::Cancelled
        );

        // Eligibility arriving does not resurrect it.
        assert_eq!(
            store
                .claim(&id, "worker", &caps(), 2_001, 10_000)
                .unwrap_err()
                .code,
            "WORKLOAD_NOT_READY"
        );

        let reopened = WorkloadStore::load(&path).unwrap();
        let cancelled = reopened.get(&id).unwrap();
        assert_eq!(cancelled.state, WorkloadState::Cancelled);
        assert!(cancelled.attempts.is_empty(), "nothing ever executed");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn a_worker_dying_after_a_claim_returns_the_work_and_it_executes_once() {
        let (path, mut store) = temporal_store("worker-death");
        let id = store.create(create()).unwrap().workload_id;

        // Worker A claims a short lease, starts, and dies without completing.
        let dead = store.claim(&id, "worker-a", &caps(), 10, 1_000).unwrap();
        store
            .start(&id, &dead.claim_id, "worker-a", dead.fencing_token, 10)
            .unwrap();
        assert_eq!(
            store.get(&id).unwrap().state,
            WorkloadState::Running,
            "the work was in flight when its worker died"
        );

        // The lease expiring is the only thing that can notice.
        let recovered = store.recover_expired(12).unwrap();
        assert_eq!(recovered, vec![id.clone()]);
        let back = store.get(&id).unwrap();
        assert_eq!(back.state, WorkloadState::Ready, "returned to the pool");
        assert_eq!(
            back.attempts.last().map(|attempt| attempt.status),
            Some(AttemptStatus::WorkerLost),
            "and the dead attempt is recorded as lost rather than erased"
        );

        // Worker B picks it up and finishes it.
        let live = store.claim(&id, "worker-b", &caps(), 12, 10_000).unwrap();
        assert!(live.fencing_token > dead.fencing_token);
        store
            .start(&id, &live.claim_id, "worker-b", live.fencing_token, 13)
            .unwrap();
        store
            .complete(
                &id,
                &live.claim_id,
                "worker-b",
                live.fencing_token,
                "result-recovered",
                serde_json::json!({"ok": true}),
                vec![],
                1,
                14,
            )
            .unwrap();

        // Exactly one execution succeeded, and the dead worker cannot add another.
        let finished = store.get(&id).unwrap();
        assert_eq!(finished.state, WorkloadState::Succeeded);
        assert_eq!(
            finished
                .attempts
                .iter()
                .filter(|attempt| attempt.status == AttemptStatus::Succeeded)
                .count(),
            1
        );
        // The dead worker cannot add a second result. Its claim was released by
        // the recovery, and completion released the successor's, so it is
        // refused for having no claim at all rather than a stale one.
        assert_eq!(
            store
                .complete(
                    &id,
                    &dead.claim_id,
                    "worker-a",
                    dead.fencing_token,
                    "resurrected",
                    Value::Null,
                    vec![],
                    1,
                    15,
                )
                .unwrap_err()
                .code,
            "WORKLOAD_CLAIM_INVALID"
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn a_recovered_workload_refuses_its_dead_worker_while_the_successor_still_holds_it() {
        let (path, mut store) = temporal_store("worker-death-inflight");
        let id = store.create(create()).unwrap().workload_id;

        let dead = store.claim(&id, "worker-a", &caps(), 10, 1_000).unwrap();
        store
            .start(&id, &dead.claim_id, "worker-a", dead.fencing_token, 10)
            .unwrap();
        store.recover_expired(12).unwrap();
        let live = store.claim(&id, "worker-b", &caps(), 12, 10_000).unwrap();

        // While the successor is mid-flight, the resurrected worker is refused
        // on the fence itself — the case a lease exists to decide.
        assert_eq!(
            store
                .complete(
                    &id,
                    &dead.claim_id,
                    "worker-a",
                    dead.fencing_token,
                    "resurrected",
                    Value::Null,
                    vec![],
                    1,
                    13,
                )
                .unwrap_err()
                .code,
            "WORKLOAD_STALE_FENCE"
        );
        assert_eq!(store.get(&id).unwrap().state, WorkloadState::Claimed);
        assert!(live.fencing_token > dead.fencing_token);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn the_scheduler_skips_ineligible_work_rather_than_failing_on_it() {
        let (path, mut store) = temporal_store("scheduler");
        let mut early = create();
        early.idempotency_key = "early".into();
        early.not_before = Some(9_000);
        let early = store.create(early).unwrap();

        let mut ready = create();
        ready.idempotency_key = "ready".into();
        let ready = store.create(ready).unwrap();

        // The ineligible workload sorts first by id in some orderings and not
        // others, so what matters is that the scheduler never returns it and
        // never fails because of it.
        let mut scheduler = LocalDurableScheduler::default();
        let claim = scheduler
            .claim(&mut store, "t", "app", "worker", &caps(), 1_000, 10_000)
            .expect("an ineligible workload must not fail the whole pass")
            .expect("the eligible workload is still handed out");
        assert_eq!(claim.workload_id, ready.workload_id);
        assert_eq!(
            store.get(&early.workload_id).unwrap().state,
            WorkloadState::Ready
        );
        let _ = fs::remove_file(path);
    }
}
