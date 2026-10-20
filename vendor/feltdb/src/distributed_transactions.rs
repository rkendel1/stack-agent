/// Distributed Transactions & Replication
///
/// Coordinates transaction execution across multiple nodes:
/// - TransactionEnvelope: Atomic unit for multi-replica delivery
/// - ReplicationMessage: Operation streaming between replicas
/// - ReplicaState: Per-node state tracking with vector clocks
/// - DistributedExecutor: Orchestrates consistency semantics
///
/// Invariant: All replicas converge to identical state_hash for same operations
use crate::causal_dependency_barrier::CausalDependencyBarrier;
use crate::convergence::VectorClock;
use crate::durable_operation_log::OperationLog;
use crate::state_hash::StateHash;
use crate::transactions::*;
use std::collections::HashMap;
use std::path::PathBuf;

/// Uniquely identifies a transaction envelope
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct EnvelopeId {
    pub originating_node: String,
    pub sequence: u64,
}

impl EnvelopeId {
    pub fn new(originating_node: String, sequence: u64) -> Self {
        Self {
            originating_node,
            sequence,
        }
    }
}

/// Atomic unit for multi-replica delivery
/// Prevents partial delivery of multi-operation transactions
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TransactionEnvelope {
    pub envelope_id: EnvelopeId,
    pub transaction_id: String,
    pub operations: Vec<Operation>,
    pub consistency_contract: ConsistencyContract,
    pub originating_node: String,
    pub vector_clock: VectorClock,
    pub timestamp_ms: u64,
}

impl TransactionEnvelope {
    pub fn new(
        originating_node: String,
        sequence: u64,
        transaction_id: String,
        operations: Vec<Operation>,
        consistency_contract: ConsistencyContract,
    ) -> Self {
        let mut vc = VectorClock::new();
        vc.increment(&originating_node);

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        Self {
            envelope_id: EnvelopeId::new(originating_node.clone(), sequence),
            transaction_id,
            operations,
            consistency_contract,
            originating_node,
            vector_clock: vc,
            timestamp_ms,
        }
    }

    /// Advance vector clock to account for replication delay
    pub fn advance_clock(&mut self, replica_clock: &VectorClock) {
        self.vector_clock.merge(replica_clock);
        self.vector_clock.increment(&self.originating_node);
    }

    /// Verify envelope integrity (all operations present)
    pub fn verify_integrity(&self) -> bool {
        !self.operations.is_empty()
            && self.transaction_id.len() > 0
            && self.originating_node.len() > 0
    }
}

/// Message format for distributed operation streaming
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReplicationMessage {
    pub envelope: TransactionEnvelope,
    pub from_node: String,
    pub to_node: String,
    pub sequence_number: u64,
}

impl ReplicationMessage {
    pub fn new(
        envelope: TransactionEnvelope,
        from_node: String,
        to_node: String,
        sequence_number: u64,
    ) -> Self {
        Self {
            envelope,
            from_node,
            to_node,
            sequence_number,
        }
    }
}

/// State of a replica
#[derive(Clone, Debug)]
pub struct ReplicaState {
    pub node_id: String,
    pub vector_clock: VectorClock,
    pub state_hash: StateHash,
    pub operations_applied: u64,
    pub last_update_ms: u64,
}

impl ReplicaState {
    pub fn new(node_id: String, initial_hash: StateHash) -> Self {
        Self {
            node_id,
            vector_clock: VectorClock::new(),
            state_hash: initial_hash,
            operations_applied: 0,
            last_update_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        }
    }

    /// Update replica state after transaction
    pub fn apply_transaction(&mut self, envelope: &TransactionEnvelope, new_state_hash: StateHash) {
        self.vector_clock.merge(&envelope.vector_clock);
        self.vector_clock.increment(&self.node_id);
        self.state_hash = new_state_hash;
        self.operations_applied += envelope.operations.len() as u64;
        self.last_update_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
    }

    /// Causal read: wait for vector clock to advance
    pub fn wait_for_clock_advance(&self, required_clock: &VectorClock) -> bool {
        !self.vector_clock.happens_before(required_clock)
    }
}

/// Distributed transaction executor with durable operation log support
///
/// Durability architecture:
/// - Operation log is the ONLY durable source of truth
/// - Dedup set is derived (reconstructed from operation log on startup)
/// - No separate dedup file (eliminates two-file atomicity problem)
pub struct DistributedTransactionExecutor {
    pub local_node_id: String,
    pub replicas: HashMap<String, ReplicaState>,
    pub envelopes_seen: std::collections::HashSet<EnvelopeId>,
    pub transaction_executor: TransactionExecutor,
    pub operation_log: Option<OperationLog>,
    /// Causal delivery gate for replicated envelopes.
    ///
    /// Holds identifiers and dependency clocks only. A blocked envelope's
    /// payload stays in the durable operation log and is replayed from there
    /// when its dependencies land.
    pub causal_barrier: CausalDependencyBarrier,
    /// The bound on the causal backlog.
    pub causal_capacity: CausalCapacity,
    /// The next identity this node will issue for its own operations.
    ///
    /// Recovered from the durable log rather than reset, because
    /// `(origin, sequence)` is an identity and receivers deduplicate on it. A
    /// counter that restarts re-issues identities that already exist, and the
    /// receiver discards the new operation as a duplicate.
    next_origin_sequence: u64,
    /// Envelopes that are durable in the log but not buffered, because the
    /// backlog was at its bound when they arrived.
    ///
    /// Derived state, like `envelopes_seen`: it is rebuilt from the log on
    /// recovery and exists so that a redelivery of a deferred envelope is
    /// recognised without a log scan.
    pub deferred: std::collections::HashSet<EnvelopeId>,
}

