#[cfg(test)]
mod acceptance_tests;
mod acquisition;
pub mod admission;
#[cfg(test)]
mod admission_contract_tests;
pub mod adversarial_transport;
pub mod analytics;
pub mod application;
pub mod application_runtime;
#[cfg(not(target_arch = "wasm32"))]
pub mod authority;
pub mod authority_failover;
pub mod authorization;
#[cfg(test)]
mod authorization_security_tests;
pub mod capabilities;
mod capability;
mod cardinality_diagnostics;
pub mod cardinality_endpoint;
pub mod causal_backlog_bound;
pub mod causal_dependency_barrier;
#[cfg(test)]
mod causal_dependency_barrier_phase_7_1;
pub mod concurrency_fuzzing;
pub mod consistency_contract;
mod content_distribution;
pub mod convergence;
pub mod crash_atomic_boundary;
pub mod crash_injection;
#[cfg(test)]
mod crash_recovery_tests;
mod cron;
pub mod dedup_bound_investigation;
pub mod distributed_indexing;
#[cfg(test)]
mod distributed_tests;
pub mod distributed_transactions;
pub mod durability_guarantees;
pub mod durable_dedup_set;
pub mod durable_operation_identity;
pub mod durable_operation_log;
pub mod durable_sync;
pub mod equality_index;
mod execution;
pub mod in_process_transport;
pub mod indexing;
#[cfg(test)]
mod managed_cas_tests;
mod materialization;
pub mod metrics;
pub mod multi_node_convergence;
pub mod multi_operation_transaction;
pub mod observability;
mod operation;
pub mod operation_algebra;
pub mod operation_log;
pub mod p1_application_atomicity;
pub mod p1_atomicity_acceptance;
pub mod partition_reconciliation;
mod peer_registry;
pub mod permutation_scheduler;
pub mod persistence_reality;
#[cfg(test)]
mod phase1b_acceptance;
#[cfg(test)]
mod phase1c1_acceptance;
#[cfg(test)]
mod phase1c2_acceptance;
#[cfg(test)]
mod phase1c3_acceptance;
#[cfg(test)]
mod phase1c_atomicity_proof;
#[cfg(test)]
mod phase5_integration;
#[cfg(test)]
mod phase5_scenarios;
#[cfg(test)]
mod phase6_adversarial_scenarios;
#[cfg(test)]
mod phase6_convergence_validator;
#[cfg(test)]
mod phase6_persistence;
#[cfg(test)]
#[cfg(test)]
mod phase_1c_real_tcp;
#[cfg(test)]
mod phase_2a_failures;
#[cfg(test)]
mod phase_2b_network;
#[cfg(test)]
mod phase_2c_cascading;
#[cfg(test)]
mod phase_3_durability;
#[cfg(test)]
mod phase_4_baseline;
#[cfg(test)]
mod phase_5_soak;
pub mod policy_evaluation;
pub mod production_api;
mod provenance;
pub mod query_execution_diagnostics;
pub mod query_performance;
pub mod read_basis;
mod references;
pub mod replay_fuzzing;
pub mod replica_acknowledgements;
pub mod replica_membership;
pub mod replication_manager;
pub mod replication_protocol;
mod routing;
pub mod semantic_decision;
pub mod semantic_query;
pub mod sharding;
pub mod agent_state;
pub mod agent_transition_contract;
pub mod state_conflict_contract;
pub mod state_contract;
pub mod state_diff_contract;
pub mod state_facade;
pub mod state_hash;
pub mod state_model;
pub mod state_transition_store;
pub mod state_trigger;
mod storage;
pub mod submission;
mod sync;
pub mod sync_contract;
#[cfg(not(target_arch = "wasm32"))]
pub mod tcp_transport;
pub mod transaction_api;
#[cfg(test)]
mod transaction_invariants;
pub mod transaction_preconditions;
pub mod transactions;
mod trigger;
pub mod trigger_contract;
pub mod versioned_object_substrate;
pub mod worker_mesh;
mod workflow;
#[cfg(test)]
mod workflow_acceptance_tests;
mod workflow_integration;
mod workflow_runtime;
pub mod workload;
pub mod workload_diagnostics;

pub use acquisition::{
    AcquisitionDeduplicator, AcquisitionPolicy, AcquisitionResult, ContentRequest, ContentResponse,
    ContentTransferError,
};
pub use admission::{ProductionRetryPolicy, RejectionReason, SubmissionOutcome};
pub use adversarial_transport::{
    AdversarialTransport, ConnectionId, ConnectivityMatrix, FaultSeed, InMemoryTransport,
    LinkState, MessageId, NetworkEvent, NetworkSchedule, NodeId, ReplicationTransport,
};
pub use analytics::{
    AutoTuner, IndexAnalytics, IndexMetrics, PerformanceReport, QueryTrace, TuningRecommendation,
};
pub use capability::{
    CapabilityAccessModel, CapabilityCheckpoint, CapabilityContext, CapabilityExecutionContext,
    CapabilityExecutionResult, CapabilityId, CapabilityLifecycleState, CapabilityMetadata,
    CapabilityOperationResult, CapabilityQuery, CapabilityQueryResult, CapabilityRef,
    CapabilityRegistry, CapabilityRequirements, CapabilityResolver, CapabilityRole,
    DistributedCapabilityAdvertisement, ExecutionRef, FlowCapability, StateRequirement,
};
pub use causal_dependency_barrier::{
    ApplyResult, CausalDependencyBarrier, OperationState, PendingOperation, ReceiveResult,
    StateCounters,
};
pub use consistency_contract::{
    CommutativeVerificationResult, ConsensusEnforcementResult, ConsensusRequirementChecker,
    ConsistencyContractEnforcer, DeterministicMergeVerificationResult,
    NoDowngradeVerificationResult, OperationApplication,
};
pub use content_distribution::{
    CapabilityLocationRegistry, ContentAcquisitionManager, ContentAddressableStore,
    DistributedReferenceResolver, ProjectionDistribution,
};
pub use convergence::{
    CausalHistory, ConflictResolution, ConflictResolutionStrategy, MergeResult, StateVersion,
    VectorClock,
};
pub use crash_atomic_boundary::{
    CrashAtomicExecutor, CrashBoundaryPoint, CrashRecoveryState, InvariantCheckResult,
    RecoveryDecision,
};
pub use cron::{CronSchedule, CronScheduler};
pub use distributed_indexing::{
    ConsistencyLevel, ConsistencyProtocol, DistributedIndexManager, DistributedSyncStatus,
    IndexOperation, IndexReplicationState, IndexSyncMessage, IndexVectorClock, RemoteIndexMetadata,
    RemoteIndexState, ReplicationStatus, VersionConflict,
};
pub use distributed_transactions::{
    DistributedTransactionExecutor, EnvelopeId, ReplicaState, ReplicationMessage,
    TransactionEnvelope,
};
pub use equality_index::{EqualityIndex, EqualityIndexStats, IndexKey};
pub use execution::{Execution, ExecutionQueue, ExecutionStatus, RetryPolicy};
pub use indexing::{IndexConfig, IndexManager, IndexStats, IndexType};
pub use materialization::{MaterializationHandler, ReactiveCollectionIntegration};
pub use multi_node_convergence::{
    ConvergenceAggregation, ConvergenceResult, MultiNodeConvergenceSimulator, NodeExecutionResult,
};
pub use operation::{MutationOrder, Operation, OperationType, DEFAULT_MUTATION_ORDER};
pub use operation_algebra::{
    ConflictType, ConvergenceSemantic, OperationAlgebra, OperationAlgebraRegistry,
};
pub use operation_log::{MemoryOperationLog, OperationLogEntry};
pub use partition_reconciliation::{
    DeterminismCheckResult, PartitionDeterminismChecker, PartitionTrace, PartitionedReplica,
    ReconciliationResult, ReplicaPartition,
};
pub use peer_registry::PeerRegistry;
pub use permutation_scheduler::{OperationSchedule, PermutationScheduler, ScheduleStrategy};
pub use persistence_reality::{
    CorruptionDetectionResult, DurablePersistenceLayer, FsyncDurabilityResult,
    PersistenceContractVerifier, PersistenceCrashPoint, RecoveryResult, WALEntry, WriteResult,
};
pub use provenance::{
    AcquisitionMetadata, DerivedState, DerivedStateProvenance, DerivedValue, OperationSource,
};
pub use references::{
    ContentIdentity, ContentRange, FlowRef, PeerAdvertisement, PeerId, ReferenceResolver,
    ResolutionMetadata, ResolutionResult,
};
pub use routing::{
    CapabilityRouter, ExecutionClaim, ExecutionClaimer, ExecutionResultPublisher, PeerStateVersion,
    RouteDecision,
};
pub use semantic_decision::{
    authorize_context, configured_decision_runtime, decide_record, decision_cache_key,
    deterministic_request_key, reuse_record_judgment,
    execute_decision_batch, execute_decision_transport, execute_inline_decision_transport, jev,
    run_choice_diagnostics, run_decision_benchmark, validate_decision_definition,
    validate_decision_result, BinaryDecisionResult, ChoiceDecisionResult, ChoiceDiagnosticsReport,
    ChoiceExpansionObservation, ChoicePermutationObservation, DecisionAuthorizationConfig,
    DecisionAuthorizationDecision, DecisionAuthorizationGate, DecisionBatch, DecisionBatchDecision,
    DecisionBatchEvaluation, DecisionBenchmarkCase, DecisionBenchmarkReport, DecisionContextPacket,
    DecisionDefinition, DecisionError, DecisionEvaluation, DecisionEvidence, DecisionEvidenceField,
    DecisionExecutionMethod, DecisionJudgmentRecord, DecisionKind, DecisionOptions,
    DecisionPrincipal, DecisionProvenance, DecisionRecordRef, DecisionRequest, DecisionResult,
    DecisionRuntime, DecisionRuntimeCapabilities, DecisionRuntimeMetadata, DecisionStateReference,
    DecisionStatus, DecisionTransportAuthorization, DecisionTransportExecutionRequest,
    DecisionTransportExecutionResponse, DecisionTransportRuntime, DeterministicDecisionRuntime,
    ScoreDecisionResult, WeightedOption, DEFAULT_DECISION_COLLECTION,
};
pub use semantic_query::{
    compose_semantic_page, semantic_clause_hash, semantic_definition_hash,
    validate_semantic_clause, SemanticAnnotation, SemanticCandidate, SemanticOrderDirection,
    SemanticPage, SemanticQueryClause, SemanticQueryError, SemanticQueryFilter,
    SemanticQueryMode, SemanticQueryOrder, SemanticStageSummary, SemanticUnevaluated,
    SemanticUnevaluatedReason, MAX_SEMANTIC_CANDIDATES,
};
pub use sharding::{
    HotspotAlert, RebalanceOperation, ShardDistributionSummary, ShardId, ShardKey, ShardManager,
    ShardMetrics, ShardRange, ShardingStrategy,
};
pub use state_conflict_contract::{
    classify_states, StateConflictInput, StateConflictOutput, STATE_CONFLICT_CONTRACT_ID,
    STATE_CONFLICT_CONTRACT_VERSION,
};
pub use state_diff_contract::{
    diff_states, StateDiffInput, StateDiffOutput, STATE_DIFF_CONTRACT_ID,
    STATE_DIFF_CONTRACT_VERSION,
};
pub use state_facade::FeltDBStateSystem;
pub use state_hash::{CanonicalState, StateHash};
pub use state_model::{
    apply_reconciliation_plan, path_relation, paths_overlap, reconcile, resolve_path, ChangeKind,
    ConflictClass, ConflictClassification, GarbageCollectionError, GarbageCollectionReport,
    MaterializationError, MaterializationProjection, PathComponent, PathConflict, PathRelation,
    ReconciliationPlan, RefError, Relationship, SemanticChange, SemanticDiff, StateId, StateObject,
    StateReconciliationResult, StateRef, StateRevision, StateStore, StateTopology,
    STATE_MODEL_VERSION,
};
pub use state_transition_store::{MemoryStateTransitionStore, StateTransitionRecord};
pub use state_trigger::{
    StateTrigger, StateTriggerChangeKind, StateTriggerError, StateTriggerPredicate,
    StateTriggerStore, TriggeredWorkload,
};
pub use storage::{CheckpointData, FileStorage, MemoryStorage, Storage};
pub use submission::{SubmissionManager, SubmissionMetrics};
pub use sync::{ChangeLog, Conflict, ConflictDetector, PeerState, SyncMessage, SyncState};
pub use transaction_api::{TransactionManager, TxError, TxResult};
pub use transactions::{
    CommitId, CommitValidator, ConsistencyContract, OperationCommand, OperationId, StateDependency,
    StateTransition, TransactionExecutor, TransitionResult,
};
pub use trigger::{Trigger, TriggerFilter, TriggerRegistry};
pub use trigger_contract::{
    canonical_input_bytes, canonical_output_bytes, evaluate_canonical, evaluate_trigger,
    ContractChange, ContractChangeKind, ContractErrorBody, ContractErrorCode, ContractPredicate,
    ContractRuntime, ContractTrigger, MatchOutcome, MatchReason, TriggerMatchInput,
    TriggerMatchOutput, TRIGGER_MATCH_CONTRACT_ID, TRIGGER_MATCH_CONTRACT_VERSION,
};
pub use versioned_object_substrate::{
    DurableObjectStore, DurableRef, LocalObjectStore, Reachability, S3CompatibleBucket,
    S3CompatibleObjectStore, SubstrateError, VersionedObjectSubstrate,
};
pub use workflow::{
    BlockedReason, WorkflowGraph, WorkflowInstance, WorkflowNode, WorkflowOperation, WorkflowRef,
    WorkflowState, WorkflowStepRef, WorkflowStepState,
};
pub use workflow_integration::{
    WorkflowCronTrigger, WorkflowIntegration, WorkflowProvenance, WorkflowTrigger,
};
pub use workflow_runtime::{WorkflowOperationEntry, WorkflowRuntime};

use crate::state_contract::AuthorizationContext;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::any::type_name;
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt::{Display, Formatter};
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::ops::Bound::{Excluded, Unbounded};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

pub type Result<T> = std::result::Result<T, FlowError>;

#[derive(Debug)]
pub enum FlowError {
    Io(std::io::Error),
    Serde(serde_json::Error),
    CorruptLogLine(Box<LogCorruption>),
    CapabilityError(String),
    /// A transaction precondition did not hold. Nothing was written.
    ///
    /// Separate from `CapabilityError` because a conflict is an expected
    /// outcome of a race and a caller is meant to branch on it, while a
    /// capability error is not.
    PreconditionFailed(Box<PreconditionFailure>),
    /// The transaction was based on a revision which is no longer current.
    RevisionConflict {
        expected: u64,
        actual: u64,
    },
    /// A backup artifact was rejected. Nothing was restored.
    BackupRejected(Box<BackupProblem>),
    /// The durable database is in a format this build does not understand.
    ///
    /// Returned by [`FeltDb::open`] **before any record is interpreted**, and
    /// before anything is written. A database that returns this error is
    /// untouched.
    IncompatibleFormat(Box<FormatIncompatibility>),
}

impl Display for FlowError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            FlowError::Io(e) => write!(f, "io error: {e}"),
            FlowError::Serde(e) => write!(f, "serde error: {e}"),
            FlowError::CorruptLogLine(corruption) => write!(f, "CORRUPT_DURABLE_LOG: {corruption}"),
            FlowError::CapabilityError(msg) => write!(f, "capability error: {msg}"),
            FlowError::PreconditionFailed(failure) => write!(f, "PRECONDITION_FAILED: {failure}"),
            FlowError::RevisionConflict { expected, actual } => write!(
                f,
                "REVISION_CONFLICT: expected authority revision {expected}, current revision is {actual}"
            ),
            FlowError::IncompatibleFormat(incompatibility) => {
                write!(f, "INCOMPATIBLE_DURABLE_FORMAT: {incompatibility}")
            }
            FlowError::BackupRejected(problem) => write!(f, "BACKUP_REJECTED: {problem}"),
        }
    }
}

impl std::error::Error for FlowError {}

impl From<std::io::Error> for FlowError {
    fn from(value: std::io::Error) -> Self {
        FlowError::Io(value)
    }
}

impl From<serde_json::Error> for FlowError {
    fn from(value: serde_json::Error) -> Self {
        FlowError::Serde(value)
    }
}

impl From<String> for FlowError {
    fn from(value: String) -> Self {
        FlowError::CapabilityError(value)
    }
}

#[derive(Clone)]
pub struct FeltDb {
    inner: Arc<Mutex<Inner>>,
    event_tx: broadcast::Sender<ChangeEvent>,
    _ownership: Arc<File>,
}

struct OpenDatabase {
    inner: Weak<Mutex<Inner>>,
    event_tx: broadcast::Sender<ChangeEvent>,
    ownership: Weak<File>,
}

static OPEN_DATABASES: OnceLock<Mutex<HashMap<PathBuf, OpenDatabase>>> = OnceLock::new();

#[derive(Default)]
#[allow(dead_code)] // Reserved runtime subsystems are initialized before their public APIs land.
struct Inner {
    path: PathBuf,
    rows: HashMap<String, BTreeMap<String, StoredRow>>,
    key_counters: HashMap<String, u64>,
    query_count_by_type: HashMap<String, usize>,
    adaptive_indexes: HashSet<String>,
    instance_id: String,
    sequence: u64,
    /// Monotonic, authority-assigned commit order. Unlike `sequence`, one
    /// transaction advances this exactly once regardless of its mutation count.
    authority_revision: u64,
    sync_state: SyncState,
    change_log: ChangeLog,
    execution_queue: ExecutionQueue,
    trigger_registry: TriggerRegistry,
    cron_scheduler: CronScheduler,
    capability_registry: CapabilityRegistry,
    peer_registry: PeerRegistry,
    content_store: ContentAddressableStore,
    capability_locations: CapabilityLocationRegistry,
    bootstrapped: bool,
    applied_transactions: HashSet<String>,
    transaction_payload_hashes: HashMap<String, String>,
    transaction_revisions: HashMap<String, (u64, u64)>,
    /// Recent commits, and who asked for them.
    ///
    /// Every transaction already records its actor: the audit document written
    /// into the durable log carries `subject`, tenant, application and
    /// revision. Until this existed that document was written and never read
    /// back — replay parsed the record and discarded the audit — so the
    /// database could say *what* changed and never *who changed it*.
    ///
    /// Bounded on purpose. This answers "who did this, recently", which is the
    /// question a human asks while operating a running system; it is not an
    /// audit archive, and the durable log remains the record of what happened.
    /// Keeping it bounded is what makes it safe to maintain on the commit path.
    transaction_attributions: VecDeque<TransactionAttribution>,
    /// Complete committed transaction order, rebuilt exclusively from the
    /// durable transaction log. Unlike the bounded operator window above this
    /// is the canonical replay/provenance index.
    transaction_history: Vec<TransactionAttribution>,
    collection_cardinality: HashMap<String, u64>,
    /// Derived equality index over `rows`. Never durable, never replicated, and
    /// never consulted for anything but candidate selection: it is maintained
    /// inside the same state mutation boundary as `rows` and rebuilt from
    /// `rows` after recovery.
    equality_index: EqualityIndex,
    /// What replaying the durable log had to do to succeed.
    ///
    /// Recorded rather than logged, so an operator can ask after the fact
    /// whether this database opened cleanly.
    log_recovery: LogRecovery,
    /// The durable format this database was accepted as, recorded at the open
    /// that accepted it.
    durable_format: DurableFormat,
    /// How far a single-record write is pushed before it returns.
    durability_mode: DurabilityMode,
    /// Writes since the last stable-storage barrier, for grouped durability.
    writes_since_barrier: u32,
    /// When a policy-driven compaction should rewrite the durable log.
    compaction_policy: CompactionPolicy,
    /// Operations pruned since the last log rewrite.
    pruned_since_rewrite: usize,
    /// Each resource's newest revision, as `(id, sequence)`.
    ///
    /// Derived from the `state` rows exactly like `equality_index` is derived
    /// from `rows`: never durable, rebuilt after recovery. It exists so that
    /// minting a revision does not have to scan a resource's history to find
    /// the parent it descends from.
    revision_heads: HashMap<String, (state_model::StateId, u64)>,
}

/// The state lock, held with its wait and hold time attributable.
///
/// FeltDB serializes every read and every write behind one mutex, so "how long
/// did an operation take" and "how long did it make everything else wait" are
/// different questions. PR34 showed the second one mattered: a query holding the
/// lock while it cloned a collection inflated the cost of unrelated writes.
///
/// This guard answers both. It records the time spent *waiting* to acquire —
/// which is pure contention, the one span where no work happens at all — and the
/// time spent *holding*, which bounds how long anything else can be forced to
/// wait. Both are off unless `FELTDB_WORKLOAD_DIAGNOSTICS=1`, where acquisition
/// costs one relaxed atomic load and no clock read.
///
/// It dereferences to [`Inner`], so every existing use site reads unchanged.
struct StateGuard<'a> {
    inner: std::sync::MutexGuard<'a, Inner>,
    held_since: Option<std::time::Instant>,
}

impl std::ops::Deref for StateGuard<'_> {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.inner
    }
}

impl std::ops::DerefMut for StateGuard<'_> {
    fn deref_mut(&mut self) -> &mut Inner {
        &mut self.inner
    }
}

impl Drop for StateGuard<'_> {
    fn drop(&mut self) {
        if let Some(since) = self.held_since {
            workload_diagnostics::record(
                workload_diagnostics::Phase::StateLockHold,
                since.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            );
        }
    }
}

/// The single state mutation boundary for authoritative records.
///
/// Every path that changes a record — insert, update, delete, CAS, transaction
/// commit, replicated operation apply, log replay, snapshot install — goes
/// through these three methods, and they change the record map and the derived
/// equality index together while the caller still holds the state lock. That is
/// the whole atomicity argument: there is no window in which a committed record
/// exists without its index entries, or an index entry survives its record,
/// because no code path can write one without the other.
///
/// The index is deliberately maintained here rather than through an independent
/// index API. An independent API would have to be called, correctly, by nine
/// separate mutation paths; a missed call would be a silent divergence that only
/// a query would reveal. Routing every write through one boundary makes the
/// omission impossible to write in the first place.
impl Inner {
    /// Write one record, replacing any record already at its key.
    ///
    /// Returns `true` when this created a record rather than replacing one, which
    /// is what collection cardinality accounting needs.
    fn put_row(&mut self, row: StoredRow) -> bool {
        let capability = row.capability.clone();
        let key = row.key.clone();
        let indexed = self.equality_index.indexes_collection(&capability);
        let mutation = workload_diagnostics::span(workload_diagnostics::Phase::RecordMutation);
        let bucket = self.rows.entry(capability.clone()).or_default();
        let previous = bucket.insert(key.clone(), row);
        drop(mutation);
        if previous.is_none() {
            *self
                .collection_cardinality
                .entry(capability.clone())
                .or_default() += 1;
        }
        if !indexed {
            return previous.is_none();
        }
        // From here on the work exists only because an index does. Measuring it
        // apart from the record write is what makes "did index maintenance
        // become a write-side cost?" a question with an answer.
        let _index = workload_diagnostics::span(workload_diagnostics::Phase::IndexMaintenance);
        // Old entries first, then new ones. Doing it in this order means a
        // record whose indexed field did not change is removed from and
        // re-added to the same bucket, which is a no-op rather than a gap.
        if let Some(previous) = &previous {
            self.equality_index
                .remove_record(&capability, &key, &previous.value);
        }
        // Disjoint field borrows: the record map is read while the index is
        // written. Nothing is cloned to maintain the index.
        let Inner {
            rows,
            equality_index,
            ..
        } = self;
        if let Some(current) = rows.get(&capability).and_then(|rows| rows.get(&key)) {
            equality_index.insert_record(&capability, &key, &current.value);
        }
        previous.is_none()
    }

    /// Remove one record. Returns `true` when a record was actually removed.
    fn remove_row(&mut self, capability: &str, key: &str) -> bool {
        let mutation = workload_diagnostics::span(workload_diagnostics::Phase::RecordMutation);
        let Some(removed) = self
            .rows
            .get_mut(capability)
            .and_then(|bucket| bucket.remove(key))
        else {
            return false;
        };
        drop(mutation);
        let count = self
            .collection_cardinality
            .entry(capability.to_string())
            .or_default();
        *count = count.saturating_sub(1);
        let _index = workload_diagnostics::span(workload_diagnostics::Phase::IndexMaintenance);
        self.equality_index
            .remove_record(capability, key, &removed.value);
        true
    }

