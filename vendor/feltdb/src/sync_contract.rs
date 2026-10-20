//! FeltDB Sync v1: transport-neutral operation synchronization semantics.
use crate::authorization::{AuthorizationContext, Resource, SignedGrant, Subject};
use crate::state_contract::{SchemaCompatibility, TransactionOperation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const SYNC_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub device_id: String,
    pub tenant_id: String,
    pub application_id: String,
    pub subject: Subject,
    pub created_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncScope {
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default)]
    pub fields: BTreeMap<String, BTreeSet<String>>,
}
impl SyncScope {
    pub fn contains(&self, uri: &str) -> bool {
        self.resources.iter().any(|scope| {
            let prefix = scope.trim_end_matches('*').trim_end_matches('/');
            uri == prefix || uri.starts_with(&(prefix.to_owned() + "/"))
        })
    }
    pub fn hash(&self) -> String {
        hash_json(self)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncCursor {
    pub contract_version: u32,
    pub tenant_id: String,
    pub application_id: String,
    pub subject: Subject,
    pub scope_hash: String,
    pub position: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSession {
    pub session_id: String,
    pub device: DeviceIdentity,
    pub scope: SyncScope,
    pub cursor: SyncCursor,
    pub grant_id: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub status: SyncSessionStatus,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SyncSessionStatus {
    Active,
    Closed,
    Expired,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OutboxStatus {
    Pending,
    Sending,
    Acknowledged,
    Rejected,
    Conflicted,
    Retryable,
    PermanentlyFailed,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReconciliationOutcome {
    Accept,
    Merge,
    Conflict,
    Reject,
    Stale,
    Unauthorized,
    SchemaMismatch,
    AlreadyCommitted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncOperation {
    pub contract_version: u32,
    pub operation_id: String,
    pub tenant_id: String,
    pub application_id: String,
    pub subject: Subject,
    pub device_id: String,
    pub resource: Resource,
    pub operation: TransactionOperation,
    #[serde(default)]
    pub causal_context: BTreeMap<String, u64>,
    pub schema_version: u64,
    #[serde(default)]
    pub authorization_context: AuthorizationContext,
    pub offline_grant: SignedGrant,
    pub application_revision: String,
    pub created_at: i64,
}
impl SyncOperation {
    pub fn validate_identity(&self) -> Result<(), SyncError> {
        if self.contract_version != SYNC_CONTRACT_VERSION
            || self.operation_id.trim().is_empty()
            || self.device_id.trim().is_empty()
        {
            return Err(SyncError::new(
                "SYNC_INVALID",
                "operation identity is incomplete",
            ));
        }
        if self.resource.uri.contains("..")
            || !self
                .resource
                .uri
                .starts_with(&format!("flow://{}/", self.application_id))
        {
            return Err(SyncError::new(
                "SYNC_SCOPE_DENIED",
                "operation resource is outside its application",
            ));
        }
        Ok(())
    }
    pub fn hash(&self) -> String {
        hash_json(self)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncBatch {
    pub session_id: String,
    pub operations: Vec<SyncOperation>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncAck {
    pub operation_id: String,
    pub outcome: ReconciliationOutcome,
    pub server_position: Option<u64>,
    pub transaction_id: Option<String>,
    pub decision_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncRejection {
    pub operation_id: String,
    pub code: String,
    pub message: String,
    pub retryable: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConflict {
    pub operation_id: String,
    pub resource: String,
    pub local_causal_context: BTreeMap<String, u64>,
    pub server_causal_context: BTreeMap<String, u64>,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncResult {
    pub acknowledgements: Vec<SyncAck>,
    pub rejections: Vec<SyncRejection>,
    pub conflicts: Vec<SyncConflict>,
    pub cursor: SyncCursor,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncCheckpoint {
    pub device_id: String,
    pub cursor: SyncCursor,
    pub causal_frontier: BTreeMap<String, u64>,
    pub updated_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tombstone {
    pub resource: String,
    pub deletion_operation: String,
    pub causal_context: BTreeMap<String, u64>,
    pub schema_version: u64,
    pub server_position: u64,
    pub retain_until: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredSyncOperation {
    pub position: u64,
    pub operation: SyncOperation,
    pub outcome: ReconciliationOutcome,
    pub decision_id: Option<String>,
    pub committed_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncLease {
    pub device_id: String,
    pub owner_runtime: String,
    pub expires_at: i64,
    pub heartbeat_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncError {
    pub code: String,
    pub message: String,
}
impl SyncError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for SyncError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStore {
    #[serde(skip)]
    path: PathBuf,
    #[serde(default)]
    pub sessions: BTreeMap<String, SyncSession>,
    #[serde(default)]
    pub operations: Vec<StoredSyncOperation>,
    #[serde(default)]
    pub operation_positions: BTreeMap<String, u64>,
    #[serde(default)]
    pub checkpoints: BTreeMap<String, SyncCheckpoint>,
    #[serde(default)]
    pub tombstones: BTreeMap<String, Tombstone>,
    #[serde(default)]
    pub leases: BTreeMap<String, SyncLease>,
    #[serde(default)]
    next_position: u64,
}
impl SyncStore {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SyncError> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Ok(Self {
                path,
                sessions: BTreeMap::new(),
                operations: vec![],
                operation_positions: BTreeMap::new(),
                checkpoints: BTreeMap::new(),
                tombstones: BTreeMap::new(),
                leases: BTreeMap::new(),
                next_position: 0,
            });
        }
        let mut value: Self = serde_json::from_slice(&fs::read(&path).map_err(io)?).map_err(io)?;
        value.path = path;
        Ok(value)
    }
    fn persist(&self) -> Result<(), SyncError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(io)?;
        }
        let temporary = self.path.with_extension("tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(self).map_err(io)?).map_err(io)?;
        fs::rename(temporary, &self.path).map_err(io)
    }
    pub fn create_session(
        &mut self,
        device: DeviceIdentity,
        scope: SyncScope,
        grant: &SignedGrant,
        now: i64,
    ) -> Result<SyncSession, SyncError> {
        if grant.grant.subject != device.subject
            || grant.grant.tenant_id != device.tenant_id
            || grant.grant.application_id != device.application_id
            || grant.grant.expires_at <= now
        {
            return Err(SyncError::new(
                "SYNC_UNAUTHORIZED",
                "offline grant does not bind this device subject and application",
            ));
        }
        if !scope.resources.iter().all(|resource| {
            grant.grant.resources.iter().any(|allowed| {
                allowed.matches(&Resource {
                    uri: resource.trim_end_matches('*').trim_end_matches('/').into(),
                    attributes: BTreeMap::new(),
                })
            })
        }) {
            return Err(SyncError::new(
                "SYNC_SCOPE_DENIED",
                "requested projection exceeds grant resources",
            ));
        }
        let seed = format!(
            "{}:{}:{}:{}",
            device.device_id,
            grant.grant.id,
            scope.hash(),
            now
        );
        let id = format!("sync_{}", &hex(&Sha256::digest(seed))[..24]);
        let cursor = SyncCursor {
            contract_version: 1,
            tenant_id: device.tenant_id.clone(),
            application_id: device.application_id.clone(),
            subject: device.subject.clone(),
            scope_hash: scope.hash(),
            position: 0,
        };
        let session = SyncSession {
            session_id: id.clone(),
            device,
            scope,
            cursor,
            grant_id: grant.grant.id.clone(),
            created_at: now,
            expires_at: grant.grant.expires_at,
            status: SyncSessionStatus::Active,
        };
        self.sessions.insert(id, session.clone());
        self.persist()?;
        Ok(session)
    }
    pub fn session(&self, id: &str, now: i64) -> Result<&SyncSession, SyncError> {
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| SyncError::new("SYNC_SESSION_INVALID", "session not found"))?;
        if session.status != SyncSessionStatus::Active || session.expires_at <= now {
            return Err(SyncError::new(
                "SYNC_SESSION_EXPIRED",
                "session is not active",
            ));
        }
        Ok(session)
    }
    pub fn committed(&self, id: &str) -> Option<u64> {
        self.operation_positions.get(id).copied()
    }
    pub fn record(
        &mut self,
        operation: SyncOperation,
        outcome: ReconciliationOutcome,
        decision_id: Option<String>,
        now: i64,
    ) -> Result<u64, SyncError> {
        if let Some(position) = self.committed(&operation.operation_id) {
            return Ok(position);
        }
        self.next_position += 1;
        let position = self.next_position;
        if matches!(
            operation.operation.kind,
            crate::state_contract::TransactionOperationKind::Delete
        ) {
            self.tombstones.insert(
                operation.resource.uri.clone(),
                Tombstone {
                    resource: operation.resource.uri.clone(),
                    deletion_operation: operation.operation_id.clone(),
                    causal_context: operation.causal_context.clone(),
                    schema_version: operation.schema_version,
                    server_position: position,
                    retain_until: now + 2_592_000,
                },
            );
        }
        self.operation_positions
            .insert(operation.operation_id.clone(), position);
        self.operations.push(StoredSyncOperation {
            position,
            operation,
            outcome,
            decision_id,
            committed_at: now,
        });
        self.persist()?;
        Ok(position)
    }
    pub fn pull(
        &self,
        session_id: &str,
        cursor: &SyncCursor,
        limit: usize,
        now: i64,
    ) -> Result<(Vec<StoredSyncOperation>, SyncCursor), SyncError> {
        let session = self.session(session_id, now)?;
        if cursor.tenant_id != session.device.tenant_id
            || cursor.application_id != session.device.application_id
            || cursor.subject != session.device.subject
            || cursor.scope_hash != session.scope.hash()
        {
            return Err(SyncError::new(
                "SYNC_CURSOR_SCOPE_DENIED",
                "cursor was issued for a different authorization projection",
            ));
        }
        let operations = self
            .operations
            .iter()
            .filter(|stored| {
                stored.position > cursor.position
                    && stored.operation.tenant_id == cursor.tenant_id
                    && stored.operation.application_id == cursor.application_id
                    && session.scope.contains(&stored.operation.resource.uri)
            })
            .take(limit.min(1000))
            .cloned()
            .collect::<Vec<_>>();
        let position = operations
            .last()
            .map(|v| v.position)
            .unwrap_or(cursor.position);
        let mut next = cursor.clone();
        next.position = position;
        Ok((operations, next))
    }
    pub fn acknowledge(
        &mut self,
        session_id: &str,
        cursor: SyncCursor,
        now: i64,
    ) -> Result<SyncCheckpoint, SyncError> {
        let session = self.session(session_id, now)?;
        if cursor.scope_hash != session.scope.hash() || cursor.position > self.next_position {
            return Err(SyncError::new(
                "SYNC_CURSOR_INVALID",
                "acknowledgement cursor is invalid",
            ));
        }
        let checkpoint = SyncCheckpoint {
            device_id: session.device.device_id.clone(),
            cursor,
            causal_frontier: BTreeMap::new(),
            updated_at: now,
        };
        self.checkpoints
            .insert(checkpoint.device_id.clone(), checkpoint.clone());
        self.persist()?;
        Ok(checkpoint)
    }
    pub fn close(&mut self, id: &str) -> Result<(), SyncError> {
        let session = self
            .sessions
            .get_mut(id)
            .ok_or_else(|| SyncError::new("SYNC_SESSION_INVALID", "session not found"))?;
        session.status = SyncSessionStatus::Closed;
        self.persist()
    }
    pub fn acquire_lease(
        &mut self,
        device_id: &str,
        runtime: &str,
        now: i64,
        ttl: i64,
    ) -> Result<SyncLease, SyncError> {
        if self
            .leases
            .get(device_id)
            .is_some_and(|lease| lease.expires_at > now && lease.owner_runtime != runtime)
        {
            return Err(SyncError::new(
                "SYNC_LEASE_HELD",
                "another runtime owns this device outbox",
            ));
        }
        let lease = SyncLease {
            device_id: device_id.into(),
            owner_runtime: runtime.into(),
            expires_at: now + ttl,
            heartbeat_at: now,
        };
        self.leases.insert(device_id.into(), lease.clone());
        self.persist()?;
        Ok(lease)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub operation: SyncOperation,
    pub status: OutboxStatus,
    pub attempts: u32,
    pub last_error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalSyncRuntime {
    #[serde(skip)]
    path: PathBuf,
    #[serde(default)]
    pub state: BTreeMap<String, Value>,
    #[serde(default)]
    pub outbox: BTreeMap<String, OutboxEntry>,
    #[serde(default)]
    pub tombstones: BTreeMap<String, Tombstone>,
}
impl LocalSyncRuntime {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SyncError> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            return Ok(Self {
                path,
                state: BTreeMap::new(),
                outbox: BTreeMap::new(),
                tombstones: BTreeMap::new(),
            });
        }
        let mut value: Self = serde_json::from_slice(&fs::read(&path).map_err(io)?).map_err(io)?;
        value.path = path;
        Ok(value)
    }
    fn persist(&self) -> Result<(), SyncError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(io)?;
        }
        let temp = self.path.with_extension("tmp");
        fs::write(&temp, serde_json::to_vec_pretty(self).map_err(io)?).map_err(io)?;
        fs::rename(temp, &self.path).map_err(io)
    }
    pub fn apply_and_enqueue(&mut self, operation: SyncOperation) -> Result<(), SyncError> {
        operation.validate_identity()?;
        if self.outbox.contains_key(&operation.operation_id) {
            return Ok(());
        }
        match operation.operation.kind {
            crate::state_contract::TransactionOperationKind::Delete => {
                self.state.remove(&operation.resource.uri);
            }
            _ => {
                self.state.insert(
                    operation.resource.uri.clone(),
                    operation.operation.value.clone(),
                );
            }
        }
        self.outbox.insert(
            operation.operation_id.clone(),
            OutboxEntry {
                operation,
                status: OutboxStatus::Pending,
                attempts: 0,
                last_error: None,
            },
        );
        self.persist()
    }
    pub fn apply_result(&mut self, result: &SyncResult) -> Result<(), SyncError> {
        for ack in &result.acknowledgements {
            if let Some(entry) = self.outbox.get_mut(&ack.operation_id) {
                entry.status = OutboxStatus::Acknowledged;
            }
        }
        for rejection in &result.rejections {
            if let Some(entry) = self.outbox.get_mut(&rejection.operation_id) {
                entry.status = if rejection.retryable {
                    OutboxStatus::Retryable
                } else {
                    OutboxStatus::Rejected
                };
                entry.last_error = Some(format!("{}: {}", rejection.code, rejection.message));
            }
        }
        for conflict in &result.conflicts {
            if let Some(entry) = self.outbox.get_mut(&conflict.operation_id) {
                entry.status = OutboxStatus::Conflicted;
                entry.last_error = Some(conflict.reason.clone());
            }
        }
        self.persist()
    }
    pub fn pending(&self) -> Vec<SyncOperation> {
        self.outbox
            .values()
            .filter(|v| matches!(v.status, OutboxStatus::Pending | OutboxStatus::Retryable))
            .map(|v| v.operation.clone())
            .collect()
    }
}

pub fn schema_sync_outcome(
    client: u64,
    server: u64,
    compatibility: SchemaCompatibility,
) -> ReconciliationOutcome {
    if client == server {
        ReconciliationOutcome::Accept
    } else {
        match compatibility {
            SchemaCompatibility::Safe => ReconciliationOutcome::Accept,
            SchemaCompatibility::RequiresMigration => ReconciliationOutcome::Merge,
            SchemaCompatibility::Destructive | SchemaCompatibility::Incompatible => {
                ReconciliationOutcome::SchemaMismatch
            }
        }
    }
}
fn io(error: impl std::fmt::Display) -> SyncError {
    SyncError::new("SYNC_STORAGE_FAILURE", error.to_string())
}
fn hash_json(value: &impl Serialize) -> String {
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
    use crate::state_contract::TransactionOperationKind;
    fn fixture() -> (GrantSigner, SignedGrant, SyncOperation) {
        let signer = GrantSigner::new("k", vec![9; 32]).unwrap();
        let grant = Grant {
            contract_version: 1,
            id: "g".into(),
            issuer: Subject::Human("h".into()),
            subject: Subject::Human("u".into()),
            tenant_id: "t".into(),
            application_id: "a".into(),
            capabilities: ["state:write".into(), "state:read".into()].into(),
            resources: vec![ResourceScope {
                kind: ResourceScopeKind::ResourcePrefix,
                uri: "flow://a/tasks/*".into(),
            }],
            constraints: GrantConstraints::default(),
            issued_at: 1,
            expires_at: 999,
            parent_grant: None,
            revision: 1,
            status: GrantStatus::Active,
        };
        let signed = signer.sign(grant).unwrap();
        let operation = SyncOperation {
            contract_version: 1,
            operation_id: "op1".into(),
            tenant_id: "t".into(),
            application_id: "a".into(),
            subject: Subject::Human("u".into()),
            device_id: "d".into(),
            resource: Resource {
                uri: "flow://a/tasks/1".into(),
                attributes: BTreeMap::new(),
            },
            operation: TransactionOperation {
                kind: TransactionOperationKind::Insert,
                collection: "tasks".into(),
                id: "1".into(),
                value: serde_json::json!({"title":"one"}),
                if_version: None,
            },
            causal_context: BTreeMap::new(),
            schema_version: 1,
            authorization_context: AuthorizationContext {
                timestamp: 10,
                ..Default::default()
            },
            offline_grant: signed.clone(),
            application_revision: "r1".into(),
            created_at: 10,
        };
        (signer, signed, operation)
    }
    #[test]
    fn local_outbox_is_durable_and_idempotent() {
        let path = std::env::temp_dir().join(format!("feltdb-sync-{}.json", std::process::id()));
        let (_, _, op) = fixture();
        let mut local = LocalSyncRuntime::load(&path).unwrap();
        local.apply_and_enqueue(op.clone()).unwrap();
        local.apply_and_enqueue(op).unwrap();
        let recovered = LocalSyncRuntime::load(&path).unwrap();
        assert_eq!(recovered.outbox.len(), 1);
        assert!(recovered.state.contains_key("flow://a/tasks/1"));
        let _ = fs::remove_file(path);
    }
    #[test]
    fn cursor_is_bound_to_projection_and_retry_is_duplicate() {
        let path =
            std::env::temp_dir().join(format!("feltdb-sync-store-{}.json", std::process::id()));
        let (_, grant, op) = fixture();
        let device = DeviceIdentity {
            device_id: "d".into(),
            tenant_id: "t".into(),
            application_id: "a".into(),
            subject: Subject::Human("u".into()),
            created_at: 1,
        };
        let scope = SyncScope {
            resources: vec!["flow://a/tasks/*".into()],
            fields: BTreeMap::new(),
        };
        let mut store = SyncStore::load(&path).unwrap();
        let session = store.create_session(device, scope, &grant, 10).unwrap();
        let first = store
            .record(op.clone(), ReconciliationOutcome::Accept, None, 11)
            .unwrap();
        let duplicate = store
            .record(op, ReconciliationOutcome::AlreadyCommitted, None, 12)
            .unwrap();
        assert_eq!(first, duplicate);
        let (pulled, cursor) = store
            .pull(&session.session_id, &session.cursor, 100, 12)
            .unwrap();
        assert_eq!(pulled.len(), 1);
        let mut wrong = session.cursor.clone();
        wrong.scope_hash = "other".into();
        assert_eq!(
            store
                .pull(&session.session_id, &wrong, 10, 12)
                .unwrap_err()
                .code,
            "SYNC_CURSOR_SCOPE_DENIED"
        );
        store.acknowledge(&session.session_id, cursor, 12).unwrap();
        let _ = fs::remove_file(path);
    }
    #[test]
    fn selective_pull_excludes_other_resources_and_lease_prevents_duplicate_workers() {
        let path =
            std::env::temp_dir().join(format!("feltdb-sync-selective-{}.json", std::process::id()));
        let (_, grant, first) = fixture();
        let device = DeviceIdentity {
            device_id: "d".into(),
            tenant_id: "t".into(),
            application_id: "a".into(),
            subject: Subject::Human("u".into()),
            created_at: 1,
        };
        let scope = SyncScope {
            resources: vec!["flow://a/tasks/*".into()],
            fields: BTreeMap::new(),
        };
        let mut store = SyncStore::load(&path).unwrap();
        let session = store.create_session(device, scope, &grant, 10).unwrap();
        store
            .record(first, ReconciliationOutcome::Accept, None, 11)
            .unwrap();
        let mut outside = fixture().2;
        outside.operation_id = "op2".into();
        outside.resource.uri = "flow://a/private/1".into();
        store
            .record(outside, ReconciliationOutcome::Accept, None, 11)
            .unwrap();
        let (pulled, _) = store
            .pull(&session.session_id, &session.cursor, 100, 12)
            .unwrap();
        assert_eq!(pulled.len(), 1);
        store.acquire_lease("d", "tab-a", 12, 30).unwrap();
        assert_eq!(
            store.acquire_lease("d", "tab-b", 13, 30).unwrap_err().code,
            "SYNC_LEASE_HELD"
        );
        let _ = fs::remove_file(path);
    }
    #[test]
    fn delete_creates_retained_tombstone_and_schema_outcomes_are_explicit() {
        let path =
            std::env::temp_dir().join(format!("feltdb-sync-tombstone-{}.json", std::process::id()));
        let (_, _, mut operation) = fixture();
        operation.operation.kind = TransactionOperationKind::Delete;
        operation.operation.value = Value::Null;
        let mut store = SyncStore::load(&path).unwrap();
        store
            .record(operation, ReconciliationOutcome::Accept, None, 20)
            .unwrap();
        let tombstone = store.tombstones.get("flow://a/tasks/1").unwrap();
        assert_eq!(tombstone.deletion_operation, "op1");
        assert_eq!(
            schema_sync_outcome(1, 2, SchemaCompatibility::Safe),
            ReconciliationOutcome::Accept
        );
        assert_eq!(
            schema_sync_outcome(1, 2, SchemaCompatibility::RequiresMigration),
            ReconciliationOutcome::Merge
        );
        assert_eq!(
            schema_sync_outcome(1, 2, SchemaCompatibility::Incompatible),
            ReconciliationOutcome::SchemaMismatch
        );
        let _ = fs::remove_file(path);
    }
    #[test]
    fn rejected_and_conflicted_results_remain_inspectable_after_restart() {
        let path =
            std::env::temp_dir().join(format!("feltdb-sync-result-{}.json", std::process::id()));
        let (_, _, operation) = fixture();
        let mut local = LocalSyncRuntime::load(&path).unwrap();
        local.apply_and_enqueue(operation).unwrap();
        let result = SyncResult {
            acknowledgements: vec![],
            rejections: vec![],
            conflicts: vec![SyncConflict {
                operation_id: "op1".into(),
                resource: "flow://a/tasks/1".into(),
                local_causal_context: BTreeMap::new(),
                server_causal_context: [("server".into(), 2)].into(),
                reason: "concurrent".into(),
            }],
            cursor: SyncCursor {
                contract_version: 1,
                tenant_id: "t".into(),
                application_id: "a".into(),
                subject: Subject::Human("u".into()),
                scope_hash: "s".into(),
                position: 2,
            },
        };
        local.apply_result(&result).unwrap();
        let recovered = LocalSyncRuntime::load(&path).unwrap();
        assert_eq!(recovered.outbox["op1"].status, OutboxStatus::Conflicted);
        let _ = fs::remove_file(path);
    }
}