/// What the receiver did with a replicated envelope.
///
/// This is a typed outcome rather than a string, because a caller has to be
/// able to tell "applied" from "held back" from "already known" without
/// matching on prose. The previous signature returned
/// `Result<StateHash, String>` and signalled a duplicate as
/// `Err("Duplicate envelope")`.
#[derive(Clone, Debug, PartialEq)]
pub enum ReceiveOutcome {
    /// Dependencies were satisfied; the envelope was applied.
    Applied { state_hash: StateHash },
    /// Durable, but not yet applied: some dependency has not arrived.
    /// The payload is in the operation log; the barrier holds its identity.
    PendingDependencies { missing: Vec<String> },
    /// This envelope has been seen before. Applying it again is a no-op.
    AlreadyKnown,
    /// Durable, but not buffered: the backlog is at its bound.
    ///
    /// This is *not* a refusal and nothing is lost. The envelope was fsynced to
    /// the operation log before this was decided, exactly as a buffered one is;
    /// what is withheld is the in-memory metadata entry. The caller's obligation
    /// is transport backpressure -- stop reading from that peer so TCP blocks
    /// the sender -- because returning a refusal to a peer means either silent
    /// loss or a retransmission protocol this repository does not have.
    ///
    /// The envelope is admitted from the log once the backlog drains.
    CapacityDeferred {
        missing: Vec<String>,
        pending_entries: usize,
        pending_bytes: usize,
    },
}

/// What the executor did with a locally submitted transaction.
///
/// Capacity refusal is a distinct outcome rather than an `Err(String)`, because
/// a caller has to tell "this replica is saturated, retry" from "this
/// transaction is invalid". The two need opposite responses and the previous
/// signature made them the same value.
#[derive(Clone, Debug)]
pub enum SubmitOutcome {
    Admitted(TransactionEnvelope),
    /// The causal backlog is at its bound, so no new local work is admitted.
    /// Nothing was executed, persisted, or applied: there is nothing to undo
    /// and nothing to recover.
    CapacityExceeded {
        pending_entries: usize,
        pending_bytes: usize,
        limit_entries: usize,
        limit_bytes: usize,
    },
}

/// The bound on the causal backlog, as decided in
/// `docs/architecture/causal-backlog-bound.md`.
///
/// Two limits, because they are driven by different variables. Bytes bound the
/// outstanding payload volume; entries bound the metadata, whose per-entry cost
/// scales with cluster size rather than with operation size, so a byte bound
/// alone would score ten thousand pending one-byte operations as nearly free.
///
/// Both are inclusive: the bound is `<=`, so filling exactly to it is accepted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CausalCapacity {
    pub max_pending_entries: usize,
    pub max_pending_bytes: usize,
}

impl CausalCapacity {
    /// The shipping default.
    ///
    /// These are policy, not measurements, and are deliberately not derived
    /// from a benchmark constant -- the figure that used to justify backlog
    /// sizing here was `backlog_depth * 1500`, which was the benchmark's own
    /// input rather than an observation. What the acceptance test establishes
    /// is that the bound is *enforced through the production path*, and it
    /// reports the occupancy it actually observed rather than asserting a
    /// predicted footprint.
    pub const DEFAULT_MAX_PENDING_ENTRIES: usize = 10_000;
    pub const DEFAULT_MAX_PENDING_BYTES: usize = 64 * 1024 * 1024;

    /// A bound small enough to be reached by a test through the real path.
    pub fn new(max_pending_entries: usize, max_pending_bytes: usize) -> Self {
        Self {
            max_pending_entries,
            max_pending_bytes,
        }
    }
}

impl Default for CausalCapacity {
    fn default() -> Self {
        Self {
            max_pending_entries: Self::DEFAULT_MAX_PENDING_ENTRIES,
            max_pending_bytes: Self::DEFAULT_MAX_PENDING_BYTES,
        }
    }
}

impl DistributedTransactionExecutor {
    /// Create a new distributed executor without durable persistence
    pub fn new(node_id: String, initial_state_hash: StateHash) -> Self {
        let mut replicas = HashMap::new();
        replicas.insert(
            node_id.clone(),
            ReplicaState::new(node_id.clone(), initial_state_hash),
        );

        Self {
            local_node_id: node_id.clone(),
            replicas,
            envelopes_seen: std::collections::HashSet::new(),
            transaction_executor: TransactionExecutor::new(node_id),
            operation_log: None,
            causal_barrier: CausalDependencyBarrier::new(),
            causal_capacity: CausalCapacity::default(),
            next_origin_sequence: 1,
            deferred: std::collections::HashSet::new(),
        }
    }

    /// Create a new distributed executor with durable operation log
    ///
    /// Note: dedup set is NOT persisted separately. It is reconstructed deterministically
    /// from the operation log on startup, eliminating atomicity issues.
    pub fn with_log(
        node_id: String,
        initial_state_hash: StateHash,
        log_path: Option<PathBuf>,
    ) -> Result<Self, std::io::Error> {
        let mut replicas = HashMap::new();
        replicas.insert(
            node_id.clone(),
            ReplicaState::new(node_id.clone(), initial_state_hash),
        );

        let operation_log = if let Some(path) = &log_path {
            Some(OperationLog::new(path)?)
        } else {
            None
        };

        // Identity is recovered here as well as in `load_from_disk`, because a
        // caller that opens an existing log and issues an operation without
        // replaying it must still not re-issue an identity that log already
        // contains.
        let next_origin_sequence = match &operation_log {
            Some(log) => Self::next_sequence_after(log, &node_id)?,
            None => 1,
        };

        Ok(Self {
            local_node_id: node_id.clone(),
            replicas,
            envelopes_seen: std::collections::HashSet::new(),
            transaction_executor: TransactionExecutor::new(node_id),
            operation_log,
            causal_barrier: CausalDependencyBarrier::new(),
            causal_capacity: CausalCapacity::default(),
            next_origin_sequence,
            deferred: std::collections::HashSet::new(),
        })
    }