    /// Drop all records, as a snapshot install does. The index follows.
    fn clear_rows(&mut self) {
        self.rows.clear();
        self.collection_cardinality.clear();
        self.equality_index.clear();
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomicMutation {
    pub capability: String,
    pub key: String,
    pub value: Option<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomicPrecondition {
    pub capability: String,
    pub key: String,
    pub expected_version: Option<u64>,
}

/// What must be true of one record for a transaction to commit.
///
/// Distinct from `AtomicPrecondition`, and deliberately so. That one compares
/// the row's internal operation *sequence*, and `None` on it means "this record
/// must not exist". This one compares the fields a caller can actually read
/// back -- the document's `__version`, its authority epoch, its lease -- which
/// are the same fields single-record CAS compares. Plumbing a caller's
/// `expectedVersion` into the sequence check would have compared a document
/// version against a storage sequence: two different numbers that happen to
/// share a name.
///
/// Every predicate is optional and only a supplied one is checked. That differs
/// from CAS, where an absent `expected_lease_id` asserts the record is *not*
/// leased; here an absent predicate asserts nothing at all. The difference is
/// deliberate -- a transaction precondition is a fence the caller opts into,
/// not a full description of the record -- and it is why the fields are named
/// `expected_*` rather than describing a state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecordPrecondition {
    pub capability: String,
    pub key: String,
    /// The record must not exist. Mutually exclusive with the predicates below.
    #[serde(default)]
    pub require_absent: bool,
    /// The document's `__version`, as `updateIfVersion` compares it.
    #[serde(default)]
    pub expected_version: Option<u64>,
    /// The record's authority epoch, at `/authority/epoch`.
    #[serde(default)]
    pub expected_epoch: Option<u64>,
    /// The id of an unexpired lease held on the record.
    #[serde(default)]
    pub expected_lease_id: Option<String>,
}

impl RecordPrecondition {
    /// True when this precondition asks for nothing, which is a caller error
    /// rather than a satisfied fence: a precondition that constrains nothing
    /// would read as protection and provide none.
    pub fn is_empty(&self) -> bool {
        !self.require_absent
            && self.expected_version.is_none()
            && self.expected_epoch.is_none()
            && self.expected_lease_id.is_none()
    }
}

/// State a decision was read from: records by identity, and equality
/// predicates over a collection (every record of `collection` whose fields
/// equal these values, including records that do not exist yet).
///
/// Generic: it names collections, ids, fields, and values, never what they mean.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReadSet {
    #[serde(default)]
    pub records: Vec<ReadRecord>,
    #[serde(default)]
    pub predicates: Vec<ReadPredicate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadRecord {
    pub collection: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadPredicate {
    pub collection: String,
    #[serde(rename = "where")]
    pub conditions: Vec<ReadCondition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadCondition {
    pub field: String,
    pub eq: Value,
}

/// A read set and what the authority held for it at one moment.
///
/// The token is opaque and meaningful only to the authority that issued it. A
/// transaction fenced by a basis commits only if re-reading the same set,
/// inside the commit's lock, yields the same token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadBasis {
    pub reads: ReadSet,
    pub token: String,
}

/// Token prefix for read bases this authority issues.
pub const READ_BASIS_PREFIX: &str = "fr1:";
/// Largest number of records plus predicates one read set may name.
pub const READ_SET_LIMIT: usize = 1_000;

impl ReadSet {
    /// Validate, de-duplicate, and order, so two descriptions of the same reads
    /// are one read set and one token.
    pub fn normalised(&self) -> Result<ReadSet> {
        let valid = |segment: &str| {
            !segment.is_empty()
                && segment.len() <= 128
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        };
        let mut records: BTreeMap<(String, String), ReadRecord> = BTreeMap::new();
        for record in &self.records {
            if !valid(&record.collection) || !valid(&record.id) {
                return Err(FlowError::CapabilityError(format!(
                    "INVALID_READ_SET:invalid read {}:{}",
                    record.collection, record.id
                )));
            }
            records.insert(
                (record.collection.clone(), record.id.clone()),
                record.clone(),
            );
        }
        let mut predicates: BTreeMap<(String, String), ReadPredicate> = BTreeMap::new();
        for predicate in &self.predicates {
            if !valid(&predicate.collection) || predicate.conditions.is_empty() {
                return Err(FlowError::CapabilityError(format!(
                    "INVALID_READ_SET:a predicate over {} must name at least one field",
                    predicate.collection
                )));
            }
            let mut conditions = predicate.conditions.clone();
            if conditions
                .iter()
                .any(|condition| condition.field.is_empty())
            {
                return Err(FlowError::CapabilityError(
                    "INVALID_READ_SET:a predicate condition requires a field".into(),
                ));
            }
            conditions.sort_by(|left, right| left.field.cmp(&right.field));
            let identity = serde_json::to_string(&conditions).unwrap_or_default();
            predicates.insert(
                (predicate.collection.clone(), identity),
                ReadPredicate {
                    collection: predicate.collection.clone(),
                    conditions,
                },
            );
        }
        if records.is_empty() && predicates.is_empty() {
            return Err(FlowError::CapabilityError(
                "INVALID_READ_SET:a read set that names nothing fences nothing".into(),
            ));
        }
        if records.len() + predicates.len() > READ_SET_LIMIT {
            return Err(FlowError::CapabilityError(format!(
                "INVALID_READ_SET:a read set may name at most {READ_SET_LIMIT} records and predicates"
            )));
        }
        Ok(ReadSet {
            records: records.into_values().collect(),
            predicates: predicates.into_values().collect(),
        })
    }
}

/// Where a read set's logical collections and ids live in storage.
///
/// One read set, one token semantics; only the storage addressing differs
/// between the routes that commit transactions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadLayout {
    /// The direct transaction route: rows under `collection`, keyed `collection:id`.
    Direct,
    /// An application state namespace: rows under `namespace:collection`, keyed by id.
    Namespace(String),
}

impl ReadLayout {
    fn capability(&self, collection: &str) -> String {
        match self {
            Self::Direct => collection.to_string(),
            Self::Namespace(namespace) => format!("{namespace}:{collection}"),
        }
    }

    fn key(&self, collection: &str, id: &str) -> String {
        match self {
            Self::Direct => format!("{collection}:{id}"),
            Self::Namespace(_) => id.to_string(),
        }
    }

    /// The authority scope a token is valid in. A token is never evaluated in
    /// another scope: it is refused as invalid there, not compared.
    fn scope_tag(&self) -> String {
        match self {
            Self::Direct => "d".into(),
            Self::Namespace(namespace) => {
                let digest = format!(
                    "{:x}",
                    Sha256::digest(format!("namespace\0{namespace}").as_bytes())
                );
                format!("n{}", &digest[..16])
            }
        }
    }

    fn token_prefix(&self) -> String {
        format!("{READ_BASIS_PREFIX}{}:", self.scope_tag())
    }
}

/// What the state a read set names is, now, as one token. Rows are read in
/// full; tombstoned rows are absent; only the named records and the named
/// collections' matching rows are read.
fn read_basis_token(
    rows: &HashMap<String, BTreeMap<String, StoredRow>>,
    reads: &ReadSet,
    layout: &ReadLayout,
) -> String {
    let live = |capability: &str, key: &str| {
        rows.get(capability)
            .and_then(|bucket| bucket.get(key))
            .filter(|row| !row.deleted)
            .map(|row| row.value.clone())
    };
    let records: Vec<Value> = reads
        .records
        .iter()
        .map(|record| {
            let value = live(
                &layout.capability(&record.collection),
                &layout.key(&record.collection, &record.id),
            );
            serde_json::json!([record.collection, record.id, value])
        })
        .collect();
    let predicates: Vec<Value> = reads
        .predicates
        .iter()
        .map(|predicate| {
            let matching: Vec<Value> = rows
                .get(&layout.capability(&predicate.collection))
                .map(|bucket| {
                    bucket
                        .iter()
                        .filter(|(_, row)| !row.deleted)
                        .filter(|(_, row)| {
                            predicate.conditions.iter().all(|condition| {
                                row.value.get(&condition.field) == Some(&condition.eq)
                            })
                        })
                        .map(|(key, row)| serde_json::json!([key, row.value]))
                        .collect()
                })
                .unwrap_or_default();
            serde_json::json!([predicate.collection, predicate.conditions, matching])
        })
        .collect();
    let observed = serde_json::json!({ "records": records, "predicates": predicates });
    format!(
        "{}{:x}",
        layout.token_prefix(),
        Sha256::digest(serde_json::to_vec(&observed).unwrap_or_default())
    )
}

/// Which predicate failed, and what the authority actually holds.
///
/// Structured rather than a formatted string, because the caller has to tell a
/// lost race from a broken deployment: the first is retried with fresh state,
/// the second is not retried at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "predicate")]
pub enum PreconditionFailure {
    /// Required absent, but the record exists.
    Present { collection: String, key: String },
    /// A predicate was given for a record that does not exist.
    Missing { collection: String, key: String },
    Version {
        collection: String,
        key: String,
        expected: u64,
        actual: u64,
    },
    Epoch {
        collection: String,
        key: String,
        expected: u64,
        actual: u64,
    },
    Lease {
        collection: String,
        key: String,
        expected: String,
        actual: Option<String>,
    },
    /// State named by a read basis changed after the basis was read.
    ///
    /// The tokens are kept for diagnostics inside the authority but never
    /// serialized: a refusal says that the basis changed, not what it digests.
    Fence {
        collection: String,
        key: String,
        #[serde(skip_serializing, default)]
        expected: String,
        #[serde(skip_serializing, default)]
        actual: String,
    },
}

impl PreconditionFailure {
    pub fn collection(&self) -> &str {
        match self {
            Self::Present { collection, .. }
            | Self::Missing { collection, .. }
            | Self::Version { collection, .. }
            | Self::Epoch { collection, .. }
            | Self::Lease { collection, .. }
            | Self::Fence { collection, .. } => collection,
        }
    }
    /// The storage key, which is namespaced as `collection:id`.
    pub fn key(&self) -> &str {
        match self {
            Self::Present { key, .. }
            | Self::Missing { key, .. }
            | Self::Version { key, .. }
            | Self::Epoch { key, .. }
            | Self::Lease { key, .. }
            | Self::Fence { key, .. } => key,
        }
    }
}

impl Display for PreconditionFailure {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Present { collection, key } => {
                write!(f, "{collection}/{key} was required to be absent but exists")
            }
            Self::Missing { collection, key } => write!(f, "{collection}/{key} does not exist"),
            Self::Version {
                collection,
                key,
                expected,
                actual,
            } => write!(
                f,
                "{collection}/{key} is at version {actual}, expected {expected}"
            ),
            Self::Epoch {
                collection,
                key,
                expected,
                actual,
            } => write!(
                f,
                "{collection}/{key} is at epoch {actual}, expected {expected}"
            ),
            Self::Lease {
                collection,
                key,
                expected,
                actual,
            } => write!(
                f,
                "{collection}/{key} holds lease {actual:?}, expected {expected}"
            ),
            Self::Fence { collection, .. } => write!(
                f,
                "state read by this transaction has changed (read basis over {collection})"
            ),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtomicCommit {
    pub transaction_id: String,
    pub base_revision: u64,
    pub commit_revision: u64,
    pub status: TransactionStatus,
    pub state_before: u64,
    pub state_after: u64,
    pub rows: Vec<StoredRow>,
    pub duplicate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransactionStatus {
    Committed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum JsonCasResult {
    Updated {
        current_version: u64,
        current_epoch: u64,
        value: Value,
    },
    VersionConflict {
        current_version: u64,
        current_epoch: u64,
    },
    AuthorityConflict {
        current_version: u64,
        current_epoch: u64,
    },
    LeaseConflict {
        current_version: u64,
        current_epoch: u64,
    },
    ReplicationConflict {
        current_version: u64,
        current_epoch: u64,
    },
    NotFound,
}
/// How many recent commits keep their attribution in memory.
///
/// Sized to cover what an operator reads in one sitting rather than to be a
/// log. A larger window costs memory on every instance to answer a question the
/// durable log already answers more completely.
const TRANSACTION_ATTRIBUTION_WINDOW: usize = 512;

/// How many mutated keys one attribution names before it stops listing them.
///
/// A bulk transaction can touch thousands of records; naming them all would
/// make one commit's attribution larger than the rest of the window. The count
/// is always exact, so a truncated list is visible as a truncated list.
const TRANSACTION_ATTRIBUTION_KEYS: usize = 32;

/// Who committed a transaction, and what it touched.
///
/// Projected from the audit document the transaction already writes. It carries
/// no record values: attribution answers *who, when, and to what*, and the
/// values are reachable through the record and revision APIs that authorize
/// each read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionAttribution {
    pub transaction_id: String,
    /// Stable operation identities assigned to the rows in this transaction.
    /// These are distinct from both transaction and revision identity.
    #[serde(default)]
    pub operation_ids: Vec<String>,
    /// The actor the caller authenticated as, when the transaction named one.
    pub subject: Option<String>,
    pub tenant_id: Option<String>,
    pub application_id: Option<String>,
    /// The application contract revision the transaction was executed against.
    pub application_revision: Option<String>,
    pub base_revision: u64,
    pub commit_revision: u64,
    pub state_before: u64,
    pub state_after: u64,
    /// `capability:key` for each mutation, truncated at
    /// [`TRANSACTION_ATTRIBUTION_KEYS`].
    pub keys: Vec<String>,
    /// How many mutations the transaction made, whether or not `keys` lists
    /// them all.
    pub operations: usize,
    #[serde(default)]
    pub principal_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub delegator_id: Option<String>,
    #[serde(default)]
    pub work_id: Option<String>,
    /// Milliseconds since the epoch, matching `StoredRow::unix_ms`.
    pub unix_ms: u128,
    /// Canonical transaction metadata exactly as committed. Record values are
    /// deliberately absent; stable work/session/agent/operation references
    /// live here so historical provenance survives process loss.
    #[serde(default)]
    pub audit: Option<Value>,
}

impl TransactionAttribution {
    /// Builds an attribution from a transaction record's own audit document.
    fn from_record(
        transaction_id: &str,
        audit: Option<&Value>,
        base_revision: u64,
        commit_revision: u64,
        state_before: u64,
        state_after: u64,
        rows: &[StoredRow],
        unix_ms: u128,
    ) -> Self {
        let text = |field: &str| {
            audit
                .and_then(|value| value.get(field))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        TransactionAttribution {
            transaction_id: transaction_id.to_string(),
            operation_ids: rows
                .iter()
                .filter_map(|row| row.operation.as_ref())
                .map(|operation| {
                    format!("operation:{}:{}", operation.instance_id, operation.sequence)
                })
                .collect(),
            subject: text("subject"),
            tenant_id: text("tenant"),
            application_id: text("application"),
            application_revision: text("revision"),
            base_revision,
            commit_revision,
            state_before,
            state_after,
            keys: rows
                .iter()
                .take(TRANSACTION_ATTRIBUTION_KEYS)
                .map(|row| format!("{}:{}", row.capability, row.key))
                .collect(),
            operations: rows.len(),
            principal_id: text("principal_id"),
            agent_id: text("agent_id"),
            session_id: text("session_id"),
            delegator_id: text("delegator_id"),
            work_id: text("work_id").or_else(|| text("workload_id")),
            unix_ms,
            audit: audit.cloned(),
        }
    }

    /// Whether this transaction touched a record, as far as `keys` records it.
    pub fn touched(&self, capability: &str, key: &str) -> bool {
        let target = format!("{capability}:{key}");
        self.keys.iter().any(|held| *held == target)
    }
}

/// Upgrade compacted attribution written before operation identities became a
/// first-class part of canonical history. The mapping is deterministic from
/// durable transaction data, so reopening never regenerates a different ID.
fn normalize_transaction_attribution(
    mut attribution: TransactionAttribution,
) -> TransactionAttribution {
    if attribution.operation_ids.is_empty() {
        if attribution.transaction_id.starts_with("operation:") {
            let operation_id = attribution.transaction_id.clone();
            attribution.transaction_id = operation_id.replacen("operation:", "transaction:", 1);
            attribution.operation_ids.push(operation_id);
        } else {
            attribution.operation_ids = (1..=attribution.operations)
                .map(|index| {
                    format!(
                        "operation:legacy-transaction:{}:{index}",
                        attribution.transaction_id
                    )
                })
                .collect();
        }
    }
    attribution
}

/// Records an attribution, discarding the oldest once the window is full.
fn remember_attribution(inner: &mut Inner, attribution: TransactionAttribution) {
    inner.transaction_history.push(attribution.clone());
    if inner.transaction_attributions.len() >= TRANSACTION_ATTRIBUTION_WINDOW {
        inner.transaction_attributions.pop_front();
    }
    inner.transaction_attributions.push_back(attribution);
}

fn remember_operation_attribution(
    inner: &mut Inner,
    row: &StoredRow,
    base_revision: u64,
    commit_revision: u64,
) {
    let Some(operation) = row.operation.as_ref() else {
        return;
    };
    remember_attribution(
        inner,
        TransactionAttribution {
            transaction_id: format!(
                "transaction:{}:{}",
                operation.instance_id, operation.sequence
            ),
            operation_ids: vec![format!(
                "operation:{}:{}",
                operation.instance_id, operation.sequence
            )],
            subject: None,
            tenant_id: None,
            application_id: None,
            application_revision: operation
                .revision
                .as_ref()
                .map(|revision| revision.id.clone()),
            base_revision,
            commit_revision,
            state_before: operation.sequence.saturating_sub(1),
            state_after: operation.sequence,
            keys: vec![format!("{}:{}", row.capability, row.key)],
            operations: 1,
            principal_id: None,
            agent_id: None,
            session_id: None,
            delegator_id: None,
            work_id: None,
            unix_ms: row.unix_ms,
            audit: None,
        },
    );
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TransactionLogRecord {
    record_type: String,
    transaction_id: String,
    #[serde(default)]
    payload_hash: Option<String>,
    state_before: u64,
    state_after: u64,
    #[serde(default)]
    base_revision: u64,
    #[serde(default)]
    commit_revision: u64,
    rows: Vec<StoredRow>,
    #[serde(default)]
    audit: Option<Value>,
}

/// An event representing a change to the database (insert or update of a record).
///
/// This event is broadcast to all live query subscribers so they can update their results.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChangeEvent {
    /// The capability namespace of the changed record.
    pub capability: String,
    /// The key of the changed record.
    pub key: String,
    /// The Rust type name of the changed record.
    pub rust_type: String,
    /// The timestamp of the change in milliseconds since UNIX epoch.
    pub unix_ms: u128,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredRow {
    pub capability: String,
    pub key: String,
    pub rust_type: String,
    pub value: Value,
    pub unix_ms: u128,
    /// Content hash for deduplication and content addressing
    #[serde(default)]
    pub content_hash: Option<String>,
    /// Reference identity if this row represents a referenced object
    #[serde(default)]
    pub flow_ref: Option<FlowRef>,
    /// Tombstone marker used to durably replay deletions.
    #[serde(default)]
    pub deleted: bool,
    /// Canonical mutation which produced this row. Older logs remain readable.
    #[serde(default)]
    pub operation: Option<Operation>,
}

/// When a compaction should actually rewrite the durable log.
///
/// Pruning acknowledged operations from memory is cheap. **Rewriting the log is
/// not**: it serializes every row in the database and fsyncs it while holding
/// the one lock that serializes every read and every write. Doing that on a
/// short timer stops the world on that timer.
///
/// The policy separates the two. Pruning still happens whenever it can; the
/// rewrite waits until enough has accumulated to be worth the stall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionPolicy {
    /// Operations that must have been pruned since the last rewrite before the
    /// next one is worth its cost.
    pub rewrite_after_pruned: usize,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        // Chosen so a busy database rewrites occasionally rather than
        // constantly. At one operation per write this is one rewrite per
        // thousand acknowledged writes instead of one every timer tick.
        CompactionPolicy {
            rewrite_after_pruned: 1000,
        }
    }
}

/// What a policy-driven compaction did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionOutcome {
    /// Nothing was acknowledged by every active peer; there was no work.
    Idle,
    /// Operations were pruned from memory and the log rewrite was **deferred**,
    /// because the accumulated gain does not yet justify the stall.
    Deferred {
        pruned: usize,
        pending_since_rewrite: usize,
    },
    /// The durable log was rewritten.
    Rewritten { pruned: usize },
}

impl CompactionOutcome {
    /// Operations pruned from the in-memory log by this call.
    pub fn pruned(&self) -> usize {
        match self {
            CompactionOutcome::Idle => 0,
            CompactionOutcome::Deferred { pruned, .. }
            | CompactionOutcome::Rewritten { pruned } => *pruned,
        }
    }

    /// Whether this call rewrote the durable log.
    pub fn rewrote_log(&self) -> bool {
        matches!(self, CompactionOutcome::Rewritten { .. })
    }
}

/// How far a single-record write is pushed before it returns.
///
/// This is a **contract**, not a tuning knob: it decides what a successful
/// mutation means. Nothing here makes a claim about surviving power loss —
/// see [`DurabilityMode::guarantee`] for what each mode does and does not
/// establish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DurabilityMode {
    /// Write and flush to the operating system. **The default.**
    ///
    /// The bytes leave the process before the call returns, so a committed
    /// write survives the process dying. They are not forced to the device.
    Flushed,
    /// Write, flush, and `fsync` before returning.
    ///
    /// Issues a stable-storage barrier per mutation. Whether that barrier
    /// actually reaches the platter depends on the filesystem, the mount
    /// options and the drive's write cache — none of which this database can
    /// observe, so this mode buys a barrier, not a proof.
    Synced,
    /// Flush every write, and `fsync` once every `every` writes.
    ///
    /// Amortises the barrier. The cost is an explicit window: up to `every - 1`
    /// acknowledged writes may not yet have been through one.
    Grouped { every: u32 },
}

impl Default for DurabilityMode {
    fn default() -> Self {
        DurabilityMode::Flushed
    }
}

impl DurabilityMode {
    /// What a successful mutation guarantees under this mode, stated so that a
    /// caller can read the contract rather than infer it from an API name.
    pub fn guarantee(&self) -> &'static str {
        match self {
            DurabilityMode::Flushed => {
                "the record has left the process; it survives a process crash.                  Stable-storage durability is not established."
            }
            DurabilityMode::Synced => {
                "a stable-storage barrier was issued for this record. Whether it                  reached the device depends on the storage stack, which this                  database cannot observe."
            }
            DurabilityMode::Grouped { .. } => {
                "the record has left the process. A stable-storage barrier is                  issued periodically, so an acknowledged write may not yet have                  been through one."
            }
        }
    }

    /// How many acknowledged writes may not yet have been through a barrier.
    ///
    /// `None` where no barrier is issued at all.
    pub fn unbarriered_window(&self) -> Option<u32> {
        match self {
            DurabilityMode::Flushed => None,
            DurabilityMode::Synced => Some(0),
            DurabilityMode::Grouped { every } => Some(every.saturating_sub(1)),
        }
    }
}

/// What is actually known about this database's durable storage.
///
/// Every variant corresponds to a fact the open established. There is no
/// variant meaning "probably fine", and none that says anything about power
/// loss — that is unproven, and health may not manufacture a guarantee the
/// database does not have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageHealth {
    /// Every durable byte replayed as a complete, valid record.
    Clean,
    /// Operational, **and the last write did not land**.
    ///
    /// The log ended in an incomplete append, which was discarded. The database
    /// is usable and is not in the same condition as one that replayed cleanly,
    /// and reporting them identically is what makes an operator unable to tell
    /// a healthy database from one that lost its most recent write.
    RecoveredIncompleteWrite {
        byte_offset: u64,
        discarded_bytes: usize,
    },
}

impl StorageHealth {
    /// Whether the durable log replayed without discarding anything.
    pub fn is_clean(&self) -> bool {
        matches!(self, StorageHealth::Clean)
    }

    /// A short stable label for an operational surface.
    pub fn label(&self) -> &'static str {
        match self {
            StorageHealth::Clean => "clean",
            StorageHealth::RecoveredIncompleteWrite { .. } => "recovered-incomplete-write",
        }
    }
}

impl Display for StorageHealth {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageHealth::Clean => write!(f, "durable log replayed cleanly"),
            StorageHealth::RecoveredIncompleteWrite {
                byte_offset,
                discarded_bytes,
            } => write!(
                f,
                "durable log ended in an incomplete append: {discarded_bytes} bytes at offset \
                 {byte_offset} were discarded, so the last write did not land"
            ),
        }
    }
}

/// What this database knows about itself, as facts rather than labels.
///
/// Deliberately small. It reports only conditions the open actually
/// established, and it is not an operations dashboard: replication health,
/// backup freshness and capacity are not here, because none of them has a
/// defined operational contract yet and health must not invent one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseHealth {
    /// The durable format this database was accepted as.
    ///
    /// A database in a format this build does not understand never opens, so
    /// reaching this at all is itself the compatibility fact.
    pub durable_format: DurableFormat,
    /// What replaying the durable log had to do.
    pub recovery: LogRecovery,
    /// The storage condition that follows from it.
    pub storage: StorageHealth,
}

impl DatabaseHealth {
    /// Whether this database is in the condition a clean open produces.
    ///
    /// **Not** a claim that the database is correct, replicated, backed up or
    /// durable against power loss. It says the durable log replayed with
    /// nothing discarded, and nothing more.
    pub fn is_nominal(&self) -> bool {
        self.storage.is_clean()
    }
}

/// The backup artifact format this build writes and understands.
///
/// Versioned separately from the on-disk database: a backup is an artifact an
/// operator keeps, moves and restores long after the database that produced it,
/// so its compatibility boundary is its own.
pub const BACKUP_VERSION: u32 = 1;

const BACKUP_RECORD_TYPE: &str = "feltdb.backup.v1";

/// The first line of a backup artifact. Everything needed to verify the rest.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackupHeader {
    record_type: String,
    backup_version: u32,
    /// The durable format the rows are in.
    format_version: u32,
    created_unix_ms: u64,
    row_count: usize,
    observed_versions: HashMap<String, u64>,
    /// Digest of the rows exactly as written. Detects any alteration.
    content_digest: String,
    /// Digest of what the database *means* — see [`BackupVerification`].
    meaning_digest: String,
}

/// Why a backup artifact was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupProblem {
    /// What is wrong, in terms of the artifact rather than the parser.
    pub reason: String,
    /// Where, when the problem has a location.
    pub line_number: Option<usize>,
    /// What the operator can do.
    pub action: String,
}

impl Display for BackupProblem {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self.line_number {
            Some(line) => write!(f, "record {line}: {}. {}", self.reason, self.action),
            None => write!(f, "{}. {}", self.reason, self.action),
        }
    }
}

/// What a backup artifact contains, established by reading it.
///
/// `meaning_digest` is the part that matters. A content digest proves the file
/// is intact; it does not prove that restoring it produces the same database.
/// The meaning digest covers what the database *is*, independent of how it was
/// written down:
///
/// - every live record: capability, key and canonical value;
/// - every revision: its resource, identity, parent, sequence and content
///   identity, and the authority that produced it;
/// - every resource's retention policy and horizon.
///
/// It deliberately excludes wall-clock timestamps and sync envelopes, which
/// differ between two databases that mean the same thing. Restore recomputes it
/// from the **restored database** and refuses if it disagrees with the
/// artifact, so a restore proves equivalence rather than mere parseability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupVerification {
    pub backup_version: u32,
    pub format_version: u32,
    pub created_unix_ms: u64,
    pub rows: usize,
    pub content_digest: String,
    pub meaning_digest: String,
}

fn backup_problem(reason: &str, line_number: Option<usize>, action: &str) -> FlowError {
    FlowError::BackupRejected(Box::new(BackupProblem {
        reason: reason.to_string(),
        line_number,
        action: action.to_string(),
    }))
}

