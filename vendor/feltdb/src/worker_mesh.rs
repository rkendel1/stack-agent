//! Worker and mesh semantics. Compute is replaceable; durable workload state is authority.
use crate::authorization::{SignedGrant, Subject};
use crate::workload::{Workload, WorkloadClaim, WorkloadError, WorkloadStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
pub const WORKER_CONTRACT_VERSION: u32 = 1;
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerRuntimeKind {
    Wasm,
    Process,
    Container,
    Browser,
    Remote,
    Hosted,
    Edge,
    Customer,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkerLifecycle {
    Registered,
    Ready,
    Busy,
    Draining,
    Offline,
    Recovering,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkerHealth {
    Healthy,
    Degraded,
    Unhealthy,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkerResourceProfile {
    pub cpu_millis: Option<u64>,
    pub memory_bytes: Option<u64>,
    pub disk_bytes: Option<u64>,
    pub gpu_count: Option<u32>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerCapacity {
    pub concurrency: u32,
    #[serde(default)]
    pub active: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRegistration {
    pub tenant_id: String,
    pub application_id: String,
    pub name: String,
    pub runtime: WorkerRuntimeKind,
    pub version: String,
    #[serde(default)]
    pub capabilities: BTreeSet<String>,
    #[serde(default)]
    pub workload_kinds: BTreeSet<String>,
    #[serde(default)]
    pub resource_profile: WorkerResourceProfile,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub capacity: WorkerCapacity,
    pub authorization_grant: SignedGrant,
    pub registered_by: Subject,
    pub registered_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worker {
    pub contract_version: u32,
    pub worker_id: String,
    pub tenant_id: String,
    pub application_id: String,
    pub name: String,
    pub runtime: WorkerRuntimeKind,
    pub version: String,
    pub declared_capabilities: BTreeSet<String>,
    pub granted_capabilities: BTreeSet<String>,
    pub workload_kinds: BTreeSet<String>,
    pub resource_profile: WorkerResourceProfile,
    pub labels: BTreeMap<String, String>,
    pub capacity: WorkerCapacity,
    pub health: WorkerHealth,
    pub lifecycle: WorkerLifecycle,
    pub heartbeat_interval_ms: u64,
    pub heartbeat_timeout_ms: u64,
    pub last_seen: i64,
    pub capability_version: u64,
    pub authorization_grant_id: String,
    #[serde(default)]
    pub claims: BTreeSet<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerHeartbeat {
    pub worker_id: String,
    pub version: String,
    pub capability_version: u64,
    pub capacity: WorkerCapacity,
    pub health: WorkerHealth,
    pub timestamp: i64,
    #[serde(default)]
    pub running_claims: BTreeSet<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerPool {
    pub pool_id: String,
    pub tenant_id: String,
    pub application_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub capabilities: BTreeSet<String>,
    #[serde(default)]
    pub required_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub worker_ids: BTreeSet<String>,
    pub max_concurrency: Option<u32>,
    pub created_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlacementPolicy {
    #[serde(default)]
    pub required_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub affinity: BTreeMap<String, String>,
    #[serde(default)]
    pub anti_affinity: BTreeMap<String, String>,
    pub pool_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacementDecision {
    pub decision_id: String,
    pub workload_id: String,
    pub eligible_workers: Vec<String>,
    pub selected_worker: Option<String>,
    pub reason: String,
    pub evaluated_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshExecutionClaim {
    pub workload_id: String,
    pub attempt_id: String,
    pub worker_id: String,
    pub lease_id: String,
    pub claim_id: String,
    pub fencing_token: u64,
    pub capability_snapshot: BTreeSet<String>,
    pub execution_context_id: String,
    pub issued_at: i64,
    pub expires_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePolicy {
    pub max_cpu_millis: Option<u64>,
    pub max_memory_bytes: Option<u64>,
    pub max_duration_ms: u64,
    pub network_policy: String,
    pub filesystem_policy: String,
    pub secret_policy: String,
    pub artifact_policy: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeExecutionResult {
    pub result_id: String,
    pub output: Value,
    #[serde(default)]
    pub logs: Vec<String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub resource_usage: BTreeMap<String, u64>,
}
pub trait WorkerRuntime {
    fn start(
        &mut self,
        claim: &MeshExecutionClaim,
        policy: &RuntimePolicy,
    ) -> Result<(), WorkerMeshError>;
    fn execute(
        &mut self,
        claim: &MeshExecutionClaim,
        input: &Value,
    ) -> Result<RuntimeExecutionResult, WorkerMeshError>;
    fn cancel(&mut self, claim: &MeshExecutionClaim) -> Result<(), WorkerMeshError>;
    fn timeout(&mut self, claim: &MeshExecutionClaim) -> Result<(), WorkerMeshError>;
}
pub trait WorkerPlacement {
    fn eligible_workers(
        &self,
        workload: &Workload,
        workers: &[Worker],
        policy: &PlacementPolicy,
    ) -> Vec<String>;
    fn select_worker(
        &self,
        workload: &Workload,
        candidates: &[Worker],
    ) -> Result<String, WorkerMeshError>;
}
#[derive(Default)]
pub struct DeterministicPlacement;
impl WorkerPlacement for DeterministicPlacement {
    fn eligible_workers(
        &self,
        w: &Workload,
        workers: &[Worker],
        policy: &PlacementPolicy,
    ) -> Vec<String> {
        let mut ids = workers
            .iter()
            .filter(|worker| {
                worker.tenant_id == w.tenant_id
                    && worker.application_id == w.application_id
                    && worker.lifecycle == WorkerLifecycle::Ready
                    && worker.health == WorkerHealth::Healthy
                    && worker.capacity.active < worker.capacity.concurrency
                    && w.capability_snapshot
                        .is_subset(&worker.granted_capabilities)
                    && (!worker.workload_kinds.is_empty())
                        .then_some(worker.workload_kinds.contains(&w.definition_id))
                        .unwrap_or(true)
                    && policy
                        .required_labels
                        .iter()
                        .all(|(k, v)| worker.labels.get(k) == Some(v))
                    && policy
                        .anti_affinity
                        .iter()
                        .all(|(k, v)| worker.labels.get(k) != Some(v))
            })
            .map(|v| v.worker_id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }
    fn select_worker(
        &self,
        _: &Workload,
        candidates: &[Worker],
    ) -> Result<String, WorkerMeshError> {
        candidates
            .iter()
            .min_by_key(|worker| {
                (
                    worker.capacity.active,
                    std::cmp::Reverse(worker.capacity.concurrency),
                    worker.worker_id.clone(),
                )
            })
            .map(|v| v.worker_id.clone())
            .ok_or_else(|| {
                WorkerMeshError::new(
                    "MESH_NO_ELIGIBLE_WORKER",
                    "no worker satisfies placement policy",
                )
            })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerReconciliation {
    pub worker_id: String,
    pub known_claims: BTreeSet<String>,
    pub running_attempts: BTreeSet<String>,
    pub completed_attempts: BTreeSet<String>,
    pub failed_attempts: BTreeSet<String>,
    pub worker_version: String,
    pub capability_version: u64,
    pub timestamp: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerReconciliationResult {
    pub authoritative_claims: BTreeSet<String>,
    pub fenced_claims: BTreeSet<String>,
    pub cancel_attempts: BTreeSet<String>,
    pub worker: Worker,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerMeshError {
    pub code: String,
    pub message: String,
}
impl WorkerMeshError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl std::fmt::Display for WorkerMeshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for WorkerMeshError {}
impl From<WorkloadError> for WorkerMeshError {
    fn from(v: WorkloadError) -> Self {
        Self::new(&v.code, v.message)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerMeshStore {
    #[serde(skip)]
    path: PathBuf,
    #[serde(default)]
    pub workers: BTreeMap<String, Worker>,
    #[serde(default)]
    pub pools: BTreeMap<String, WorkerPool>,
    #[serde(default)]
    pub placements: Vec<PlacementDecision>,
}
impl WorkerMeshStore {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, WorkerMeshError> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Ok(Self {
                path,
                workers: BTreeMap::new(),
                pools: BTreeMap::new(),
                placements: vec![],
            });
        }
        let mut value: Self = serde_json::from_slice(&fs::read(&path).map_err(io)?).map_err(io)?;
        value.path = path;
        Ok(value)
    }
    fn persist(&self) -> Result<(), WorkerMeshError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(io)?;
        }
        let temp = self.path.with_extension("tmp");
        fs::write(&temp, serde_json::to_vec_pretty(self).map_err(io)?).map_err(io)?;
        fs::rename(temp, &self.path).map_err(io)
    }
    pub fn register(&mut self, input: WorkerRegistration) -> Result<Worker, WorkerMeshError> {
        if input.capacity.concurrency == 0 {
            return Err(WorkerMeshError::new(
                "WORKER_INVALID",
                "concurrency must be positive",
            ));
        }
        if input.authorization_grant.grant.subject != input.registered_by
            || input.authorization_grant.grant.tenant_id != input.tenant_id
            || input.authorization_grant.grant.application_id != input.application_id
        {
            return Err(WorkerMeshError::new(
                "WORKER_UNAUTHORIZED",
                "registration grant scope mismatch",
            ));
        }
        let allowed = input
            .authorization_grant
            .grant
            .capabilities
            .iter()
            .filter_map(|v| v.strip_prefix("worker:capability:").map(String::from))
            .collect::<BTreeSet<_>>();
        let granted = input.capabilities.intersection(&allowed).cloned().collect();
        let seed = format!(
            "{}:{}:{}",
            input.tenant_id, input.application_id, input.name
        );
        let id = format!("worker_{}", &hex(&Sha256::digest(seed))[..24]);
        if let Some(existing) = self.workers.get(&id) {
            return Ok(existing.clone());
        }
        let worker = Worker {
            contract_version: 1,
            worker_id: id.clone(),
            tenant_id: input.tenant_id,
            application_id: input.application_id,
            name: input.name,
            runtime: input.runtime,
            version: input.version,
            declared_capabilities: input.capabilities,
            granted_capabilities: granted,
            workload_kinds: input.workload_kinds,
            resource_profile: input.resource_profile,
            labels: input.labels,
            capacity: input.capacity,
            health: WorkerHealth::Unknown,
            lifecycle: WorkerLifecycle::Registered,
            heartbeat_interval_ms: 10_000,
            heartbeat_timeout_ms: 30_000,
            last_seen: input.registered_at,
            capability_version: input.authorization_grant.grant.revision,
            authorization_grant_id: input.authorization_grant.grant.id,
            claims: BTreeSet::new(),
        };
        self.workers.insert(id, worker.clone());
        self.persist()?;
        Ok(worker)
    }
    pub fn heartbeat(
        &mut self,
        id: &str,
        input: WorkerHeartbeat,
        clock_tolerance_seconds: i64,
    ) -> Result<Worker, WorkerMeshError> {
        let worker = self
            .workers
            .get_mut(id)
            .ok_or_else(|| WorkerMeshError::new("WORKER_NOT_FOUND", "worker not found"))?;
        if input.worker_id != id
            || input.capability_version != worker.capability_version
            || input.version != worker.version
        {
            return Err(WorkerMeshError::new(
                "WORKER_HEARTBEAT_REJECTED",
                "worker identity, version, or capability version changed",
            ));
        }
        if input.timestamp + clock_tolerance_seconds < worker.last_seen {
            return Err(WorkerMeshError::new(
                "WORKER_CLOCK_INVALID",
                "heartbeat is outside clock tolerance",
            ));
        }
        if matches!(worker.lifecycle, WorkerLifecycle::Offline) {
            return Err(WorkerMeshError::new(
                "WORKER_RECOVERY_REQUIRED",
                "offline worker must reconcile before heartbeat",
            ));
        }
        worker.last_seen = input.timestamp;
        worker.health = input.health;
        worker.capacity.active = input.capacity.active.min(worker.capacity.concurrency);
        worker.claims = input.running_claims;
        worker.lifecycle = if worker.lifecycle == WorkerLifecycle::Draining {
            if worker.claims.is_empty() {
                WorkerLifecycle::Offline
            } else {
                WorkerLifecycle::Draining
            }
        } else if worker.capacity.active >= worker.capacity.concurrency {
            WorkerLifecycle::Busy
        } else {
            WorkerLifecycle::Ready
        };
        let result = worker.clone();
        self.persist()?;
        Ok(result)
    }
    pub fn transition(&mut self, id: &str, to: WorkerLifecycle) -> Result<Worker, WorkerMeshError> {
        let worker = self
            .workers
            .get_mut(id)
            .ok_or_else(|| WorkerMeshError::new("WORKER_NOT_FOUND", "worker not found"))?;
        if !worker_transition(worker.lifecycle, to) {
            return Err(WorkerMeshError::new(
                "WORKER_TRANSITION_DENIED",
                format!("{:?} cannot transition to {:?}", worker.lifecycle, to),
            ));
        }
        worker.lifecycle = to;
        let result = worker.clone();
        self.persist()?;
        Ok(result)
    }
    pub fn drain(&mut self, id: &str) -> Result<Worker, WorkerMeshError> {
        self.transition(id, WorkerLifecycle::Draining)
    }
    pub fn recover(&mut self, id: &str) -> Result<Worker, WorkerMeshError> {
        self.transition(id, WorkerLifecycle::Recovering)
    }
    pub fn remove(&mut self, id: &str) -> Result<(), WorkerMeshError> {
        let worker = self
            .workers
            .get(id)
            .ok_or_else(|| WorkerMeshError::new("WORKER_NOT_FOUND", "worker not found"))?;
        if !worker.claims.is_empty() {
            return Err(WorkerMeshError::new(
                "WORKER_BUSY",
                "worker with active claims cannot be removed",
            ));
        }
        self.workers.remove(id);
        for pool in self.pools.values_mut() {
            pool.worker_ids.remove(id);
        }
        self.persist()
    }
    pub fn detect_failures(
        &mut self,
        now: i64,
        workloads: &mut WorkloadStore,
    ) -> Result<Vec<String>, WorkerMeshError> {
        let failed = self
            .workers
            .values()
            .filter(|w| {
                !matches!(w.lifecycle, WorkerLifecycle::Offline)
                    && w.last_seen + (w.heartbeat_timeout_ms / 1000) as i64 <= now
            })
            .map(|w| w.worker_id.clone())
            .collect::<Vec<_>>();
        for id in &failed {
            if let Some(worker) = self.workers.get_mut(id) {
                worker.lifecycle = WorkerLifecycle::Offline;
                worker.health = WorkerHealth::Unhealthy;
                worker.claims.clear();
            }
        }
        workloads.recover_expired(now)?;
        self.persist()?;
        Ok(failed)
    }
    pub fn create_pool(&mut self, pool: WorkerPool) -> Result<WorkerPool, WorkerMeshError> {
        if self.pools.contains_key(&pool.pool_id) {
            return Err(WorkerMeshError::new(
                "WORKER_POOL_EXISTS",
                "worker pool exists",
            ));
        }
        for worker in &pool.worker_ids {
            if !self.workers.contains_key(worker) {
                return Err(WorkerMeshError::new("WORKER_NOT_FOUND", worker));
            }
        }
        self.pools.insert(pool.pool_id.clone(), pool.clone());
        self.persist()?;
        Ok(pool)
    }
    pub fn eligible(&self, workload: &Workload, policy: &PlacementPolicy) -> Vec<String> {
        let mut workers = self.workers.values().cloned().collect::<Vec<_>>();
        if let Some(pool) = policy.pool_id.as_ref().and_then(|id| self.pools.get(id)) {
            workers.retain(|v| {
                pool.worker_ids.contains(&v.worker_id)
                    && pool.capabilities.is_subset(&v.granted_capabilities)
            });
        }
        DeterministicPlacement.eligible_workers(workload, &workers, policy)
    }
    pub fn place_and_claim(
        &mut self,
        workloads: &mut WorkloadStore,
        workload_id: &str,
        worker_id: &str,
        now: i64,
        lease_ms: u64,
    ) -> Result<MeshExecutionClaim, WorkerMeshError> {
        let workload = workloads.get(workload_id)?.clone();
        let worker = self
            .workers
            .get(worker_id)
            .ok_or_else(|| WorkerMeshError::new("WORKER_NOT_FOUND", "worker not found"))?
            .clone();
        if !self
            .eligible(&workload, &PlacementPolicy::default())
            .contains(&worker_id.to_string())
        {
            return Err(WorkerMeshError::new(
                "MESH_PLACEMENT_DENIED",
                "worker is not eligible",
            ));
        }
        let claim = workloads.claim(
            workload_id,
            worker_id,
            &worker.granted_capabilities,
            now,
            lease_ms,
        )?;
        let attempt = workloads
            .get(workload_id)?
            .attempts
            .last()
            .unwrap()
            .attempt_id
            .clone();
        let mesh = mesh_claim(&claim, &attempt);
        let current = self.workers.get_mut(worker_id).unwrap();
        current.claims.insert(claim.claim_id.clone());
        current.capacity.active += 1;
        current.lifecycle = if current.capacity.active >= current.capacity.concurrency {
            WorkerLifecycle::Busy
        } else {
            WorkerLifecycle::Ready
        };
        self.placements.push(PlacementDecision {
            decision_id: format!("placement_{}", self.placements.len() + 1),
            workload_id: workload_id.into(),
            eligible_workers: vec![worker_id.into()],
            selected_worker: Some(worker_id.into()),
            reason: "deterministic capability and capacity match".into(),
            evaluated_at: now,
        });
        self.persist()?;
        Ok(mesh)
    }
    pub fn release_claim(
        &mut self,
        worker_id: &str,
        claim_id: &str,
    ) -> Result<(), WorkerMeshError> {
        let worker = self
            .workers
            .get_mut(worker_id)
            .ok_or_else(|| WorkerMeshError::new("WORKER_NOT_FOUND", "worker not found"))?;
        worker.claims.remove(claim_id);
        worker.capacity.active = worker.capacity.active.saturating_sub(1);
        if worker.lifecycle == WorkerLifecycle::Draining && worker.claims.is_empty() {
            worker.lifecycle = WorkerLifecycle::Offline
        } else if worker.lifecycle == WorkerLifecycle::Busy {
            worker.lifecycle = WorkerLifecycle::Ready
        }
        self.persist()
    }
    pub fn reconcile(
        &mut self,
        input: WorkerReconciliation,
        workloads: &WorkloadStore,
    ) -> Result<WorkerReconciliationResult, WorkerMeshError> {
        let worker = self
            .workers
            .get_mut(&input.worker_id)
            .ok_or_else(|| WorkerMeshError::new("WORKER_NOT_FOUND", "worker not found"))?;
        if input.worker_version != worker.version
            || input.capability_version != worker.capability_version
        {
            return Err(WorkerMeshError::new(
                "WORKER_RECONCILIATION_DENIED",
                "worker contract version changed",
            ));
        }
        let authoritative = workloads
            .workloads
            .values()
            .filter_map(|w| w.current_claim.as_ref())
            .filter(|c| c.worker_id == input.worker_id && c.expires_at > input.timestamp)
            .map(|c| c.claim_id.clone())
            .collect::<BTreeSet<_>>();
        let fenced = input
            .known_claims
            .difference(&authoritative)
            .cloned()
            .collect();
        let cancel = input
            .running_attempts
            .iter()
            .filter(|attempt| {
                !workloads.workloads.values().any(|w| {
                    w.attempts.iter().any(|a| &a.attempt_id == *attempt)
                        && matches!(
                            w.state,
                            crate::workload::WorkloadState::Running
                                | crate::workload::WorkloadState::Checkpointed
                        )
                })
            })
            .cloned()
            .collect();
        worker.claims = authoritative.clone();
        worker.last_seen = input.timestamp;
        worker.lifecycle = WorkerLifecycle::Ready;
        worker.health = WorkerHealth::Healthy;
        let result = WorkerReconciliationResult {
            authoritative_claims: authoritative,
            fenced_claims: fenced,
            cancel_attempts: cancel,
            worker: worker.clone(),
        };
        self.persist()?;
        Ok(result)
    }
}
fn mesh_claim(claim: &WorkloadClaim, attempt: &str) -> MeshExecutionClaim {
    MeshExecutionClaim {
        workload_id: claim.workload_id.clone(),
        attempt_id: attempt.into(),
        worker_id: claim.worker_id.clone(),
        lease_id: claim.lease_id.clone(),
        claim_id: claim.claim_id.clone(),
        fencing_token: claim.fencing_token,
        capability_snapshot: claim.capability_snapshot.clone(),
        execution_context_id: format!("context_{}", claim.claim_id),
        issued_at: claim.claimed_at,
        expires_at: claim.expires_at,
    }
}
pub fn worker_transition(from: WorkerLifecycle, to: WorkerLifecycle) -> bool {
    use WorkerLifecycle::*;
    matches!(
        (from, to),
        (Registered, Ready)
            | (Ready, Busy)
            | (Busy, Ready)
            | (Ready, Draining)
            | (Busy, Draining)
            | (Draining, Offline)
            | (Ready, Offline)
            | (Busy, Offline)
            | (Offline, Recovering)
            | (Recovering, Ready)
            | (Recovering, Offline)
    )
}
pub fn validate_mesh_claim(
    claim: &MeshExecutionClaim,
    worker: &Worker,
    workload: &Workload,
    now: i64,
) -> Result<(), WorkerMeshError> {
    if claim.worker_id != worker.worker_id
        || claim.workload_id != workload.workload_id
        || worker.tenant_id != workload.tenant_id
        || worker.application_id != workload.application_id
    {
        return Err(WorkerMeshError::new(
            "MESH_CLAIM_SCOPE_DENIED",
            "claim crosses worker/workload scope",
        ));
    }
    if claim.expires_at <= now
        || workload.current_claim.as_ref().is_none_or(|current| {
            current.claim_id != claim.claim_id || current.fencing_token != claim.fencing_token
        })
    {
        return Err(WorkerMeshError::new(
            "MESH_STALE_FENCE",
            "claim is expired or fenced",
        ));
    }
    if !claim
        .capability_snapshot
        .is_subset(&worker.granted_capabilities)
        || claim.capability_snapshot != workload.capability_snapshot
    {
        return Err(WorkerMeshError::new(
            "MESH_CAPABILITY_DENIED",
            "claim capability snapshot is invalid",
        ));
    }
    Ok(())
}
fn io(e: impl std::fmt::Display) -> WorkerMeshError {
    WorkerMeshError::new("MESH_STORAGE_FAILURE", e.to_string())
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
    use crate::workload::{CreateWorkload, WorkloadRetryPolicy, WorkloadTimeouts};
    fn setup(
        name: &str,
    ) -> (
        PathBuf,
        PathBuf,
        WorkerMeshStore,
        WorkloadStore,
        SignedGrant,
    ) {
        let mesh = std::env::temp_dir().join(format!("mesh-{name}-{}.json", std::process::id()));
        let work =
            std::env::temp_dir().join(format!("mesh-work-{name}-{}.json", std::process::id()));
        let signer = GrantSigner::new("k", vec![4; 32]).unwrap();
        let subject = Subject::Service("registrar".into());
        let grant = Grant {
            contract_version: 1,
            id: "g".into(),
            issuer: Subject::Human("h".into()),
            subject,
            tenant_id: "t".into(),
            application_id: "a".into(),
            capabilities: [
                "worker:register".into(),
                "worker:capability:documents.extract".into(),
            ]
            .into(),
            resources: vec![ResourceScope {
                kind: ResourceScopeKind::ResourcePrefix,
                uri: "flow://a/workers/*".into(),
            }],
            constraints: GrantConstraints::default(),
            issued_at: 1,
            expires_at: 999,
            parent_grant: None,
            revision: 1,
            status: GrantStatus::Active,
        };
        (
            mesh.clone(),
            work.clone(),
            WorkerMeshStore::load(&mesh).unwrap(),
            WorkloadStore::load(&work).unwrap(),
            signer.sign(grant).unwrap(),
        )
    }
    fn register(store: &mut WorkerMeshStore, grant: SignedGrant) -> Worker {
        store
            .register(WorkerRegistration {
                tenant_id: "t".into(),
                application_id: "a".into(),
                name: "edge".into(),
                runtime: WorkerRuntimeKind::Customer,
                version: "1".into(),
                capabilities: ["documents.extract".into(), "database.admin".into()].into(),
                workload_kinds: BTreeSet::new(),
                resource_profile: Default::default(),
                labels: [("environment".into(), "production".into())].into(),
                capacity: WorkerCapacity {
                    concurrency: 1,
                    active: 0,
                },
                registered_by: Subject::Service("registrar".into()),
                authorization_grant: grant,
                registered_at: 10,
            })
            .unwrap()
    }
    #[test]
    fn registration_intersects_capabilities_and_lifecycle_is_strict() {
        let (m, w, mut mesh, _, grant) = setup("register");
        let worker = register(&mut mesh, grant);
        assert_eq!(
            worker.granted_capabilities,
            ["documents.extract".into()].into()
        );
        assert!(mesh
            .transition(&worker.worker_id, WorkerLifecycle::Busy)
            .is_err());
        mesh.heartbeat(
            &worker.worker_id,
            WorkerHeartbeat {
                worker_id: worker.worker_id.clone(),
                version: "1".into(),
                capability_version: 1,
                capacity: WorkerCapacity {
                    concurrency: 1,
                    active: 0,
                },
                health: WorkerHealth::Healthy,
                timestamp: 11,
                running_claims: BTreeSet::new(),
            },
            2,
        )
        .unwrap();
        let _ = fs::remove_file(m);
        let _ = fs::remove_file(w);
    }
    #[test]
    fn placement_claim_fencing_failure_and_recovery() {
        let (m, wpath, mut mesh, mut workloads, grant) = setup("claim");
        let worker = register(&mut mesh, grant.clone());
        mesh.heartbeat(
            &worker.worker_id,
            WorkerHeartbeat {
                worker_id: worker.worker_id.clone(),
                version: "1".into(),
                capability_version: 1,
                capacity: WorkerCapacity {
                    concurrency: 1,
                    active: 0,
                },
                health: WorkerHealth::Healthy,
                timestamp: 11,
                running_claims: BTreeSet::new(),
            },
            2,
        )
        .unwrap();
        let subject = grant.grant.subject.clone();
        let workload = workloads
            .create(CreateWorkload {
                tenant_id: "t".into(),
                application_id: "a".into(),
                environment: "production".into(),
                definition_id: "extract".into(),
                definition_revision: "r".into(),
                trigger: "manual".into(),
                input: serde_json::json!({"file":"artifact://x"}),
                requested_by: subject,
                session_id: None,
                authorization_snapshot: grant,
                capability_snapshot: ["documents.extract".into()].into(),
                connection_scopes: BTreeSet::new(),
                priority: 0,
                idempotency_key: "x".into(),
                retry_policy: WorkloadRetryPolicy::default(),
                timeouts: WorkloadTimeouts::default(),
                dependencies: vec![],
                not_before: None,
                created_at: 11,
            })
            .unwrap();
        let claim = mesh
            .place_and_claim(
                &mut workloads,
                &workload.workload_id,
                &worker.worker_id,
                12,
                1000,
            )
            .unwrap();
        assert!(validate_mesh_claim(
            &claim,
            mesh.workers.get(&worker.worker_id).unwrap(),
            workloads.get(&workload.workload_id).unwrap(),
            12
        )
        .is_ok());
        mesh.detect_failures(50, &mut workloads).unwrap();
        assert_eq!(
            validate_mesh_claim(
                &claim,
                mesh.workers.get(&worker.worker_id).unwrap(),
                workloads.get(&workload.workload_id).unwrap(),
                50
            )
            .unwrap_err()
            .code,
            "MESH_STALE_FENCE"
        );
        let _ = fs::remove_file(m);
        let _ = fs::remove_file(wpath);
    }
}