    /// Register a replica (peer node)
    pub fn register_replica(&mut self, node_id: String, initial_state_hash: StateHash) {
        self.replicas.insert(
            node_id.clone(),
            ReplicaState::new(node_id, initial_state_hash),
        );
    }

    /// Execute transaction locally and prepare for replication
    /// Ordering: execute → persist → update state → record dedup
    // ---------------------------------------------------------------------
    // Durable operation identity
    //
    // `(origin, sequence)` is an identity, not a counter. Receivers
    // deduplicate on it, so two operations must never be assigned the same
    // one -- and "must never" has to hold across a process restart, which is
    // where it previously did not: `feltdb_node` kept the counter in memory
    // and started it at 1, so a restarted origin re-issued identities its own
    // earlier operations already held and its peers discarded the new writes
    // as duplicates. See `dedup_bound_investigation.rs`.
    //
    // The authority is the durable log, and specifically the log rather than
    // the causal clock. Those are different identity domains: the clock
    // component advances only on a successful apply, while the sequence is
    // burned by an attempt, so the two disagree by exactly the number of
    // refused transactions. `replication-convergence.md` fixes that the
    // sequence is sparse and that making it dense is a separate deferred
    // decision, so this derives identity from the sequence domain and leaves
    // that decision alone.
    // ---------------------------------------------------------------------

    /// The highest identity committed to the durable log, per origin.
    ///
    /// For this node it is its own identity high-water mark; for others it is
    /// how far this replica's log carries their identities. A checkpoint that
    /// discards log prefix must preserve these, because the log is also the
    /// source `next_origin_sequence` is recovered from.
    pub fn origin_sequence_high_water(&self) -> std::collections::BTreeMap<String, u64> {
        let mut marks = std::collections::BTreeMap::new();
        let Some(ref log) = self.operation_log else {
            return marks;
        };
        let Ok(envelopes) = log.load_all() else {
            return marks;
        };
        for envelope in envelopes {
            let entry = marks
                .entry(envelope.envelope_id.originating_node.clone())
                .or_insert(0u64);
            *entry = (*entry).max(envelope.envelope_id.sequence);
        }
        marks
    }

    /// The next identity this node would issue.
    pub fn next_origin_sequence(&self) -> u64 {
        self.next_origin_sequence
    }

    /// Claim the next identity for an operation this node originates.
    ///
    /// Advancing on the attempt rather than on success is deliberate: it
    /// preserves the sparse-sequence semantics the convergence contract froze,
    /// where a refused transaction burns a number. Making the sequence dense is
    /// a separate decision and is not made here as a side effect.
    ///
    /// A number burned and then lost to a crash is safe to re-issue, because
    /// nothing carrying it was ever persisted: recovery derives from what the
    /// log holds, so an identity is only reserved forever once an envelope
    /// bearing it is durable. That is the invariant -- no two *persisted*
    /// envelopes from one origin share a sequence -- and it is the one
    /// receivers depend on.
    pub fn allocate_origin_sequence(&mut self) -> u64 {
        let claimed = self.next_origin_sequence;
        self.next_origin_sequence = claimed.saturating_add(1);
        claimed
    }

    /// Derive the next identity from a durable log: one past the highest this
    /// node has already committed to it.
    ///
    /// Filtered to locally-originated envelopes, because the log also holds
    /// replicated ones and another node's sequence says nothing about this
    /// node's identity space.
    fn next_sequence_after(log: &OperationLog, node_id: &str) -> Result<u64, std::io::Error> {
        let highest = log
            .load_all()?
            .iter()
            .filter(|envelope| envelope.envelope_id.originating_node == node_id)
            .map(|envelope| envelope.envelope_id.sequence)
            .max()
            .unwrap_or(0);
        Ok(highest.saturating_add(1))
    }

    /// Submit local work, allocating its identity from durable state.
    ///
    /// This is the entry point a node should use: it owns the allocation, so
    /// the identity cannot be assigned by one caller and persisted by another,
    /// and there is no counter outside the executor to fall out of step with
    /// the log.
    pub fn submit_local_transaction(
        &mut self,
        transaction_id: String,
        parent_version: StateVersion,
        operations: Vec<Operation>,
        consistency_contract: ConsistencyContract,
    ) -> Result<SubmitOutcome, String> {
        // Allocation happens after the capacity check inside
        // `try_execute_local_transaction`, so a refusal does not burn an
        // identity: nothing was admitted, and there is nothing to account for.
        if self.would_breach_bound(Self::operations_bytes(&operations)) {
            return Ok(SubmitOutcome::CapacityExceeded {
                pending_entries: self.causal_barrier.pending_count(),
                pending_bytes: self.causal_barrier.pending_payload_bytes(),
                limit_entries: self.causal_capacity.max_pending_entries,
                limit_bytes: self.causal_capacity.max_pending_bytes,
            });
        }
        let sequence = self.allocate_origin_sequence();
        self.execute_local_transaction(
            sequence,
            transaction_id,
            parent_version,
            operations,
            consistency_contract,
        )
        .map(SubmitOutcome::Admitted)
    }