/// Digest the rows exactly as they will be written.
fn backup_content_digest(rows: &[StoredRow]) -> Result<String> {
    let mut hasher = Sha256::new();
    for row in rows {
        hasher.update(serde_json::to_vec(row)?);
        hasher.update(b"\n");
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Digest what a set of rows *means*, independent of how it was written.
///
/// Canonical by construction: rows are ordered by capability and key, and only
/// the fields that carry meaning take part. Two databases with the same digest
/// hold the same records, the same revision graph and the same retention state.
fn backup_meaning_digest(rows: &[StoredRow]) -> Result<String> {
    let mut meaningful: Vec<(&str, &str, String)> = rows
        .iter()
        .filter(|row| !row.deleted)
        .map(|row| {
            (
                row.capability.as_str(),
                row.key.as_str(),
                row.value.to_string(),
            )
        })
        .collect();
    meaningful.sort();

    let mut hasher = Sha256::new();
    hasher.update(b"feltdb.backup.meaning.v1");
    for (capability, key, value) in meaningful {
        hasher.update(capability.as_bytes());
        hasher.update([0u8]);
        hasher.update(key.as_bytes());
        hasher.update([0u8]);
        hasher.update(value.as_bytes());
        hasher.update([0u8]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Read and check a backup artifact **without opening it as a database**.
///
/// Structural and semantic checks only, and it never writes: verifying a backup
/// must be something an operator can do to an artifact they are not yet willing
/// to restore.
pub fn verify_backup(path: &Path) -> Result<BackupVerification> {
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines();

    let Some(header_line) = lines.next() else {
        return Err(backup_problem(
            "the artifact is empty",
            None,
            "Restore from a different backup.",
        ));
    };
    let header: BackupHeader = serde_json::from_str(header_line).map_err(|error| {
        backup_problem(
            &format!("the header is not a FeltDB backup header: {error}"),
            Some(1),
            "Check that this file is a FeltDB backup and was not truncated at the start.",
        )
    })?;
    if header.record_type != BACKUP_RECORD_TYPE {
        return Err(backup_problem(
            &format!("unexpected record type {}", header.record_type),
            Some(1),
            "Check that this file is a FeltDB backup.",
        ));
    }
    if header.backup_version != BACKUP_VERSION {
        return Err(backup_problem(
            &format!(
                "backup format version {} is not the version this build reads ({BACKUP_VERSION})",
                header.backup_version
            ),
            Some(1),
            "Restore with a FeltDB build that reads this backup version.",
        ));
    }
    if header.format_version != DURABLE_FORMAT_VERSION {
        return Err(backup_problem(
            &format!(
                "the rows are in durable format version {} and this build requires version {DURABLE_FORMAT_VERSION}",
                header.format_version
            ),
            Some(1),
            "Restore with a FeltDB build matching the format the backup was taken in.",
        ));
    }

    let mut rows = Vec::with_capacity(header.row_count);
    for (index, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: StoredRow = serde_json::from_str(line).map_err(|error| {
            backup_problem(
                &format!("not a durable record: {error}"),
                Some(index + 2),
                "The backup is damaged. Restore from another copy.",
            )
        })?;
        rows.push(row);
    }

    if rows.len() != header.row_count {
        return Err(backup_problem(
            &format!(
                "the header declares {} records and the artifact holds {}",
                header.row_count,
                rows.len()
            ),
            None,
            "The backup is truncated or was appended to. Restore from another copy.",
        ));
    }

    let content_digest = backup_content_digest(&rows)?;
    if content_digest != header.content_digest {
        return Err(backup_problem(
            "the records do not match the digest in the header",
            None,
            "The backup has been altered or corrupted. Restore from another copy.",
        ));
    }

    let meaning_digest = backup_meaning_digest(&rows)?;
    if meaning_digest != header.meaning_digest {
        return Err(backup_problem(
            "the records do not mean what the header says they mean",
            None,
            "The backup has been altered. Restore from another copy.",
        ));
    }

    Ok(BackupVerification {
        backup_version: header.backup_version,
        format_version: header.format_version,
        created_unix_ms: header.created_unix_ms,
        rows: rows.len(),
        content_digest,
        meaning_digest,
    })
}

/// Where durable corruption was found, and what is wrong with it.
///
/// Carries a location so an operator can find the damage, and an excerpt so
/// they can recognise it, without the error having to quote a whole record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogCorruption {
    /// 1-based line number in the durable log.
    pub line_number: usize,
    /// Byte offset of the start of the record.
    pub byte_offset: u64,
    /// What is wrong, in terms of the record rather than the parser.
    pub reason: String,
    /// The beginning of the record, truncated.
    pub excerpt: String,
}

impl Display for LogCorruption {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "durable log record {} at byte {} is corrupt: {}. Excerpt: {}. \
             The database was not opened and was not modified.",
            self.line_number, self.byte_offset, self.reason, self.excerpt
        )
    }
}

/// What replaying the durable log had to do to succeed.
///
/// A clean open and an open that discarded an incomplete final append are
/// different facts, and the difference must survive past an internal debug
/// message: an operator needs to know their database opened *and* that its last
/// write did not land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogRecovery {
    /// Every durable byte was a complete, valid record.
    Clean,
    /// The log ended in an incomplete append.
    ///
    /// The final bytes carried no record terminator, so they cannot be a record
    /// this database finished writing. They were not replayed, and the log was
    /// truncated back to the last complete record so that the next append does
    /// not splice onto a partial line.
    ///
    /// **This is the only condition under which durable bytes are discarded.**
    /// A *complete* record that is invalid is corruption, not a torn write, and
    /// refuses the open however close to the end of the file it sits.
    RecoveredTornTail {
        /// Byte offset where the incomplete append began.
        byte_offset: u64,
        /// How many bytes were discarded.
        discarded_bytes: usize,
    },
}

impl Default for LogRecovery {
    fn default() -> Self {
        LogRecovery::Clean
    }
}

impl Default for DurableFormat {
    fn default() -> Self {
        DurableFormat::Versioned(DURABLE_FORMAT_VERSION)
    }
}

impl LogRecovery {
    /// Whether the open replayed the log without discarding anything.
    pub fn is_clean(&self) -> bool {
        matches!(self, LogRecovery::Clean)
    }
}

impl Display for LogRecovery {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            LogRecovery::Clean => write!(f, "clean"),
            LogRecovery::RecoveredTornTail {
                byte_offset,
                discarded_bytes,
            } => write!(
                f,
                "recovered from an incomplete final append: {discarded_bytes} bytes at offset \
                 {byte_offset} were not a complete record and were discarded"
            ),
        }
    }
}

/// The durable format this build writes and understands.
///
/// Independent of the package version on purpose: an application release that
/// changes nothing about persistence must not appear to change the database.
///
/// - **1** — everything before revisions became resource-scoped. Never written
///   by any build; it names the era that predates format versioning.
/// - **2** — resource-scoped revisions: a `StateRevision` carries `resource`,
///   `content_id` and `sequence`.
pub const DURABLE_FORMAT_VERSION: u32 = 2;

/// The log record that carries the format version.
const FORMAT_RECORD_TYPE: &str = "feltdb.format.v1";

#[derive(Debug, Serialize, Deserialize)]
struct FormatRecord {
    record_type: String,
    format_version: u32,
}

/// What format a durable database is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurableFormat {
    /// A format record states the version.
    Versioned(u32),
    /// No format record: the database predates format versioning.
    ///
    /// This is a version, not an absence of one. It is the only case where the
    /// records themselves are examined to decide compatibility, and that
    /// examination is a deliberate one-time probe rather than a deserialization
    /// attempt whose failure is discarded.
    Unversioned,
}

impl Display for DurableFormat {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            DurableFormat::Versioned(version) => write!(f, "version {version}"),
            DurableFormat::Unversioned => write!(f, "unversioned (predates format versioning)"),
        }
    }
}

/// Why a durable database cannot be opened, and what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatIncompatibility {
    /// What the database is.
    pub found: DurableFormat,
    /// What this build requires.
    pub required: u32,
    /// What is wrong, in terms of the data rather than the code.
    pub reason: String,
    /// What the operator can do.
    pub action: String,
}

impl Display for FormatIncompatibility {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "durable format {} is incompatible with this FeltDB, which requires \
             format version {}; {}. No data was read or modified. {}",
            self.found, self.required, self.reason, self.action
        )
    }
}

/// The result of examining a durable database without opening it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatCompatibility {
    /// Safe to open.
    Compatible(DurableFormat),
    /// Must not be opened.
    Incompatible(FormatIncompatibility),
}

impl FormatCompatibility {
    /// Whether opening this database is safe.
    pub fn is_compatible(&self) -> bool {
        matches!(self, FormatCompatibility::Compatible(_))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseSnapshot {
    pub rows: Vec<StoredRow>,
    pub versions: HashMap<String, u64>,
    pub content_hash: String,
    /// The durable format the rows are in.
    ///
    /// A snapshot carries whole `StoredRow`s, revisions included, so it is a
    /// durable interchange artifact and not merely a protocol value: installing
    /// one from a different format would reintroduce exactly the silent
    /// misinterpretation the on-disk version exists to prevent.
    ///
    /// Defaults to 0 so a snapshot serialized before versioning still
    /// deserializes; 0 is treated as unversioned and refused rather than
    /// assumed current. `content_hash` deliberately does not cover this field,
    /// so hashes computed before it existed remain valid.
    #[serde(default)]
    pub format_version: u32,
}

impl DatabaseSnapshot {
    fn compute_hash(rows: &[StoredRow], versions: &HashMap<String, u64>) -> Result<String> {
        let mut rows = rows.to_vec();
        rows.sort_by(|left, right| left.key.cmp(&right.key));
        let versions: std::collections::BTreeMap<_, _> = versions.iter().collect();
        let mut hasher = Sha256::new();
        hasher.update(serde_json::to_vec(&(rows, versions))?);
        Ok(format!("{:x}", hasher.finalize()))
    }

    pub fn verify(&self) -> Result<()> {
        if self.content_hash != Self::compute_hash(&self.rows, &self.versions)? {
            return Err(FlowError::CapabilityError(
                "snapshot content hash mismatch".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct LogHeader {
    record_type: String,
    local_sequence: u64,
    observed_versions: HashMap<String, u64>,
    /// Present since format versioning. Absent in headers written before it,
    /// which deserialize as 0 and are classified as unversioned.
    #[serde(default)]
    format_version: u32,
    /// Canonical commit provenance retained when operation-log compaction
    /// rewrites current state. This is history, not a derived event store.
    #[serde(default)]
    transaction_history: Vec<TransactionAttribution>,
    /// Durable binding between a caller transaction identity and its semantic
    /// request. Compaction must retain this or an old id could be reused for a
    /// different mutation after restart.
    #[serde(default)]
    transaction_payload_hashes: HashMap<String, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SyncMetadata {
    local_sequence: u64,
    observed_versions: HashMap<String, u64>,
    peer_acknowledgements: HashMap<String, HashMap<String, u64>>,
}

pub fn open<P: AsRef<Path>>(path: P) -> Result<FeltDb> {
    FeltDb::open(path)
}

/// Capabilities whose rows are not themselves versioned.
///
/// `state` holds the revisions, and `_retention` holds the watermark that says
/// how much of each resource's history survives. Minting a revision of either
/// would mint a revision of the mint.
const UNVERSIONED_CAPABILITIES: &[&str] = &["state", "_retention"];

/// Whether a write to this capability creates a revision.
fn is_versioned(capability: &str) -> bool {
    !UNVERSIONED_CAPABILITIES.contains(&capability)
}

/// Rebuild the derived head index from the durable revisions.
///
/// Called after recovery, for the same reason `equality_index` is: the index is
/// a convenience over `rows` and must never be the authority for what `rows`
/// contains.
fn rebuild_revision_heads(inner: &mut Inner) {
    let mut heads: HashMap<String, (state_model::StateId, u64)> = HashMap::new();
    if let Some(bucket) = inner.rows.get("state") {
        for row in bucket.values() {
            if row.deleted || !row.key.starts_with("state:revision:") {
                continue;
            }
            let Ok(revision) =
                serde_json::from_value::<state_model::StateRevision>(row.value.clone())
            else {
                continue;
            };
            let entry = heads
                .entry(revision.resource.clone())
                .or_insert_with(|| (revision.id.clone(), revision.sequence));
            if revision.sequence >= entry.1 {
                *entry = (revision.id.clone(), revision.sequence);
            }
        }
    }
    inner.revision_heads = heads;
}

/// Write one derived row inside an already-held state lock.
///
/// Deliberately **not** a mutation: no sequence number, no vector clock tick,
/// no entry in the change log. A revision is a *derived* record of a mutation
/// that already has all three, and giving it its own would make every write
/// advance replication twice and double the operation stream.
///
/// It is still durable — the row is appended to the log and enters `rows`
/// inside the same lock as the state it records, so a reader never observes a
/// mutation whose revision is missing, and a reopened database has both.
///
/// The consequence is that revisions are **local and derived**, not replicated.
/// That is sound in principle, because a revision's identity is a function of
/// the resource, the content, the parent and the sequence — all of which a peer
/// that applies the same operation stream has. Whether the sync-apply path
/// actually mints is a separate question this change does not answer.
fn append_derived_row_locked(
    inner: &mut Inner,
    capability: &str,
    key: &str,
    value: Value,
) -> Result<()> {
    let row = StoredRow {
        capability: capability.to_string(),
        key: key.to_string(),
        rust_type: "feltdb::state_model::StateRevision".to_string(),
        value,
        unix_ms: now_ms(),
        content_hash: None,
        flow_ref: None,
        deleted: false,
        operation: None,
    };
    append_event_locked(inner, &row)?;
    inner.put_row(row);
    Ok(())
}

/// Work out the revision an authoritative write will produce, without writing
/// anything.
///
/// Separated from persisting it so the operation that carries this mutation can
/// name the revision it produced. A peer then **reconstructs that revision**
/// rather than minting a local substitute for the resulting state — which is
/// the difference between replicating history and replicating the present.
fn plan_revision(inner: &Inner, resource: &str, content: String) -> state_model::StateRevision {
    let parent = inner.revision_heads.get(resource).cloned();
    let (parent_id, sequence) = match &parent {
        Some((id, sequence)) => (Some(id.clone()), sequence + 1),
        None => (None, 0),
    };
    state_model::StateRevision::at(
        resource.to_string(),
        content,
        parent_id,
        sequence,
        inner.instance_id.clone(),
    )
}

/// What a peer needs to rebuild this revision exactly.
fn provenance_of(revision: &state_model::StateRevision) -> operation::RevisionProvenance {
    operation::RevisionProvenance {
        resource: revision.resource.clone(),
        id: revision.id.as_hex().to_string(),
        parent_id: revision
            .parent_id
            .as_ref()
            .map(|id| id.as_hex().to_string()),
        sequence: revision.sequence,
        content_id: revision.content_id.as_hex().to_string(),
        authority: revision.authority.clone(),
    }
}

/// Persist a revision under the caller's lock, and apply the resource's
/// retention policy.
///
/// Idempotent: a revision already stored is left exactly as it is. Committing
/// the same historical fact twice — which replication does routinely — must not
/// rewrite it.
fn persist_revision_locked(inner: &mut Inner, revision: state_model::StateRevision) -> Result<()> {
    let key = format!("state:revision:{}", revision.id.as_hex());
    let already = inner
        .rows
        .get("state")
        .and_then(|bucket| bucket.get(&key))
        .is_some_and(|row| !row.deleted);

    let resource = revision.resource.clone();
    let id = revision.id.clone();
    let sequence = revision.sequence;

    if !already {
        let value = serde_json::to_value(&revision)?;
        append_derived_row_locked(inner, "state", &key, value)?;
    }

    // The head advances only forward. A fork leaves two revisions at one
    // sequence; whichever is seen last does not become "the" head by accident.
    let advance = inner
        .revision_heads
        .get(&resource)
        .is_none_or(|(_, current)| sequence > *current);
    if advance {
        inner
            .revision_heads
            .insert(resource.clone(), (id, sequence));
    }

    apply_retention_locked(inner, &resource)
}

/// Rebuild the revision an operation carries, and check it is the one the
/// originating authority actually committed.
///
/// The identity is recomputed from the parts rather than trusted: if the value
/// that arrived is not the value the revision was computed from, or the stated
/// identity does not follow from the stated resource, parent and sequence, the
/// operation is refused rather than turned into a plausible local history.
fn reconstruct_revision(
    provenance: &operation::RevisionProvenance,
    value: &Value,
) -> Result<state_model::StateRevision> {
    let content = value.to_string();
    let revision = state_model::StateRevision::at(
        provenance.resource.clone(),
        content,
        provenance
            .parent_id
            .as_ref()
            .map(|id| state_model::StateId::from_hex(id.clone())),
        provenance.sequence,
        provenance.authority.clone(),
    );
    if revision.content_id.as_hex() != provenance.content_id {
        return Err(FlowError::CapabilityError(format!(
            "replicated revision {} does not match the value it arrived with",
            provenance.id
        )));
    }
    if revision.id.as_hex() != provenance.id {
        return Err(FlowError::CapabilityError(format!(
            "replicated revision identity {} does not follow from its own resource, parent and sequence",
            provenance.id
        )));
    }
    Ok(revision)
}

/// Expire whatever the resource's configured policy no longer retains.
/// Expire whatever the resource's configured policy no longer retains.
///
/// The common path is one map lookup: with no policy configured there is
/// nothing to scan. A resource's history is only walked once a policy exists to
/// bound it.
fn apply_retention_locked(inner: &mut Inner, resource: &str) -> Result<()> {
    let retention_key = format!("_retention:{resource}");
    let Some(state_row) = inner
        .rows
        .get("_retention")
        .and_then(|bucket| bucket.get(&retention_key))
    else {
        return Ok(());
    };
    let Ok(mut state) = serde_json::from_value::<RetentionRecord>(state_row.value.clone()) else {
        return Ok(());
    };
    if state.policy.keep_last.is_none() {
        return Ok(());
    }

    let mut history: Vec<(state_model::StateId, u64)> = Vec::new();
    if let Some(bucket) = inner.rows.get("state") {
        for row in bucket.values() {
            if row.deleted || !row.key.starts_with("state:revision:") {
                continue;
            }
            if let Ok(revision) =
                serde_json::from_value::<state_model::StateRevision>(row.value.clone())
            {
                if revision.resource == resource {
                    history.push((revision.id, revision.sequence));
                }
            }
        }
    }
    history.sort_by_key(|(_, sequence)| *sequence);

    let (expire, horizon) = state_model::revisions_to_expire(&history, state.policy.keep_last);
    if expire.is_empty() {
        return Ok(());
    }
    for id in &expire {
        let key = format!("state:revision:{}", id.as_hex());
        if let Some(bucket) = inner.rows.get_mut("state") {
            bucket.remove(&key);
        }
        let tombstone = StoredRow {
            capability: "state".to_string(),
            key,
            rust_type: "feltdb::state_model::StateRevision".to_string(),
            value: Value::Null,
            unix_ms: now_ms(),
            content_hash: None,
            flow_ref: None,
            deleted: true,
            operation: None,
        };
        append_event(&inner.path, &tombstone)?;
    }
    if let Some(horizon) = horizon {
        state.horizon = horizon;
    }
    let value = serde_json::to_value(&state)?;
    append_derived_row_locked(inner, "_retention", &retention_key, value)
}

/// The durable shape of a resource's retention state.
///
/// Mirrors `state_model`'s private record so the write boundary can read and
/// advance the horizon without the store type.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct RetentionRecord {
    policy: state_model::RetentionPolicy,
    horizon: u64,
}

impl FeltDb {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let requested_path = path.as_ref();
        let parent = requested_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let path = if requested_path.exists() {
            fs::canonicalize(requested_path)?
        } else {
            let file_name = requested_path.file_name().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "FeltDB database path has no file name",
                )
            })?;
            fs::canonicalize(parent)?.join(file_name)
        };

        let databases = OPEN_DATABASES.get_or_init(|| Mutex::new(HashMap::new()));
        let mut databases = databases.lock().expect("open database registry poisoned");
        if let Some(open) = databases.get(&path) {
            if let (Some(inner), Some(ownership)) = (open.inner.upgrade(), open.ownership.upgrade())
            {
                return Ok(Self {
                    inner,
                    event_tx: open.event_tx.clone(),
                    _ownership: ownership,
                });
            }
        }
        databases.remove(&path);

        let mut ownership_path = path.as_os_str().to_os_string();
        ownership_path.push(".lock");
        let ownership = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(PathBuf::from(ownership_path))?;
        ownership.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!(
                    "FeltDB database {} is already owned by another process",
                    path.display()
                ),
            ),
            std::fs::TryLockError::Error(error) => error,
        })?;

        // Fail closed. The format is decided before any record is interpreted
        // and before anything is written, so a database this build does not
        // understand is refused with its bytes untouched rather than opened
        // with whatever happened to deserialize.
        let accepted_format = match inspect_durable_format(&path)? {
            FormatCompatibility::Incompatible(incompatibility) => {
                return Err(FlowError::IncompatibleFormat(Box::new(incompatibility)));
            }
            FormatCompatibility::Compatible(format) => format,
        };

        if !path.exists() {
            let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
            // A new database states its format in its first record, so it never
            // has to be inferred again.
            write_json_line(
                &mut file,
                &FormatRecord {
                    record_type: FORMAT_RECORD_TYPE.to_string(),
                    format_version: DURABLE_FORMAT_VERSION,
                },
            )?;
        }

        // Generate instance ID from path hash
        let instance_id = format!("feltdb-{:x}", {
            let mut hasher = DefaultHasher::new();
            path.to_string_lossy().hash(&mut hasher);
            hasher.finish()
        });

        let mut inner = Inner {
            path: path.clone(),
            instance_id: instance_id.clone(),
            durable_format: accepted_format,
            ..Inner::default()
        };

        // Initialize sync state with instance ID
        inner.sync_state = SyncState::new(instance_id);
        replay_log(&mut inner)?;
        load_sync_metadata(&mut inner)?;
        rebuild_revision_heads(&mut inner);

        let (event_tx, _) = broadcast::channel(1024);

        let database = Self {
            inner: Arc::new(Mutex::new(inner)),
            event_tx,
            _ownership: Arc::new(ownership),
        };
        databases.insert(
            path,
            OpenDatabase {
                inner: Arc::downgrade(&database.inner),
                event_tx: database.event_tx.clone(),
                ownership: Arc::downgrade(&database._ownership),
            },
        );
        Ok(database)
    }

    /// Write a self-contained durable backup artifact.
    ///
    /// The artifact does not depend on this database remaining present: it
    /// carries every live record, the revision graph, retention state and the
    /// observed operation versions, plus the digests needed to check all of it
    /// without a live database.
    pub fn write_backup(&self, path: &Path) -> Result<BackupVerification> {
        let (rows, observed_versions) = {
            let inner = self.state();
            let mut rows: Vec<StoredRow> = inner
                .rows
                .values()
                .flat_map(|bucket| bucket.values().cloned())
                .filter(|row| !row.deleted)
                .collect();
            rows.sort_by(|left, right| {
                left.capability
                    .cmp(&right.capability)
                    .then_with(|| left.key.cmp(&right.key))
            });
            (rows, inner.change_log.versions())
        };

        let content_digest = backup_content_digest(&rows)?;
        let meaning_digest = backup_meaning_digest(&rows)?;
        let header = BackupHeader {
            record_type: BACKUP_RECORD_TYPE.to_string(),
            backup_version: BACKUP_VERSION,
            format_version: DURABLE_FORMAT_VERSION,
            created_unix_ms: now_ms() as u64,
            row_count: rows.len(),
            observed_versions,
            content_digest: content_digest.clone(),
            meaning_digest: meaning_digest.clone(),
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        write_json_line(&mut file, &header)?;
        for row in &rows {
            write_json_line(&mut file, row)?;
        }
        file.flush()?;
        file.sync_all()?;

        Ok(BackupVerification {
            backup_version: BACKUP_VERSION,
            format_version: DURABLE_FORMAT_VERSION,
            created_unix_ms: header.created_unix_ms,
            rows: rows.len(),
            content_digest,
            meaning_digest,
        })
    }

    /// Restore a backup artifact into a **new** database.
    ///
    /// Refuses to write over an existing database: a restore that could
    /// overwrite the thing an operator is trying to recover is not a recovery
    /// tool. The artifact is verified first, and the restored database is
    /// verified again afterwards — its meaning is recomputed from the live
    /// database and compared with the artifact's. A restore therefore proves
    /// that the result *means the same thing*, not merely that the file parsed.
    pub fn restore_backup(backup: &Path, target: &Path) -> Result<(Self, BackupVerification)> {
        let verification = verify_backup(backup)?;
        if target.exists() {
            return Err(backup_problem(
                "the restore target already exists",
                None,
                "Restore into a new path. FeltDB will not write over an existing database.",
            ));
        }

        let text = std::fs::read_to_string(backup)?;
        let mut lines = text.lines();
        let header: BackupHeader = serde_json::from_str(lines.next().unwrap_or_default())?;

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(target)?;
            write_json_line(
                &mut file,
                &FormatRecord {
                    record_type: FORMAT_RECORD_TYPE.to_string(),
                    format_version: header.format_version,
                },
            )?;
            for line in lines {
                if line.trim().is_empty() {
                    continue;
                }
                writeln!(file, "{line}")?;
            }
            file.flush()?;
            file.sync_all()?;
        }

        let restored = FeltDb::open(target)?;

        // The equivalence check. Anything that changed the database's meaning
        // between the artifact and the live result fails here.
        let live_rows = {
            let inner = restored.state();
            let mut rows: Vec<StoredRow> = inner
                .rows
                .values()
                .flat_map(|bucket| bucket.values().cloned())
                .filter(|row| !row.deleted)
                .collect();
            rows.sort_by(|left, right| {
                left.capability
                    .cmp(&right.capability)
                    .then_with(|| left.key.cmp(&right.key))
            });
            rows
        };
        let restored_meaning = backup_meaning_digest(&live_rows)?;
        if restored_meaning != verification.meaning_digest {
            return Err(backup_problem(
                "the restored database does not mean what the backup says it means",
                None,
                "The backup or the restore path is damaged. Do not use this database.",
            ));
        }

        Ok((restored, verification))
    }

    /// A digest of this database's **application state**: the records an
    /// application reads, and nothing else.
    ///
    /// Excludes revisions and retention state, so it answers only "do these two
    /// databases hold the same values?". Pair it with
    /// `StateStore::history_digest` to ask the separate and stronger question
    /// of whether they hold the same history — two databases can match on this
    /// and differ completely on that, which is exactly the failure replication
    /// used to have.
    pub fn state_digest(&self) -> Result<String> {
        let inner = self.state();
        let mut entries: Vec<(String, String, String)> = inner
            .rows
            .iter()
            .filter(|(capability, _)| is_versioned(capability))
            .flat_map(|(capability, bucket)| {
                bucket
                    .values()
                    .filter(|row| !row.deleted)
                    .map(move |row| (capability.clone(), row.key.clone(), row.value.to_string()))
            })
            .collect();
        entries.sort();

        let mut hasher = Sha256::new();
        hasher.update(b"feltdb.state.v1");
        for (capability, key, value) in entries {
            hasher.update(capability.as_bytes());
            hasher.update([0u8]);
            hasher.update(key.as_bytes());
            hasher.update([0u8]);
            hasher.update(value.as_bytes());
            hasher.update([0u8]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    /// How far a single-record write is pushed before it returns.
    pub fn durability_mode(&self) -> DurabilityMode {
        self.state().durability_mode
    }

    /// Choose what a successful single-record write guarantees.
    ///
    /// This changes the database's contract, not merely its speed. Transaction
    /// commits always issue a stable-storage barrier and are unaffected.
    pub fn set_durability_mode(&self, mode: DurabilityMode) {
        let mut inner = self.state();
        inner.durability_mode = mode;
        inner.writes_since_barrier = 0;
    }

    /// What this database knows about itself.
    ///
    /// Reports facts the open established — the durable format it was accepted
    /// as, and whether replay discarded an incomplete final append. It reports
    /// nothing it has not observed: **health can expose a proven fact, it
    /// cannot create a stronger guarantee.**
    ///
    /// In particular this says nothing about power-loss durability, which is
    /// unproven, nor about replication or backup state, which have no
    /// operational contract yet.
    pub fn health(&self) -> DatabaseHealth {
        let recovery = self.log_recovery();
        let storage = match &recovery {
            LogRecovery::Clean => StorageHealth::Clean,
            LogRecovery::RecoveredTornTail {
                byte_offset,
                discarded_bytes,
            } => StorageHealth::RecoveredIncompleteWrite {
                byte_offset: *byte_offset,
                discarded_bytes: *discarded_bytes,
            },
        };
        DatabaseHealth {
            durable_format: self.state().durable_format.clone(),
            recovery,
            storage,
        }
    }

    /// What replaying the durable log had to do for this database to open.
    ///
    /// [`LogRecovery::Clean`] means every durable byte was a complete, valid
    /// record. [`LogRecovery::RecoveredTornTail`] means the log ended in an
    /// incomplete append that was discarded — the database is usable and its
    /// **last write did not land**, which is a different fact from healthy and
    /// one an operator has to be able to learn.
    ///
    /// A corrupt log does not reach here at all: it refuses the open.
    pub fn log_recovery(&self) -> LogRecovery {
        self.state().log_recovery.clone()
    }

    pub fn insert<T: Serialize>(&self, key: &str, value: T) -> Result<()> {
        let capability = key
            .split_once(':')
            .map(|(cap, _)| cap)
            .unwrap_or("default")
            .to_string();
        self.insert_internal(capability, key.to_string(), value, false)?;
        Ok(())
    }

    /// Insert only if the logical record is absent, deciding presence under the
    /// same lock that appends the operation and mints its first revision.
    pub fn insert_if_absent<T: Serialize>(&self, key: &str, value: T) -> Result<()> {
        let capability = key
            .split_once(':')
            .map(|(cap, _)| cap)
            .unwrap_or("default")
            .to_string();
        self.insert_internal(capability, key.to_string(), value, true)
    }

    /// Update an existing record with vector clock tracking
    pub fn update<T: Serialize>(&self, key: &str, value: T) -> Result<()> {
        let capability = key
            .split_once(':')
            .map(|(cap, _)| cap)
            .unwrap_or("default")
            .to_string();
        self.update_internal(capability, key.to_string(), value)?;
        Ok(())
    }

    /// Atomically compare the logical record version and authority epoch before
    /// durably replacing a JSON object. Failed predicates append no operation
    /// and emit no change event.
    pub fn compare_and_set_json(
        &self,
        key: &str,
        expected_version: u64,
        expected_epoch: Option<u64>,
        expected_lease_id: Option<&str>,
        reject_if_diverged: bool,
        mut value: Value,
    ) -> Result<JsonCasResult> {
        let capability = key
            .split_once(':')
            .map(|(cap, _)| cap)
            .unwrap_or("default")
            .to_string();
        let rust_type = type_name::<Value>().to_string();
        let (result, event) = {
            let mut inner = self.state();
            let Some(current) = inner.rows.get(&capability).and_then(|rows| rows.get(key)) else {
                return Ok(JsonCasResult::NotFound);
            };
            let current_version = current
                .value
                .get("__version")
                .and_then(Value::as_u64)
                .unwrap_or(1);
            let current_epoch = current
                .value
                .pointer("/authority/epoch")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if current_version != expected_version {
                return Ok(JsonCasResult::VersionConflict {
                    current_version,
                    current_epoch,
                });
            }
            if expected_epoch.is_some_and(|expected| expected != current_epoch) {
                return Ok(JsonCasResult::AuthorityConflict {
                    current_version,
                    current_epoch,
                });
            }
            let persisted_lease = current.value.get("lease").filter(|lease| !lease.is_null());
            let lease_matches = match (persisted_lease, expected_lease_id) {
                (None, None) => true,
                (None, Some(_)) => false,
                (Some(lease), Some(expected_id)) => {
                    lease.get("leaseId").and_then(Value::as_str) == Some(expected_id)
                        && lease
                            .get("expiresAt")
                            .and_then(Value::as_u64)
                            .is_some_and(|expires_at| expires_at > now_ms() as u64)
                }
                (Some(lease), None) => lease
                    .get("expiresAt")
                    .and_then(Value::as_u64)
                    .is_some_and(|expires_at| expires_at <= now_ms() as u64),
            };
            if !lease_matches {
                return Ok(JsonCasResult::LeaseConflict {
                    current_version,
                    current_epoch,
                });
            }
            if reject_if_diverged && capability == "_cells" {
                let replica_key = key
                    .strip_prefix("_cells:")
                    .map(|suffix| format!("_replicas:{suffix}"));
                let divergent = replica_key.as_ref().and_then(|replica_key| {
                    inner
                        .rows
                        .get("_replicas")
                        .and_then(|rows| rows.get(replica_key))
                        .and_then(|row| row.value.get("divergent"))
                        .and_then(Value::as_bool)
                }) == Some(true);
                if divergent {
                    return Ok(JsonCasResult::ReplicationConflict {
                        current_version,
                        current_epoch,
                    });
                }
            }

            let object = value.as_object_mut().ok_or_else(|| {
                FlowError::CapabilityError("CAS value must be a JSON object".to_string())
            })?;
            object.insert("__version".to_string(), Value::from(expected_version + 1));

            inner.sequence += 1;
            inner.sync_state.increment_vector_clock();
            let vector_clock = inner.sync_state.get_or_init_vector_clock().clone();
            let planned =
                is_versioned(&capability).then(|| plan_revision(&inner, key, value.to_string()));
            let mut operation = Operation::update(
                inner.sequence,
                inner.instance_id.clone(),
                inner.sequence,
                key.to_string(),
                value.clone(),
                rust_type.clone(),
                capability.clone(),
            )
            .with_vector_clock(vector_clock);
            operation.revision = planned.as_ref().map(provenance_of);
            let row = StoredRow {
                capability: capability.clone(),
                key: key.to_string(),
                rust_type: rust_type.clone(),
                value: value.clone(),
                unix_ms: now_ms(),
                content_hash: Some(operation.content_hash.clone()),
                flow_ref: None,
                deleted: false,
                operation: Some(operation.clone()),
            };
            append_event_locked(&mut inner, &row)?;
            inner.authority_revision += 1;
            inner.change_log.add_operation(operation);
            inner.put_row(row.clone());
            if let Some(revision) = planned {
                persist_revision_locked(&mut inner, revision)?;
            }
            let event = ChangeEvent {
                capability,
                key: key.to_string(),
                rust_type,
                unix_ms: row.unix_ms,
            };
            (
                JsonCasResult::Updated {
                    current_version: expected_version + 1,
                    current_epoch: value
                        .pointer("/authority/epoch")
                        .and_then(Value::as_u64)
                        .unwrap_or(current_epoch),
                    value,
                },
                event,
            )
        };
        let _ = self.event_tx.send(event);
        Ok(result)
    }

    /// Delete a record with vector clock tracking
    pub fn delete(&self, key: &str) -> Result<()> {
        let capability = key
            .split_once(':')
            .map(|(cap, _)| cap)
            .unwrap_or("default")
            .to_string();
        self.delete_internal(capability, key.to_string())?;
        Ok(())
    }

    /// Evaluate authorization for a mutation
    fn evaluate_authorization(&self, context: &AuthorizationContext) -> Result<()> {
        // Check basic capabilities
        if !context.capabilities.contains(&"state:write".to_string()) {
            return Err(FlowError::CapabilityError(
                "AUTHORIZATION_DENIED:missing_state_write_capability".to_string(),
            ));
        }

        // Tenant isolation check happens implicitly at mutation boundary
        // by only checking against current database tenant_id

        Ok(())
    }

    /// Insert a record with authorization context
    pub fn insert_with_authorization<T: Serialize>(
        &self,
        key: &str,
        value: T,
        context: &AuthorizationContext,
    ) -> Result<()> {
        // Authorize mutation inside FeltDB boundary
        self.evaluate_authorization(context)?;

        let capability = key
            .split_once(':')
            .map(|(cap, _)| cap)
            .unwrap_or("default")
            .to_string();

        // Check if capability is writable
        if !context.writable_collections.is_empty()
            && !context.writable_collections.contains(&capability)
        {
            return Err(FlowError::CapabilityError(format!(
                "AUTHORIZATION_DENIED:not_writable_collection:{}",
                capability
            )));
        }

        // Authorized: apply mutation
        self.insert_internal(capability, key.to_string(), value, false)?;
        Ok(())
    }

    /// Update a record with authorization context
    pub fn update_with_authorization<T: Serialize>(
        &self,
        key: &str,
        value: T,
        context: &AuthorizationContext,
    ) -> Result<()> {
        // Authorize mutation inside FeltDB boundary
        self.evaluate_authorization(context)?;

        let capability = key
            .split_once(':')
            .map(|(cap, _)| cap)
            .unwrap_or("default")
            .to_string();

        // Check if capability is writable
        if !context.writable_collections.is_empty()
            && !context.writable_collections.contains(&capability)
        {
            return Err(FlowError::CapabilityError(format!(
                "AUTHORIZATION_DENIED:not_writable_collection:{}",
                capability
            )));
        }

        // Authorized: apply mutation
        // Pre-state evaluation would happen here in full implementation
        self.update_internal(capability, key.to_string(), value)?;
        Ok(())
    }

    /// Delete a record with authorization context
    pub fn delete_with_authorization(
        &self,
        key: &str,
        context: &AuthorizationContext,
    ) -> Result<()> {
        // Authorize mutation inside FeltDB boundary
        self.evaluate_authorization(context)?;

        let capability = key
            .split_once(':')
            .map(|(cap, _)| cap)
            .unwrap_or("default")
            .to_string();

        // Check if capability is writable
        if !context.writable_collections.is_empty()
            && !context.writable_collections.contains(&capability)
        {
            return Err(FlowError::CapabilityError(format!(
                "AUTHORIZATION_DENIED:not_writable_collection:{}",
                capability
            )));
        }

        // Authorized: apply mutation
        // Pre-state evaluation would happen here in full implementation
        self.delete_internal(capability, key.to_string())?;
        Ok(())
    }

    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let capability = key
            .split_once(':')
            .map(|(cap, _)| cap)
            .unwrap_or("default")
            .to_string();
        let inner = self.state();
        let value = inner
            .rows
            .get(&capability)
            .and_then(|bucket| bucket.get(key))
            .map(|row| serde_json::from_value::<T>(row.value.clone()))
            .transpose()?;
        Ok(value)
    }

    pub fn query<T, F>(&self, predicate: F) -> Result<Vec<T>>
    where
        T: DeserializeOwned,
        F: Fn(&T) -> bool,
    {
        let rust_type = type_name::<T>().to_string();
        let mut inner = self.state();
        *inner
            .query_count_by_type
            .entry(rust_type.clone())
            .or_default() += 1;
        if inner.query_count_by_type[&rust_type] >= 3 {
            inner.adaptive_indexes.insert(rust_type.clone());
        }

        let mut out = Vec::new();
        for bucket in inner.rows.values() {
            for row in bucket.values() {
                if row.rust_type != rust_type {
                    continue;
                }
                let parsed = serde_json::from_value::<T>(row.value.clone())?;
                if predicate(&parsed) {
                    out.push(parsed);
                }
            }
        }
        Ok(out)
    }

    pub fn capability(&self, name: &str) -> Capability {
        Capability {
            db: self.clone(),
            name: name.to_string(),
        }
    }

    pub fn is_auto_indexed<T>(&self) -> bool {
        let rust_type = type_name::<T>().to_string();
        let inner = self.state();
        inner.adaptive_indexes.contains(&rust_type)
    }

    /// Get the instance ID for this FeltDB instance
    pub fn instance_id(&self) -> Result<String> {
        let inner = self.state();
        Ok(inner.instance_id.clone())
    }

    /// Get the current sequence number for this instance
    pub fn sequence(&self) -> Result<u64> {
        let inner = self.state();
        Ok(inner.sequence)
    }

    /// Revision of the snapshot held by this embedded authority instance.
    /// In multi-process topology callers must use `AuthorityClient::read` to
    /// cross the authority boundary and obtain a current authoritative value.
    pub fn current_revision(&self) -> Result<u64> {
        Ok(self.state().authority_revision)
    }

    /// Capture revision and rows under one lock. Authority reads must never
    /// pair metadata from one commit with state from another.
    pub(crate) fn authority_state(&self) -> (u64, Vec<StoredRow>) {
        let inner = self.state();
        let rows = inner
            .rows
            .values()
            .flat_map(|bucket| bucket.values().cloned())
            .collect();
        (inner.authority_revision, rows)
    }

    /// Get the maintained cardinality (record count) for a collection capability.
    /// Returns the count of non-deleted records in the collection.
    pub fn collection_cardinality(&self, capability: &str) -> Result<u64> {
        let inner = self.state();
        Ok(inner
            .collection_cardinality
            .get(capability)
            .copied()
            .unwrap_or(0))
    }

    /// Diagnostic: List all collection capabilities and their cardinalities.
    /// Used to detect capability format mismatches.
    pub fn list_cardinalities(&self) -> Result<Vec<(String, u64)>> {
        let inner = self.state();
        let mut result: Vec<_> = inner
            .collection_cardinality
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        result.sort_by(|a, b| b.1.cmp(&a.1)); // Sort by count descending
        Ok(result)
    }

    /// Diagnostic: Count actual rows for a given capability pattern.
    /// Used to verify that rows exist even if cardinality is 0.
    pub fn diagnostic_row_count(&self, capability_pattern: &str) -> Result<usize> {
        let inner = self.state();
        Ok(inner
            .rows
            .iter()
            .filter(|(cap, _)| cap.contains(capability_pattern))
            .map(|(_, bucket)| bucket.values().filter(|row| !row.deleted).count())
            .sum())
    }

    /// Diagnostic: Get actual persisted capability strings matching a pattern.
    /// Used to identify capability identity formats in use.
    pub fn diagnostic_capability_keys(&self, capability_pattern: &str) -> Result<Vec<String>> {
        let inner = self.state();
        let mut keys: Vec<_> = inner
            .rows
            .keys()
            .filter(|k| k.contains(capability_pattern))
            .cloned()
            .collect();
        keys.sort();
        Ok(keys)
    }

    /// The record write path's head per resource: the revision its next
    /// authoritative write will name as parent. Garbage collection treats each
    /// one as a root, so a write can never mint a child of a collected revision.
    pub(crate) fn revision_head_ids(&self) -> Vec<state_model::StateId> {
        self.state()
            .revision_heads
            .values()
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn has_applied_transaction(&self, transaction_id: &str) -> Result<bool> {
        Ok(self
            .inner
            .lock()
            .expect("lock poisoned")
            .applied_transactions
            .contains(transaction_id))
    }

    pub fn applied_transaction_payload_hash(&self, transaction_id: &str) -> Result<Option<String>> {
        Ok(self
            .inner
            .lock()
            .expect("lock poisoned")
            .transaction_payload_hashes
            .get(transaction_id)
            .cloned())
    }

    /// Atomically validates and durably applies an ordered mutation batch.
    /// The complete transaction is represented by one log record and becomes
    /// visible in memory only after that record has been appended successfully.
    pub fn apply_atomic_transaction(
        &self,
        transaction_id: &str,
        expected_parent: Option<u64>,
        preconditions: &[AtomicPrecondition],
        mutations: &[AtomicMutation],
        audit: Option<Value>,
    ) -> Result<AtomicCommit> {
        self.apply_atomic_transaction_content_addressed(
            transaction_id,
            None,
            expected_parent,
            preconditions,
            mutations,
            audit,
        )
    }

    /// Applies a transaction whose identifier is durably bound to a canonical
    /// payload hash. The same ID with a different payload is always a conflict.
    pub fn apply_atomic_transaction_content_addressed(
        &self,
        transaction_id: &str,
        payload_hash: Option<&str>,
        expected_parent: Option<u64>,
        preconditions: &[AtomicPrecondition],
        mutations: &[AtomicMutation],
        audit: Option<Value>,
    ) -> Result<AtomicCommit> {
        self.apply_atomic_transaction_guarded(
            transaction_id,
            payload_hash,
            expected_parent,
            preconditions,
            &[],
            mutations,
            audit,
        )
    }

    /// Commit a transaction only if every record precondition holds.
    ///
    /// The preconditions are evaluated inside the same lock as the writes and
    /// before any of them, so there is no state in which one record is checked
    /// while the others commit. A failure returns
    /// `FlowError::PreconditionFailed` and applies nothing -- not a prefix, not
    /// the guarded record, nothing.
    ///
    /// A failed precondition does **not** consume the transaction id. The id is
    /// recorded only after a commit, so a caller that loses a race may retry
    /// the same id once it has re-read state, and a caller that wants the
    /// retry to be a distinct decision may use a new one. Both work; the choice
    /// is the caller's and is asserted in the tests rather than left implied.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_atomic_transaction_guarded(
        &self,
        transaction_id: &str,
        payload_hash: Option<&str>,
        expected_parent: Option<u64>,
        preconditions: &[AtomicPrecondition],
        record_preconditions: &[RecordPrecondition],
        mutations: &[AtomicMutation],
        audit: Option<Value>,
    ) -> Result<AtomicCommit> {
        self.apply_atomic_transaction_fenced(
            transaction_id,
            payload_hash,
            expected_parent,
            preconditions,
            record_preconditions,
            &[],
            mutations,
            audit,
        )
    }

    /// Capture a read basis on the direct route, under the same lock every commit takes.
    pub fn read_basis(&self, reads: &ReadSet) -> Result<ReadBasis> {
        self.read_basis_in(reads, &ReadLayout::Direct)
    }

    /// Capture a read basis in a storage layout, under the same lock every commit takes.
    pub fn read_basis_in(&self, reads: &ReadSet, layout: &ReadLayout) -> Result<ReadBasis> {
        let reads = reads.normalised()?;
        let inner = self.state();
        let token = read_basis_token(&inner.rows, &reads, layout);
        Ok(ReadBasis { reads, token })
    }

    /// `apply_atomic_transaction_guarded`, additionally fenced by read bases.
    ///
    /// Each basis is re-evaluated inside the same lock as the writes, after the
    /// record preconditions and before anything is staged. If the state it
    /// names differs from what was read -- whichever write path changed it --
    /// the transaction fails with `PreconditionFailed(Fence)` and applies
    /// nothing. State outside every basis is never read and never conflicts.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_atomic_transaction_fenced(
        &self,
        transaction_id: &str,
        payload_hash: Option<&str>,
        expected_parent: Option<u64>,
        preconditions: &[AtomicPrecondition],
        record_preconditions: &[RecordPrecondition],
        fences: &[ReadBasis],
        mutations: &[AtomicMutation],
        audit: Option<Value>,
    ) -> Result<AtomicCommit> {
        self.apply_atomic_transaction_fenced_in(
            transaction_id,
            payload_hash,
            expected_parent,
            preconditions,
            record_preconditions,
            &ReadLayout::Direct,
            fences,
            mutations,
            audit,
        )
    }

    /// `apply_atomic_transaction_fenced`, with the read bases addressed in
    /// `layout`. The same evaluation, inside the same lock, for every route.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_atomic_transaction_fenced_in(
        &self,
        transaction_id: &str,
        payload_hash: Option<&str>,
        expected_parent: Option<u64>,
        preconditions: &[AtomicPrecondition],
        record_preconditions: &[RecordPrecondition],
        layout: &ReadLayout,
        fences: &[ReadBasis],
        mutations: &[AtomicMutation],
        audit: Option<Value>,
    ) -> Result<AtomicCommit> {
        let (commit, events) = {
            let mut inner = self.state();
            if inner.applied_transactions.contains(transaction_id) {
                if let Some(expected_hash) = payload_hash {
                    if inner
                        .transaction_payload_hashes
                        .get(transaction_id)
                        .map(String::as_str)
                        != Some(expected_hash)
                    {
                        return Err(FlowError::CapabilityError(format!(
                            "TRANSACTION_ID_PAYLOAD_MISMATCH:{transaction_id}"
                        )));
                    }
                }
                let (base_revision, commit_revision) = inner
                    .transaction_revisions
                    .get(transaction_id)
                    .copied()
                    .unwrap_or((inner.authority_revision, inner.authority_revision));
                let (state_before, state_after) = inner
                    .transaction_history
                    .iter()
                    .find(|attribution| attribution.transaction_id == transaction_id)
                    .map(|attribution| (attribution.state_before, attribution.state_after))
                    .unwrap_or((inner.sequence, inner.sequence));
                return Ok(AtomicCommit {
                    transaction_id: transaction_id.into(),
                    base_revision,
                    commit_revision,
                    status: TransactionStatus::Committed,
                    state_before,
                    state_after,
                    rows: vec![],
                    duplicate: true,
                });
            }
            // Record preconditions, evaluated before anything is staged and
            // under the same lock the writes take. A caller reads state,
            // decides, and commits; this is the fence that makes the decision
            // still true at the moment of the write.
            for condition in record_preconditions {
                if condition.is_empty() {
                    return Err(FlowError::CapabilityError(format!(
                        "EMPTY_PRECONDITION:{}:{}",
                        condition.capability, condition.key
                    )));
                }
                let collection = condition.capability.clone();
                let key = condition.key.clone();
                let current = inner
                    .rows
                    .get(&condition.capability)
                    .and_then(|bucket| bucket.get(&condition.key));

                if condition.require_absent {
                    if current.is_some() {
                        return Err(FlowError::PreconditionFailed(Box::new(
                            PreconditionFailure::Present { collection, key },
                        )));
                    }
                    continue;
                }

                let Some(current) = current else {
                    return Err(FlowError::PreconditionFailed(Box::new(
                        PreconditionFailure::Missing { collection, key },
                    )));
                };

                // The same fields single-record CAS compares, read the same way.
                if let Some(expected) = condition.expected_version {
                    let actual = current
                        .value
                        .get("__version")
                        .and_then(Value::as_u64)
                        .unwrap_or(1);
                    if actual != expected {
                        return Err(FlowError::PreconditionFailed(Box::new(
                            PreconditionFailure::Version {
                                collection,
                                key,
                                expected,
                                actual,
                            },
                        )));
                    }
                }
                if let Some(expected) = condition.expected_epoch {
                    let actual = current
                        .value
                        .pointer("/authority/epoch")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if actual != expected {
                        return Err(FlowError::PreconditionFailed(Box::new(
                            PreconditionFailure::Epoch {
                                collection,
                                key,
                                expected,
                                actual,
                            },
                        )));
                    }
                }
                if let Some(expected) = &condition.expected_lease_id {
                    let lease = current.value.get("lease").filter(|lease| !lease.is_null());
                    let held = lease.and_then(|lease| {
                        lease
                            .get("leaseId")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    });
                    // An expired lease is not held, so naming it is a conflict:
                    // the caller believes it owns something it no longer does.
                    let unexpired = lease
                        .and_then(|lease| lease.get("expiresAt").and_then(Value::as_u64))
                        .is_some_and(|expires_at| expires_at > now_ms() as u64);
                    if held.as_deref() != Some(expected.as_str()) || !unexpired {
                        return Err(FlowError::PreconditionFailed(Box::new(
                            PreconditionFailure::Lease {
                                collection,
                                key,
                                expected: expected.clone(),
                                actual: held,
                            },
                        )));
                    }
                }
            }

            for fence in fences {
                if !fence.token.starts_with(&layout.token_prefix()) {
                    return Err(FlowError::CapabilityError(
                        "INVALID_READ_SET:a read basis was not issued by this authority scope"
                            .into(),
                    ));
                }
                let reads = fence.reads.normalised()?;
                let actual = read_basis_token(&inner.rows, &reads, layout);
                if actual != fence.token {
                    let collection = reads
                        .records
                        .first()
                        .map(|record| record.collection.clone())
                        .or_else(|| {
                            reads
                                .predicates
                                .first()
                                .map(|predicate| predicate.collection.clone())
                        })
                        .unwrap_or_default();
                    return Err(FlowError::PreconditionFailed(Box::new(
                        PreconditionFailure::Fence {
                            collection,
                            key: "read-basis".into(),
                            expected: fence.token.clone(),
                            actual,
                        },
                    )));
                }
            }

            // Every precondition held. The authority owns both ends of the
            // public document-version lifecycle inside this lock:
            //
            // - a create protected by `requireAbsent` writes version 1;
            // - a write fenced by `expected_version = N` writes N + 1.
            //
            // An unconditional write remains verbatim, and a guard-only
            // precondition writes nothing. See
            // docs/architecture/transaction-version-contract.md.
            let advanced: Vec<AtomicMutation>;
            let creates: HashSet<(&str, &str)> = preconditions
                .iter()
                .filter(|condition| condition.expected_version.is_none())
                .map(|condition| (condition.capability.as_str(), condition.key.as_str()))
                .chain(
                    record_preconditions
                        .iter()
                        .filter(|condition| condition.require_absent)
                        .map(|condition| (condition.capability.as_str(), condition.key.as_str())),
                )
                .collect();
            let fenced: HashMap<(&str, &str), u64> = record_preconditions
                .iter()
                .filter_map(|condition| {
                    condition.expected_version.map(|version| {
                        (
                            (condition.capability.as_str(), condition.key.as_str()),
                            version,
                        )
                    })
                })
                .collect();
            let mutations: &[AtomicMutation] = if fenced.is_empty() && creates.is_empty() {
                mutations
            } else {
                advanced = mutations
                    .iter()
                    .map(|mutation| {
                        let record = (mutation.capability.as_str(), mutation.key.as_str());
                        let version = if creates.contains(&record) {
                            Some(1)
                        } else {
                            fenced.get(&record).map(|expected| expected + 1)
                        };
                        let Some(version) = version else {
                            return mutation.clone();
                        };
                        // A delete has no record left to carry a version, so a
                        // fenced delete is fence-then-remove and nothing more.
                        let Some(Value::Object(fields)) = mutation.value.clone() else {
                            return mutation.clone();
                        };
                        let mut fields = fields;
                        // The caller cannot manufacture either the initial or
                        // next authoritative version.
                        fields.insert("__version".to_string(), Value::from(version));
                        AtomicMutation {
                            capability: mutation.capability.clone(),
                            key: mutation.key.clone(),
                            value: Some(Value::Object(fields)),
                        }
                    })
                    .collect();
                &advanced
            };

            // Transaction bookkeeping is measured as one span per commit even
            // though it brackets two regions: everything before the durable
            // append, and the revision, dedup and payload-hash accounting after
            // the rows are applied. The append itself is `Persistence` and the
            // apply loop is `RecordMutation` + `IndexMaintenance`, so summing
            // this with those does not double-count any of them.
            let mut bookkeeping_nanos = 0u64;
            let prepare_began = workload_diagnostics::enabled().then(std::time::Instant::now);
            let base_revision = inner.authority_revision;
            if let Some(expected) = expected_parent {
                if expected != base_revision {
                    return Err(FlowError::RevisionConflict {
                        expected,
                        actual: base_revision,
                    });
                }
            }
            for condition in preconditions {
                let actual = inner
                    .rows
                    .get(&condition.capability)
                    .and_then(|bucket| bucket.get(&condition.key))
                    .and_then(|row| row.operation.as_ref())
                    .map(|op| op.sequence);
                if actual != condition.expected_version {
                    return Err(FlowError::CapabilityError(format!(
                        "PRECONDITION_FAILED:{}:{}:{:?}:{actual:?}",
                        condition.capability, condition.key, condition.expected_version
                    )));
                }
            }
            let state_before = inner.sequence;
            let mut next_sequence = state_before;
            let mut rows = Vec::with_capacity(mutations.len());
            let mut vector_clock = inner.sync_state.get_or_init_vector_clock().clone();
            for mutation in mutations {
                next_sequence += 1;
                *vector_clock.entry(inner.instance_id.clone()).or_default() += 1;
                let rust_type = "serde_json::Value".to_string();
                let operation = match &mutation.value {
                    Some(value)
                        if inner
                            .rows
                            .get(&mutation.capability)
                            .is_some_and(|bucket| bucket.contains_key(&mutation.key)) =>
                    {
                        Operation::update(
                            next_sequence,
                            inner.instance_id.clone(),
                            next_sequence,
                            mutation.key.clone(),
                            value.clone(),
                            rust_type.clone(),
                            mutation.capability.clone(),
                        )
                    }
                    Some(value) => Operation::insert(
                        next_sequence,
                        inner.instance_id.clone(),
                        next_sequence,
                        mutation.key.clone(),
                        value.clone(),
                        rust_type.clone(),
                        mutation.capability.clone(),
                    ),
                    None => Operation::delete(
                        next_sequence,
                        inner.instance_id.clone(),
                        next_sequence,
                        mutation.key.clone(),
                        rust_type.clone(),
                        mutation.capability.clone(),
                    ),
                }
                .with_vector_clock(vector_clock.clone());
                rows.push(StoredRow {
                    capability: mutation.capability.clone(),
                    key: mutation.key.clone(),
                    rust_type,
                    value: mutation.value.clone().unwrap_or(Value::Null),
                    unix_ms: now_ms(),
                    content_hash: Some(operation.content_hash.clone()),
                    flow_ref: None,
                    deleted: mutation.value.is_none(),
                    operation: Some(operation),
                });
            }
            let mut audit = audit;
            if let Some(object) = audit.as_mut().and_then(Value::as_object_mut) {
                object.insert("state_before".into(), Value::from(state_before));
                object.insert("state_after".into(), Value::from(next_sequence));
            }
            let record = TransactionLogRecord {
                record_type: "feltdb.transaction.v1".into(),
                transaction_id: transaction_id.into(),
                payload_hash: payload_hash.map(str::to_owned),
                state_before,
                state_after: next_sequence,
                base_revision,
                commit_revision: base_revision + 1,
                rows: rows.clone(),
                audit,
            };
            if let Some(began) = prepare_began {
                bookkeeping_nanos += began.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            }
            append_transaction(&inner.path, &record)?;
            // Retained from the record that was just made durable, so the
            // in-memory answer to "who committed this" cannot describe a
            // transaction the log does not hold.
            remember_attribution(
                &mut inner,
                TransactionAttribution::from_record(
                    transaction_id,
                    record.audit.as_ref(),
                    base_revision,
                    base_revision + 1,
                    state_before,
                    next_sequence,
                    &record.rows,
                    now_ms(),
                ),
            );
            inner.authority_revision = base_revision + 1;
            inner.sequence = next_sequence;
            for row in &rows {
                if let Some(operation) = row.operation.clone() {
                    inner.change_log.add_operation(operation);
                }
                if row.deleted {
                    inner.remove_row(&row.capability, &row.key);
                } else {
                    // Record and index change together, inside the commit that
                    // is already holding the state lock: a transaction that is
                    // refused leaves neither, and one that commits publishes both.
                    inner.put_row(row.clone());
                }
            }
            let tail_began = workload_diagnostics::enabled().then(std::time::Instant::now);
            inner.sync_state.merge_vector_clock(&vector_clock);
            inner.applied_transactions.insert(transaction_id.into());
            let commit_revision = inner.authority_revision;
            inner
                .transaction_revisions
                .insert(transaction_id.into(), (base_revision, commit_revision));
            if let Some(hash) = payload_hash {
                inner
                    .transaction_payload_hashes
                    .insert(transaction_id.into(), hash.into());
            }
            if let Some(began) = tail_began {
                bookkeeping_nanos += began.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            }
            workload_diagnostics::record(
                workload_diagnostics::Phase::TransactionCommit,
                bookkeeping_nanos,
            );
            let events = rows
                .iter()
                .map(|row| ChangeEvent {
                    capability: row.capability.clone(),
                    key: row.key.clone(),
                    rust_type: row.rust_type.clone(),
                    unix_ms: row.unix_ms,
                })
                .collect::<Vec<_>>();
            (
                AtomicCommit {
                    transaction_id: transaction_id.into(),
                    base_revision,
                    commit_revision: inner.authority_revision,
                    status: TransactionStatus::Committed,
                    state_before,
                    state_after: inner.sequence,
                    rows,
                    duplicate: false,
                },
                events,
            )
        };
        for event in events {
            let _ = self.event_tx.send(event);
        }
        Ok(commit)
    }