    /// Submit local work, subject to the causal backlog bound.
    ///
    /// The bound is checked *before* anything is executed, persisted or
    /// applied, so a refusal leaves nothing to undo and nothing to recover:
    /// the operation was never admitted. That is what makes refusal the right
    /// answer on this path and the wrong one for a replicated arrival, which
    /// was already admitted and made durable somewhere else.
    pub fn try_execute_local_transaction(
        &mut self,
        sequence: u64,
        transaction_id: String,
        parent_version: StateVersion,
        operations: Vec<Operation>,
        consistency_contract: ConsistencyContract,
    ) -> Result<SubmitOutcome, String> {
        let projected_bytes = Self::operations_bytes(&operations);
        if self.would_breach_bound(projected_bytes) {
            return Ok(SubmitOutcome::CapacityExceeded {
                pending_entries: self.causal_barrier.pending_count(),
                pending_bytes: self.causal_barrier.pending_payload_bytes(),
                limit_entries: self.causal_capacity.max_pending_entries,
                limit_bytes: self.causal_capacity.max_pending_bytes,
            });
        }
        self.execute_local_transaction(
            sequence,
            transaction_id,
            parent_version,
            operations,
            consistency_contract,
        )
        .map(SubmitOutcome::Admitted)
    }

    /// The size local work would contribute, measured before it is executed.
    fn operations_bytes(operations: &[Operation]) -> usize {
        serde_json::to_string(operations)
            .map(|json| json.len())
            .unwrap_or(0)
    }

    /// Execute local work unconditionally.
    ///
    /// This does not consult the backlog bound; `try_execute_local_transaction`
    /// is the admitting entry point and the one the node binary uses. This
    /// remains for callers that have already made the admission decision.
    pub fn execute_local_transaction(
        &mut self,
        sequence: u64,
        transaction_id: String,
        parent_version: StateVersion,
        operations: Vec<Operation>,
        consistency_contract: ConsistencyContract,
    ) -> Result<TransactionEnvelope, String> {
        // Step 1: Execute transaction through 14C
        let transition = self.transaction_executor.execute_transaction(
            transaction_id.clone(),
            parent_version,
            operations.clone(),
            consistency_contract.clone(),
        );

        if !transition.is_successful() {
            return Err("Transaction execution failed".to_string());
        }

        // Step 2: Create envelope for replication
        let mut envelope = TransactionEnvelope::new(
            self.local_node_id.clone(),
            sequence,
            transaction_id,
            operations,
            consistency_contract,
        );

        // Step 2a: Stamp the envelope with this node's causal knowledge.
        //
        // `TransactionEnvelope::new` starts from an empty clock and increments
        // only the origin, so every envelope it built carried {origin: 1}
        // whatever the node had already seen. That is a position, not a
        // history: a receiver given those clocks has no dependencies to
        // enforce, which is why causal ordering could not have worked on this
        // path even once the barrier was wired to it. The clock must carry the
        // applied frontier this operation was produced on top of.
        let mut stamped = self.causal_barrier.get_frontier().clone();
        stamped.increment(&self.local_node_id);
        envelope.vector_clock = stamped;

        // Step 3: Durably persist operation to log (with fsync)
        // This must succeed before we update in-memory state
        self.persist_envelope(&envelope)?;

        // Step 4: Update local replica state (in-memory)
        if let Some(local_replica) = self.replicas.get_mut(&self.local_node_id) {
            let state_hash = StateHash::from_hex(transition.to_version.state_hash.clone());
            local_replica.apply_transaction(&envelope, state_hash);
        }

        // Step 5: Record envelope to prevent duplicates (in-memory, derived from log)
        self.envelopes_seen.insert(envelope.envelope_id.clone());

        // Step 6: A locally executed operation is applied, so it advances the
        // frontier exactly as a replicated one does. Own writes are always
        // causally eligible: they are built on the frontier they advance.
        self.causal_barrier.record_applied(
            &Self::barrier_key(&envelope.envelope_id),
            &envelope.vector_clock,
        );

        Ok(envelope)
    }

    /// Receive replicated transaction from peer
    /// Ordering: dedup check → execute → persist → update state
    pub fn receive_replicated_transaction(
        &mut self,
        message: ReplicationMessage,
        parent_version: StateVersion,
    ) -> Result<StateHash, String> {
        match self.receive_replicated(message, parent_version)? {
            ReceiveOutcome::Applied { state_hash } => Ok(state_hash),
            ReceiveOutcome::PendingDependencies { missing } => Err(format!(
                "Envelope is durable but waiting on dependencies: {}",
                missing.join(", ")
            )),
            ReceiveOutcome::AlreadyKnown => Err("Duplicate envelope".to_string()),
            ReceiveOutcome::CapacityDeferred { missing, .. } => Err(format!(
                "CAPACITY_DEFERRED: envelope is durable but the causal backlog is at its bound, waiting on: {}",
                missing.join(", ")
            )),
        }
    }

    /// The causal receive path.
    ///
    /// Ordering is deliberate and is the substance of this change:
    ///
    ///   dedup -> persist -> causal eligibility -> apply -> release
    ///
    /// Persisting *before* deciding eligibility is what makes "held back" a
    /// durable state rather than an in-memory one. An envelope that arrives
    /// ahead of its dependencies is fsynced to the operation log and then
    /// retained in the barrier by identity alone; if the process dies at that
    /// moment, recovery replays the log and rediscovers it as pending. This is
    /// also what catch-up will require, because a peer sending A3, A1, A2 in
    /// transport order needs the receiver to be able to say "I have A3
    /// durably, and I cannot apply it yet".
    pub fn receive_replicated(
        &mut self,
        message: ReplicationMessage,
        parent_version: StateVersion,
    ) -> Result<ReceiveOutcome, String> {
        let envelope_id = message.envelope.envelope_id.clone();
        let key = Self::barrier_key(&envelope_id);

        // Step 1: Duplicate delivery is a no-op, whether the original was
        // applied or is still waiting. Both states are recorded before this
        // point is reached again, so one logical operation stays one.
        if self.envelopes_seen.contains(&envelope_id)
            || self.causal_barrier.is_pending(&key)
            || self.deferred.contains(&envelope_id)
        {
            return Ok(ReceiveOutcome::AlreadyKnown);
        }

        // Step 2: Durability first. The log is the source of truth, so an
        // envelope becomes durable on receipt and not on application.
        self.persist_envelope(&message.envelope)?;

        // Step 3: Eligibility. The dependencies are everything the origin had
        // seen when it produced this operation, which is its clock with its own
        // entry stepped back by one; the operation itself is not its own
        // dependency.
        let dependencies = Self::dependencies_of(&message.envelope);
        if !self.causal_barrier.is_eligible(&dependencies) {
            let missing = self.missing_dependencies(&dependencies);

            // Step 3a: The bound. Buffering costs an entry and holds a payload
            // outstanding in the log; if either would breach its limit, the
            // metadata entry is withheld and the envelope stays durable and
            // deferred. It is never refused and never evicted -- refusing a
            // peer means silent loss or a retransmission protocol that does not
            // exist here, and eviction is silent loss under another name.
            let payload_bytes = Self::envelope_bytes(&message.envelope);
            if self.would_breach_bound(payload_bytes) {
                self.deferred.insert(envelope_id);
                return Ok(ReceiveOutcome::CapacityDeferred {
                    missing,
                    pending_entries: self.causal_barrier.pending_count(),
                    pending_bytes: self.causal_barrier.pending_payload_bytes(),
                });
            }

            self.causal_barrier
                .retain_pending_sized(key, dependencies, payload_bytes);
            return Ok(ReceiveOutcome::PendingDependencies { missing });
        }

        // Step 4: Apply, which advances the frontier.
        let state_hash = self.apply_envelope(&message.envelope, parent_version.clone())?;

        // Step 5: This application may have unblocked others, and may have
        // freed the capacity a deferred envelope was waiting for. Drain both.
        self.drain(parent_version)?;

        Ok(ReceiveOutcome::Applied { state_hash })
    }

    /// Drive the backlog forward from outside the receive path.
    ///
    /// A deferred envelope is durable but unbuffered, and the event that frees
    /// capacity for it may arrive on a different connection than the one that
    /// was deferred. A caller applying transport backpressure needs a way to
    /// make progress without reading from the backpressured peer, and this is
    /// it.
    pub fn drain_backlog(&mut self, parent_version: StateVersion) -> Result<usize, String> {
        self.drain(parent_version)
    }

    /// Whether admitting one more entry of this size would breach either bound.
    ///
    /// Inclusive, as the contract specifies: filling exactly to the bound is
    /// accepted, and the breach is the step past it.
    fn would_breach_bound(&self, payload_bytes: usize) -> bool {
        let entries = self.causal_barrier.pending_count();
        let bytes = self.causal_barrier.pending_payload_bytes();
        entries + 1 > self.causal_capacity.max_pending_entries
            || bytes + payload_bytes > self.causal_capacity.max_pending_bytes
    }

    /// The envelope's size as the durable log stores it.
    ///
    /// Measured with the log's own serialisation rather than a nominal
    /// per-operation constant, so a large envelope consumes its actual byte
    /// count.
    fn envelope_bytes(envelope: &TransactionEnvelope) -> usize {
        serde_json::to_string(envelope)
            .map(|json| json.len())
            .unwrap_or(0)
    }

    /// Apply one envelope and advance the applied frontier.
    fn apply_envelope(
        &mut self,
        envelope: &TransactionEnvelope,
        parent_version: StateVersion,
    ) -> Result<StateHash, String> {
        let transition = self.transaction_executor.execute_transaction(
            envelope.transaction_id.clone(),
            parent_version,
            envelope.operations.clone(),
            envelope.consistency_contract.clone(),
        );

        if !transition.is_successful() {
            return Err("Transaction execution failed".to_string());
        }

        self.envelopes_seen.insert(envelope.envelope_id.clone());

        let state_hash = StateHash::from_hex(transition.to_version.state_hash.clone());
        if let Some(replica) = self.replicas.get_mut(&self.local_node_id) {
            replica.apply_transaction(envelope, state_hash.clone());
        }

        // The frontier advances here and only here, so it describes what has
        // been successfully applied rather than what was claimed or received.
        self.causal_barrier.record_applied(
            &Self::barrier_key(&envelope.envelope_id),
            &envelope.vector_clock,
        );

        Ok(state_hash)
    }

    /// Drive the backlog forward until it stops moving.
    ///
    /// Two things can make progress and each can enable the other: applying a
    /// pending envelope advances the frontier, which may make other pending
    /// envelopes eligible; and applying anything frees capacity, which may let
    /// a deferred envelope be admitted from the log. So this alternates until
    /// neither step changes anything, which terminates because every iteration
    /// that continues has strictly reduced the number of unapplied envelopes.
    fn drain(&mut self, parent_version: StateVersion) -> Result<usize, String> {
        let mut progressed_total = 0;
        loop {
            let released = self.release_eligible(parent_version.clone())?;
            let admitted = self.admit_deferred(parent_version.clone())?;
            progressed_total += released + admitted;
            if released == 0 && admitted == 0 {
                return Ok(progressed_total);
            }
        }
    }

    /// Apply every pending envelope the frontier now satisfies, repeatedly,
    /// because releasing one can make the next eligible.
    ///
    /// The log is read once per pass rather than once per key. It used to be
    /// once per key: `replay_from_log` called `load_all()` for every eligible
    /// entry, so a release cascade over a backlog of n entries against a log of
    /// m records did n full scans. That is the exact shape of load this change
    /// makes reachable -- a bounded backlog is one that is allowed to get
    /// large -- so the quadratic scan had to go with it.
    fn release_eligible(&mut self, parent_version: StateVersion) -> Result<usize, String> {
        let mut released = 0;
        loop {
            let ready = self.causal_barrier.eligible_pending();
            if ready.is_empty() {
                return Ok(released);
            }
            let by_key = self.log_by_key()?;
            let mut progressed = false;
            for key in ready {
                let Some(envelope) = by_key.get(&key) else {
                    // The barrier holds an identity whose payload is not in the
                    // log. Dropping the entry would be silent loss, so leave it
                    // pending and surface it rather than pretend it applied.
                    continue;
                };
                self.apply_envelope(&envelope.clone(), parent_version.clone())?;
                released += 1;
                progressed = true;
            }
            if !progressed {
                return Ok(released);
            }
        }
    }