    /// Apply atomic transaction with authorization context (atomic rejection on deny)
    pub fn apply_atomic_transaction_with_authorization(
        &self,
        transaction_id: &str,
        expected_parent: Option<u64>,
        preconditions: &[AtomicPrecondition],
        mutations: &[AtomicMutation],
        audit: Option<Value>,
        context: &AuthorizationContext,
    ) -> Result<AtomicCommit> {
        // Authorize ALL mutations before applying ANY of them (atomicity guarantee)
        self.evaluate_authorization(context)?;

        // Check writable collections for all mutations
        for mutation in mutations {
            if !context.writable_collections.is_empty()
                && !context.writable_collections.contains(&mutation.capability)
            {
                return Err(FlowError::CapabilityError(format!(
                    "AUTHORIZATION_DENIED:not_writable_collection:{}",
                    mutation.capability
                )));
            }
        }

        // All authorized: proceed with atomic transaction (no partial application)
        self.apply_atomic_transaction(
            transaction_id,
            expected_parent,
            preconditions,
            mutations,
            audit,
        )
    }

    fn insert_internal<T: Serialize>(
        &self,
        capability: String,
        key: String,
        value: T,
        require_absent: bool,
    ) -> Result<()> {
        let rust_type = type_name::<T>().to_string();
        let value_json = serde_json::to_value(value)?;
        // Canonical because `serde_json` is built without `preserve_order`, so
        // `Map` is a `BTreeMap` and `to_string` emits keys in sorted order.
        let canonical = value_json.to_string();
        let mut row = StoredRow {
            capability: capability.clone(),
            key: key.clone(),
            rust_type: rust_type.clone(),
            value: value_json.clone(),
            unix_ms: now_ms(),
            content_hash: None,
            flow_ref: None,
            deleted: false,
            operation: None,
        };

        {
            let mut inner = self.state();

            if require_absent
                && inner
                    .rows
                    .get(&capability)
                    .and_then(|bucket| bucket.get(&key))
                    .is_some()
            {
                return Err(FlowError::PreconditionFailed(Box::new(
                    PreconditionFailure::Present {
                        collection: capability,
                        key,
                    },
                )));
            }

            // Increment sequence number
            inner.sequence += 1;

            // Increment vector clock for this instance
            inner.sync_state.increment_vector_clock();

            // Get current vector clock to attach to operation
            let vector_clock = inner.sync_state.get_or_init_vector_clock().clone();

            // The revision this write produces is worked out first, so the
            // operation can name it and a peer can rebuild the same one.
            let planned =
                is_versioned(&capability).then(|| plan_revision(&inner, &key, canonical.clone()));

            // Create an operation with vector clock for convergence
            let mut op = Operation::insert(
                inner.sequence,
                inner.instance_id.clone(),
                inner.sequence,
                key.clone(),
                value_json,
                rust_type.clone(),
                capability.clone(),
            )
            .with_vector_clock(vector_clock);
            op.revision = planned.as_ref().map(provenance_of);

            // Add operation to change log for sync
            row.operation = Some(op.clone());
            inner.change_log.add_operation(op.clone());

            append_event_locked(&mut inner, &row)?;
            let base_revision = inner.authority_revision;
            inner.authority_revision += 1;
            remember_operation_attribution(&mut inner, &row, base_revision, base_revision + 1);
            inner.put_row(row);
            if let Some(revision) = planned {
                persist_revision_locked(&mut inner, revision)?;
            }
            let suffix = key.rsplit_once(':').map(|(_, right)| right);
            if let Some(id) = suffix.and_then(|s| s.parse::<u64>().ok()) {
                let entry = inner.key_counters.entry(capability.clone()).or_default();
                *entry = (*entry).max(id + 1);
            }
        }

        let event = ChangeEvent {
            capability,
            key,
            rust_type,
            unix_ms: now_ms(),
        };
        let _ = self.event_tx.send(event);

        Ok(())
    }

    fn update_internal<T: Serialize>(
        &self,
        capability: String,
        key: String,
        value: T,
    ) -> Result<()> {
        let rust_type = type_name::<T>().to_string();
        let value_json = serde_json::to_value(value)?;
        // Canonical because `serde_json` is built without `preserve_order`, so
        // `Map` is a `BTreeMap` and `to_string` emits keys in sorted order.
        let canonical = value_json.to_string();
        let mut row = StoredRow {
            capability: capability.clone(),
            key: key.clone(),
            rust_type: rust_type.clone(),
            value: value_json.clone(),
            unix_ms: now_ms(),
            content_hash: None,
            flow_ref: None,
            deleted: false,
            operation: None,
        };

        {
            let mut inner = self.state();

            // Increment sequence number
            inner.sequence += 1;

            // Increment vector clock for this instance
            inner.sync_state.increment_vector_clock();

            // Get current vector clock to attach to operation
            let vector_clock = inner.sync_state.get_or_init_vector_clock().clone();

            let planned =
                is_versioned(&capability).then(|| plan_revision(&inner, &key, canonical.clone()));

            // Create an update operation with vector clock for convergence
            let mut op = Operation::update(
                inner.sequence,
                inner.instance_id.clone(),
                inner.sequence,
                key.clone(),
                value_json,
                rust_type.clone(),
                capability.clone(),
            )
            .with_vector_clock(vector_clock);
            op.revision = planned.as_ref().map(provenance_of);

            // Add operation to change log for sync
            row.operation = Some(op.clone());
            inner.change_log.add_operation(op.clone());

            append_event_locked(&mut inner, &row)?;
            let base_revision = inner.authority_revision;
            inner.authority_revision += 1;
            remember_operation_attribution(&mut inner, &row, base_revision, base_revision + 1);
            inner.put_row(row);
            if let Some(revision) = planned {
                persist_revision_locked(&mut inner, revision)?;
            }
        }

        let event = ChangeEvent {
            capability,
            key,
            rust_type,
            unix_ms: now_ms(),
        };
        let _ = self.event_tx.send(event);

        Ok(())
    }

    fn delete_internal(&self, capability: String, key: String) -> Result<()> {
        {
            let mut inner = self.state();

            // Get the row to determine its type
            let rust_type = inner
                .rows
                .get(&capability)
                .and_then(|bucket| bucket.get(&key))
                .map(|row| row.rust_type.clone())
                .unwrap_or_else(|| "Unknown".to_string());

            // Increment sequence number
            inner.sequence += 1;

            // Increment vector clock for this instance
            inner.sync_state.increment_vector_clock();

            // Get current vector clock to attach to operation
            let vector_clock = inner.sync_state.get_or_init_vector_clock().clone();

            // Create a delete operation with vector clock for convergence
            let op = Operation::delete(
                inner.sequence,
                inner.instance_id.clone(),
                inner.sequence,
                key.clone(),
                rust_type.clone(),
                capability.clone(),
            )
            .with_vector_clock(vector_clock);

            // Add operation to change log for sync
            inner.change_log.add_operation(op.clone());

            // Persist a tombstone through the same append-only log used by
            // inserts and updates. Without this, deleted rows reappeared after
            // reopening the database.
            let tombstone = StoredRow {
                capability: capability.clone(),
                key: key.clone(),
                rust_type,
                value: serde_json::Value::Null,
                unix_ms: now_ms(),
                content_hash: None,
                flow_ref: None,
                deleted: true,
                operation: Some(op),
            };
            append_event(&inner.path, &tombstone)?;
            let base_revision = inner.authority_revision;
            inner.authority_revision += 1;
            remember_operation_attribution(
                &mut inner,
                &tombstone,
                base_revision,
                base_revision + 1,
            );

            // Remove from in-memory store, index entries included.
            inner.remove_row(&capability, &key);
        }

        let event = ChangeEvent {
            capability,
            key,
            rust_type: "Deleted".to_string(),
            unix_ms: now_ms(),
        };
        let _ = self.event_tx.send(event);

        Ok(())
    }
    ///
    /// The subscription returns an initial set of matching results, and provides a stream
    /// of updated results whenever records matching the predicate are inserted or updated.
    ///
    /// # Example
    ///
    /// ```ignore
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// use feltdb::open;
    /// let db = open("app.data")?;
    /// let mut subscription = db.live_query::<User, _>(|u| u.active).await?;
    /// let initial_users = subscription.initial_results();
    ///
    /// while let Some(users) = subscription.next().await {
    ///     println!("Active users: {:?}", users);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn live_query<T, F>(&self, predicate: F) -> Result<LiveQuerySubscription<T>>
    where
        T: DeserializeOwned + Send + Sync + 'static,
        F: Fn(&T) -> bool + Send + Sync + 'static,
    {
        let rust_type = type_name::<T>().to_string();

        let initial_results = {
            let mut inner = self.state();
            *inner
                .query_count_by_type
                .entry(rust_type.clone())
                .or_default() += 1;
            if inner.query_count_by_type[&rust_type] >= 3 {
                inner.adaptive_indexes.insert(rust_type.clone());
            }

            let mut out = Vec::new();
            for bucket in inner.rows.values() {
                for row in bucket.values() {
                    if row.rust_type != rust_type {
                        continue;
                    }
                    let parsed = serde_json::from_value::<T>(row.value.clone())?;
                    if predicate(&parsed) {
                        out.push(parsed);
                    }
                }
            }
            out
        };

        let rx = self.event_tx.subscribe();
        let db = self.clone();
        let subscription = LiveQuerySubscription {
            initial_results,
            rx,
            db,
            rust_type,
            predicate: Arc::new(Mutex::new(None)),
        };

        Ok(subscription)
    }

    /// Get current sync state information
    pub fn sync_info(&self) -> Result<SyncState> {
        let inner = self.state();
        Ok(inner.sync_state.clone())
    }

    /// The authority's wall-clock time in milliseconds.
    ///
    /// The same reading the commit-time lease predicate uses, exposed so a
    /// decision that depends on lease expiry can be made against the clock that
    /// will later judge the commit. A caller's own clock is never a substitute.
    pub fn authority_time_ms(&self) -> u64 {
        now_ms() as u64
    }

    /// Read an untyped value for protocol adapters such as the HTTP server.
    pub fn get_value(&self, key: &str) -> Result<Option<Value>> {
        self.get::<Value>(key)
    }

    /// List the canonical values in one collection/capability namespace.
    ///
    /// This clones the whole collection, so its cost is linear in collection
    /// size. Readers that discard most of what they receive — a filtered query,
    /// for example — should use [`FeltDb::query_collection`] instead, which
    /// evaluates borrowed rows and clones only what it keeps.
    pub fn list_collection(&self, capability: &str) -> Result<Vec<StoredRow>> {
        let inner = self.state();
        let rows: Vec<StoredRow> = inner
            .rows
            .get(capability)
            .map(|rows| rows.values().cloned().collect())
            .unwrap_or_default();
        query_execution_diagnostics::record_full_materialization(rows.len());
        Ok(rows)
    }

    /// Evaluate a predicate over one collection without materializing it.
    ///
    /// The predicate observes each live row *borrowed* from state, so a record
    /// the predicate rejects is never cloned. Only retained rows are cloned,
    /// which happens while state protection is still held: the returned rows are
    /// owned, so nothing the caller keeps can reference storage a concurrent
    /// mutation may move or drop after the lock is released.
    ///
    /// Traversal is the collection's own record-key order, the same order
    /// [`FeltDb::list_collection_page`] walks. `limit` bounds accumulation and
    /// stops the traversal as soon as it is satisfied; because early stopping
    /// truncates in record-key order, a caller that must order results by some
    /// other field has to pass `None` and bound its own already-filtered set.
    ///
    /// The predicate runs under the state lock, so it must not call back into
    /// the database or block.
    pub fn query_collection<F>(
        &self,
        capability: &str,
        limit: Option<usize>,
        mut predicate: F,
    ) -> Result<Vec<StoredRow>>
    where
        F: FnMut(&StoredRow) -> bool,
    {
        let mut matched = Vec::new();
        let mut visited = 0usize;
        {
            let inner = self.state();
            let _query = workload_diagnostics::span(workload_diagnostics::Phase::ScanQuery);
            if let Some(rows) = inner.rows.get(capability) {
                for row in rows.values() {
                    if limit.is_some_and(|bound| matched.len() >= bound) {
                        break;
                    }
                    visited += 1;
                    // The same predicate phase the indexed execution reports to,
                    // so the two executions' predicate cost is directly
                    // comparable rather than being two different measurements.
                    let _predicate = workload_diagnostics::span(
                        workload_diagnostics::Phase::PredicateEvaluation,
                    );
                    if predicate(row) {
                        drop(_predicate);
                        matched.push(row.clone());
                    }
                }
            }
        }
        query_execution_diagnostics::record_scan(visited, matched.len());
        Ok(matched)
    }

    // ----------------------------------------------------------------------
    // Equality index — lifecycle and execution.
    //
    // The index is derived state. Every method below either declares what is
    // derived, re-derives it from authoritative records, or reads candidate
    // record keys out of it. None of them is a source of truth, and none of
    // them can answer a query on its own: `query_collection_by_equality` still
    // evaluates the caller's full predicate against authoritative records.
    // ----------------------------------------------------------------------

    /// Acquire the state lock, attributing the wait and the hold.
    ///
    /// Every path in this file that touches authoritative state goes through
    /// here, which is what makes lock contention a measured quantity rather than
    /// a residual inferred from wall-clock time.
    fn state(&self) -> StateGuard<'_> {
        if !workload_diagnostics::enabled() {
            return StateGuard {
                inner: self.inner.lock().expect("lock poisoned"),
                held_since: None,
            };
        }
        let requested = std::time::Instant::now();
        let inner = self.inner.lock().expect("lock poisoned");
        let acquired = std::time::Instant::now();
        workload_diagnostics::record(
            workload_diagnostics::Phase::StateLockWait,
            acquired
                .duration_since(requested)
                .as_nanos()
                .min(u64::MAX as u128) as u64,
        );
        StateGuard {
            inner,
            held_since: Some(acquired),
        }
    }

    /// Declare an equality index on one collection field and populate it from
    /// authoritative state. Returns whether this call created the index.
    ///
    /// Creation and population happen under one lock, so an index is never
    /// observable in a half-built state. Declaring an index that already exists
    /// is not an error and does not rebuild it.
    ///
    /// `recordId` is refused: on the query surface it is authority metadata that
    /// shadows a document field of the same name on object-valued records only,
    /// so an index over the document field would answer a different question
    /// from the one the predicate asks.
    pub fn create_equality_index(&self, collection: &str, field: &str) -> Result<bool> {
        let mut inner = self.state();
        let created = inner
            .equality_index
            .declare(collection, field)
            .map_err(FlowError::CapabilityError)?;
        if created {
            let Inner {
                rows,
                equality_index,
                ..
            } = &mut *inner;
            equality_index.rebuild_field(collection, field, rows);
        }
        Ok(created)
    }

    /// Drop an equality index and its buckets. Returns whether one existed.
    pub fn drop_equality_index(&self, collection: &str, field: &str) -> Result<bool> {
        let mut inner = self.state();
        Ok(inner.equality_index.undeclare(collection, field))
    }

    /// Every declared index, as `(collection, field)` in a stable order.
    pub fn equality_indexes(&self) -> Result<Vec<(String, String)>> {
        let inner = self.state();
        Ok(inner.equality_index.declarations())
    }

    /// Re-derive every declared index from authoritative records.
    ///
    /// This is the recovery path. An index is never read from disk and never
    /// received from a peer: after a restart, a snapshot install, or any other
    /// reload of authoritative state, the index is whatever this produces.
    pub fn rebuild_equality_indexes(&self) -> Result<EqualityIndexStats> {
        let mut inner = self.state();
        let Inner {
            rows,
            equality_index,
            ..
        } = &mut *inner;
        equality_index.rebuild(rows);
        Ok(equality_index.stats())
    }

    /// Index size, as counts. Never a value, never a record id.
    pub fn equality_index_stats(&self) -> Result<EqualityIndexStats> {
        let inner = self.state();
        Ok(inner.equality_index.stats())
    }

    /// A fully ordered rendering of the live index.
    ///
    /// Test instrumentation. It exists so a test can compare the live index
    /// against one rebuilt from authoritative state; nothing in execution reads
    /// it, and it is not part of any product surface.
    #[doc(hidden)]
    pub fn equality_index_snapshot(
        &self,
    ) -> Result<BTreeMap<String, BTreeMap<String, BTreeMap<String, Vec<String>>>>> {
        let inner = self.state();
        Ok(inner.equality_index.snapshot())
    }

    /// The index invariant: the live index equals one derived from records now.
    ///
    /// Returns a description of the first divergence rather than a bare bool, so
    /// a failing assertion says what diverged. This is the strongest statement
    /// available about index maintenance, because it compares the maintained
    /// structure against the authoritative state it claims to describe rather
    /// than against a second copy of its own bookkeeping.
    pub fn verify_equality_index(&self) -> Result<std::result::Result<(), String>> {
        let inner = self.state();
        let live = inner.equality_index.snapshot();
        let mut expected = inner.equality_index.clone();
        expected.clear();
        expected.rebuild(&inner.rows);
        let expected = expected.snapshot();
        if live == expected {
            return Ok(Ok(()));
        }
        for (collection, fields) in expected.iter() {
            for (field, buckets) in fields {
                let observed = live.get(collection).and_then(|fields| fields.get(field));
                if observed != Some(buckets) {
                    return Ok(Err(format!(
                        "index for {collection}.{field} diverges from authoritative state: \
                         {} value buckets held, {} derived",
                        observed.map(BTreeMap::len).unwrap_or(0),
                        buckets.len(),
                    )));
                }
            }
        }
        Ok(Err(
            "the live index holds entries no authoritative record derives".to_string(),
        ))
    }

    /// Candidate record keys for one equality condition, copied out of the index.
    ///
    /// `None` means the index cannot answer that condition — the field is not
    /// indexed, or the value is not an indexable scalar — and the caller must
    /// fall back to a scan. `Some(empty)` is an answer, not a failure.
    ///
    /// The keys are copied while the state lock is held. Nothing borrowed from
    /// the index or from the record map escapes this call.
    pub fn equality_index_candidates(
        &self,
        collection: &str,
        field: &str,
        value: &Value,
    ) -> Result<Option<Vec<String>>> {
        let inner = self.state();
        Ok(inner
            .equality_index
            .candidates(collection, field, value)
            .map(|keys| keys.iter().cloned().collect()))
    }

    /// Execute a bounded query through the equality index when one applies.
    ///
    /// `equalities` are the query's equality conditions, in the caller's order.
    /// At least one of them must be indexed and hold an indexable value, or this
    /// returns `Ok(None)` and the caller falls back to [`FeltDb::query_collection`].
    /// Returning `None` rather than an empty result is the difference between
    /// "the index does not apply" and "nothing matches", and conflating the two
    /// would silently drop records.
    ///
    /// When the index does apply, every applicable condition contributes a
    /// candidate set and the sets are intersected smallest-first. The result is
    /// still only *candidates*: `predicate` — the caller's existing, unmodified
    /// predicate — decides what matches, evaluated against authoritative records.
    /// The index therefore cannot change what a query returns, only how many
    /// records the predicate has to look at.
    ///
    /// Candidate keys, the record lookups they drive and the predicate all run
    /// under one acquisition of the state lock, so a query observes one coherent
    /// committed state exactly as a scan does. Records are cloned only when the
    /// predicate keeps them, and nothing borrowed from state outlives the lock.
    pub fn query_collection_by_equality<F>(
        &self,
        capability: &str,
        equalities: &[(&str, &Value)],
        mut predicate: F,
    ) -> Result<Option<Vec<StoredRow>>>
    where
        F: FnMut(&StoredRow) -> bool,
    {
        let mut matched = Vec::new();
        let mut examined = 0usize;
        let mut evaluated = 0usize;
        {
            let inner = self.state();
            let _query = workload_diagnostics::span(workload_diagnostics::Phase::IndexedQuery);
            // Candidate selection, timed apart from record access: PR36 has to
            // be able to say whether the index lookup itself has become a cost,
            // and "index query time" alone could not distinguish a slow lookup
            // from a large candidate set.
            let lookup = workload_diagnostics::span(workload_diagnostics::Phase::IndexLookup);
            let mut applicable: Vec<&BTreeSet<String>> = Vec::new();
            for (field, value) in equalities {
                if let Some(keys) = inner.equality_index.candidates(capability, field, value) {
                    applicable.push(keys);
                }
            }
            if applicable.is_empty() {
                drop(lookup);
                drop(_query);
                drop(inner);
                query_execution_diagnostics::record_index_miss();
                return Ok(None);
            }
            // Smallest first. This is not cost estimation: a set's length is
            // already held by the set, so intersecting from the smallest is
            // simply the cheaper order to do the same work.
            applicable.sort_by_key(|keys| keys.len());
            drop(lookup);
            let (smallest, rest) = applicable.split_first().expect("non-empty");
            let bucket = inner.rows.get(capability);
            for key in smallest.iter() {
                if !rest.iter().all(|keys| keys.contains(key)) {
                    continue;
                }
                examined += 1;
                let retrieval =
                    workload_diagnostics::span(workload_diagnostics::Phase::CandidateRetrieval);
                let row = bucket.and_then(|rows| rows.get(key));
                drop(retrieval);
                let Some(row) = row else {
                    // An index entry with no record behind it would be a
                    // maintenance defect. Skipping it keeps the query correct
                    // rather than surfacing a record that does not exist.
                    continue;
                };
                evaluated += 1;
                let _predicate =
                    workload_diagnostics::span(workload_diagnostics::Phase::PredicateEvaluation);
                if predicate(row) {
                    drop(_predicate);
                    matched.push(row.clone());
                }
            }
        }
        query_execution_diagnostics::record_indexed_query(examined, evaluated, matched.len());
        Ok(Some(matched))
    }

    /// Traverse one collection in immutable record-key order without materializing it.
    pub fn list_collection_page(
        &self,
        capability: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredRow>> {
        let inner = self.state();
        let Some(rows) = inner.rows.get(capability) else {
            return Ok(vec![]);
        };
        let values: Box<dyn Iterator<Item = &StoredRow>> = match after {
            Some(key) => Box::new(
                rows.range::<str, _>((Excluded(key), Unbounded))
                    .map(|(_, row)| row),
            ),
            None => Box::new(rows.values()),
        };
        Ok(values
            .filter(|row| !row.deleted)
            .take(limit)
            .cloned()
            .collect())
    }

    /// Fetch one live collection record without materializing collection state.
    pub fn get_collection_record(&self, capability: &str, key: &str) -> Result<Option<StoredRow>> {
        let inner = self.state();
        Ok(inner
            .rows
            .get(capability)
            .and_then(|rows| rows.get(key))
            .filter(|row| !row.deleted)
            .cloned())
    }

    /// Fetch several live collection records without materializing collection state.
    pub fn get_collection_records<'a>(
        &self,
        capability: &str,
        keys: impl IntoIterator<Item = &'a str>,
    ) -> Result<Vec<StoredRow>> {
        let inner = self.state();
        let Some(rows) = inner.rows.get(capability) else {
            return Ok(Vec::new());
        };
        Ok(keys
            .into_iter()
            .filter_map(|key| rows.get(key))
            .filter(|row| !row.deleted)
            .cloned()
            .collect())
    }

    /// Subscribe to canonical mutation events.
    pub fn subscribe_changes(&self) -> broadcast::Receiver<ChangeEvent> {
        self.event_tx.subscribe()
    }

    /// Add a peer for synchronization
    pub fn add_sync_peer(&self, peer_id: String) -> Result<()> {
        let mut inner = self.state();
        inner.sync_state.add_peer(peer_id);
        Ok(())
    }

    /// Remove a peer from synchronization
    pub fn remove_sync_peer(&self, peer_id: &str) -> Result<()> {
        let mut inner = self.state();
        inner.sync_state.remove_peer(peer_id);
        Ok(())
    }

    /// Get pending operations for a peer
    pub fn get_pending_for_peer(
        &self,
        _peer_id: &str,
        since_sequence: u64,
    ) -> Result<Vec<Operation>> {
        let inner = self.state();
        let pending: Vec<Operation> = inner
            .change_log
            .pending_operations()
            .into_iter()
            .filter(|op| op.sequence > since_sequence)
            .collect();
        Ok(pending)
    }

    /// Return operations missing from a peer's per-origin version vector.
    pub fn operations_since(
        &self,
        versions: &std::collections::HashMap<String, u64>,
    ) -> Result<Vec<Operation>> {
        let inner = self.state();
        Ok(inner.change_log.operations_since(versions))
    }

    /// Return the highest durable operation sequence observed from each origin.
    pub fn operation_versions(&self) -> Result<std::collections::HashMap<String, u64>> {
        let inner = self.state();
        Ok(inner.change_log.versions())
    }

    pub fn export_snapshot(&self) -> Result<DatabaseSnapshot> {
        let inner = self.state();
        let mut rows: Vec<StoredRow> = inner
            .rows
            .values()
            .flat_map(|bucket| bucket.values().cloned())
            .collect();
        for row in &mut rows {
            row.operation = None;
        }
        rows.sort_by(|left, right| left.key.cmp(&right.key));
        let versions = inner.change_log.versions();
        let content_hash = DatabaseSnapshot::compute_hash(&rows, &versions)?;
        Ok(DatabaseSnapshot {
            rows,
            versions,
            content_hash,
            format_version: DURABLE_FORMAT_VERSION,
        })
    }
    /// Current materialized rows including their causal operation envelopes.
    /// State-contract execution uses this for record-version preconditions;
    /// portable snapshots intentionally continue to strip those envelopes.
    pub fn state_rows(&self) -> Result<Vec<StoredRow>> {
        let inner = self.state();
        Ok(inner
            .rows
            .values()
            .flat_map(|bucket| bucket.values().cloned())
            .collect())
    }

    /// Materialized rows belonging to one application state namespace.
    ///
    /// State-contract capabilities are encoded as `<state namespace>:<collection>`.
    /// Selecting the capability buckets before cloning prevents one application's
    /// read snapshot from materializing every other application's durable rows.
    pub fn state_rows_for_namespace(&self, state_namespace: &str) -> Result<Vec<StoredRow>> {
        let _span = workload_diagnostics::span(workload_diagnostics::Phase::NamespaceSnapshot);
        let inner = self.state();
        let prefix = format!("{state_namespace}:");
        Ok(inner
            .rows
            .iter()
            .filter(|(capability, _)| capability.starts_with(&prefix))
            .flat_map(|(_, bucket)| bucket.values().cloned())
            .collect())
    }

    pub fn can_install_snapshot(&self) -> Result<bool> {
        let inner = self.state();
        Ok(!inner.bootstrapped && inner.rows.is_empty() && inner.change_log.pending.is_empty())
    }

    /// Install a verified bootstrap snapshot into a pristine node. A node with
    /// local history must converge through operations rather than overwrite it.
    pub fn install_snapshot(&self, snapshot: DatabaseSnapshot) -> Result<()> {
        // A snapshot carries whole rows, revisions included, so installing one
        // from another format would reintroduce exactly the silent
        // misinterpretation the on-disk version prevents.
        if snapshot.format_version != DURABLE_FORMAT_VERSION {
            return Err(FlowError::IncompatibleFormat(Box::new(
                FormatIncompatibility {
                    found: if snapshot.format_version == 0 {
                        DurableFormat::Unversioned
                    } else {
                        DurableFormat::Versioned(snapshot.format_version)
                    },
                    required: DURABLE_FORMAT_VERSION,
                    reason: "the snapshot was produced in a different durable format".to_string(),
                    action: "Take a fresh snapshot from a FeltDB build matching this one."
                        .to_string(),
                },
            )));
        }
        snapshot.verify()?;
        let mut inner = self.state();
        if !inner.rows.is_empty() || !inner.change_log.pending.is_empty() {
            return Err(FlowError::CapabilityError(
                "snapshot bootstrap requires a pristine node".to_string(),
            ));
        }
        for row in &snapshot.rows {
            if row.deleted
                || row.operation.is_some()
                || !row.key.starts_with(&format!("{}:", row.capability))
            {
                return Err(FlowError::CapabilityError(
                    "snapshot contains an invalid state row".to_string(),
                ));
            }
        }

        let temporary = inner.path.with_extension("snapshot.tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temporary)?;
        write_json_line(
            &mut file,
            &LogHeader {
                record_type: "feltdb.snapshot.v1".to_string(),
                format_version: DURABLE_FORMAT_VERSION,
                local_sequence: inner.sequence,
                observed_versions: snapshot.versions.clone(),
                transaction_history: Vec::new(),
                transaction_payload_hashes: HashMap::new(),
            },
        )?;
        for row in &snapshot.rows {
            write_json_line(&mut file, row)?;
        }
        file.flush()?;
        file.sync_all()?;
        fs::rename(temporary, &inner.path)?;

        inner.clear_rows();
        inner.key_counters.clear();
        for row in snapshot.rows {
            if let Some(id) = row
                .key
                .rsplit_once(':')
                .and_then(|(_, right)| right.parse::<u64>().ok())
            {
                let counter = inner
                    .key_counters
                    .entry(row.capability.clone())
                    .or_default();
                *counter = (*counter).max(id + 1);
            }
            inner.put_row(row);
        }
        inner.change_log.observed_versions = snapshot.versions;
        inner.bootstrapped = true;
        persist_sync_metadata(&inner)
    }

    /// Acknowledge receipt of operations from a peer
    pub fn acknowledge_peer_operations(&self, peer_id: String, sequence: u64) -> Result<()> {
        let mut inner = self.state();
        inner.change_log.acknowledge_peer(peer_id, sequence);
        persist_sync_metadata(&inner)?;
        Ok(())
    }

    /// Durably record a peer's per-origin acknowledgement vector.
    pub fn acknowledge_peer_versions(
        &self,
        peer_id: String,
        versions: HashMap<String, u64>,
    ) -> Result<()> {
        let mut inner = self.state();
        inner.change_log.acknowledge_versions(peer_id, versions);
        persist_sync_metadata(&inner)
    }

    pub fn acknowledged_peer_versions(&self, peer_id: &str) -> Result<HashMap<String, u64>> {
        let inner = self.state();
        Ok(inner.change_log.acknowledged_versions(peer_id))
    }

    /// Earliest operation still available for incremental replay, by origin.
    /// Recent commits and who made them, newest first.
    ///
    /// Bounded by [`TRANSACTION_ATTRIBUTION_WINDOW`]. A transaction older than
    /// the window is not absent from history — it is in the durable log — it is
    /// simply no longer answerable from memory, and callers that present this
    /// to a human must say which of the two they are showing.
    pub fn recent_transactions(&self, limit: usize) -> Result<Vec<TransactionAttribution>> {
        let inner = self.state();
        Ok(inner
            .transaction_attributions
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect())
    }

    /// One transaction's durable attribution, regardless of the operator
    /// window. This lookup is canonical historical provenance.
    pub fn transaction_attribution(
        &self,
        transaction_id: &str,
    ) -> Result<Option<TransactionAttribution>> {
        let inner = self.state();
        Ok(inner
            .transaction_history
            .iter()
            .rev()
            .find(|attribution| attribution.transaction_id == transaction_id)
            .cloned())
    }

    /// Canonical commit history in authority order, suitable for resumable
    /// event projection and historical provenance reconstruction.
    pub fn committed_transactions_after(
        &self,
        commit_revision: u64,
        limit: usize,
    ) -> Result<Vec<TransactionAttribution>> {
        let inner = self.state();
        Ok(inner
            .transaction_history
            .iter()
            .filter(|transaction| transaction.commit_revision > commit_revision)
            .take(limit)
            .cloned()
            .collect())
    }

    /// Durable commits that named this record, newest first.
    ///
    /// This deliberately reads complete canonical history rather than the
    /// bounded operator window. Historical provenance must not disappear when
    /// a cache rolls over.
    pub fn transactions_touching(
        &self,
        capability: &str,
        key: &str,
        limit: usize,
    ) -> Result<Vec<TransactionAttribution>> {
        let inner = self.state();
        Ok(inner
            .transaction_history
            .iter()
            .rev()
            .filter(|attribution| attribution.touched(capability, key))
            .take(limit)
            .cloned()
            .collect())
    }

    /// How much of the attribution window is in use, and what it can hold.
    ///
    /// Reported so a reader can tell an idle system from a window that has
    /// already rolled over.
    pub fn transaction_attribution_window(&self) -> Result<(usize, usize)> {
        let inner = self.state();
        Ok((
            inner.transaction_attributions.len(),
            TRANSACTION_ATTRIBUTION_WINDOW,
        ))
    }

    pub fn retained_operation_floors(&self) -> Result<HashMap<String, u64>> {
        let inner = self.state();
        Ok(inner.change_log.retained_from())
    }

    pub fn acknowledged_operation_count(&self, active_peers: &[String]) -> Result<usize> {
        let inner = self.state();
        Ok(inner.change_log.acknowledged_operation_count(active_peers))
    }

    /// Atomically replace acknowledged history with a state snapshot and any
    /// operations still needed by at least one configured peer.
    pub fn compact_operation_log(&self, active_peers: &[String]) -> Result<usize> {
        self.compact_internal(active_peers, true)
            .map(|outcome| outcome.pruned())
    }

    /// Compact under the database's [`CompactionPolicy`].
    ///
    /// Prunes acknowledged operations from memory whenever it can, and rewrites
    /// the durable log **only when enough has accumulated to justify the
    /// stall**. Use this on a timer; use
    /// [`compact_operation_log`](Self::compact_operation_log) when a caller is
    /// explicitly asking for a rewrite now.
    ///
    /// The distinction is the whole point. Pruning is cheap and in memory.
    /// Rewriting serializes every row in the database and fsyncs it while
    /// holding the one lock that serializes every read and every write, so
    /// doing it on a short timer stops the world on that timer.
    pub fn maybe_compact_operation_log(
        &self,
        active_peers: &[String],
    ) -> Result<CompactionOutcome> {
        self.compact_internal(active_peers, false)
    }

    /// The policy deciding when a timer-driven compaction rewrites the log.
    pub fn compaction_policy(&self) -> CompactionPolicy {
        self.state().compaction_policy
    }

    /// Set the policy deciding when a timer-driven compaction rewrites the log.
    pub fn set_compaction_policy(&self, policy: CompactionPolicy) {
        self.state().compaction_policy = policy;
    }

    fn compact_internal(&self, active_peers: &[String], force: bool) -> Result<CompactionOutcome> {
        let mut inner = self.state();
        let removed = inner.change_log.prune_acknowledged(active_peers);
        if removed == 0 {
            return Ok(CompactionOutcome::Idle);
        }
        persist_sync_metadata(&inner)?;
        inner.pruned_since_rewrite += removed;

        // The cheap half is done. The expensive half runs only when asked for
        // explicitly, or when the accumulated gain has earned it.
        if !force && inner.pruned_since_rewrite < inner.compaction_policy.rewrite_after_pruned {
            return Ok(CompactionOutcome::Deferred {
                pruned: removed,
                pending_since_rewrite: inner.pruned_since_rewrite,
            });
        }

        let temporary = inner.path.with_extension("compact.tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temporary)?;
        write_json_line(
            &mut file,
            &LogHeader {
                record_type: "feltdb.snapshot.v1".to_string(),
                format_version: DURABLE_FORMAT_VERSION,
                local_sequence: inner.sequence,
                observed_versions: inner.change_log.versions(),
                transaction_history: inner.transaction_history.clone(),
                transaction_payload_hashes: inner.transaction_payload_hashes.clone(),
            },
        )?;
        for operation in inner.change_log.pending_operations() {
            write_json_line(&mut file, &stored_row_for_operation(operation))?;
        }
        for bucket in inner.rows.values() {
            for row in bucket.values() {
                let mut snapshot_row = row.clone();
                snapshot_row.operation = None;
                write_json_line(&mut file, &snapshot_row)?;
            }
        }
        file.flush()?;
        file.sync_all()?;
        fs::rename(temporary, &inner.path)?;
        inner.bootstrapped = true;
        inner.pruned_since_rewrite = 0;
        Ok(CompactionOutcome::Rewritten { pruned: removed })
    }

    /// Register a peer in the distributed fabric
    pub fn register_peer(&self, advertisement: PeerAdvertisement) -> Result<()> {
        let inner = self.state();
        inner
            .peer_registry
            .register(advertisement)
            .map_err(|e| FlowError::CapabilityError(e))
    }

    /// Get a peer's advertisement
    pub fn get_peer(&self, peer_id: &PeerId) -> Result<Option<PeerAdvertisement>> {
        let inner = self.state();
        inner
            .peer_registry
            .get(peer_id)
            .map_err(|e| FlowError::CapabilityError(e))
    }

    /// Find all peers providing a namespace
    pub fn peers_for_namespace(&self, namespace: &str) -> Result<Vec<PeerAdvertisement>> {
        let inner = self.state();
        inner
            .peer_registry
            .peers_for_namespace(namespace)
            .map_err(|e| FlowError::CapabilityError(e))
    }

    /// Find all peers providing a capability
    pub fn peers_for_capability(&self, capability: &str) -> Result<Vec<PeerAdvertisement>> {
        let inner = self.state();
        inner
            .peer_registry
            .peers_for_capability(capability)
            .map_err(|e| FlowError::CapabilityError(e))
    }

    /// Get all registered peers
    pub fn all_peers(&self) -> Result<Vec<PeerAdvertisement>> {
        let inner = self.state();
        inner
            .peer_registry
            .all_peers()
            .map_err(|e| FlowError::CapabilityError(e))
    }

    /// Resolve a FlowRef locally
    pub fn resolve_ref<T: DeserializeOwned>(&self, flow_ref: &FlowRef) -> Result<Option<T>> {
        // First, try to resolve locally
        let key = format!("{}:{}", flow_ref.namespace, flow_ref.object_id);
        self.get::<T>(&key)
    }

    /// Get current sync state
    pub fn sync_state(&self) -> Result<SyncState> {
        let inner = self.state();
        Ok(inner.sync_state.clone())
    }

    /// Apply an operation from a remote peer
    /// This merges the vector clock, resolves conflicts, and applies the mutation
    pub fn apply_remote_operation(&self, op: Operation) -> Result<()> {
        let mut inner = self.state();

        if op.content_hash != op.compute_content_hash() {
            return Err(FlowError::CapabilityError(
                "remote operation content hash mismatch".to_string(),
            ));
        }
        if inner.change_log.has_operation(&op.instance_id, op.sequence) {
            return Ok(());
        }
        let expected = inner
            .change_log
            .versions()
            .get(&op.instance_id)
            .copied()
            .unwrap_or(0)
            + 1;
        if op.sequence != expected {
            return Err(FlowError::CapabilityError(format!(
                "remote operation sequence gap for {}: expected {}, received {}",
                op.instance_id, expected, op.sequence
            )));
        }

        // Merge the remote vector clock into our own
        if let Some(remote_clock) = &op.vector_clock {
            inner.sync_state.merge_vector_clock(remote_clock);
        }

        // Check if we have a local operation for this key that hasn't been synced yet
        let local_op_for_key = inner
            .change_log
            .pending_operations()
            .into_iter()
            .find(|local_op| local_op.key == op.key && local_op.capability == op.capability);

        // Determine which operation to apply (remote or local if conflict)
        let op_to_apply = if let Some(local_op) = local_op_for_key {
            // We have a conflict - use LWW with vector clock awareness
            // Create causal histories for conflict resolution
            let local_vc = local_op.vector_clock.clone().unwrap_or_default();
            let remote_vc = op.vector_clock.clone().unwrap_or_default();

            let local_history = CausalHistory::new(
                StateVersion::from_clocks(local_vc, local_op.timestamp_ms),
                local_op.instance_id.clone(),
                local_op.sequence,
            );

            let remote_history = CausalHistory::new(
                StateVersion::from_clocks(remote_vc, op.timestamp_ms),
                op.instance_id.clone(),
                op.sequence,
            );

            // Resolve using LWW strategy
            let resolution = ConflictResolution::resolve_lww(&[local_history, remote_history]);

            // Winner index: 0 = local, 1 = remote
            if resolution.winner_index == Some(1) {
                op.clone()
            } else {
                // The local value wins, but the received operation still belongs
                // to the durable causal history so peers do not resend it forever.
                if let Some(mut history_row) = inner
                    .rows
                    .get(&op.capability)
                    .and_then(|rows| rows.get(&op.key))
                    .cloned()
                {
                    history_row.operation = Some(op.clone());
                    append_event(&inner.path, &history_row)?;
                    inner.authority_revision += 1;
                }
                inner.change_log.add_operation(op);
                inner.sync_state.increment_received(1);
                return Ok(());
            }
        } else {
            op.clone()
        };

        // Apply the winning operation to our state
        let row = StoredRow {
            capability: op_to_apply.capability.clone(),
            key: op_to_apply.key.clone(),
            rust_type: op_to_apply.rust_type.clone(),
            value: op_to_apply.value.clone().unwrap_or(serde_json::Value::Null),
            unix_ms: op_to_apply.timestamp_ms,
            content_hash: Some(op_to_apply.content_hash.clone()),
            flow_ref: None,
            deleted: op_to_apply.op_type == OperationType::Delete,
            operation: Some(op.clone()),
        };

        match op_to_apply.op_type {
            OperationType::Insert | OperationType::Update => {
                append_event(&inner.path, &row)?;
                inner.authority_revision += 1;
                inner.put_row(row);
            }
            OperationType::Delete => {
                append_event(&inner.path, &row)?;
                inner.authority_revision += 1;
                inner.remove_row(&op_to_apply.capability, &op_to_apply.key);
            }
        }
        if let Some(id) = op_to_apply
            .key
            .rsplit_once(':')
            .and_then(|(_, right)| right.parse::<u64>().ok())
        {
            let counter = inner
                .key_counters
                .entry(op_to_apply.capability.clone())
                .or_default();
            *counter = (*counter).max(id + 1);
        }

        // Reconstruct the historical fact the originating authority recorded.
        //
        // Not a local mint of the resulting state: the identity, parent and
        // sequence come from the operation, so the peer ends up holding the
        // *same* revision rather than an equivalent-looking one of its own.
        // Deliberately independent of which value won any last-writer race
        // above — the origin committed this revision whether or not its value
        // is what is current here, and dropping it would lose the divergence
        // that reconciliation needs to see.
        if let Some(provenance) = op.revision.clone() {
            if let Some(value) = op.value.clone() {
                let revision = reconstruct_revision(&provenance, &value)?;
                persist_revision_locked(&mut inner, revision)?;
            }
        }

        inner.change_log.add_operation(op);
        inner.sync_state.increment_received(1);
        let event = ChangeEvent {
            capability: op_to_apply.capability,
            key: op_to_apply.key,
            rust_type: op_to_apply.rust_type,
            unix_ms: op_to_apply.timestamp_ms,
        };
        drop(inner);
        let _ = self.event_tx.send(event);

        Ok(())
    }

    /// Get the current vector clock state
    pub fn get_vector_clock(&self) -> Result<Option<std::collections::HashMap<String, u64>>> {
        let inner = self.state();
        Ok(inner.sync_state.vector_clock.clone())
    }
}

#[derive(Clone)]
pub struct Capability {
    db: FeltDb,
    name: String,
}

impl Capability {
    pub fn insert<T: Serialize>(&self, value: T) -> Result<String> {
        let key = {
            let mut inner = self.db.state();
            let next = inner.key_counters.entry(self.name.clone()).or_default();
            let key = format!("{}:{}", self.name, *next);
            *next += 1;
            key
        };
        self.db
            .insert_internal(self.name.clone(), key.clone(), value, false)?;
        Ok(key)
    }

    pub fn insert_with_key<T: Serialize>(&self, key: &str, value: T) -> Result<()> {
        let full_key = if key.contains(':') {
            key.to_string()
        } else {
            format!("{}:{key}", self.name)
        };
        self.db
            .insert_internal(self.name.clone(), full_key, value, false)
    }

    pub fn update<T: Serialize>(&self, key: &str, value: T) -> Result<()> {
        let full_key = if key.contains(':') {
            key.to_string()
        } else {
            format!("{}:{key}", self.name)
        };
        self.db.update_internal(self.name.clone(), full_key, value)
    }

    pub fn delete(&self, key: &str) -> Result<()> {
        let full_key = if key.contains(':') {
            key.to_string()
        } else {
            format!("{}:{key}", self.name)
        };
        self.db.delete_internal(self.name.clone(), full_key)
    }

    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let full_key = if key.contains(':') {
            key.to_string()
        } else {
            format!("{}:{key}", self.name)
        };
        self.db.get(&full_key)
    }

    pub fn find<T, F>(&self, predicate: F) -> Result<Vec<T>>
    where
        T: DeserializeOwned,
        F: Fn(&T) -> bool,
    {
        let rust_type = type_name::<T>().to_string();
        let mut inner = self.db.state();
        *inner
            .query_count_by_type
            .entry(rust_type.clone())
            .or_default() += 1;
        if inner.query_count_by_type[&rust_type] >= 3 {
            inner.adaptive_indexes.insert(rust_type.clone());
        }

        let mut out = Vec::new();
        if let Some(bucket) = inner.rows.get(&self.name) {
            for row in bucket.values() {
                if row.rust_type != rust_type {
                    continue;
                }
                let parsed = serde_json::from_value::<T>(row.value.clone())?;
                if predicate(&parsed) {
                    out.push(parsed);
                }
            }
        }
        Ok(out)
    }

    /// Creates a live query subscription scoped to this capability.
    ///
    /// The subscription returns initial matching results from this capability only, and streams
    /// updates whenever records within this capability matching the predicate are inserted or updated.
    ///
    /// # Example
    ///
    /// ```ignore
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// use feltdb::open;
    /// let db = open("app.data")?;
    /// let users = db.capability("users");
    /// let mut subscription = users.live_find::<User, _>(|u| u.active).await?;
    /// let initial_users = subscription.initial_results();
    ///
    /// while let Some(users) = subscription.next().await {
    ///     println!("Active users: {:?}", users);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn live_find<T, F>(&self, predicate: F) -> Result<LiveCapabilitySubscription<T>>
    where
        T: DeserializeOwned + Send + Sync + 'static,
        F: Fn(&T) -> bool + Send + Sync + 'static,
    {
        let rust_type = type_name::<T>().to_string();
        let capability_name = self.name.clone();

        let initial_results = {
            let mut inner = self.db.state();
            *inner
                .query_count_by_type
                .entry(rust_type.clone())
                .or_default() += 1;
            if inner.query_count_by_type[&rust_type] >= 3 {
                inner.adaptive_indexes.insert(rust_type.clone());
            }

            let mut out = Vec::new();
            if let Some(bucket) = inner.rows.get(&capability_name) {
                for row in bucket.values() {
                    if row.rust_type != rust_type {
                        continue;
                    }
                    let parsed = serde_json::from_value::<T>(row.value.clone())?;
                    if predicate(&parsed) {
                        out.push(parsed);
                    }
                }
            }
            out
        };

        let rx = self.db.event_tx.subscribe();
        let subscription = LiveCapabilitySubscription {
            initial_results,
            rx,
            db: self.db.clone(),
            capability_name,
            rust_type,
            predicate: Arc::new(Mutex::new(None)),
        };

        Ok(subscription)
    }
}

// Type alias for boxed predicates
type PredicateBox<T> = Arc<Mutex<Option<Box<dyn Fn(&T) -> bool + Send + Sync>>>>;

/// A subscription to a live query that streams updates whenever matching records change.
///
/// This struct holds the initial query results and provides a stream interface to receive
/// updates whenever records matching the query predicate are inserted or updated.
pub struct LiveQuerySubscription<T> {
    /// The initial set of records matching the query predicate at subscription time.
    pub initial_results: Vec<T>,
    rx: broadcast::Receiver<ChangeEvent>,
    db: FeltDb,
    rust_type: String,
    predicate: PredicateBox<T>,
}

impl<T> LiveQuerySubscription<T>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    pub fn set_predicate<F>(&mut self, predicate: F)
    where
        F: Fn(&T) -> bool + Send + Sync + 'static,
    {
        *self.predicate.lock().expect("lock poisoned") = Some(Box::new(predicate));
    }

    /// Waits for the next change event and returns the updated query results.
    ///
    /// This method is async and will block until a change event is received for the
    /// subscribed type. When an event arrives, it re-evaluates the full dataset and
    /// returns all matching records.
    ///
    /// Returns `None` if the broadcast channel has been dropped (all senders are gone).
    pub async fn next(&mut self) -> Option<Vec<T>> {
        loop {
            match self.rx.recv().await {
                Ok(event) => {
                    if event.rust_type != self.rust_type {
                        continue;
                    }

                    let results = {
                        let inner = self.db.state();
                        let mut out = Vec::new();
                        for bucket in inner.rows.values() {
                            for row in bucket.values() {
                                if row.rust_type != self.rust_type {
                                    continue;
                                }
                                if let Ok(parsed) = serde_json::from_value::<T>(row.value.clone()) {
                                    out.push(parsed);
                                }
                            }
                        }
                        out
                    };

                    return Some(results);
                }
                Err(_) => {
                    return None;
                }
            }
        }
    }

    /// Returns a reference to the initial query results captured at subscription time.
    ///
    /// This provides access to the baseline set of matching records before any changes
    /// have been streamed through `next()`.
    pub fn initial_results(&self) -> &[T] {
        &self.initial_results
    }
}