    /// Move envelopes that are durable but unbuffered into the backlog, as far
    /// as the bound now allows.
    ///
    /// This is what makes `CapacityDeferred` safe: the envelope was fsynced
    /// before it was deferred, so admitting it later needs no cooperation from
    /// the sender and survives a restart in between. Entries are admitted in
    /// key order so that two replicas draining the same set drain it
    /// identically -- behaviour at capacity must not depend on iteration order
    /// or timing.
    fn admit_deferred(&mut self, parent_version: StateVersion) -> Result<usize, String> {
        if self.deferred.is_empty() {
            return Ok(0);
        }
        let by_key = self.log_by_key()?;
        let mut keys: Vec<String> = self.deferred.iter().map(Self::barrier_key).collect();
        keys.sort();

        let mut admitted = 0;
        for key in keys {
            let Some(envelope) = by_key.get(&key).cloned() else {
                continue;
            };
            let dependencies = Self::dependencies_of(&envelope);
            let payload_bytes = Self::envelope_bytes(&envelope);

            if self.causal_barrier.is_eligible(&dependencies) {
                self.deferred.remove(&envelope.envelope_id);
                self.apply_envelope(&envelope, parent_version.clone())?;
                admitted += 1;
                continue;
            }
            if self.would_breach_bound(payload_bytes) {
                // Still no room. Leave it deferred; it is durable and will be
                // reconsidered on the next drain or after a restart.
                continue;
            }
            self.deferred.remove(&envelope.envelope_id);
            self.causal_barrier
                .retain_pending_sized(key, dependencies, payload_bytes);
            admitted += 1;
        }
        Ok(admitted)
    }

    /// The durable log, indexed by barrier key.
    ///
    /// This is why the barrier never needs the payload: it holds the identity,
    /// and the bytes stay where they were already fsynced.
    fn log_by_key(&self) -> Result<HashMap<String, TransactionEnvelope>, String> {
        let Some(ref log) = self.operation_log else {
            return Ok(HashMap::new());
        };
        let envelopes = log
            .load_all()
            .map_err(|e| format!("Failed to replay operation log: {}", e))?;
        Ok(envelopes
            .into_iter()
            .map(|envelope| (Self::barrier_key(&envelope.envelope_id), envelope))
            .collect())
    }

    /// The stable string identity the barrier is keyed by.
    fn barrier_key(envelope_id: &EnvelopeId) -> String {
        format!("{}:{}", envelope_id.originating_node, envelope_id.sequence)
    }

    /// The causal dependencies of an envelope: what its origin had applied
    /// before producing it.
    fn dependencies_of(envelope: &TransactionEnvelope) -> VectorClock {
        let mut dependencies = envelope.vector_clock.clone();
        let own = dependencies.get(&envelope.originating_node);
        dependencies
            .clocks
            .insert(envelope.originating_node.clone(), own.saturating_sub(1));
        dependencies
    }

    /// Which dimensions of a dependency clock the frontier has not reached.
    fn missing_dependencies(&self, dependencies: &VectorClock) -> Vec<String> {
        let frontier = self.causal_barrier.get_frontier();
        let mut missing: Vec<String> = dependencies
            .clocks
            .iter()
            .filter(|(node, required)| frontier.get(node) < **required)
            .map(|(node, required)| format!("{}@{} (have {})", node, required, frontier.get(node)))
            .collect();
        missing.sort();
        missing
    }

    /// How many envelopes are durable but waiting on dependencies.
    ///
    /// Instrumentation only; no limit is enforced. See
    /// docs/architecture/causal-backlog-bound.md.
    pub fn pending_causal_count(&self) -> usize {
        self.causal_barrier.pending_count()
    }

    /// Approximate heap cost of the pending causal metadata, in bytes.
    pub fn pending_causal_metadata_bytes(&self) -> usize {
        self.causal_barrier.pending_metadata_bytes()
    }

    /// Durable payload bytes the backlog is waiting on. The byte bound is
    /// expressed in these.
    pub fn pending_causal_payload_bytes(&self) -> usize {
        self.causal_barrier.pending_payload_bytes()
    }

    /// Envelopes that are durable but not buffered, because the bound was
    /// reached when they arrived. They are not lost and not evicted.
    pub fn deferred_causal_count(&self) -> usize {
        self.deferred.len()
    }

    /// Keys of envelopes that are durable but not buffered, sorted.
    pub fn deferred_causal_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.deferred.iter().map(Self::barrier_key).collect();
        keys.sort();
        keys
    }

    /// Maximum occupancy ever reached, for entries and payload bytes.
    ///
    /// Reported alongside the current values because a backlog that filled and
    /// drained between two reads is invisible to a current-value gauge, and the
    /// question an operator has is whether the bound was ever approached.
    pub fn max_causal_occupancy(&self) -> (usize, usize) {
        (
            self.causal_barrier.max_pending_entries_seen(),
            self.causal_barrier.max_pending_payload_bytes_seen(),
        )
    }

    /// The bound in force on this executor.
    pub fn causal_capacity(&self) -> CausalCapacity {
        self.causal_capacity
    }

    /// Set the bound. Used by the node binary so an acceptance test can reach
    /// the bound through the production path rather than by constructing a
    /// barrier directly.
    pub fn set_causal_capacity(&mut self, capacity: CausalCapacity) {
        self.causal_capacity = capacity;
    }

    /// The applied causal frontier: what this replica has successfully applied.
    pub fn causal_frontier(&self) -> &VectorClock {
        self.causal_barrier.get_frontier()
    }

    /// Identities of envelopes durable but not yet applied.
    pub fn pending_causal_keys(&self) -> Vec<String> {
        self.causal_barrier.pending_keys()
    }

    /// Materialize the applied records, order-independently.
    ///
    /// This exists because the chained `H(parent, command)` state hash is a
    /// missing-operation detector for an ordered history, not a convergence
    /// identity: two replicas that applied the same set of concurrent
    /// operations in different orders hold the same data and report different
    /// chained hashes. `CanonicalState` is a BTreeMap keyed by
    /// (collection, record_id), so its hash depends on the resulting records
    /// and not on the path taken to them, which is what "the replicas agree"
    /// should mean.
    ///
    /// Built from the durable log, filtered to what the barrier records as
    /// applied and taken in application order, so a pending envelope's
    /// operations are excluded until it is actually released.
    pub fn canonical_state(&self) -> Result<crate::state_hash::CanonicalState, String> {
        let mut state = crate::state_hash::CanonicalState::new();
        let Some(ref log) = self.operation_log else {
            return Ok(state);
        };
        let envelopes = log
            .load_all()
            .map_err(|e| format!("Failed to read operation log: {}", e))?;
        let by_key: HashMap<String, &TransactionEnvelope> = envelopes
            .iter()
            .map(|envelope| (Self::barrier_key(&envelope.envelope_id), envelope))
            .collect();

        for key in self.causal_barrier.applied_keys() {
            let Some(envelope) = by_key.get(key) else {
                continue;
            };
            for operation in &envelope.operations {
                let command = &operation.command;
                let fields: serde_json::Map<String, serde_json::Value> = command
                    .fields
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect();
                state.set_record(
                    command.collection.clone(),
                    command.record_id.clone(),
                    serde_json::Value::Object(fields),
                );
            }
        }
        Ok(state)
    }

    /// Get current state of a replica
    pub fn get_replica_state(&self, node_id: &str) -> Option<ReplicaState> {
        self.replicas.get(node_id).cloned()
    }

    /// Check if replicas have converged to same state
    pub fn check_convergence(&self) -> bool {
        if self.replicas.is_empty() {
            return true;
        }

        let first_hash = self.replicas.values().next().map(|r| r.state_hash.clone());

        if let Some(first) = first_hash {
            self.replicas.values().all(|r| r.state_hash == first)
        } else {
            false
        }
    }

    /// Get all replica states
    pub fn get_all_replicas(&self) -> Vec<ReplicaState> {
        self.replicas.values().cloned().collect()
    }

    /// Causal read: wait for specific version
    pub fn causal_read(&self, required_clock: &VectorClock) -> bool {
        if let Some(local) = self.replicas.get(&self.local_node_id) {
            local.wait_for_clock_advance(required_clock)
        } else {
            false
        }
    }

    /// Get the local node ID
    pub fn get_local_node_id(&self) -> String {
        self.local_node_id.clone()
    }

    /// Load persisted operations from disk and rebuild internal state
    ///
    /// Atomicity guarantee:
    /// - Load operation log (single durable source of truth)
    /// - Reconstruct dedup set deterministically from envelopes
    /// - No separate dedup file (no two-file atomicity problem)
    pub fn load_from_disk(&mut self, log_path: PathBuf) -> Result<usize, std::io::Error> {
        let log = OperationLog::new(&log_path)?;
        let envelopes = log.load_all()?;

        self.operation_log = Some(log);

        // Rebuild the causal state, not just the dedup set.
        //
        // The log records what was *received*, in arrival order, and arrival
        // order is not causal order -- that is the whole reason the barrier
        // exists. So recovery cannot simply mark everything seen: it has to
        // replay eligibility, exactly as the live path does, and come out with
        // the same split between applied and still-pending. This is what makes
        // "deliver B, restart, deliver A" work: B is durable across the
        // restart, is rediscovered as pending rather than as applied, and is
        // released when A finally arrives.
        let mut undecided: Vec<TransactionEnvelope> = envelopes.clone();
        loop {
            let mut progressed = false;
            let mut still_undecided = Vec::new();
            for envelope in undecided.into_iter() {
                let dependencies = Self::dependencies_of(&envelope);
                if self.causal_barrier.is_eligible(&dependencies) {
                    self.envelopes_seen.insert(envelope.envelope_id.clone());
                    self.causal_barrier.record_applied(
                        &Self::barrier_key(&envelope.envelope_id),
                        &envelope.vector_clock,
                    );
                    progressed = true;
                } else {
                    still_undecided.push(envelope);
                }
            }
            undecided = still_undecided;
            if !progressed || undecided.is_empty() {
                break;
            }
        }

        // Whatever could not be made eligible is durable and waiting, and is
        // restored to the barrier in that state -- as far as the bound allows.
        //
        // Recovery has to respect the same bound the live path does, or a node
        // could restart into an over-full backlog that it would never have
        // accepted while running, and the bound would hold only until the first
        // crash. Entries are restored in key order so that recovery is
        // deterministic: the same log and the same bound produce the same split
        // between pending and deferred every time.
        undecided.sort_by_key(|envelope| Self::barrier_key(&envelope.envelope_id));
        for envelope in undecided {
            let dependencies = Self::dependencies_of(&envelope);
            let payload_bytes = Self::envelope_bytes(&envelope);
            if self.would_breach_bound(payload_bytes) {
                self.deferred.insert(envelope.envelope_id.clone());
                continue;
            }
            self.causal_barrier.retain_pending_sized(
                Self::barrier_key(&envelope.envelope_id),
                dependencies,
                payload_bytes,
            );
        }

        // Restore the observable local replica state from the same applied
        // subset. Recovery previously rebuilt the barrier and canonical data
        // but left this counter and clock at their constructor values, making
        // a fully recovered node report that it had applied zero operations.
        let applied_keys = self.causal_barrier.applied_keys();
        let operations_applied = envelopes
            .iter()
            .filter(|envelope| applied_keys.contains(&Self::barrier_key(&envelope.envelope_id)))
            .map(|envelope| envelope.operations.len() as u64)
            .sum();
        let recovered_clock = self.causal_barrier.get_frontier().clone();
        let recovered_hash = self
            .canonical_state()
            .map(|state| state.hash())
            .unwrap_or_else(|_| StateHash::from_hex("0".repeat(64)));
        if let Some(local) = self.replicas.get_mut(&self.local_node_id) {
            local.vector_clock = recovered_clock;
            local.operations_applied = operations_applied;
            local.state_hash = recovered_hash;
        }

        // Identity is recovered from the same log, and this is the half that
        // was missing: the causal frontier was already rebuilt here, so a
        // restarted origin came back knowing its causal position and not its
        // identity position, and issued colliding identities on top of a
        // correct clock.
        self.next_origin_sequence = envelopes
            .iter()
            .filter(|envelope| envelope.envelope_id.originating_node == self.local_node_id)
            .map(|envelope| envelope.envelope_id.sequence)
            .max()
            .unwrap_or(0)
            .saturating_add(1);

        Ok(envelopes.len())
    }

    /// Persist an envelope to the durable operation log
    /// Must be called after successful state application but before returning to caller
    ///
    /// Note: dedup set is NOT persisted (it's derived from the operation log).
    fn persist_envelope(&mut self, envelope: &TransactionEnvelope) -> Result<(), String> {
        if let Some(ref mut log) = self.operation_log {
            log.append(envelope)
                .map_err(|e| format!("Failed to persist operation: {}", e))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_envelope_creation() {
        let envelope = TransactionEnvelope::new(
            "node1".to_string(),
            1,
            "txn1".to_string(),
            Vec::new(),
            ConsistencyContract::causal(),
        );

        assert_eq!(envelope.originating_node, "node1");
        assert_eq!(envelope.transaction_id, "txn1");
        assert_eq!(envelope.envelope_id.sequence, 1);
    }

    #[test]
    fn test_envelope_integrity() {
        let envelope = TransactionEnvelope::new(
            "node1".to_string(),
            1,
            "txn1".to_string(),
            vec![], // Empty operations
            ConsistencyContract::causal(),
        );

        assert!(!envelope.verify_integrity());
    }

    #[test]
    fn test_replication_message_creation() {
        let envelope = TransactionEnvelope::new(
            "node1".to_string(),
            1,
            "txn1".to_string(),
            Vec::new(),
            ConsistencyContract::causal(),
        );

        let msg = ReplicationMessage::new(envelope, "node1".to_string(), "node2".to_string(), 1);

        assert_eq!(msg.from_node, "node1");
        assert_eq!(msg.to_node, "node2");
    }

    #[test]
    fn test_replica_state_creation() {
        let state = ReplicaState::new(
            "node1".to_string(),
            StateHash::from_hex("hash0".to_string()),
        );

        assert_eq!(state.node_id, "node1");
        assert_eq!(state.operations_applied, 0);
    }

    #[test]
    fn test_distributed_executor_creation() {
        let executor = DistributedTransactionExecutor::new(
            "node1".to_string(),
            StateHash::from_hex("hash0".to_string()),
        );

        assert!(executor.replicas.contains_key("node1"));
    }

    #[test]
    fn test_register_replica() {
        let mut executor = DistributedTransactionExecutor::new(
            "node1".to_string(),
            StateHash::from_hex("hash0".to_string()),
        );

        executor.register_replica(
            "node2".to_string(),
            StateHash::from_hex("hash0".to_string()),
        );

        assert!(executor.replicas.contains_key("node2"));
    }

    #[test]
    fn test_convergence_check_empty() {
        let executor = DistributedTransactionExecutor::new(
            "node1".to_string(),
            StateHash::from_hex("hash0".to_string()),
        );

        // Single replica should be converged
        assert!(executor.check_convergence());
    }

    #[test]
    fn test_convergence_check_multiple_replicas() {
        let mut executor = DistributedTransactionExecutor::new(
            "node1".to_string(),
            StateHash::from_hex("hash0".to_string()),
        );

        executor.register_replica(
            "node2".to_string(),
            StateHash::from_hex("hash0".to_string()),
        );
        executor.register_replica(
            "node3".to_string(),
            StateHash::from_hex("hash0".to_string()),
        );

        assert!(executor.check_convergence());
    }

    #[test]
    fn test_convergence_check_diverged() {
        let mut executor = DistributedTransactionExecutor::new(
            "node1".to_string(),
            StateHash::from_hex("hash0".to_string()),
        );

        executor.register_replica(
            "node2".to_string(),
            StateHash::from_hex("hash1".to_string()),
        );

        assert!(!executor.check_convergence());
    }

    #[test]
    fn test_causal_read_initial_state() {
        let executor = DistributedTransactionExecutor::new(
            "node1".to_string(),
            StateHash::from_hex("hash0".to_string()),
        );

        let vc = VectorClock::new();
        assert!(executor.causal_read(&vc));
    }

    #[test]
    fn test_envelope_clock_advance() {
        let mut envelope = TransactionEnvelope::new(
            "node1".to_string(),
            1,
            "txn1".to_string(),
            Vec::new(),
            ConsistencyContract::causal(),
        );

        let mut other_clock = VectorClock::new();
        other_clock.increment("node2");

        envelope.advance_clock(&other_clock);

        assert!(envelope.vector_clock.happens_before(&other_clock) == false);
    }
}