/// A capability-scoped subscription to a live query that streams updates for records within a specific capability.
///
/// Similar to `LiveQuerySubscription`, but only receives updates for records within the specified capability.
/// This provides more granular control over subscription scope.
pub struct LiveCapabilitySubscription<T> {
    pub initial_results: Vec<T>,
    rx: broadcast::Receiver<ChangeEvent>,
    db: FeltDb,
    capability_name: String,
    rust_type: String,
    predicate: PredicateBox<T>,
}

impl<T> LiveCapabilitySubscription<T>
where
    T: DeserializeOwned + Send + Sync + 'static,
{
    pub fn set_predicate<F>(&mut self, predicate: F)
    where
        F: Fn(&T) -> bool + Send + Sync + 'static,
    {
        *self.predicate.lock().expect("lock poisoned") = Some(Box::new(predicate));
    }

    /// Waits for the next change event within this capability and returns the updated query results.
    ///
    /// Similar to `LiveQuerySubscription::next()`, but only responds to changes within the
    /// scoped capability.
    pub async fn next(&mut self) -> Option<Vec<T>> {
        loop {
            match self.rx.recv().await {
                Ok(event) => {
                    if event.rust_type != self.rust_type || event.capability != self.capability_name
                    {
                        continue;
                    }

                    let results = {
                        let inner = self.db.state();
                        let mut out = Vec::new();
                        if let Some(bucket) = inner.rows.get(&self.capability_name) {
                            for row in bucket.values() {
                                if row.rust_type != self.rust_type {
                                    continue;
                                }
                                if let Ok(parsed) = serde_json::from_value::<T>(row.value.clone()) {
                                    out.push(parsed);
                                }
                            }
                        }
                        out
                    };

                    return Some(results);
                }
                Err(_) => {
                    return None;
                }
            }
        }
    }

    pub fn initial_results(&self) -> &[T] {
        &self.initial_results
    }
}

/// Read the log, separating a torn tail from real corruption.
///
/// A process that dies while appending leaves a final record that was never
/// terminated. That record was never committed, so recovery discards it and
/// truncates it away: leaving the bytes in place would make the next append
/// splice onto them and turn a normal crash into permanent corruption.
///
/// Damage anywhere earlier is not an interrupted write. Recovery refuses it
/// rather than silently dropping committed history.
/// Examine a durable database's format **without opening or modifying it**.
///
/// This runs before any record is interpreted, and reads the file only. A
/// database this reports as incompatible is left exactly as it was found.
///
/// The order matters: an explicit format record decides, and the records
/// themselves are examined *only* when no format record exists — the
/// unversioned era. Compatibility is never inferred from whether a
/// deserialization happened to succeed.
pub fn inspect_durable_format(path: &Path) -> Result<FormatCompatibility> {
    if !path.exists() {
        // A database that does not exist yet will be created in the current
        // format.
        return Ok(FormatCompatibility::Compatible(DurableFormat::Versioned(
            DURABLE_FORMAT_VERSION,
        )));
    }

    // Read-only: inspecting a database it is about to reject must not touch it.
    let scan = scan_log(path)?;
    let lines: Vec<String> = scan.lines.into_iter().map(|line| line.text).collect();
    let mut declared: Option<u32> = None;
    let mut legacy_revision = false;

    for line in &lines {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("record_type").and_then(Value::as_str) {
            Some(FORMAT_RECORD_TYPE) => {
                if let Some(version) = value.get("format_version").and_then(Value::as_u64) {
                    declared = Some(version as u32);
                }
            }
            Some("feltdb.snapshot.v1" | "flowdb.snapshot.v1") => {
                match value.get("format_version").and_then(Value::as_u64) {
                    Some(version) if version > 0 => declared = Some(version as u32),
                    _ => {}
                }
            }
            _ => {
                // The unversioned-era probe: a revision record that predates
                // resource-scoped identity. Deliberate and specific — it looks
                // for a named absence rather than trying to parse and shrugging
                // at failure.
                let is_revision = value
                    .get("key")
                    .and_then(Value::as_str)
                    .is_some_and(|key| key.starts_with("state:revision:"));
                if is_revision
                    && value
                        .get("value")
                        .and_then(Value::as_object)
                        .is_some_and(|revision| !revision.contains_key("resource"))
                {
                    legacy_revision = true;
                }
            }
        }
    }

    if let Some(version) = declared {
        return Ok(match version.cmp(&DURABLE_FORMAT_VERSION) {
            std::cmp::Ordering::Equal => {
                FormatCompatibility::Compatible(DurableFormat::Versioned(version))
            }
            std::cmp::Ordering::Greater => {
                FormatCompatibility::Incompatible(FormatIncompatibility {
                    found: DurableFormat::Versioned(version),
                    required: DURABLE_FORMAT_VERSION,
                    reason: "the database was written by a newer FeltDB".to_string(),
                    action: "Run a FeltDB build that understands this format, or restore \
                             a backup taken in an older format."
                        .to_string(),
                })
            }
            std::cmp::Ordering::Less => FormatCompatibility::Incompatible(FormatIncompatibility {
                found: DurableFormat::Versioned(version),
                required: DURABLE_FORMAT_VERSION,
                reason: "the database was written in an older durable format".to_string(),
                action: "Migrate the database with a FeltDB build that supports both \
                             formats, or restore a backup."
                    .to_string(),
            }),
        });
    }

    if legacy_revision {
        return Ok(FormatCompatibility::Incompatible(FormatIncompatibility {
            found: DurableFormat::Unversioned,
            required: DURABLE_FORMAT_VERSION,
            reason: "it holds revision records written before revisions belonged to a \
                     resource, and those records cannot be migrated without inventing \
                     information they never contained: a revision recorded no resource, \
                     so which history each one belongs to is not recoverable from the data"
                .to_string(),
            action: "Export the application state you need from a FeltDB build that \
                     understands the old format, then load it into a new database. The \
                     existing file has not been modified."
                .to_string(),
        }));
    }

    // No format record and no legacy revision: nothing in this database depends
    // on the difference.
    Ok(FormatCompatibility::Compatible(DurableFormat::Unversioned))
}

/// One complete record in the durable log, with where it came from.
struct ScannedLine {
    text: String,
    line_number: usize,
    byte_offset: u64,
}

/// The bytes at the end of a log that are not a complete record.
struct TornTail {
    byte_offset: u64,
    bytes: usize,
}

/// The durable log, read and classified. **This never writes.**
struct LogScan {
    lines: Vec<ScannedLine>,
    torn_tail: Option<TornTail>,
}

/// Read the durable log without modifying it.
///
/// The only thing separated out here is the **terminator**: a final record with
/// no newline cannot be one this database finished writing, so it is reported
/// as a torn tail. Everything else — including a complete final record that
/// happens to be invalid — is returned as a line, to be validated by the
/// caller and refused rather than quietly dropped for being near the end.
///
/// Being read-only is load-bearing twice over: a refused open must leave the
/// database byte-identical, and format inspection must be able to run on a
/// database it is about to reject.
fn scan_log(path: &Path) -> Result<LogScan> {
    let mut file = BufReader::new(OpenOptions::new().read(true).open(path)?);
    let mut lines = Vec::new();
    let mut torn_tail = None;
    let mut buffer = Vec::new();
    let mut offset = 0u64;
    let mut line_number = 0usize;

    loop {
        buffer.clear();
        let read = file.read_until(b'\n', &mut buffer)?;
        if read == 0 {
            break;
        }
        let terminated = buffer.ends_with(b"\n");
        let text = String::from_utf8_lossy(&buffer)
            .trim_end_matches('\n')
            .to_string();
        line_number += 1;

        if !terminated {
            // An unterminated final record: the write did not finish. Empty
            // trailing bytes are nothing at all and are simply ignored.
            if !text.trim().is_empty() {
                torn_tail = Some(TornTail {
                    byte_offset: offset,
                    bytes: read,
                });
            }
            break;
        }

        lines.push(ScannedLine {
            text,
            line_number,
            byte_offset: offset,
        });
        offset += read as u64;
    }

    Ok(LogScan { lines, torn_tail })
}

/// Discard an incomplete final append, so the next append does not splice onto
/// a partial record.
///
/// Called only after the whole log has replayed successfully. A log that is
/// going to be refused is never truncated.
fn truncate_torn_tail(path: &Path, tail: &TornTail) -> Result<()> {
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len(tail.byte_offset)?;
    file.sync_all()?;
    Ok(())
}

fn corruption(line: &ScannedLine, reason: &str) -> FlowError {
    let excerpt: String = line.text.chars().take(120).collect();
    FlowError::CorruptLogLine(Box::new(LogCorruption {
        line_number: line.line_number,
        byte_offset: line.byte_offset,
        reason: reason.to_string(),
        excerpt,
    }))
}

fn replay_log(inner: &mut Inner) -> Result<()> {
    let scan = scan_log(&inner.path)?;
    let torn_tail = scan.torn_tail;
    let lines = scan.lines;

    let mut snapshot_log = false;
    let mut recoverable_revisions: Vec<(operation::RevisionProvenance, Value)> = Vec::new();
    for line in &lines {
        if line.text.trim().is_empty() {
            continue;
        }
        let value: Value =
            serde_json::from_str(&line.text).map_err(|_| corruption(line, "not valid JSON"))?;
        if matches!(
            value.get("record_type").and_then(Value::as_str),
            Some("feltdb.snapshot.v1" | "flowdb.snapshot.v1")
        ) {
            let header: LogHeader = serde_json::from_value(value)
                .map_err(|error| corruption(line, &format!("malformed log header: {error}")))?;
            inner.sequence = inner.sequence.max(header.local_sequence);
            inner.change_log.observed_versions = header.observed_versions;
            for attribution in header.transaction_history {
                let attribution = normalize_transaction_attribution(attribution);
                inner.authority_revision =
                    inner.authority_revision.max(attribution.commit_revision);
                inner.transaction_revisions.insert(
                    attribution.transaction_id.clone(),
                    (attribution.base_revision, attribution.commit_revision),
                );
                inner
                    .applied_transactions
                    .insert(attribution.transaction_id.clone());
                remember_attribution(inner, attribution);
            }
            inner
                .transaction_payload_hashes
                .extend(header.transaction_payload_hashes);
            inner.bootstrapped = true;
            snapshot_log = true;
            continue;
        }
        if value.get("record_type").and_then(Value::as_str) == Some(FORMAT_RECORD_TYPE) {
            // Already honoured by `inspect_durable_format` before the open.
            continue;
        }
        if value.get("record_type").and_then(Value::as_str) == Some("feltdb.transaction.v1") {
            let transaction: TransactionLogRecord =
                serde_json::from_value(value).map_err(|error| {
                    corruption(line, &format!("malformed transaction record: {error}"))
                })?;
            // Older records did not carry a commit revision. Their durable
            // order in the log supplies an unambiguous upgrade path.
            inner.authority_revision =
                inner
                    .authority_revision
                    .max(if transaction.commit_revision == 0 {
                        inner.authority_revision + 1
                    } else {
                        transaction.commit_revision
                    });
            let base_revision = if transaction.commit_revision == 0 {
                inner.authority_revision.saturating_sub(1)
            } else {
                transaction.base_revision
            };
            inner.transaction_revisions.insert(
                transaction.transaction_id.clone(),
                (base_revision, inner.authority_revision),
            );
            if let Some(hash) = transaction.payload_hash.clone() {
                inner
                    .transaction_payload_hashes
                    .insert(transaction.transaction_id.clone(), hash);
            }
            // Attribution is restored on replay for the same reason it is kept
            // on commit: an operator who restarts a server must not lose the
            // answer to "who did this" for work the server already holds.
            let attribution = TransactionAttribution::from_record(
                &transaction.transaction_id,
                transaction.audit.as_ref(),
                base_revision,
                inner.authority_revision,
                transaction.state_before,
                transaction.state_after,
                &transaction.rows,
                transaction
                    .rows
                    .first()
                    .map(|row| row.unix_ms)
                    .unwrap_or_default(),
            );
            remember_attribution(inner, attribution);
            if inner
                .applied_transactions
                .insert(transaction.transaction_id)
            {
                for row in transaction.rows {
                    if let Some(operation) = row.operation.clone() {
                        inner.sequence = inner.sequence.max(operation.sequence);
                        if let Some(clock) = &operation.vector_clock {
                            inner.sync_state.merge_vector_clock(clock);
                        }
                        inner.change_log.add_operation(operation);
                    }
                    if row.deleted {
                        inner.remove_row(&row.capability, &row.key);
                    } else {
                        inner.put_row(row);
                    }
                }
            }
            inner.sequence = inner.sequence.max(transaction.state_after);
            continue;
        }
        let row: StoredRow = serde_json::from_value(value)
            .map_err(|_| corruption(line, "not a durable record this build understands"))?;
        let derived_revision = row.capability == "state" && row.operation.is_none();
        if let Some(operation) = row.operation.clone() {
            // A revision is a deterministic function of the operation that
            // produced it, so it never has to be lost. If the crash landed
            // between a record and the revision recording it, the revision is
            // rebuilt below rather than left missing.
            if let (Some(provenance), Some(value)) = (&operation.revision, &operation.value) {
                recoverable_revisions.push((provenance.clone(), value.clone()));
            }
            inner.sequence = inner.sequence.max(
                (operation.instance_id == inner.instance_id)
                    .then_some(operation.sequence)
                    .unwrap_or(0),
            );
            if let Some(clock) = &operation.vector_clock {
                inner.sync_state.merge_vector_clock(clock);
            }
            inner.change_log.add_operation(operation);
        } else if !snapshot_log && !derived_revision {
            // Pre-operation-envelope logs were produced only by this local
            // store. Preserve a monotonic sequence when upgrading them.
            inner.sequence += 1;
        }
        if !derived_revision {
            let base_revision = inner.authority_revision;
            inner.authority_revision += 1;
            if !snapshot_log {
                remember_operation_attribution(inner, &row, base_revision, base_revision + 1);
            }
        }
        if let Some(id) = row
            .key
            .rsplit_once(':')
            .and_then(|(_, right)| right.parse::<u64>().ok())
        {
            let counter = inner
                .key_counters
                .entry(row.capability.clone())
                .or_default();
            *counter = (*counter).max(id + 1);
        }
        if row.deleted {
            inner.remove_row(&row.capability, &row.key);
        } else {
            inner.put_row(row);
        }
    }

    // Rebuild any revision whose record survived but whose own row did not.
    //
    // This is the crash window between appending a record and appending the
    // revision that records it. The revision is derivable from the operation —
    // resource, content, parent, sequence and authority all travel with it —
    // so recovery reconstructs it instead of leaving an authority holding less
    // history than its own operation stream implies. Nothing is invented: the
    // identity is recomputed from its parts and discarded if it does not follow
    // from them.
    //
    // The rebuilt rows are not appended here. They are derived, and stay
    // derivable from the same operations on every subsequent open.
    for (provenance, value) in recoverable_revisions {
        let key = format!("state:revision:{}", provenance.id);
        let present = inner
            .rows
            .get("state")
            .and_then(|bucket| bucket.get(&key))
            .is_some_and(|row| !row.deleted);
        if present {
            continue;
        }

        // A revision below the resource's retention horizon is absent because
        // it was expired on purpose. Rebuilding it would make recovery
        // resurrect history a policy deliberately discarded, which is a
        // different kind of wrong from losing it.
        let horizon = inner
            .rows
            .get("_retention")
            .and_then(|bucket| bucket.get(&format!("_retention:{}", provenance.resource)))
            .and_then(|row| serde_json::from_value::<RetentionRecord>(row.value.clone()).ok())
            .map(|state| state.horizon);
        if horizon.is_some_and(|horizon| provenance.sequence < horizon) {
            continue;
        }
        let Ok(revision) = reconstruct_revision(&provenance, &value) else {
            continue;
        };
        let Ok(encoded) = serde_json::to_value(&revision) else {
            continue;
        };
        inner.put_row(StoredRow {
            capability: "state".to_string(),
            key,
            rust_type: "feltdb::state_model::StateRevision".to_string(),
            value: encoded,
            unix_ms: now_ms(),
            content_hash: None,
            flow_ref: None,
            deleted: false,
            operation: None,
        });
    }

    // Compute collection cardinality from final row state (count non-deleted rows per capability)
    for (capability, bucket) in &inner.rows {
        let count = bucket.values().filter(|row| !row.deleted).count() as u64;
        inner
            .collection_cardinality
            .insert(capability.clone(), count);
    }

    // Only now, with every complete record replayed, is it safe to discard an
    // incomplete final append. A log that was going to be refused never reaches
    // this point, so a refused open leaves the file byte-identical.
    if let Some(tail) = torn_tail {
        truncate_torn_tail(&inner.path, &tail)?;
        inner.log_recovery = LogRecovery::RecoveredTornTail {
            byte_offset: tail.byte_offset,
            discarded_bytes: tail.bytes,
        };
    }

    Ok(())
}

fn write_json_line<T: Serialize>(file: &mut std::fs::File, value: &T) -> Result<()> {
    serde_json::to_writer(&mut *file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn stored_row_for_operation(operation: Operation) -> StoredRow {
    StoredRow {
        capability: operation.capability.clone(),
        key: operation.key.clone(),
        rust_type: operation.rust_type.clone(),
        value: operation.value.clone().unwrap_or(Value::Null),
        unix_ms: operation.timestamp_ms,
        content_hash: Some(operation.content_hash.clone()),
        flow_ref: None,
        deleted: operation.op_type == OperationType::Delete,
        operation: Some(operation),
    }
}

fn sync_metadata_path(path: &Path) -> PathBuf {
    path.with_extension("sync.json")
}

fn load_sync_metadata(inner: &mut Inner) -> Result<()> {
    let path = sync_metadata_path(&inner.path);
    if !path.exists() {
        return Ok(());
    }
    let metadata: SyncMetadata = serde_json::from_slice(&fs::read(path)?)?;
    inner.sequence = inner.sequence.max(metadata.local_sequence);
    for (origin, sequence) in metadata.observed_versions {
        inner
            .change_log
            .observed_versions
            .entry(origin)
            .and_modify(|current| *current = (*current).max(sequence))
            .or_insert(sequence);
    }
    inner.change_log.peer_acknowledgements = metadata.peer_acknowledgements;
    Ok(())
}

fn persist_sync_metadata(inner: &Inner) -> Result<()> {
    let path = sync_metadata_path(&inner.path);
    let temporary = path.with_extension("sync.tmp");
    let metadata = SyncMetadata {
        local_sequence: inner.sequence,
        observed_versions: inner.change_log.versions(),
        peer_acknowledgements: inner.change_log.peer_acknowledgements.clone(),
    };
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec_pretty(&metadata)?)?;
    file.flush()?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn append_event(path: &Path, event: &StoredRow) -> Result<()> {
    append_event_with(path, event, DurabilityMode::Flushed, &mut 0)
}

/// Append one record under the database's configured durability mode.
fn append_event_locked(inner: &mut Inner, event: &StoredRow) -> Result<()> {
    let path = inner.path.clone();
    let mode = inner.durability_mode;
    append_event_with(&path, event, mode, &mut inner.writes_since_barrier)
}

/// Append one record, pushing it as far as the durability mode requires.
///
/// `since_barrier` counts writes since the last stable-storage barrier and is
/// only read by [`DurabilityMode::Grouped`].
fn append_event_with(
    path: &Path,
    event: &StoredRow,
    mode: DurabilityMode,
    since_barrier: &mut u32,
) -> Result<()> {
    let _span = workload_diagnostics::span(workload_diagnostics::Phase::Persistence);
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    file.write_all(line.as_bytes())?;
    file.flush()?;

    // The flush above is what makes a record survive the process dying. What
    // follows is the only thing that even asks the storage stack for more.
    match mode {
        DurabilityMode::Flushed => {}
        DurabilityMode::Synced => file.sync_all()?,
        DurabilityMode::Grouped { every } => {
            *since_barrier += 1;
            if *since_barrier >= every.max(1) {
                file.sync_all()?;
                *since_barrier = 0;
            }
        }
    }
    Ok(())
}

fn append_transaction(path: &Path, transaction: &TransactionLogRecord) -> Result<()> {
    // A transaction commit fsyncs. This span therefore includes the fsync, which
    // is the point: it is the durability cost of a commit, measured rather than
    // assumed.
    let _span = workload_diagnostics::span(workload_diagnostics::Phase::Persistence);
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    write_json_line(&mut file, transaction)?;
    file.sync_data()?;
    Ok(())
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time moved backwards")
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
    struct User {
        email: String,
        age: u8,
        active: bool,
    }

    fn temp_file() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "feltdb-test-{}-{nanos}-{sequence}.data",
            std::process::id()
        ))
    }

    /// A commit is attributable: the audit document a transaction already
    /// writes is retained, so a human can ask who made a change.
    #[test]
    fn a_committed_transaction_records_who_committed_it() {
        let path = temp_file();
        let db = open(&path).expect("open");
        db.apply_atomic_transaction(
            "tx_agent_1",
            None,
            &[],
            &[AtomicMutation {
                capability: "billing:Customer".into(),
                key: "cus_1".into(),
                value: Some(serde_json::json!({"name": "Ada", "status": "active"})),
            }],
            Some(serde_json::json!({
                "subject": "agent_data_cleanup",
                "tenant": "acme",
                "application": "app_billing",
                "revision": "rev_7",
            })),
        )
        .expect("commit");

        let recent = db.recent_transactions(10).expect("attributions");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].transaction_id, "tx_agent_1");
        assert_eq!(recent[0].subject.as_deref(), Some("agent_data_cleanup"));
        assert_eq!(recent[0].application_id.as_deref(), Some("app_billing"));
        assert_eq!(recent[0].operations, 1);
        assert_eq!(recent[0].keys, vec!["billing:Customer:cus_1".to_string()]);
        // Attribution says who and what, never what was written.
        assert!(!serde_json::to_string(&recent[0]).unwrap().contains("Ada"));

        let touching = db
            .transactions_touching("billing:Customer", "cus_1", 10)
            .expect("touching");
        assert_eq!(touching.len(), 1);
        assert_eq!(touching[0].transaction_id, "tx_agent_1");
        assert!(db
            .transactions_touching("billing:Customer", "cus_2", 10)
            .expect("touching")
            .is_empty());

        assert_eq!(
            db.transaction_attribution("tx_agent_1")
                .expect("lookup")
                .map(|value| value.subject)
                .flatten()
                .as_deref(),
            Some("agent_data_cleanup")
        );

        // Attribution survives a restart, because it is rebuilt from the same
        // durable records the rest of recovery replays.
        drop(db);
        let reopened = open(&path).expect("reopen");
        let after = reopened.recent_transactions(10).expect("attributions");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].subject.as_deref(), Some("agent_data_cleanup"));
        assert_eq!(after[0].keys, vec!["billing:Customer:cus_1".to_string()]);
        drop(reopened);
        let _ = fs::remove_file(path);
    }

    /// The window is bounded, and reports that it is.
    #[test]
    fn transaction_attribution_is_bounded_and_newest_first() {
        let path = temp_file();
        let db = open(&path).expect("open");
        let commits = TRANSACTION_ATTRIBUTION_WINDOW + 5;
        for index in 0..commits {
            db.apply_atomic_transaction(
                &format!("tx_{index}"),
                None,
                &[],
                &[AtomicMutation {
                    capability: "app:Note".into(),
                    key: format!("note_{index}"),
                    value: Some(serde_json::json!({"index": index})),
                }],
                Some(serde_json::json!({"subject": format!("actor_{index}")})),
            )
            .expect("commit");
        }
        let (held, capacity) = db.transaction_attribution_window().expect("window");
        assert_eq!(capacity, TRANSACTION_ATTRIBUTION_WINDOW);
        assert_eq!(held, TRANSACTION_ATTRIBUTION_WINDOW);
        let recent = db.recent_transactions(3).expect("attributions");
        assert_eq!(
            recent
                .iter()
                .map(|value| value.transaction_id.clone())
                .collect::<Vec<_>>(),
            vec![
                format!("tx_{}", commits - 1),
                format!("tx_{}", commits - 2),
                format!("tx_{}", commits - 3),
            ]
        );
        // The oldest commit rolled out of the recent window but remains
        // addressable through canonical history.
        assert!(db
            .transaction_attribution("tx_0")
            .expect("lookup")
            .is_some());
        drop(db);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn canonical_transaction_history_survives_window_restart_and_compaction() {
        let path = temp_file();
        let db = open(&path).expect("open");
        let commits = TRANSACTION_ATTRIBUTION_WINDOW + 1;
        for index in 0..commits {
            db.apply_atomic_transaction(
                &format!("history_tx_{index}"),
                None,
                &[],
                &[AtomicMutation {
                    capability: "history:Note".into(),
                    key: format!("note_{index}"),
                    value: Some(serde_json::json!({"index": index})),
                }],
                Some(serde_json::json!({"subject": "agent", "work_id": "work-1"})),
            )
            .expect("commit");
        }
        let complete = db.committed_transactions_after(0, usize::MAX).unwrap();
        assert_eq!(complete.len(), commits);
        assert_eq!(complete[0].transaction_id, "history_tx_0");
        assert_eq!(complete[0].operation_ids.len(), 1);
        assert_ne!(complete[0].operation_ids[0], complete[0].transaction_id);
        assert_eq!(complete[0].work_id.as_deref(), Some("work-1"));
        assert_eq!(
            db.transactions_touching("history:Note", "note_0", 1)
                .unwrap()[0]
                .transaction_id,
            "history_tx_0"
        );
        assert_eq!(
            db.transaction_attribution("history_tx_0")
                .unwrap()
                .unwrap()
                .audit
                .unwrap()["work_id"],
            "work-1"
        );
        let versions = db.operation_versions().unwrap();
        db.acknowledge_peer_versions("history-peer".into(), versions)
            .unwrap();
        db.compact_operation_log(&["history-peer".into()]).unwrap();
        drop(db);

        let reopened = open(&path).expect("reopen");
        assert_eq!(
            reopened
                .committed_transactions_after(0, usize::MAX)
                .unwrap()
                .len(),
            commits
        );
        assert!(reopened
            .transaction_attribution("history_tx_0")
            .unwrap()
            .is_some());
        assert_eq!(
            reopened
                .transactions_touching("history:Note", "note_0", 1)
                .unwrap()[0]
                .transaction_id,
            "history_tx_0"
        );
        assert_eq!(
            reopened
                .transaction_attribution("history_tx_0")
                .unwrap()
                .unwrap()
                .operation_ids,
            complete[0].operation_ids
        );
        let _ = fs::remove_file(sync_metadata_path(&path));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn transaction_payload_binding_survives_restart_and_compaction() {
        let path = temp_file();
        let db = open(&path).expect("open");
        let mutation = AtomicMutation {
            capability: "managed:State".into(),
            key: "record-1".into(),
            value: Some(serde_json::json!({"value": 1})),
        };
        let first = db
            .apply_atomic_transaction_content_addressed(
                "managed-idempotency-1",
                Some("sha256:request-a"),
                None,
                &[],
                &[mutation.clone()],
                None,
            )
            .expect("initial commit");
        let duplicate = db
            .apply_atomic_transaction_content_addressed(
                "managed-idempotency-1",
                Some("sha256:request-a"),
                None,
                &[],
                &[mutation.clone()],
                None,
            )
            .expect("same semantic request");
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.base_revision, first.base_revision);
        assert_eq!(duplicate.commit_revision, first.commit_revision);
        assert_eq!(duplicate.state_before, first.state_before);
        assert_eq!(duplicate.state_after, first.state_after);

        let versions = db.operation_versions().unwrap();
        db.acknowledge_peer_versions("managed-peer".into(), versions)
            .unwrap();
        db.compact_operation_log(&["managed-peer".into()]).unwrap();
        drop(db);

        let reopened = open(&path).expect("restart");
        let replay = reopened
            .apply_atomic_transaction_content_addressed(
                "managed-idempotency-1",
                Some("sha256:request-a"),
                None,
                &[],
                &[mutation.clone()],
                None,
            )
            .expect("retry after restart");
        assert!(replay.duplicate);
        assert_eq!(replay.commit_revision, first.commit_revision);
        let conflict = reopened
            .apply_atomic_transaction_content_addressed(
                "managed-idempotency-1",
                Some("sha256:request-b"),
                None,
                &[],
                &[mutation],
                None,
            )
            .unwrap_err();
        assert!(conflict
            .to_string()
            .contains("TRANSACTION_ID_PAYLOAD_MISMATCH:"));
        let _ = fs::remove_file(sync_metadata_path(&path));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn single_record_commits_are_canonical_history_positions() {
        let path = temp_file();
        let db = open(&path).unwrap();
        db.insert("orders:one", serde_json::json!({"status":"paid"}))
            .unwrap();
        let history = db.committed_transactions_after(0, 10).unwrap();
        assert_eq!(history.len(), 1);
        assert!(history[0].transaction_id.starts_with("transaction:"));
        assert!(history[0].operation_ids[0].starts_with("operation:"));
        assert_ne!(history[0].transaction_id, history[0].operation_ids[0]);
        assert_eq!(history[0].keys, vec!["orders:orders:one"]);
        drop(db);
        let reopened = open(&path).unwrap();
        assert_eq!(
            reopened.committed_transactions_after(0, 10).unwrap()[0].transaction_id,
            history[0].transaction_id
        );
        assert_eq!(
            reopened.committed_transactions_after(0, 10).unwrap()[0].operation_ids,
            history[0].operation_ids
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn compacted_legacy_attribution_gets_stable_distinct_operation_identity() {
        let legacy = TransactionAttribution {
            transaction_id: "legacy-transaction".into(),
            operation_ids: Vec::new(),
            subject: Some("agent-1".into()),
            tenant_id: Some("tenant-1".into()),
            application_id: Some("app-1".into()),
            application_revision: None,
            base_revision: 4,
            commit_revision: 5,
            state_before: 4,
            state_after: 5,
            keys: vec!["orders:one".into()],
            operations: 2,
            principal_id: None,
            agent_id: Some("agent-1".into()),
            session_id: None,
            delegator_id: None,
            work_id: None,
            unix_ms: 1,
            audit: None,
        };
        let first = normalize_transaction_attribution(legacy.clone());
        let second = normalize_transaction_attribution(legacy);
        assert_eq!(first, second);
        assert_eq!(first.transaction_id, "legacy-transaction");
        assert_eq!(first.operation_ids.len(), 2);
        assert!(first
            .operation_ids
            .iter()
            .all(|operation| operation.starts_with("operation:legacy-transaction:")));

        let single = TransactionAttribution {
            transaction_id: "operation:authority:9".into(),
            operation_ids: Vec::new(),
            operations: 1,
            ..first
        };
        let upgraded = normalize_transaction_attribution(single);
        assert_eq!(upgraded.transaction_id, "transaction:authority:9");
        assert_eq!(upgraded.operation_ids, vec!["operation:authority:9"]);
    }

    /// A bulk transaction names a bounded sample of keys and an exact count.
    #[test]
    fn a_bulk_transaction_truncates_its_key_list_and_keeps_the_count() {
        let path = temp_file();
        let db = open(&path).expect("open");
        let mutations = (0..TRANSACTION_ATTRIBUTION_KEYS + 20)
            .map(|index| AtomicMutation {
                capability: "app:Customer".into(),
                key: format!("cus_{index}"),
                value: Some(serde_json::json!({"index": index})),
            })
            .collect::<Vec<_>>();
        db.apply_atomic_transaction(
            "tx_bulk",
            None,
            &[],
            &mutations,
            Some(serde_json::json!({"subject": "migration_agent"})),
        )
        .expect("commit");
        let attribution = db
            .transaction_attribution("tx_bulk")
            .expect("lookup")
            .expect("held");
        assert_eq!(attribution.operations, mutations.len());
        assert_eq!(attribution.keys.len(), TRANSACTION_ATTRIBUTION_KEYS);
        drop(db);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn open_is_zero_init_and_creates_store() {
        let path = temp_file();
        let db = open(&path).expect("open");
        assert!(path.exists());
        drop(db);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn insert_get_and_reopen_are_durable() {
        let path = temp_file();
        let db = open(&path).expect("open");
        let user = User {
            email: "a@feltdb.dev".to_string(),
            age: 33,
            active: true,
        };
        db.insert("user:1", &user).expect("insert");
        let loaded: User = db.get("user:1").expect("get").expect("some");
        assert_eq!(loaded, user);
        drop(db);

        let reopened = open(&path).expect("reopen");
        let loaded_after_restart: User = reopened.get("user:1").expect("get").expect("some");
        assert_eq!(loaded_after_restart, user);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn delete_and_reopen_preserves_tombstone() {
        let path = temp_file();
        let db = open(&path).expect("open");
        db.insert("users:deleted", serde_json::json!({"name": "gone"}))
            .expect("insert");
        db.delete("users:deleted").expect("delete");
        drop(db);

        let reopened = open(&path).expect("reopen");
        assert_eq!(reopened.get_value("users:deleted").expect("get"), None);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn closure_query_and_capability_find_work() {
        let path = temp_file();
        let db = open(&path).expect("open");
        let users = db.capability("users");
        users
            .insert(User {
                email: "one@feltdb.dev".to_string(),
                age: 28,
                active: true,
            })
            .expect("insert");
        users
            .insert(User {
                email: "two@feltdb.dev".to_string(),
                age: 41,
                active: false,
            })
            .expect("insert");

        let global = db.query::<User, _>(|u| u.age > 30).expect("query");
        assert_eq!(global.len(), 1);

        let active = users.find::<User, _>(|u| u.active).expect("find");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].email, "one@feltdb.dev");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn adaptive_index_marks_frequent_query_types() {
        let path = temp_file();
        let db = open(&path).expect("open");
        db.insert(
            "user:1",
            User {
                email: "index@feltdb.dev".to_string(),
                age: 21,
                active: true,
            },
        )
        .expect("insert");

        for _ in 0..3 {
            let _ = db.query::<User, _>(|u| u.active).expect("query");
        }
        assert!(db.is_auto_indexed::<User>());
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn live_query_returns_initial_results() {
        let path = temp_file();
        let db = open(&path).expect("open");
        let users = db.capability("users");
        users
            .insert(User {
                email: "live@feltdb.dev".to_string(),
                age: 28,
                active: true,
            })
            .expect("insert");
        users
            .insert(User {
                email: "inactive@feltdb.dev".to_string(),
                age: 35,
                active: false,
            })
            .expect("insert");

        let subscription = db
            .live_query::<User, _>(|u| u.active)
            .await
            .expect("live_query");
        let initial = subscription.initial_results();
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].email, "live@feltdb.dev");
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn live_query_streams_new_changes() {
        let path = temp_file();
        let db = open(&path).expect("open");
        let users = db.capability("users");
        users
            .insert(User {
                email: "initial@feltdb.dev".to_string(),
                age: 28,
                active: true,
            })
            .expect("insert");

        let mut subscription = db
            .live_query::<User, _>(|u| u.active)
            .await
            .expect("live_query");
        let initial = subscription.initial_results();
        assert_eq!(initial.len(), 1);

        let db_for_insert = db.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let users_inner = db_for_insert.capability("users");
            users_inner
                .insert(User {
                    email: "new@feltdb.dev".to_string(),
                    age: 25,
                    active: true,
                })
                .expect("insert");
        });

        let updated = subscription.next().await;
        assert!(updated.is_some());
        let results = updated.unwrap();
        assert_eq!(results.len(), 2);
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn live_query_filters_by_predicate() {
        let path = temp_file();
        let db = open(&path).expect("open");
        let users = db.capability("users");
        users
            .insert(User {
                email: "active@feltdb.dev".to_string(),
                age: 28,
                active: true,
            })
            .expect("insert");
        users
            .insert(User {
                email: "inactive@feltdb.dev".to_string(),
                age: 35,
                active: false,
            })
            .expect("insert");

        let subscription = db
            .live_query::<User, _>(|u| u.active)
            .await
            .expect("live_query");
        let initial = subscription.initial_results();
        assert_eq!(initial.len(), 1);
        assert!(initial[0].active);
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn change_events_are_emitted_on_insert() {
        let path = temp_file();
        let db = open(&path).expect("open");

        let mut subscription = db
            .live_query::<User, _>(|_| true)
            .await
            .expect("live_query");
        let initial = subscription.initial_results();
        assert_eq!(initial.len(), 0);

        let db_for_insert = db.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let users_inner = db_for_insert.capability("users");
            users_inner
                .insert(User {
                    email: "event@feltdb.dev".to_string(),
                    age: 30,
                    active: true,
                })
                .expect("insert");
        });

        let update = subscription.next().await;
        assert!(update.is_some());
        let users_result = update.unwrap();
        assert_eq!(users_result.len(), 1);
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn capability_live_find_returns_initial_results() {
        let path = temp_file();
        let db = open(&path).expect("open");
        let users = db.capability("users");
        users
            .insert(User {
                email: "cap_live@feltdb.dev".to_string(),
                age: 28,
                active: true,
            })
            .expect("insert");
        users
            .insert(User {
                email: "cap_inactive@feltdb.dev".to_string(),
                age: 35,
                active: false,
            })
            .expect("insert");

        let subscription = users
            .live_find::<User, _>(|u| u.active)
            .await
            .expect("live_find");
        let initial = subscription.initial_results();
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].email, "cap_live@feltdb.dev");
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn capability_live_find_streams_scoped_changes() {
        let path = temp_file();
        let db = open(&path).expect("open");
        let users = db.capability("users");
        let _orders = db.capability("orders");
        users
            .insert(User {
                email: "user_scoped@feltdb.dev".to_string(),
                age: 28,
                active: true,
            })
            .expect("insert");

        let mut subscription = users
            .live_find::<User, _>(|u| u.active)
            .await
            .expect("live_find");
        let initial = subscription.initial_results();
        assert_eq!(initial.len(), 1);

        let db_for_insert = db.clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let users_inner = db_for_insert.capability("users");
            users_inner
                .insert(User {
                    email: "new_user_scoped@feltdb.dev".to_string(),
                    age: 25,
                    active: true,
                })
                .expect("insert");
        });

        let updated = subscription.next().await;
        assert!(updated.is_some());
        let results = updated.unwrap();
        assert_eq!(results.len(), 2);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn vector_clocks_increment_on_mutations() {
        let path = temp_file();
        let db = open(&path).expect("open");

        // Initial vector clock should be empty or have default values
        let initial_clock = db.get_vector_clock().expect("get_vector_clock");
        assert!(initial_clock.is_none() || initial_clock.as_ref().unwrap().is_empty());

        // Insert first record
        db.insert(
            "users:1",
            User {
                email: "user1@feltdb.dev".to_string(),
                age: 25,
                active: true,
            },
        )
        .expect("insert");

        // Check pending operations have vector clock
        let pending = db
            .get_pending_for_peer("peer-1", 0)
            .expect("get_pending_for_peer");
        assert_eq!(pending.len(), 1);
        assert!(pending[0].vector_clock.is_some());

        // Insert second record
        db.insert(
            "users:2",
            User {
                email: "user2@feltdb.dev".to_string(),
                age: 30,
                active: false,
            },
        )
        .expect("insert");

        // Check we have 2 operations with increasing sequence numbers
        let pending = db
            .get_pending_for_peer("peer-1", 0)
            .expect("get_pending_for_peer");
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].sequence, 1);
        assert_eq!(pending[1].sequence, 2);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn update_creates_operation_with_vector_clock() {
        let path = temp_file();
        let db = open(&path).expect("open");

        // Insert initial record
        db.insert(
            "users:1",
            User {
                email: "user1@feltdb.dev".to_string(),
                age: 25,
                active: true,
            },
        )
        .expect("insert");

        // Update the record
        db.update(
            "users:1",
            User {
                email: "user1@feltdb.dev".to_string(),
                age: 26,
                active: true,
            },
        )
        .expect("update");

        // Check we have operations for both insert and update
        let pending = db
            .get_pending_for_peer("peer-1", 0)
            .expect("get_pending_for_peer");
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].op_type, OperationType::Insert);
        assert_eq!(pending[1].op_type, OperationType::Update);

        // Both should have vector clocks
        assert!(pending[0].vector_clock.is_some());
        assert!(pending[1].vector_clock.is_some());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn delete_creates_operation_with_vector_clock() {
        let path = temp_file();
        let db = open(&path).expect("open");

        // Insert record
        db.insert(
            "users:1",
            User {
                email: "user1@feltdb.dev".to_string(),
                age: 25,
                active: true,
            },
        )
        .expect("insert");

        // Delete the record
        db.delete("users:1").expect("delete");

        // Check we have operations for both insert and delete
        let pending = db
            .get_pending_for_peer("peer-1", 0)
            .expect("get_pending_for_peer");
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].op_type, OperationType::Insert);
        assert_eq!(pending[1].op_type, OperationType::Delete);

        // Both should have vector clocks
        assert!(pending[0].vector_clock.is_some());
        assert!(pending[1].vector_clock.is_some());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn apply_remote_operation_merges_vector_clocks() {
        let path = temp_file();
        let db = open(&path).expect("open");

        // Create a remote operation with a vector clock
        let remote_op = Operation::insert(
            1,
            "remote-instance".to_string(),
            1,
            "users:100".to_string(),
            serde_json::json!({"email": "remote@feltdb.dev", "age": 30, "active": true}),
            "User".to_string(),
            "users".to_string(),
        )
        .with_vector_clock({
            let mut vc = std::collections::HashMap::new();
            vc.insert("remote-instance".to_string(), 1);
            vc
        });

        // Apply the remote operation
        db.apply_remote_operation(remote_op)
            .expect("apply_remote_operation");

        // Check the local vector clock was merged
        let clock = db.get_vector_clock().expect("get_vector_clock");
        assert!(clock.is_some());
        let clock = clock.unwrap();
        assert_eq!(clock.get("remote-instance"), Some(&1));

        // Verify the data was applied
        let user: Option<User> = db.get("users:100").expect("get");
        assert!(user.is_some());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn operation_history_survives_restart_and_relays_transitively() {
        let source_path = temp_file();
        let relay_path = temp_file();
        let destination_path = temp_file();
        let source = open(&source_path).expect("open source");
        source
            .insert("docs:1", serde_json::json!({"title": "one"}))
            .unwrap();
        source
            .insert("docs:2", serde_json::json!({"title": "two"}))
            .unwrap();

        let relay = open(&relay_path).expect("open relay");
        for operation in source.operations_since(&HashMap::new()).unwrap() {
            relay.apply_remote_operation(operation).unwrap();
        }
        drop(relay);

        let relay = open(&relay_path).expect("reopen relay");
        let relayed = relay.operations_since(&HashMap::new()).unwrap();
        assert_eq!(relayed.len(), 2);
        let destination = open(&destination_path).expect("open destination");
        for operation in relayed {
            destination.apply_remote_operation(operation).unwrap();
        }
        assert_eq!(
            destination.get_value("docs:2").unwrap(),
            Some(serde_json::json!({"title": "two"}))
        );

        let _ = fs::remove_file(source_path);
        let _ = fs::remove_file(relay_path);
        let _ = fs::remove_file(destination_path);
    }

    #[test]
    fn remote_operation_sequence_gaps_are_rejected() {
        let path = temp_file();
        let db = open(&path).unwrap();
        let operation = Operation::insert(
            2,
            "remote".to_string(),
            2,
            "docs:2".to_string(),
            serde_json::json!({"title": "two"}),
            "Value".to_string(),
            "docs".to_string(),
        );
        assert!(db.apply_remote_operation(operation).is_err());
        assert!(db.get_value("docs:2").unwrap().is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn peer_version_acknowledgements_survive_restart() {
        let path = temp_file();
        let db = open(&path).unwrap();
        db.insert("docs:1", serde_json::json!({"title": "one"}))
            .unwrap();
        let versions = db.operation_versions().unwrap();
        db.acknowledge_peer_versions("peer-a".to_string(), versions.clone())
            .unwrap();
        drop(db);

        let reopened = open(&path).unwrap();
        assert_eq!(
            reopened.acknowledged_peer_versions("peer-a").unwrap(),
            versions
        );
        assert_eq!(
            reopened
                .acknowledged_operation_count(&["peer-a".to_string()])
                .unwrap(),
            1
        );
        let _ = fs::remove_file(sync_metadata_path(&path));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn compacted_history_bootstraps_a_new_peer_without_sequence_gaps() {
        let source_path = temp_file();
        let destination_path = temp_file();
        let source = open(&source_path).unwrap();
        source
            .insert("docs:1", serde_json::json!({"title": "one"}))
            .unwrap();
        source
            .insert("docs:2", serde_json::json!({"title": "two"}))
            .unwrap();
        let versions = source.operation_versions().unwrap();
        source
            .acknowledge_peer_versions("peer-a".to_string(), versions.clone())
            .unwrap();
        assert_eq!(
            source
                .compact_operation_log(&["peer-a".to_string()])
                .unwrap(),
            2
        );
        assert!(source.operations_since(&HashMap::new()).unwrap().is_empty());
        drop(source);

        let source = open(&source_path).unwrap();
        assert_eq!(source.operation_versions().unwrap(), versions);
        let snapshot = source.export_snapshot().unwrap();
        let destination = open(&destination_path).unwrap();
        destination.install_snapshot(snapshot).unwrap();
        assert_eq!(destination.operation_versions().unwrap(), versions);
        assert_eq!(
            destination.get_value("docs:2").unwrap(),
            Some(serde_json::json!({"title": "two"}))
        );

        source
            .insert("docs:3", serde_json::json!({"title": "three"}))
            .unwrap();
        let next = source.operations_since(&versions).unwrap();
        assert_eq!(next.len(), 1);
        destination.apply_remote_operation(next[0].clone()).unwrap();
        assert!(destination.get_value("docs:3").unwrap().is_some());

        let _ = fs::remove_file(sync_metadata_path(&source_path));
        let _ = fs::remove_file(sync_metadata_path(&destination_path));
        let _ = fs::remove_file(source_path);
        let _ = fs::remove_file(destination_path);
    }

    #[test]
    fn maintained_cardinality_unfiltered_count_basic() {
        let path = temp_file();
        let db = open(&path).unwrap();
        let capability = "test_collection";

        let mutations = vec![
            AtomicMutation {
                capability: capability.to_string(),
                key: "rec-1".to_string(),
                value: Some(serde_json::json!({"id": 1, "name": "Record 1"})),
            },
            AtomicMutation {
                capability: capability.to_string(),
                key: "rec-2".to_string(),
                value: Some(serde_json::json!({"id": 2, "name": "Record 2"})),
            },
        ];

        db.apply_atomic_transaction("txn-1", None, &[], &mutations, None)
            .unwrap();

        let cardinality = db.collection_cardinality(capability).unwrap();
        assert_eq!(cardinality, 2, "Cardinality should be 2 after 2 inserts");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn maintained_cardinality_insert_increments() {
        let path = temp_file();
        let db = open(&path).unwrap();
        let capability = "test_collection";

        for i in 1..=5 {
            let mutations = vec![AtomicMutation {
                capability: capability.to_string(),
                key: format!("rec-{}", i),
                value: Some(serde_json::json!({"id": i, "name": format!("Record {}", i)})),
            }];

            db.apply_atomic_transaction(&format!("txn-{}", i), None, &[], &mutations, None)
                .unwrap();
            let cardinality = db.collection_cardinality(capability).unwrap();
            assert_eq!(
                cardinality, i as u64,
                "Cardinality should be {} after {} inserts",
                i, i
            );
        }

        let _ = fs::remove_file(path);
    }

    #[test]
    fn maintained_cardinality_delete_decrements() {
        let path = temp_file();
        let db = open(&path).unwrap();
        let capability = "test_collection";

        let insert_mutations = vec![
            AtomicMutation {
                capability: capability.to_string(),
                key: "rec-1".to_string(),
                value: Some(serde_json::json!({"id": 1})),
            },
            AtomicMutation {
                capability: capability.to_string(),
                key: "rec-2".to_string(),
                value: Some(serde_json::json!({"id": 2})),
            },
            AtomicMutation {
                capability: capability.to_string(),
                key: "rec-3".to_string(),
                value: Some(serde_json::json!({"id": 3})),
            },
        ];

        db.apply_atomic_transaction("txn-insert", None, &[], &insert_mutations, None)
            .unwrap();
        assert_eq!(db.collection_cardinality(capability).unwrap(), 3);

        let delete_mutations = vec![AtomicMutation {
            capability: capability.to_string(),
            key: "rec-1".to_string(),
            value: None,
        }];

        db.apply_atomic_transaction("txn-delete", None, &[], &delete_mutations, None)
            .unwrap();
        assert_eq!(
            db.collection_cardinality(capability).unwrap(),
            2,
            "Cardinality should be 2 after deleting 1 record"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn maintained_cardinality_update_does_not_change_count() {
        let path = temp_file();
        let db = open(&path).unwrap();
        let capability = "test_collection";

        let insert_mutations = vec![AtomicMutation {
            capability: capability.to_string(),
            key: "rec-1".to_string(),
            value: Some(serde_json::json!({"id": 1, "status": "active"})),
        }];

        db.apply_atomic_transaction("txn-insert", None, &[], &insert_mutations, None)
            .unwrap();
        assert_eq!(db.collection_cardinality(capability).unwrap(), 1);

        let update_mutations = vec![AtomicMutation {
            capability: capability.to_string(),
            key: "rec-1".to_string(),
            value: Some(serde_json::json!({"id": 1, "status": "inactive"})),
        }];

        db.apply_atomic_transaction("txn-update", None, &[], &update_mutations, None)
            .unwrap();
        assert_eq!(
            db.collection_cardinality(capability).unwrap(),
            1,
            "Cardinality should remain 1 after update"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn maintained_cardinality_survives_restart() {
        let path = temp_file();

        {
            let db = open(&path).unwrap();
            let capability = "test_collection";

            let mutations = vec![
                AtomicMutation {
                    capability: capability.to_string(),
                    key: "rec-1".to_string(),
                    value: Some(serde_json::json!({"id": 1})),
                },
                AtomicMutation {
                    capability: capability.to_string(),
                    key: "rec-2".to_string(),
                    value: Some(serde_json::json!({"id": 2})),
                },
            ];

            db.apply_atomic_transaction("txn-1", None, &[], &mutations, None)
                .unwrap();
            assert_eq!(db.collection_cardinality(capability).unwrap(), 2);
        }

        let reopened = open(&path).unwrap();
        let capability = "test_collection";
        let cardinality = reopened.collection_cardinality(capability).unwrap();
        assert_eq!(
            cardinality, 2,
            "Cardinality should survive restart and be recomputed from rows"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn maintained_cardinality_empty_collection() {
        let path = temp_file();
        let db = open(&path).unwrap();
        let capability = "empty_collection";

        let cardinality = db.collection_cardinality(capability).unwrap();
        assert_eq!(cardinality, 0, "Empty collection should have cardinality 0");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn maintained_cardinality_large_collection() {
        let path = temp_file();
        let db = open(&path).unwrap();
        let capability = "large_collection";

        let mut mutations = Vec::new();
        for i in 1..=100 {
            mutations.push(AtomicMutation {
                capability: capability.to_string(),
                key: format!("rec-{}", i),
                value: Some(serde_json::json!({"id": i, "name": format!("Record {}", i)})),
            });
        }

        db.apply_atomic_transaction("txn-large", None, &[], &mutations, None)
            .unwrap();
        let cardinality = db.collection_cardinality(capability).unwrap();
        assert_eq!(
            cardinality, 100,
            "Large collection with 100 records should have cardinality 100"
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn maintained_cardinality_multiple_collections_isolated() {
        let path = temp_file();
        let db = open(&path).unwrap();

        let col1 = "collection_1";
        let col2 = "collection_2";

        let mutations1 = vec![
            AtomicMutation {
                capability: col1.to_string(),
                key: "rec-1".to_string(),
                value: Some(serde_json::json!({"id": 1})),
            },
            AtomicMutation {
                capability: col1.to_string(),
                key: "rec-2".to_string(),
                value: Some(serde_json::json!({"id": 2})),
            },
        ];

        db.apply_atomic_transaction("txn-col1", None, &[], &mutations1, None)
            .unwrap();

        let mutations2 = vec![
            AtomicMutation {
                capability: col2.to_string(),
                key: "rec-1".to_string(),
                value: Some(serde_json::json!({"id": 1})),
            },
            AtomicMutation {
                capability: col2.to_string(),
                key: "rec-2".to_string(),
                value: Some(serde_json::json!({"id": 2})),
            },
            AtomicMutation {
                capability: col2.to_string(),
                key: "rec-3".to_string(),
                value: Some(serde_json::json!({"id": 3})),
            },
        ];

        db.apply_atomic_transaction("txn-col2", None, &[], &mutations2, None)
            .unwrap();

        assert_eq!(db.collection_cardinality(col1).unwrap(), 2);
        assert_eq!(db.collection_cardinality(col2).unwrap(), 3);

        let _ = fs::remove_file(path);
    }
}
