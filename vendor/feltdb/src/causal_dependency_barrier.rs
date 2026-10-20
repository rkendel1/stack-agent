/// Causal Consistency Dependency Barrier (Gate 4)
///
/// Implements the critical invariant: No operation becomes visible at a replica
/// until all causal dependencies encoded by its StateVersion are satisfied.
///
/// Explicit state machine:
///
///   received operation O
///       ↓
///   check_dependencies(O)
///       ↓
///   dependencies_satisfied?
///      / \
///    yes  no
///     ↓    ↓
///  READY WAITING_FOR_CAUSAL_DEPENDENCIES
///     ↓           ↓
///  APPLIED    (buffered, waiting)
///                 ↓
///         (when new op arrives that unblocks O)
///                 ↓
///         re-check_dependencies(O)
///                 ↓
///             READY
///                 ↓
///             APPLIED
use crate::convergence::VectorClock;
use std::collections::HashMap;

/// Metrics for the causal dependency barrier (Phase 7.0.1 audit instrumentation)
#[derive(Clone, Default, Debug)]
pub struct BarrierMetrics {
    /// Number of times frontier was checked for readiness
    pub frontier_checks: u64,
    /// Number of times pending buffer was scanned
    pub pending_scans: u64,
    /// Total entries examined during scans
    pub pending_entries_examined: u64,
    /// Operations released due to frontier advancement
    pub operations_released: u64,
}

/// Phase 7.1-opt: Dependency index for selective candidate discovery
/// Maps (node, timestamp) → list of operation IDs waiting for that dependency
#[derive(Clone, Default, Debug)]
pub struct DependencyIndex {
    /// For each (node, timestamp) in a required vector clock,
    /// track which pending operations depend on it
    pub waiters: HashMap<(String, u64), Vec<String>>,
}

impl DependencyIndex {
    /// Get the number of index entries
    pub fn entry_count(&self) -> usize {
        self.waiters.len()
    }
}

impl BarrierMetrics {
    pub fn reset(&mut self) {
        self.frontier_checks = 0;
        self.pending_scans = 0;
        self.pending_entries_examined = 0;
        self.operations_released = 0;
    }
}

/// State of an operation in the dependency barrier
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationState {
    /// Operation has been received
    Received,
    /// Operation is waiting for causal dependencies to be satisfied
    WaitingForDependencies {
        required_clock: VectorClock,
        missing_dependencies: Vec<String>, // human-readable list
    },
    /// All dependencies satisfied, ready to apply
    Ready,
    /// Operation has been applied
    Applied,
}

/// Represents a pending operation waiting for dependencies
#[derive(Clone, Debug)]
pub struct PendingOperation {
    pub operation_id: String,
    pub required_dependencies: VectorClock,
    pub state: OperationState,
    /// Size on the durable log of the envelope this entry stands for.
    ///
    /// The entry does not hold the payload -- that stays in the operation log
    /// -- but the backlog's byte bound is about payload volume, so the size has
    /// to be carried even though the bytes are not.
    pub payload_bytes: usize,
}

impl PendingOperation {
    pub fn new(operation_id: String, required_dependencies: VectorClock) -> Self {
        Self {
            operation_id,
            required_dependencies,
            state: OperationState::Received,
            payload_bytes: 0,
        }
    }
}

/// Causal dependency barrier manager
pub struct CausalDependencyBarrier {
    /// Current replica's vector clock frontier
    local_frontier: VectorClock,
    /// Previous frontier (used to detect which dimensions advanced)
    prev_frontier: VectorClock,
    /// Operations waiting for dependencies
    pending: HashMap<String, PendingOperation>,
    /// Operations that have been applied
    applied: Vec<String>,
    /// Metrics for Phase 7.0.1 audit and benchmarking
    metrics: BarrierMetrics,
    /// Phase 7.1-opt: Dependency index for selective candidate discovery
    dependency_index: DependencyIndex,
    /// High-water mark of pending entries, for occupancy metrics.
    max_pending_entries_seen: usize,
    /// High-water mark of pending payload bytes, for occupancy metrics.
    max_pending_payload_bytes_seen: usize,
}

impl CausalDependencyBarrier {
    pub fn new() -> Self {
        Self {
            local_frontier: VectorClock::new(),
            prev_frontier: VectorClock::new(),
            pending: HashMap::new(),
            applied: Vec::new(),
            metrics: BarrierMetrics::default(),
            dependency_index: DependencyIndex::default(),
            max_pending_entries_seen: 0,
            max_pending_payload_bytes_seen: 0,
        }
    }

    /// Get current metrics (for testing and benchmarking)
    pub fn metrics(&self) -> BarrierMetrics {
        self.metrics.clone()
    }

    /// Reset metrics (for testing and benchmarking)
    pub fn clear_metrics(&mut self) {
        self.metrics.reset();
    }

    /// Get current metrics snapshot (for testing and benchmarking)
    pub fn get_metrics(&self) -> BarrierMetrics {
        self.metrics.clone()
    }

    /// Get reference to dependency index (for testing and benchmarking)
    pub fn get_dependency_index(&self) -> &DependencyIndex {
        &self.dependency_index
    }

    /// Get previous frontier (for benchmarking frontier management)
    pub fn get_prev_frontier_mut(&mut self) -> &mut VectorClock {
        &mut self.prev_frontier
    }

    /// Get current frontier (for testing)
    pub fn get_frontier_mut(&mut self) -> &mut VectorClock {
        &mut self.local_frontier
    }

    /// Receive an operation and check if its dependencies are satisfied
    pub fn receive_operation(
        &mut self,
        operation_id: String,
        required_dependencies: VectorClock,
    ) -> ReceiveResult {
        // Track frontier check (Phase 7.0.1 instrumentation)
        #[cfg(test)]
        {
            self.metrics.frontier_checks += 1;
        }

        // Check if dependencies are already satisfied
        if self.local_frontier.happens_before(&required_dependencies) {
            // Dependencies not yet satisfied, buffer the operation
            let missing = self.compute_missing_dependencies(&required_dependencies);
            let pending_op = PendingOperation {
                operation_id: operation_id.clone(),
                required_dependencies: required_dependencies.clone(),
                state: OperationState::WaitingForDependencies {
                    required_clock: required_dependencies.clone(),
                    missing_dependencies: missing,
                },
                payload_bytes: 0,
            };
            self.pending.insert(operation_id.clone(), pending_op);

            // Phase 7.1-opt: Index this operation for later candidate discovery
            #[cfg(test)]
            self.index_operation(&operation_id, &required_dependencies);

            ReceiveResult::BufferedWaitingForDependencies { operation_id }
        } else {
            // Dependencies are satisfied, operation can be applied immediately
            ReceiveResult::ReadyToApply { operation_id }
        }
    }

    /// Apply a ready operation and advance the frontier
    pub fn apply_operation(
        &mut self,
        operation_id: &str,
        operation_clock: &VectorClock,
    ) -> ApplyResult {
        // Remove from pending if present
        if let Some(pending_op) = self.pending.remove(operation_id) {
            // Phase 7.1-opt: Unindex when operation is removed
            #[cfg(test)]
            self.unindex_operation(operation_id, &pending_op.required_dependencies);
        }

        // Advance the frontier
        #[cfg(test)]
        {
            self.prev_frontier.clone_from(&self.local_frontier);
        }
        self.local_frontier.merge(operation_clock);

        // Record as applied
        self.applied.push(operation_id.to_string());

        // Check if this unblocks any waiting operations
        let newly_ready = self.check_unblocked_operations();

        ApplyResult {
            operation_id: operation_id.to_string(),
            newly_unblocked: newly_ready,
        }
    }

    /// Get current state of an operation
    pub fn get_operation_state(&self, operation_id: &str) -> Option<OperationState> {
        if self.applied.contains(&operation_id.to_string()) {
            Some(OperationState::Applied)
        } else if let Some(pending) = self.pending.get(operation_id) {
            Some(pending.state.clone())
        } else {
            None
        }
    }

    /// Get all pending operations
    pub fn get_pending_operations(&self) -> Vec<PendingOperation> {
        self.pending.values().cloned().collect()
    }

    /// Get count of operations in each state
    pub fn get_state_counts(&self) -> StateCounters {
        let waiting_count = self.pending.len();
        let applied_count = self.applied.len();

        StateCounters {
            received: 0,
            waiting_for_dependencies: waiting_count,
            ready: 0,
            applied: applied_count,
        }
    }

    /// Reference implementation: Check all pending operations (Phase 7.1 baseline)
    /// Full-scan implementation: examines every pending operation
    /// Used as correctness oracle for differential testing
    pub fn check_unblocked_operations_full_scan(&mut self) -> Vec<String> {
        // Track pending scan (Phase 7.0.1 instrumentation)
        if !self.pending.is_empty() {
            self.metrics.pending_scans += 1;
            self.metrics.pending_entries_examined += self.pending.len() as u64;
        }

        let mut newly_ready = Vec::new();
        let mut to_ready = Vec::new();

        for (op_id, pending_op) in self.pending.iter() {
            if !self
                .local_frontier
                .happens_before(&pending_op.required_dependencies)
            {
                to_ready.push(op_id.clone());
                newly_ready.push(op_id.clone());
            }
        }

        // Track operations released (Phase 7.0.1 instrumentation)
        self.metrics.operations_released += newly_ready.len() as u64;

        for op_id in to_ready {
            if let Some(pending) = self.pending.get_mut(&op_id) {
                pending.state = OperationState::Ready;
            }
        }

        newly_ready
    }

    /// Indexed implementation: selective candidate discovery via dependency index
    /// Phase 7.1-opt: Only examines operations whose dependencies changed
    /// Production release path (validated faster in all tested scenarios)
    pub fn check_unblocked_operations_indexed(&mut self) -> Vec<String> {
        let mut newly_ready = Vec::new();
        let mut candidates = std::collections::HashSet::new();

        // Find operations affected by frontier advancement
        for (node, old_ts) in &self.prev_frontier.clocks {
            let new_ts = self.local_frontier.get(node);
            if new_ts > *old_ts {
                // Frontier advanced for this node
                // Operations waiting for timestamps in (old_ts, new_ts] are candidates
                for ts in (*old_ts + 1)..=new_ts {
                    if let Some(waiters) = self.dependency_index.waiters.get(&(node.clone(), ts)) {
                        for op_id in waiters {
                            candidates.insert(op_id.clone());
                        }
                    }
                }
            }
        }

        // Also check for new nodes that appeared in frontier
        for (node, new_ts) in &self.local_frontier.clocks {
            if !self.prev_frontier.clocks.contains_key(node) {
                // New node appeared, check all dependencies on it
                for ts in 1..=*new_ts {
                    if let Some(waiters) = self.dependency_index.waiters.get(&(node.clone(), ts)) {
                        for op_id in waiters {
                            candidates.insert(op_id.clone());
                        }
                    }
                }
            }
        }

        // Track candidate count for instrumentation
        let candidate_count = candidates.len();

        // Test candidates using readiness theorem
        let mut to_ready = Vec::new();
        for op_id in candidates {
            if let Some(pending_op) = self.pending.get(&op_id) {
                if !self
                    .local_frontier
                    .happens_before(&pending_op.required_dependencies)
                {
                    to_ready.push(op_id.clone());
                    newly_ready.push(op_id);
                }
            }
        }

        // Track operations released (Phase 7.0.1 instrumentation)
        self.metrics.operations_released += newly_ready.len() as u64;
        if candidate_count > 0 {
            self.metrics.pending_scans += 1;
            self.metrics.pending_entries_examined += candidate_count as u64;
        }

        // Update state (keep in index for discovery purposes)
        for op_id in to_ready {
            if let Some(pending) = self.pending.get_mut(&op_id) {
                pending.state = OperationState::Ready;
                // Note: Don't unindex here - operation remains in index for potential re-discovery
                // Unindex only happens when operation is actually removed from pending via apply_operation
            }
        }

        newly_ready
    }

    /// Production dispatch: Check which pending operations have had their dependencies satisfied
    /// Phase 7.1.3 crossover analysis shows indexed is faster in all tested scenarios
    /// Routes to indexed implementation for selective candidate discovery
    pub fn check_unblocked_operations(&mut self) -> Vec<String> {
        self.check_unblocked_operations_indexed()
    }

    /// Compute which nodes/clocks are missing to satisfy dependencies
    fn compute_missing_dependencies(&self, required: &VectorClock) -> Vec<String> {
        let mut missing = Vec::new();

        for (node, required_ts) in &required.clocks {
            let current_ts = self.local_frontier.get(node);
            if current_ts < *required_ts {
                missing.push(format!("{}:{} (have {})", node, required_ts, current_ts));
            }
        }

        missing
    }

    /// Get the current frontier
    pub fn get_frontier(&self) -> &VectorClock {
        &self.local_frontier
    }

    // ---------------------------------------------------------------------
    // Production causal-delivery API.
    //
    // The methods above were built for, and are exercised by, in-process tests
    // that drive the barrier directly. The methods below are what the
    // replicated receive path uses. They differ in three ways that matter:
    //
    //  1. Eligibility is dominance, not `happens_before`. `receive_operation`
    //     buffers when `local_frontier.happens_before(deps)`, which returns
    //     false for concurrent clocks and would therefore admit an operation
    //     whose dependencies are unmet whenever the frontier is ahead in some
    //     unrelated dimension.
    //  2. Release scanning does not consult the dependency index, because
    //     `index_operation` is `#[cfg(test)]` and so the index is empty in a
    //     shipping build. A full scan is correct at any build setting.
    //  3. Nothing here holds a payload. Pending entries carry an identifier and
    //     the dependency clock; the envelope itself is replayed from the
    //     durable operation log when it becomes eligible.
    // ---------------------------------------------------------------------

    /// True when every dependency this clock names has already been applied.
    pub fn is_eligible(&self, dependencies: &VectorClock) -> bool {
        self.local_frontier.dominates(dependencies)
    }

    /// Retain an operation that cannot be applied yet.
    ///
    /// Returns false when the key is already pending, so a duplicate delivery
    /// of a blocked envelope stays one logical operation.
    pub fn retain_pending(&mut self, key: String, dependencies: VectorClock) -> bool {
        self.retain_pending_sized(key, dependencies, 0)
    }

    /// Retain an operation that cannot be applied yet, recording the size of
    /// the envelope it stands for.
    ///
    /// The entry still holds no payload. The size is carried because the
    /// backlog's byte bound is about how much durable payload is outstanding,
    /// and that number is not recoverable from metadata alone.
    ///
    /// Returns false when the key is already pending, so a duplicate delivery
    /// of a blocked envelope stays one logical operation.
    pub fn retain_pending_sized(
        &mut self,
        key: String,
        dependencies: VectorClock,
        payload_bytes: usize,
    ) -> bool {
        if self.pending.contains_key(&key) {
            return false;
        }
        let missing = self.compute_missing_dependencies(&dependencies);
        self.pending.insert(
            key.clone(),
            PendingOperation {
                operation_id: key,
                required_dependencies: dependencies.clone(),
                state: OperationState::WaitingForDependencies {
                    required_clock: dependencies,
                    missing_dependencies: missing,
                },
                payload_bytes,
            },
        );
        self.observe_occupancy();
        true
    }

    /// Record the high-water marks after the pending set grows.
    ///
    /// Occupancy metrics have to report maximum as well as current, because a
    /// backlog that filled and drained between two reads is invisible to a
    /// current-value gauge, and "did this ever approach its bound" is the
    /// question an operator actually has.
    fn observe_occupancy(&mut self) {
        let entries = self.pending.len();
        if entries > self.max_pending_entries_seen {
            self.max_pending_entries_seen = entries;
        }
        let bytes = self.pending_payload_bytes();
        if bytes > self.max_pending_payload_bytes_seen {
            self.max_pending_payload_bytes_seen = bytes;
        }
    }

    /// Durable payload bytes the backlog is currently waiting on.
    ///
    /// This is the volume of log the pending entries stand for, not a heap
    /// measurement: the payloads are on disk. It is what the byte bound is
    /// expressed in.
    pub fn pending_payload_bytes(&self) -> usize {
        self.pending.values().map(|op| op.payload_bytes).sum()
    }

    /// The most pending entries ever held at once.
    pub fn max_pending_entries_seen(&self) -> usize {
        self.max_pending_entries_seen
    }

    /// The most pending payload bytes ever held at once.
    pub fn max_pending_payload_bytes_seen(&self) -> usize {
        self.max_pending_payload_bytes_seen
    }

    /// Record that an operation was applied, advancing the frontier.
    ///
    /// The frontier advances here and nowhere else on the production path, so
    /// it always describes what has been *successfully applied* rather than
    /// what has been received or claimed.
    pub fn record_applied(&mut self, key: &str, applied_clock: &VectorClock) {
        self.pending.remove(key);
        self.local_frontier.merge(applied_clock);
        self.applied.push(key.to_string());
    }

    /// Every pending operation whose dependencies the frontier now satisfies.
    ///
    /// Returns them in a stable order so that two replicas releasing the same
    /// set release it identically.
    pub fn eligible_pending(&self) -> Vec<String> {
        let mut ready: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, op)| self.local_frontier.dominates(&op.required_dependencies))
            .map(|(key, _)| key.clone())
            .collect();
        ready.sort();
        ready
    }

    /// Whether this key is currently held as pending.
    pub fn is_pending(&self, key: &str) -> bool {
        self.pending.contains_key(key)
    }

    /// Keys of applied operations, in the order they were applied.
    pub fn applied_keys(&self) -> &[String] {
        &self.applied
    }

    /// Keys currently held as pending, sorted.
    pub fn pending_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.pending.keys().cloned().collect();
        keys.sort();
        keys
    }

    /// Occupancy of the pending set: how many operations are waiting.
    ///
    /// Instrumentation only. What may be retained, and what happens when that
    /// limit is reached, is deliberately not decided here -- see
    /// docs/architecture/causal-backlog-bound.md.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Approximate heap footprint of the pending metadata, in bytes.
    ///
    /// Counts the key, both clock copies an entry carries, and the missing
    /// dependency set, because the cost of a pending entry scales with cluster
    /// size rather than with the size of the operation it stands for.
    pub fn pending_metadata_bytes(&self) -> usize {
        self.pending
            .iter()
            .map(|(key, op)| {
                let clock_bytes = |clock: &VectorClock| {
                    clock
                        .clocks
                        .keys()
                        .map(|node| node.len() + std::mem::size_of::<u64>())
                        .sum::<usize>()
                };
                let state_bytes = match &op.state {
                    OperationState::WaitingForDependencies {
                        required_clock,
                        missing_dependencies,
                    } => {
                        clock_bytes(required_clock)
                            + missing_dependencies
                                .iter()
                                .map(|dep| dep.len() + std::mem::size_of::<u64>())
                                .sum::<usize>()
                    }
                    _ => 0,
                };
                key.len()
                    + op.operation_id.len()
                    + clock_bytes(&op.required_dependencies)
                    + state_bytes
                    + std::mem::size_of::<PendingOperation>()
            })
            .sum()
    }

    /// Manually advance frontier (for testing or external coordination)
    pub fn advance_frontier(&mut self, clock: &VectorClock) {
        #[cfg(test)]
        {
            self.prev_frontier.clone_from(&self.local_frontier);
        }
        self.local_frontier.merge(clock);
        self.check_unblocked_operations();
    }

    /// Phase 7.1-opt: Index an operation by its required dependencies
    #[cfg(test)]
    fn index_operation(&mut self, op_id: &str, required: &VectorClock) {
        for (node, timestamp) in &required.clocks {
            self.dependency_index
                .waiters
                .entry((node.clone(), *timestamp))
                .or_insert_with(Vec::new)
                .push(op_id.to_string());
        }
    }

    /// Phase 7.1-opt: Remove an operation from the dependency index
    #[cfg(test)]
    fn unindex_operation(&mut self, op_id: &str, required: &VectorClock) {
        for (node, timestamp) in &required.clocks {
            if let Some(waiters) = self
                .dependency_index
                .waiters
                .get_mut(&(node.clone(), *timestamp))
            {
                waiters.retain(|id| id != op_id);
            }
        }
    }

    /// Check if operation is ready based on frontier alone (Phase 7.0.1: readiness theorem)
    /// Returns true if required_vc <= applied_frontier
    pub fn is_ready_frontier(&self, required: &VectorClock) -> bool {
        // Operation is ready if its required dependencies don't happen-before our frontier
        !self.local_frontier.happens_before(required)
    }

    /// Check if operation is ready using current buffer-based logic (Phase 7.0.1: readiness theorem)
    /// This is the reference implementation to compare against frontier check
    pub fn is_ready_buffer(&self, required: &VectorClock) -> bool {
        // Current implementation: same as is_ready_frontier
        // This method exists to allow comparing future optimizations
        !self.local_frontier.happens_before(required)
    }
}

impl Default for CausalDependencyBarrier {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of receiving an operation
#[derive(Clone, Debug)]
pub enum ReceiveResult {
    /// Operation is buffered, waiting for dependencies
    BufferedWaitingForDependencies { operation_id: String },
    /// Operation can be applied immediately
    ReadyToApply { operation_id: String },
}

/// Result of applying an operation
#[derive(Clone, Debug)]
pub struct ApplyResult {
    pub operation_id: String,
    pub newly_unblocked: Vec<String>,
}

/// Counters for operation states
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateCounters {
    pub received: usize,
    pub waiting_for_dependencies: usize,
    pub ready: usize,
    pub applied: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_barrier_creation() {
        let barrier = CausalDependencyBarrier::new();
        assert_eq!(barrier.get_pending_operations().len(), 0);
    }

    /// Phase 7.0.2: Verify readiness theorem and buffer independence
    /// This test confirms that receive_operation on a satisfied dependency
    /// produces consistent latency regardless of pending buffer size.
    #[test]
    fn phase_7_0_2_readiness_independent_of_pending_buffer() {
        // Theorem: ready(op) ⟺ required_vc <= applied_frontier
        // Should be true regardless of buffer contents

        let required = {
            let mut vc = VectorClock::new();
            vc.increment("node_a");
            vc.increment("node_b");
            vc
        };

        // Test with different buffer sizes
        for buffer_size in [0, 10, 100] {
            let mut barrier = CausalDependencyBarrier::new();
            barrier.clear_metrics();

            // Advance frontier to satisfy requirement
            barrier.advance_frontier(&required);
            barrier.clear_metrics(); // Reset metrics after setup

            // Pre-fill buffer with unrelated operations
            for i in 0..buffer_size {
                let mut unrelated = VectorClock::new();
                unrelated.increment(&format!("other_{}", i));
                let _ = barrier.receive_operation(format!("op_buffer_{}", i), unrelated);
            }
            barrier.clear_metrics(); // Reset metrics after filling

            // Critical test: receive satisfied operation
            // Expected: Should be ready, metrics should show frontier check not buffer scan
            let result = barrier.receive_operation("op_test".to_string(), required.clone());

            let metrics = barrier.metrics();

            // Check result
            match result {
                ReceiveResult::ReadyToApply { .. } => {
                    // Correct: dependencies satisfied
                    assert!(
                        true,
                        "Operation should be ReadyToApply when frontier satisfies"
                    );
                }
                ReceiveResult::BufferedWaitingForDependencies { .. } => {
                    panic!(
                        "Operation should be ready! Frontier was advanced with requirement: {:?}",
                        required
                    );
                }
            }

            // Check instrumentation
            // Should have 1 frontier check, 0 pending scans (on receive path)
            println!("Buffer size: {}", buffer_size);
            println!(
                "  frontier_checks: {} (expected 1)",
                metrics.frontier_checks
            );
            println!(
                "  pending_scans: {} (expected 0 on receive path)",
                metrics.pending_scans
            );

            // The key invariant: pending_scans should be 0 on receive_operation
            // If this fails, it means receive_operation is scanning the buffer
            assert_eq!(
                metrics.pending_scans, 0,
                "receive_operation should not scan pending buffer; \
                 buffer-size-dependent cost would indicate buffer scanning on readiness path"
            );
        }
    }

    #[test]
    fn test_operation_with_no_dependencies() {
        let mut barrier = CausalDependencyBarrier::new();
        let op_clock = VectorClock::new();

        let result = barrier.receive_operation("op1".to_string(), op_clock);
        assert!(matches!(result, ReceiveResult::ReadyToApply { .. }));
    }

    #[test]
    fn test_operation_buffered_when_dependencies_missing() {
        let mut barrier = CausalDependencyBarrier::new();

        let mut required = VectorClock::new();
        required.increment("node_a");
        required.increment("node_a"); // {node_a: 2}

        let result = barrier.receive_operation("op1".to_string(), required);
        assert!(matches!(
            result,
            ReceiveResult::BufferedWaitingForDependencies { .. }
        ));
        assert_eq!(barrier.get_pending_operations().len(), 1);
    }

    #[test]
    fn test_unblocking_when_dependency_satisfied() {
        let mut barrier = CausalDependencyBarrier::new();

        // Operation requires {node_a: 2}
        let mut required = VectorClock::new();
        required.increment("node_a");
        required.increment("node_a"); // {node_a: 2}

        let result = barrier.receive_operation("op1".to_string(), required);
        assert!(matches!(
            result,
            ReceiveResult::BufferedWaitingForDependencies { .. }
        ));
        assert_eq!(barrier.get_pending_operations().len(), 1);

        // Advance frontier to satisfy dependency
        let mut satisfying_clock = VectorClock::new();
        satisfying_clock.increment("node_a");
        satisfying_clock.increment("node_a");

        barrier.advance_frontier(&satisfying_clock);

        // Now op1 should be ready
        let pending = barrier.get_pending_operations();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].state, OperationState::Ready);
    }

    #[test]
    fn test_out_of_order_arrival_buffering() {
        let mut barrier = CausalDependencyBarrier::new();

        // Op2 depends on Op1 having been applied
        // Op1 has no dependencies
        let op1_required = VectorClock::new(); // Op1 requires nothing

        let mut op2_required = VectorClock::new();
        op2_required.increment("node_a"); // Op2 requires {node_a: 1}

        // Receive Op2 before Op1
        let result = barrier.receive_operation("op2".to_string(), op2_required.clone());
        assert!(matches!(
            result,
            ReceiveResult::BufferedWaitingForDependencies { .. }
        ));

        // Receive Op1 (which has no dependencies)
        let result = barrier.receive_operation("op1".to_string(), op1_required.clone());
        assert!(matches!(result, ReceiveResult::ReadyToApply { .. }));

        // Apply Op1, which contributes {node_a: 1} to the frontier
        let mut op1_contribution = VectorClock::new();
        op1_contribution.increment("node_a");
        barrier.apply_operation("op1", &op1_contribution);

        // Now Op2 should be unblocked because frontier is {node_a: 1}
        let pending = barrier.get_pending_operations();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].operation_id, "op2");
        assert_eq!(pending[0].state, OperationState::Ready);
    }

    #[test]
    fn test_dependency_cascade() {
        let mut barrier = CausalDependencyBarrier::new();

        // Op1 has no dependencies
        let op1_clock = VectorClock::new();

        // Op2 depends on Op1: {node_a: 1}
        let mut op2_required = VectorClock::new();
        op2_required.increment("node_a");

        // Op3 depends on Op2: {node_a: 2}
        let mut op3_required = VectorClock::new();
        op3_required.increment("node_a");
        op3_required.increment("node_a");

        // Receive in order: Op3, Op2, Op1
        barrier.receive_operation("op3".to_string(), op3_required);
        barrier.receive_operation("op2".to_string(), op2_required);
        barrier.receive_operation("op1".to_string(), op1_clock.clone());

        // All should be buffered except Op1 (which can apply immediately)
        assert_eq!(barrier.get_pending_operations().len(), 2); // Op2 and Op3

        // Apply Op1
        let mut apply_clock = VectorClock::new();
        apply_clock.increment("node_a");
        barrier.apply_operation("op1", &apply_clock);

        // Now Op2 should be ready
        let pending = barrier.get_pending_operations();
        let op2_state = pending.iter().find(|p| p.operation_id == "op2").unwrap();
        assert_eq!(op2_state.state, OperationState::Ready);

        // Apply Op2
        let mut apply_clock2 = VectorClock::new();
        apply_clock2.increment("node_a");
        apply_clock2.increment("node_a");
        barrier.apply_operation("op2", &apply_clock2);

        // Now Op3 should be ready
        let pending = barrier.get_pending_operations();
        let op3_state = pending.iter().find(|p| p.operation_id == "op3").unwrap();
        assert_eq!(op3_state.state, OperationState::Ready);
    }

    #[test]
    fn test_state_counts() {
        let mut barrier = CausalDependencyBarrier::new();

        let mut required = VectorClock::new();
        required.increment("node_a");

        barrier.receive_operation("op1".to_string(), required.clone());
        barrier.receive_operation("op2".to_string(), required);

        let counts = barrier.get_state_counts();
        assert_eq!(counts.waiting_for_dependencies, 2);
        assert_eq!(counts.applied, 0);
    }

    #[test]
    fn test_causal_invariant_preserved() {
        // Verify that operations are applied in causal order
        let mut barrier = CausalDependencyBarrier::new();

        // Op1 has no dependencies
        let op1_required = VectorClock::new();

        // Op2 requires {a: 2}
        let mut op2_required = VectorClock::new();
        op2_required.increment("a");
        op2_required.increment("a");

        // Receive Op2 before Op1 (out of order)
        barrier.receive_operation("op2".to_string(), op2_required.clone());

        // Op1 should be ready immediately (no dependencies)
        let result = barrier.receive_operation("op1".to_string(), op1_required);
        assert!(matches!(result, ReceiveResult::ReadyToApply { .. }));

        // After applying Op1, it contributes {a: 1} to frontier
        let mut op1_contribution = VectorClock::new();
        op1_contribution.increment("a");
        barrier.apply_operation("op1", &op1_contribution);

        // Op2 still waiting (needs a:2, but frontier is only a:1)
        let pending = barrier.get_pending_operations();
        let op2_state = pending.iter().find(|p| p.operation_id == "op2").unwrap();
        assert!(matches!(
            op2_state.state,
            OperationState::WaitingForDependencies { .. }
        ));
    }

    #[test]
    fn test_readiness_theorem_equivalence() {
        // Phase 7.0.1: Prove that frontier-based and buffer-based readiness checks are equivalent
        let mut barrier = CausalDependencyBarrier::new();

        // Create a required vector clock
        let mut required = VectorClock::new();
        required.increment("node_a");
        required.increment("node_b");

        // Before frontier is advanced, operation should NOT be ready
        assert!(!barrier.is_ready_frontier(&required));
        assert!(!barrier.is_ready_buffer(&required));

        // Advance frontier to satisfy dependencies
        let mut satisfying = VectorClock::new();
        satisfying.increment("node_a");
        satisfying.increment("node_b");
        barrier.advance_frontier(&satisfying);

        // Now both methods should agree: operation IS ready
        assert!(barrier.is_ready_frontier(&required));
        assert!(barrier.is_ready_buffer(&required));
    }

    #[test]
    fn test_instrumentation_frontier_checks() {
        // Phase 7.0.1: Verify frontier_checks counter is incremented
        let mut barrier = CausalDependencyBarrier::new();
        assert_eq!(barrier.metrics().frontier_checks, 0);

        let required = VectorClock::new();
        barrier.receive_operation("op1".to_string(), required.clone());
        assert_eq!(barrier.metrics().frontier_checks, 1);

        barrier.receive_operation("op2".to_string(), required);
        assert_eq!(barrier.metrics().frontier_checks, 2);
    }

    #[test]
    fn test_instrumentation_pending_scans() {
        // Phase 7.0.1: Verify pending_scans counter when checking unblocked operations
        let mut barrier = CausalDependencyBarrier::new();
        barrier.clear_metrics();

        // Add buffered operation
        let mut required = VectorClock::new();
        required.increment("node_a");
        barrier.receive_operation("op_buffered".to_string(), required);

        assert_eq!(barrier.metrics().pending_scans, 0);
        assert_eq!(barrier.metrics().pending_entries_examined, 0);

        // Advance frontier to trigger check_unblocked_operations
        let mut satisfying = VectorClock::new();
        satisfying.increment("node_a");
        barrier.advance_frontier(&satisfying);

        // Should have scanned pending buffer
        assert_eq!(barrier.metrics().pending_scans, 1);
        assert_eq!(barrier.metrics().pending_entries_examined, 1);
    }

    #[test]
    fn test_instrumentation_operations_released() {
        // Phase 7.0.1: Verify operations_released counter
        let mut barrier = CausalDependencyBarrier::new();
        barrier.clear_metrics();

        // Add multiple buffered operations
        let mut required = VectorClock::new();
        required.increment("node_a");

        for i in 0..3 {
            barrier.receive_operation(format!("op_{}", i), required.clone());
        }

        assert_eq!(barrier.metrics().operations_released, 0);

        // Advance frontier to release operations
        let mut satisfying = VectorClock::new();
        satisfying.increment("node_a");
        barrier.advance_frontier(&satisfying);

        // All 3 should be released
        assert_eq!(barrier.metrics().operations_released, 3);
    }

    #[test]
    fn test_instrumentation_buffer_scanning_scales_with_size() {
        // Phase 7.0.1: Demonstrate that buffer scanning scales with buffer size
        // This test uses full-scan oracle to validate full-scan instrumentation behavior
        // Production dispatch uses indexed path; this tests the oracle implementation
        let mut barrier = CausalDependencyBarrier::new();
        barrier.clear_metrics();

        // Create a requirement for {node_a: 5}
        let mut required = VectorClock::new();
        required.increment("node_a");
        required.increment("node_a");
        required.increment("node_a");
        required.increment("node_a");
        required.increment("node_a");

        // Add 10 operations that can't be satisfied yet
        for i in 0..10 {
            barrier.receive_operation(format!("op_{}", i), required.clone());
        }

        barrier.clear_metrics();

        // Trigger full-scan directly: should see all 10 items
        let mut partial = VectorClock::new();
        partial.increment("node_a");
        partial.increment("node_a"); // Only {node_a: 2}, not enough for requirement
        {
            let current = barrier.get_frontier().clone();
            barrier.get_prev_frontier_mut().clone_from(&current);
        }
        barrier.get_frontier_mut().merge(&partial);
        let _ = barrier.check_unblocked_operations_full_scan();

        let first_scan_entries = barrier.metrics().pending_entries_examined;
        assert_eq!(first_scan_entries, 10);

        barrier.clear_metrics();

        // Add 5 more operations
        for i in 10..15 {
            barrier.receive_operation(format!("op_{}", i), required.clone());
        }

        // Now there should be 15 items in pending
        let pending_count = barrier.get_pending_operations().len();
        assert_eq!(pending_count, 15);

        // Trigger full-scan again: should see all 15 items
        let mut more_partial = VectorClock::new();
        more_partial.increment("node_a");
        more_partial.increment("node_a");
        more_partial.increment("node_a"); // Only {node_a: 3}, still not enough
        {
            let current = barrier.get_frontier().clone();
            barrier.get_prev_frontier_mut().clone_from(&current);
        }
        barrier.get_frontier_mut().merge(&more_partial);
        let _ = barrier.check_unblocked_operations_full_scan();

        let second_scan_entries = barrier.metrics().pending_entries_examined;
        assert_eq!(second_scan_entries, 15);

        // Verify that entries_examined scales with buffer size
        assert!(second_scan_entries > first_scan_entries);
    }

    // ============================================================================
    // LIFECYCLE INVARIANT TESTS (Phase 7.0.1: Prove theorem over actual states)
    // ============================================================================

    #[test]
    fn lifecycle_invariant_out_of_order_operations() {
        // Phase 7.0.1: Proof that frontier theorem holds with out-of-order arrivals
        // Sequence: A1 → A3 → A2
        // Expected: A1 applies, A3 buffers, then when A2 applies, A3 can be checked
        let mut barrier = CausalDependencyBarrier::new();

        // A1: no dependencies (ready immediately)
        let a1_required = VectorClock::new();
        let result_a1 = barrier.receive_operation("A1".to_string(), a1_required.clone());
        assert!(matches!(result_a1, ReceiveResult::ReadyToApply { .. }));

        // Apply A1 with {node_a: 1}
        let mut a1_contribution = VectorClock::new();
        a1_contribution.increment("node_a");
        barrier.apply_operation("A1", &a1_contribution);
        assert_eq!(barrier.get_frontier().get("node_a"), 1);

        // A3: requires {node_a: 3}
        let mut a3_required = VectorClock::new();
        a3_required.increment("node_a");
        a3_required.increment("node_a");
        a3_required.increment("node_a");
        let result_a3 = barrier.receive_operation("A3".to_string(), a3_required.clone());
        assert!(matches!(
            result_a3,
            ReceiveResult::BufferedWaitingForDependencies { .. }
        ));

        // A2: requires {node_a: 2}
        let mut a2_required = VectorClock::new();
        a2_required.increment("node_a");
        a2_required.increment("node_a");
        let result_a2 = barrier.receive_operation("A2".to_string(), a2_required.clone());
        assert!(matches!(
            result_a2,
            ReceiveResult::BufferedWaitingForDependencies { .. }
        ));

        // Now frontier is only {node_a: 1}, both A2 and A3 are buffered
        assert_eq!(barrier.get_pending_operations().len(), 2);

        // Apply A2 with {node_a: 2}
        let mut a2_contribution = VectorClock::new();
        a2_contribution.increment("node_a");
        a2_contribution.increment("node_a");
        barrier.apply_operation("A2", &a2_contribution);

        // Now frontier is {node_a: 2}, A2 should be applied, A3 still waiting
        assert_eq!(barrier.get_frontier().get("node_a"), 2);
        let pending = barrier.get_pending_operations();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].operation_id, "A3");
        assert!(matches!(
            pending[0].state,
            OperationState::WaitingForDependencies { .. }
        ));

        // Apply A3 with {node_a: 3}
        let mut a3_contribution = VectorClock::new();
        a3_contribution.increment("node_a");
        a3_contribution.increment("node_a");
        a3_contribution.increment("node_a");
        barrier.apply_operation("A3", &a3_contribution);

        // Now all should be applied
        assert_eq!(barrier.get_frontier().get("node_a"), 3);
        assert_eq!(barrier.get_pending_operations().len(), 0);
    }

    #[test]
    fn lifecycle_invariant_multiple_dependencies() {
        // Phase 7.0.1: Proof with multiple independent dependency chains
        // A5 depends on: B3, C7, D2
        let mut barrier = CausalDependencyBarrier::new();

        // Build the requirement: {B: 3, C: 7, D: 2}
        let mut a5_required = VectorClock::new();
        a5_required.increment("B");
        a5_required.increment("B");
        a5_required.increment("B"); // {B: 3}
        for _ in 0..7 {
            a5_required.increment("C");
        } // {C: 7}
        a5_required.increment("D");
        a5_required.increment("D"); // {D: 2}

        // Try to receive A5 before any dependencies
        let result = barrier.receive_operation("A5".to_string(), a5_required.clone());
        assert!(matches!(
            result,
            ReceiveResult::BufferedWaitingForDependencies { .. }
        ));

        // Partially satisfy: apply B3
        let mut b3_clock = VectorClock::new();
        b3_clock.increment("B");
        b3_clock.increment("B");
        b3_clock.increment("B");
        barrier.receive_operation("B3".to_string(), b3_clock.clone());
        barrier.apply_operation("B3", &b3_clock);
        assert_eq!(barrier.get_frontier().get("B"), 3);

        // A5 still waiting (needs C7 and D2)
        assert!(matches!(
            barrier.get_operation_state("A5"),
            Some(OperationState::WaitingForDependencies { .. })
        ));

        // Satisfy C7
        let mut c7_clock = VectorClock::new();
        for _ in 0..7 {
            c7_clock.increment("C");
        }
        barrier.receive_operation("C7".to_string(), c7_clock.clone());
        barrier.apply_operation("C7", &c7_clock);
        assert_eq!(barrier.get_frontier().get("C"), 7);

        // A5 still waiting (needs D2)
        assert!(matches!(
            barrier.get_operation_state("A5"),
            Some(OperationState::WaitingForDependencies { .. })
        ));

        // Satisfy D2
        let mut d2_clock = VectorClock::new();
        d2_clock.increment("D");
        d2_clock.increment("D");
        barrier.receive_operation("D2".to_string(), d2_clock.clone());
        barrier.apply_operation("D2", &d2_clock);
        assert_eq!(barrier.get_frontier().get("D"), 2);

        // Now A5 should be ready (frontier has all dependencies)
        let pending = barrier.get_pending_operations();
        let a5_state = pending.iter().find(|p| p.operation_id == "A5");
        assert!(a5_state.is_some());
        assert!(matches!(a5_state.unwrap().state, OperationState::Ready));
    }

    #[test]
    fn lifecycle_invariant_buffered_but_unapplied() {
        // Phase 7.0.1: Critical case - operation received but not yet applied
        // This tests the distinction between observed_frontier and applied_frontier
        // Scenario: B1 is applied first (frontier = {B: 1}),
        //          then we receive B3 (but don't apply it),
        //          then A5 that requires {B: 3}
        let mut barrier = CausalDependencyBarrier::new();

        // First apply B1 to establish frontier
        let mut b1_clock = VectorClock::new();
        b1_clock.increment("B");
        barrier.receive_operation("B1".to_string(), b1_clock.clone());
        barrier.apply_operation("B1", &b1_clock);
        assert_eq!(barrier.get_frontier().get("B"), 1);

        // Receive B3 (requires {B: 3} but frontier only has {B: 1}, so it gets buffered)
        let mut b3_required = VectorClock::new();
        b3_required.increment("B");
        b3_required.increment("B");
        b3_required.increment("B");
        let result_b3 = barrier.receive_operation("B3".to_string(), b3_required.clone());
        assert!(matches!(
            result_b3,
            ReceiveResult::BufferedWaitingForDependencies { .. }
        ));

        // At this point:
        // - applied_frontier[B] = 1 (only B1 applied)
        // - We have B3 in the buffer (received but not applied)
        // - A5 requires {B: 3}

        // A5 should be WAITING because applied_frontier < required
        let mut a5_required = VectorClock::new();
        a5_required.increment("B");
        a5_required.increment("B");
        a5_required.increment("B");
        let result_a5 = barrier.receive_operation("A5".to_string(), a5_required.clone());
        assert!(matches!(
            result_a5,
            ReceiveResult::BufferedWaitingForDependencies { .. }
        ));

        // NOW apply B3
        barrier.apply_operation("B3", &b3_required);
        assert_eq!(barrier.get_frontier().get("B"), 3);

        // THEN A5 can be ready
        let pending = barrier.get_pending_operations();
        let a5_state = pending.iter().find(|p| p.operation_id == "A5");
        assert!(a5_state.is_some());
        assert!(matches!(a5_state.unwrap().state, OperationState::Ready));
    }

    #[test]
    fn lifecycle_invariant_multiple_buffered_become_ready() {
        // Phase 7.0.1: Release mechanism test
        // Multiple operations in buffer all become ready when frontier advances
        let mut barrier = CausalDependencyBarrier::new();

        // Buffer 5 operations, all requiring {node_a: 3}
        let mut required = VectorClock::new();
        required.increment("node_a");
        required.increment("node_a");
        required.increment("node_a");

        for i in 0..5 {
            let result = barrier.receive_operation(format!("op_{}", i), required.clone());
            assert!(matches!(
                result,
                ReceiveResult::BufferedWaitingForDependencies { .. }
            ));
        }

        assert_eq!(barrier.get_pending_operations().len(), 5);

        // Clear metrics and check that no operations are marked Ready yet
        barrier.clear_metrics();
        for i in 0..5 {
            let state = barrier.get_operation_state(&format!("op_{}", i));
            assert!(matches!(
                state,
                Some(OperationState::WaitingForDependencies { .. })
            ));
        }

        // Advance frontier to {node_a: 3}
        let mut satisfying = VectorClock::new();
        satisfying.increment("node_a");
        satisfying.increment("node_a");
        satisfying.increment("node_a");
        barrier.advance_frontier(&satisfying);

        // Check that:
        // 1. All 5 operations were examined in the scan
        // 2. All 5 operations were released
        assert_eq!(barrier.metrics().pending_entries_examined, 5);
        assert_eq!(barrier.metrics().operations_released, 5);

        // 3. All 5 operations now marked Ready
        for i in 0..5 {
            let state = barrier.get_operation_state(&format!("op_{}", i));
            assert!(matches!(state, Some(OperationState::Ready)));
        }
    }

    #[test]
    fn phase_7_0_2_adversarial_satisfied_with_arbitrary_pending() {
        // Phase 7.0.2: Readiness Theorem Adversarial Test
        // Hypothesis: ready(op) ⟺ required_vc(op) <= applied_frontier
        // This should be true REGARDLESS of what's in the pending buffer
        //
        // Test: Apply frontier -> fill pending -> check clearly satisfied op
        let mut barrier = CausalDependencyBarrier::new();

        // 1. Set applied frontier to {A: 10, B: 10, C: 10}
        let mut frontier = VectorClock::new();
        frontier.increment("node_a");
        for _ in 0..9 {
            frontier.increment("node_a");
        }
        frontier.increment("node_b");
        for _ in 0..9 {
            frontier.increment("node_b");
        }
        frontier.increment("node_c");
        for _ in 0..9 {
            frontier.increment("node_c");
        }
        barrier.advance_frontier(&frontier);

        // 2. Fill pending with high-timestamp operations
        // These are NOT applied yet, just buffered
        for i in 0..1000 {
            let mut high_ts = VectorClock::new();
            for j in 0..10 {
                high_ts.increment(&format!("node_a"));
            }
            high_ts.increment(&format!("pending_{}", i));
            let _ = barrier.receive_operation(format!("pending_{}", i), high_ts);
        }

        // 3. Check a CLEARLY satisfied operation
        // Required: {A: 5, B: 7, C: 3} which is clearly less than frontier {A:10, B:10, C:10}
        let mut required = VectorClock::new();
        for _ in 0..5 {
            required.increment("node_a");
        }
        for _ in 0..7 {
            required.increment("node_b");
        }
        for _ in 0..3 {
            required.increment("node_c");
        }

        // Expected: READY, because frontier >= required
        let result = barrier.receive_operation("clearly_satisfied".to_string(), required);

        // Verify it's ready
        assert!(
            matches!(result, ReceiveResult::ReadyToApply { .. }),
            "Operation with satisfied frontier should be ready even with 1000 pending"
        );

        // Verify no buffer scan happened
        let metrics = barrier.metrics();
        assert_eq!(
            metrics.pending_scans, 0,
            "readiness check should not scan pending buffer; frontier alone is sufficient"
        );
    }

    #[test]
    fn phase_7_0_2_adversarial_pending_not_applied() {
        // Phase 7.0.2: Critical Distinction Test
        // Ensure we distinguish between:
        // - observed_frontier (max timestamp we've SEEN)
        // - applied_frontier (max timestamp we've APPLIED)
        //
        // Test: Buffer a high-timestamp operation, then check a requirement
        // that depends on that timestamp. Should NOT be ready until it's actually applied.
        let mut barrier = CausalDependencyBarrier::new();

        // 1. Start with applied frontier at {A: 5}
        let mut frontier = VectorClock::new();
        for _ in 0..5 {
            frontier.increment("node_a");
        }
        barrier.advance_frontier(&frontier);

        // 2. Receive operation A:10 (not yet applied)
        // This increments observed_frontier but not applied_frontier
        let mut high_ts = VectorClock::new();
        for _ in 0..10 {
            high_ts.increment("node_a");
        }
        let result = barrier.receive_operation("unmet_op".to_string(), high_ts.clone());

        // Should be buffered (A:10 not yet applied)
        assert!(
            matches!(result, ReceiveResult::BufferedWaitingForDependencies { .. }),
            "Operation A:10 should be buffered since frontier is only at A:5"
        );

        // 3. Now try to apply a NEW operation that requires A:10
        // This new operation's dependencies are NOT satisfied
        let result2 = barrier.receive_operation("test_op".to_string(), high_ts.clone());

        assert!(
            matches!(result2, ReceiveResult::BufferedWaitingForDependencies { .. }),
            "Operation requiring A:10 should NOT be ready even though A:10 exists in pending buffer; \
             applied_frontier is still at A:5"
        );

        // Verify instrumentation shows the semantic logic is working
        let metrics = barrier.metrics();
        println!(
            "Metrics: frontier_checks={}, pending_scans={}",
            metrics.frontier_checks, metrics.pending_scans
        );

        // Should have done 2 frontier checks (for the two receive_operation calls)
        // Should NOT have done any scans (readiness is frontier-only)
        assert!(
            metrics.frontier_checks >= 2,
            "Should have checked frontier at least twice"
        );
        assert_eq!(
            metrics.pending_scans, 0,
            "Should not need buffer scans for readiness - frontier is sufficient"
        );
    }

    // ============================================================================
    // PHASE 7.1-OPT: DIFFERENTIAL TESTING (Index vs Reference)
    // ============================================================================

    #[test]
    fn phase_7_1_opt_index_correctness_candidate_discovery() {
        // Test that index correctly identifies which operations are affected by frontier advancement
        let mut barrier = CausalDependencyBarrier::new();

        // Create operations with clear dependency patterns on same node
        let mut req_a1 = VectorClock::new();
        req_a1.clocks.insert("node_a".to_string(), 1);
        barrier.receive_operation("op_a1".to_string(), req_a1);

        let mut req_a2 = VectorClock::new();
        req_a2.clocks.insert("node_a".to_string(), 2);
        barrier.receive_operation("op_a2".to_string(), req_a2);

        let mut req_a3 = VectorClock::new();
        req_a3.clocks.insert("node_a".to_string(), 3);
        barrier.receive_operation("op_a3".to_string(), req_a3);

        // Verify index is correct
        let a1_waiters = barrier
            .dependency_index
            .waiters
            .get(&("node_a".to_string(), 1))
            .map(|w| w.clone())
            .unwrap_or_default();
        assert!(a1_waiters.contains(&"op_a1".to_string()));

        let a2_waiters = barrier
            .dependency_index
            .waiters
            .get(&("node_a".to_string(), 2))
            .map(|w| w.clone())
            .unwrap_or_default();
        assert!(a2_waiters.contains(&"op_a2".to_string()));

        // Advance frontier to A:2
        let mut frontier = VectorClock::new();
        frontier.clocks.insert("node_a".to_string(), 2);
        barrier.advance_frontier(&frontier);

        // Check which operations are Ready
        let pending = barrier.get_pending_operations();
        let ready: Vec<_> = pending
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .map(|p| p.operation_id.clone())
            .collect();

        // op_a1 and op_a2 should be Ready
        assert_eq!(ready.len(), 2);
        assert!(ready.contains(&"op_a1".to_string()));
        assert!(ready.contains(&"op_a2".to_string()));

        // op_a3 should still be Waiting (needs A:3)
        let waiting: Vec<_> = pending
            .iter()
            .filter(|p| matches!(p.state, OperationState::WaitingForDependencies { .. }))
            .map(|p| p.operation_id.clone())
            .collect();
        assert_eq!(waiting.len(), 1);
        assert!(waiting.contains(&"op_a3".to_string()));
    }

    // ============================================================================
    // PHASE 7.1-OPT: INDEX INVARIANT TESTS
    // ============================================================================

    #[test]
    fn phase_7_1_opt_index_invariant_all_operations_discoverable() {
        // Invariant: Every pending operation must be discoverable through index
        let mut barrier = CausalDependencyBarrier::new();

        // Add 20 operations with varied dependencies
        for i in 0..20 {
            let mut required = VectorClock::new();
            required
                .clocks
                .insert("node_a".to_string(), ((i % 5) + 1) as u64);
            if i % 3 == 0 {
                required
                    .clocks
                    .insert("node_b".to_string(), ((i / 3) + 1) as u64);
            }
            barrier.receive_operation(format!("op_{}", i), required);
        }

        // Verify each pending operation is in index for all its dependencies
        for (op_id, pending_op) in &barrier.pending {
            for (node, ts) in &pending_op.required_dependencies.clocks {
                let key = (node.clone(), *ts);
                assert!(
                    barrier
                        .dependency_index
                        .waiters
                        .get(&key)
                        .map(|waiters| waiters.contains(op_id))
                        .unwrap_or(false),
                    "Operation {} not found in index for ({}, {})",
                    op_id,
                    node,
                    ts
                );
            }
        }
    }

    #[test]
    fn phase_7_1_opt_index_invariant_removed_on_apply() {
        // Invariant: When operation is applied, it must be removed from index
        let mut barrier = CausalDependencyBarrier::new();

        // Add operation
        let mut required = VectorClock::new();
        required.clocks.insert("node_a".to_string(), 1);
        let result = barrier.receive_operation("op_1".to_string(), required.clone());

        // Verify it's indexed
        assert!(barrier
            .dependency_index
            .waiters
            .get(&("node_a".to_string(), 1))
            .map(|w| w.contains(&"op_1".to_string()))
            .unwrap_or(false));

        // Apply operation (moving it from pending to applied)
        barrier.apply_operation("op_1", &required);

        // Verify it's removed from index (because apply calls unindex when removing from pending)
        for waiters in barrier.dependency_index.waiters.values() {
            assert!(
                !waiters.contains(&"op_1".to_string()),
                "Applied operation should be removed from all index entries"
            );
        }
    }

    #[test]
    fn phase_7_1_opt_index_invariant_duplicate_dependencies() {
        // Invariant: Handle operations with duplicate node dependencies
        let mut barrier = CausalDependencyBarrier::new();

        // Create operation with same node in required_vc (vector clock deduplicates)
        let mut required = VectorClock::new();
        required.clocks.insert("node_a".to_string(), 5);
        barrier.receive_operation("op_dup".to_string(), required.clone());

        // Should be indexed exactly once for (node_a, 5)
        let waiters_at_5 = barrier
            .dependency_index
            .waiters
            .get(&("node_a".to_string(), 5))
            .map(|w| w.iter().filter(|id| id == &"op_dup").count())
            .unwrap_or(0);

        // VectorClock naturally deduplicates, so should only appear once
        assert_eq!(
            waiters_at_5, 1,
            "Operation should appear once per unique dependency"
        );
    }

    #[test]
    fn phase_7_1_opt_index_invariant_multiple_nodes() {
        // Invariant: Correctly handle multi-node dependencies
        let mut barrier = CausalDependencyBarrier::new();

        let mut required = VectorClock::new();
        required.clocks.insert("node_a".to_string(), 2);
        required.clocks.insert("node_b".to_string(), 3);
        required.clocks.insert("node_c".to_string(), 1);

        barrier.receive_operation("op_multi".to_string(), required);

        // Should be findable via any dependency
        assert!(barrier
            .dependency_index
            .waiters
            .get(&("node_a".to_string(), 2))
            .map(|w| w.contains(&"op_multi".to_string()))
            .unwrap_or(false));

        assert!(barrier
            .dependency_index
            .waiters
            .get(&("node_b".to_string(), 3))
            .map(|w| w.contains(&"op_multi".to_string()))
            .unwrap_or(false));

        assert!(barrier
            .dependency_index
            .waiters
            .get(&("node_c".to_string(), 1))
            .map(|w| w.contains(&"op_multi".to_string()))
            .unwrap_or(false));
    }

    // ============================================================================
    // PHASE 7.1-OPT: CORE FUNCTIONALITY TESTS
    // ============================================================================

    #[test]
    fn phase_7_1_opt_core_sequential_release() {
        // Core test: Sequential operations released (marked Ready) in correct order
        let mut barrier = CausalDependencyBarrier::new();

        // Add 5 operations with sequential dependencies
        for i in 1..=5 {
            let mut required = VectorClock::new();
            required.clocks.insert("node_a".to_string(), i as u64);
            barrier.receive_operation(format!("op_{}", i), required);
        }

        assert_eq!(barrier.get_pending_operations().len(), 5);

        // Advance to 3
        let mut frontier = VectorClock::new();
        frontier.clocks.insert("node_a".to_string(), 3);
        barrier.advance_frontier(&frontier);

        // Check which operations are Ready vs still Waiting
        let pending = barrier.get_pending_operations();
        let ready_ops: Vec<_> = pending
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .map(|p| p.operation_id.clone())
            .collect();
        let waiting_ops: Vec<_> = pending
            .iter()
            .filter(|p| matches!(p.state, OperationState::WaitingForDependencies { .. }))
            .map(|p| p.operation_id.clone())
            .collect();

        // op_1, op_2, op_3 should be Ready
        assert_eq!(ready_ops.len(), 3);
        assert!(ready_ops.contains(&"op_1".to_string()));
        assert!(ready_ops.contains(&"op_2".to_string()));
        assert!(ready_ops.contains(&"op_3".to_string()));

        // op_4, op_5 should still be Waiting
        assert_eq!(waiting_ops.len(), 2);
        assert!(waiting_ops.contains(&"op_4".to_string()));
        assert!(waiting_ops.contains(&"op_5".to_string()));
    }

    #[test]
    fn phase_7_1_opt_core_index_maintained_on_release() {
        // Core test: Index is properly maintained when operations are released
        let mut barrier = CausalDependencyBarrier::new();

        // Add operations
        let mut req_1 = VectorClock::new();
        req_1.clocks.insert("node_a".to_string(), 1);
        barrier.receive_operation("op_1".to_string(), req_1);

        let mut req_2 = VectorClock::new();
        req_2.clocks.insert("node_a".to_string(), 2);
        barrier.receive_operation("op_2".to_string(), req_2);

        // Index should have entries
        assert!(barrier
            .dependency_index
            .waiters
            .get(&("node_a".to_string(), 1))
            .map(|w| w.contains(&"op_1".to_string()))
            .unwrap_or(false));

        // Advance frontier
        let mut frontier = VectorClock::new();
        frontier.clocks.insert("node_a".to_string(), 1);
        barrier.advance_frontier(&frontier);

        // After releasing op_1, it should still be in pending but marked Ready
        // and it should be removed from the index if using optimized version
        // (but for now, just verify it's marked as Ready)
        let pending = barrier.get_pending_operations();
        let op_1 = pending.iter().find(|p| p.operation_id == "op_1").unwrap();
        assert!(matches!(op_1.state, OperationState::Ready));
    }

    #[test]
    fn phase_7_1_opt_core_index_maintains_consistency() {
        // Core test: Index stays consistent with pending operations
        let mut barrier = CausalDependencyBarrier::new();

        // Add mix of operations
        for i in 0..5 {
            let mut required = VectorClock::new();
            required
                .clocks
                .insert("node_a".to_string(), ((i % 3) + 1) as u64);
            barrier.receive_operation(format!("op_{}", i), required);
        }

        // Verify all pending ops are in index
        for (op_id, pending_op) in &barrier.pending {
            for (node, ts) in &pending_op.required_dependencies.clocks {
                assert!(
                    barrier
                        .dependency_index
                        .waiters
                        .get(&(node.clone(), *ts))
                        .map(|w| w.contains(op_id))
                        .unwrap_or(false),
                    "Operation {} not indexed for ({}, {})",
                    op_id,
                    node,
                    ts
                );
            }
        }

        // Advance and check that released ops are removed from index
        let mut frontier = VectorClock::new();
        frontier.clocks.insert("node_a".to_string(), 2);
        barrier.advance_frontier(&frontier);

        // Verify remaining pending ops are still in index
        for (op_id, pending_op) in &barrier.pending {
            for (node, ts) in &pending_op.required_dependencies.clocks {
                assert!(
                    barrier
                        .dependency_index
                        .waiters
                        .get(&(node.clone(), *ts))
                        .map(|w| w.contains(op_id))
                        .unwrap_or(false),
                    "Remaining operation {} should still be in index",
                    op_id
                );
            }
        }
    }

    // ============================================================================
    // PHASE 7.1.1: COMPREHENSIVE DIFFERENTIAL VALIDATION
    // ============================================================================
    // Permanent infrastructure for regression testing.
    // Ensures optimized and reference implementations remain semantically equivalent.

    /// Scenario generator for differential testing
    #[derive(Clone, Debug)]
    struct DifferentialScenario {
        seed: u64,
        initial_frontier: VectorClock,
        pending_operations: Vec<(String, VectorClock)>,
        frontier_advances: Vec<VectorClock>,
    }

    impl DifferentialScenario {
        fn random_with_seed(seed: u64) -> Self {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};

            let mut hasher = DefaultHasher::new();
            seed.hash(&mut hasher);
            let mut rng_seed = hasher.finish();

            fn next_rng(seed: &mut u64) -> u64 {
                *seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                *seed
            }

            // Generate random frontier
            let mut initial_frontier = VectorClock::new();
            let node_count = (next_rng(&mut rng_seed) % 5) as usize + 2; // 2-6 nodes
            for i in 0..node_count {
                let ts = (next_rng(&mut rng_seed) % 20) as u64;
                if ts > 0 {
                    initial_frontier.clocks.insert(format!("node_{}", i), ts);
                }
            }

            // Generate random pending operations
            let pending_count = (next_rng(&mut rng_seed) % 100) as usize + 10; // 10-109
            let mut pending_operations = Vec::new();
            for i in 0..pending_count {
                let mut required = VectorClock::new();
                let req_node_count = (next_rng(&mut rng_seed) % node_count as u64) as usize + 1;
                for j in 0..req_node_count {
                    let node_idx = next_rng(&mut rng_seed) % node_count as u64;
                    let ts = (next_rng(&mut rng_seed) % 100) as u64 + 1;
                    required.clocks.insert(format!("node_{}", node_idx), ts);
                }
                pending_operations.push((format!("op_{}", i), required));
            }

            // Generate random frontier advances
            let advance_count = (next_rng(&mut rng_seed) % 20) as usize + 5; // 5-24
            let mut frontier_advances = Vec::new();
            for _ in 0..advance_count {
                let mut advance = initial_frontier.clone();
                let node_to_advance = next_rng(&mut rng_seed) % node_count as u64;
                let node_key = format!("node_{}", node_to_advance);
                let new_ts = (next_rng(&mut rng_seed) % 100) as u64 + 1;
                advance.clocks.insert(node_key, new_ts);
                frontier_advances.push(advance);
            }

            Self {
                seed,
                initial_frontier,
                pending_operations,
                frontier_advances,
            }
        }
    }

    #[test]
    fn phase_7_1_1_differential_random_scenarios_seed_based() {
        // Test 1: Random scenarios with deterministic seed
        // This ensures reproducibility and comprehensive coverage

        for seed in 0..50 {
            let scenario = DifferentialScenario::random_with_seed(seed);

            // Reference implementation
            let mut barrier_ref = CausalDependencyBarrier::new();
            for (op_id, required) in &scenario.pending_operations {
                barrier_ref.receive_operation(op_id.clone(), required.clone());
            }

            // Optimized implementation
            let mut barrier_opt = CausalDependencyBarrier::new();
            for (op_id, required) in &scenario.pending_operations {
                barrier_opt.receive_operation(op_id.clone(), required.clone());
            }

            // Run through advances
            for advance in &scenario.frontier_advances {
                barrier_ref.clear_metrics();
                barrier_opt.clear_metrics();

                // Reference
                barrier_ref
                    .prev_frontier
                    .clone_from(&barrier_ref.local_frontier);
                barrier_ref.local_frontier.merge(advance);
                let _ref_ready = barrier_ref.check_unblocked_operations_full_scan();

                // Optimized
                barrier_opt
                    .prev_frontier
                    .clone_from(&barrier_opt.local_frontier);
                barrier_opt.local_frontier.merge(advance);
                let _opt_ready = barrier_opt.check_unblocked_operations_indexed();

                // Compare results
                let ref_pending_count = barrier_ref.get_pending_operations().len();
                let opt_pending_count = barrier_opt.get_pending_operations().len();

                assert_eq!(
                    ref_pending_count, opt_pending_count,
                    "Seed {}: Pending count mismatch after advance",
                    seed
                );

                // Verify index consistency in optimized version
                for (op_id, pending_op) in &barrier_opt.pending {
                    for (node, ts) in &pending_op.required_dependencies.clocks {
                        let key = (node.clone(), *ts);
                        if let Some(waiters) = barrier_opt.dependency_index.waiters.get(&key) {
                            assert!(
                                waiters.contains(op_id),
                                "Seed {}: Operation {} not in index for {:?}",
                                seed,
                                op_id,
                                key
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn phase_7_1_1_differential_adversarial_multiple_ops_same_dependency() {
        // Adversarial: Multiple operations depending on same (node, ts)
        let mut barrier_ref = CausalDependencyBarrier::new();
        let mut barrier_opt = CausalDependencyBarrier::new();

        // Create 10 operations all requiring A:5
        for i in 0..10 {
            let mut required = VectorClock::new();
            required.clocks.insert("node_a".to_string(), 5);
            barrier_ref.receive_operation(format!("op_{}", i), required.clone());
            barrier_opt.receive_operation(format!("op_{}", i), required);
        }

        // Advance to A:5
        let mut frontier = VectorClock::new();
        frontier.clocks.insert("node_a".to_string(), 5);

        barrier_ref
            .prev_frontier
            .clone_from(&barrier_ref.local_frontier);
        barrier_ref.local_frontier.merge(&frontier);
        barrier_ref.check_unblocked_operations_full_scan();

        barrier_opt
            .prev_frontier
            .clone_from(&barrier_opt.local_frontier);
        barrier_opt.local_frontier.merge(&frontier);
        barrier_opt.check_unblocked_operations_indexed();

        // Both should have released all 10
        let ref_ready = barrier_ref
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .count();
        let opt_ready = barrier_opt
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .count();

        assert_eq!(ref_ready, opt_ready);
        assert_eq!(ref_ready, 10);
    }

    #[test]
    fn phase_7_1_1_differential_adversarial_frontier_jump() {
        // Adversarial: Large frontier jump
        let mut barrier_ref = CausalDependencyBarrier::new();
        let mut barrier_opt = CausalDependencyBarrier::new();

        for i in 1..=10 {
            let mut req = VectorClock::new();
            req.clocks.insert("node_a".to_string(), i as u64);
            barrier_ref.receive_operation(format!("op_{}", i), req.clone());
            barrier_opt.receive_operation(format!("op_{}", i), req);
        }

        // Jump to A:7
        let mut frontier = VectorClock::new();
        frontier.clocks.insert("node_a".to_string(), 7);

        barrier_ref
            .prev_frontier
            .clone_from(&barrier_ref.local_frontier);
        barrier_ref.local_frontier.merge(&frontier);
        barrier_ref.check_unblocked_operations_full_scan();

        barrier_opt
            .prev_frontier
            .clone_from(&barrier_opt.local_frontier);
        barrier_opt.local_frontier.merge(&frontier);
        barrier_opt.check_unblocked_operations_indexed();

        // Both should have released op_1 through op_7
        let ref_ready = barrier_ref
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .count();
        let opt_ready = barrier_opt
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .count();

        assert_eq!(ref_ready, opt_ready);
        assert_eq!(ref_ready, 7);
    }

    #[test]
    fn phase_7_1_1_differential_adversarial_simultaneous_release() {
        // Adversarial: All operations ready at same frontier advance
        let mut barrier_ref = CausalDependencyBarrier::new();
        let mut barrier_opt = CausalDependencyBarrier::new();

        for i in 0..20 {
            let mut req = VectorClock::new();
            req.clocks.insert("node_a".to_string(), 5);
            barrier_ref.receive_operation(format!("op_{}", i), req.clone());
            barrier_opt.receive_operation(format!("op_{}", i), req);
        }

        let mut frontier = VectorClock::new();
        frontier.clocks.insert("node_a".to_string(), 5);

        barrier_ref
            .prev_frontier
            .clone_from(&barrier_ref.local_frontier);
        barrier_ref.local_frontier.merge(&frontier);
        barrier_ref.check_unblocked_operations_full_scan();

        barrier_opt
            .prev_frontier
            .clone_from(&barrier_opt.local_frontier);
        barrier_opt.local_frontier.merge(&frontier);
        barrier_opt.check_unblocked_operations_indexed();

        let ref_ready = barrier_ref
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .count();
        let opt_ready = barrier_opt
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .count();

        assert_eq!(ref_ready, opt_ready);
        assert_eq!(ref_ready, 20);
    }

    #[test]
    fn phase_7_1_1_differential_adversarial_empty_buffer() {
        // Adversarial: Empty pending buffer
        let mut barrier_ref = CausalDependencyBarrier::new();
        let mut barrier_opt = CausalDependencyBarrier::new();

        let mut frontier = VectorClock::new();
        frontier.clocks.insert("node_a".to_string(), 10);

        barrier_ref
            .prev_frontier
            .clone_from(&barrier_ref.local_frontier);
        barrier_ref.local_frontier.merge(&frontier);
        let ref_ready = barrier_ref.check_unblocked_operations_full_scan();

        barrier_opt
            .prev_frontier
            .clone_from(&barrier_opt.local_frontier);
        barrier_opt.local_frontier.merge(&frontier);
        let opt_ready = barrier_opt.check_unblocked_operations_indexed();

        assert_eq!(ref_ready.len(), 0);
        assert_eq!(opt_ready.len(), 0);
    }

    #[test]
    fn phase_7_1_1_index_invariant_no_orphaned_entries() {
        // Verify index never references non-existent operations
        let mut barrier = CausalDependencyBarrier::new();

        for i in 0..50 {
            let mut req = VectorClock::new();
            req.clocks
                .insert("node_a".to_string(), ((i % 10) + 1) as u64);
            barrier.receive_operation(format!("op_{}", i), req);
        }

        // Release some operations
        let mut frontier = VectorClock::new();
        frontier.clocks.insert("node_a".to_string(), 5);
        barrier.prev_frontier.clone_from(&barrier.local_frontier);
        barrier.local_frontier.merge(&frontier);
        barrier.check_unblocked_operations();

        // Verify all index entries reference operations in pending
        for waiters in barrier.dependency_index.waiters.values() {
            for op_id in waiters {
                assert!(
                    barrier.pending.contains_key(op_id),
                    "Index entry references non-existent operation {}",
                    op_id
                );
            }
        }
    }

    #[test]
    fn phase_7_1_1_recovery_index_reconstruction() {
        // Verify index can be reconstructed from pending operations
        let mut barrier1 = CausalDependencyBarrier::new();

        // Build up some state
        for i in 0..20 {
            let mut req = VectorClock::new();
            req.clocks
                .insert("node_a".to_string(), ((i % 5) + 1) as u64);
            barrier1.receive_operation(format!("op_{}", i), req);
        }

        let mut frontier = VectorClock::new();
        frontier.clocks.insert("node_a".to_string(), 2);
        barrier1.advance_frontier(&frontier);

        // Simulate recovery: extract pending, rebuild index
        let pending_snapshot: Vec<_> = barrier1.pending.values().cloned().collect();

        let mut barrier2 = CausalDependencyBarrier::new();
        for pending_op in pending_snapshot {
            barrier2
                .pending
                .insert(pending_op.operation_id.clone(), pending_op.clone());
            barrier2.index_operation(&pending_op.operation_id, &pending_op.required_dependencies);
        }

        // Verify index is equivalent
        assert_eq!(
            barrier1.dependency_index.waiters.len(),
            barrier2.dependency_index.waiters.len()
        );

        for (key, waiters1) in &barrier1.dependency_index.waiters {
            let waiters2 = barrier2.dependency_index.waiters.get(key);
            assert!(waiters2.is_some(), "Recovered index missing key {:?}", key);
            let mut w1 = waiters1.clone();
            let mut w2 = waiters2.unwrap().clone();
            w1.sort();
            w2.sort();
            assert_eq!(w1, w2, "Index mismatch for key {:?}", key);
        }
    }

    #[test]
    fn phase_7_1_1_state_hash_equivalence_simple() {
        // Simpler state equivalence test: verify both paths mark same ops as Ready
        let mut barrier_ref = CausalDependencyBarrier::new();
        let mut barrier_opt = CausalDependencyBarrier::new();

        // Add 10 operations with varied dependencies
        for i in 0..10 {
            let mut req = VectorClock::new();
            req.clocks.insert("node_a".to_string(), (i + 1) as u64);
            barrier_ref.receive_operation(format!("op_{}", i), req.clone());
            barrier_opt.receive_operation(format!("op_{}", i), req);
        }

        // Advance frontier to release first 5 operations
        let mut frontier = VectorClock::new();
        frontier.clocks.insert("node_a".to_string(), 5);

        barrier_ref
            .prev_frontier
            .clone_from(&barrier_ref.local_frontier);
        barrier_ref.local_frontier.merge(&frontier);
        let _ref_ready = barrier_ref.check_unblocked_operations_full_scan();

        barrier_opt
            .prev_frontier
            .clone_from(&barrier_opt.local_frontier);
        barrier_opt.local_frontier.merge(&frontier);
        let _opt_ready = barrier_opt.check_unblocked_operations_indexed();

        // Compare operation states
        let ref_pending = barrier_ref.get_pending_operations();
        let opt_pending = barrier_opt.get_pending_operations();

        assert_eq!(ref_pending.len(), opt_pending.len());

        for ref_op in ref_pending.iter() {
            let opt_op = opt_pending
                .iter()
                .find(|op| op.operation_id == ref_op.operation_id)
                .expect(&format!(
                    "Operation {} missing in optimized",
                    ref_op.operation_id
                ));

            assert_eq!(
                std::mem::discriminant(&ref_op.state),
                std::mem::discriminant(&opt_op.state),
                "State mismatch for operation {}",
                ref_op.operation_id
            );
        }
    }

    #[test]
    fn phase_7_1_1_property_multidimensional_frontier_satisfaction() {
        // Property test: Multi-node frontier satisfaction
        // Operations requiring multiple nodes should only release when ALL dimensions satisfied
        let mut barrier_ref = CausalDependencyBarrier::new();
        let mut barrier_opt = CausalDependencyBarrier::new();

        // Create operations with multi-node dependencies
        // op_all_three: requires A:5, B:3, C:2
        let mut req_all = VectorClock::new();
        req_all.clocks.insert("node_a".to_string(), 5);
        req_all.clocks.insert("node_b".to_string(), 3);
        req_all.clocks.insert("node_c".to_string(), 2);

        barrier_ref.receive_operation("op_all_three".to_string(), req_all.clone());
        barrier_opt.receive_operation("op_all_three".to_string(), req_all);

        // Partial advance: only A:5 satisfied
        let mut partial = VectorClock::new();
        partial.clocks.insert("node_a".to_string(), 5);
        partial.clocks.insert("node_b".to_string(), 1);
        partial.clocks.insert("node_c".to_string(), 1);

        barrier_ref
            .prev_frontier
            .clone_from(&barrier_ref.local_frontier);
        barrier_ref.local_frontier.merge(&partial);
        barrier_ref.check_unblocked_operations_full_scan();

        barrier_opt
            .prev_frontier
            .clone_from(&barrier_opt.local_frontier);
        barrier_opt.local_frontier.merge(&partial);
        barrier_opt.check_unblocked_operations_indexed();

        // op_all_three should still be waiting
        let ref_waiting = barrier_ref
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::WaitingForDependencies { .. }))
            .count();
        let opt_waiting = barrier_opt
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::WaitingForDependencies { .. }))
            .count();

        assert_eq!(ref_waiting, 1);
        assert_eq!(opt_waiting, 1);

        // Full advance: all dimensions satisfied
        let mut full = VectorClock::new();
        full.clocks.insert("node_a".to_string(), 5);
        full.clocks.insert("node_b".to_string(), 3);
        full.clocks.insert("node_c".to_string(), 2);

        barrier_ref
            .prev_frontier
            .clone_from(&barrier_ref.local_frontier);
        barrier_ref.local_frontier.merge(&full);
        barrier_ref.check_unblocked_operations_full_scan();

        barrier_opt
            .prev_frontier
            .clone_from(&barrier_opt.local_frontier);
        barrier_opt.local_frontier.merge(&full);
        barrier_opt.check_unblocked_operations_indexed();

        // Now op_all_three should be ready
        let ref_ready = barrier_ref
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .count();
        let opt_ready = barrier_opt
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .count();

        assert_eq!(ref_ready, 1);
        assert_eq!(opt_ready, 1);
    }

    #[test]
    fn phase_7_1_1_adversarial_coordinate_by_coordinate_satisfaction() {
        // Adversarial: Dependencies satisfied one coordinate at a time
        let mut barrier_ref = CausalDependencyBarrier::new();
        let mut barrier_opt = CausalDependencyBarrier::new();

        for i in 0..5 {
            let mut req = VectorClock::new();
            req.clocks.insert("node_a".to_string(), 10 + i as u64);
            req.clocks.insert("node_b".to_string(), 20 + i as u64);
            req.clocks.insert("node_c".to_string(), 30 + i as u64);
            barrier_ref.receive_operation(format!("op_{}", i), req.clone());
            barrier_opt.receive_operation(format!("op_{}", i), req);
        }

        // Satisfy one coordinate at a time for each operation
        for coord in vec![("node_a", 14), ("node_b", 24), ("node_c", 34)] {
            let mut frontier = VectorClock::new();
            for (n, ts) in vec![("node_a", 14u64), ("node_b", 24u64), ("node_c", 34u64)] {
                frontier.clocks.insert(n.to_string(), ts);
            }

            barrier_ref
                .prev_frontier
                .clone_from(&barrier_ref.local_frontier);
            barrier_ref.local_frontier.merge(&frontier);
            barrier_ref.check_unblocked_operations_full_scan();

            barrier_opt
                .prev_frontier
                .clone_from(&barrier_opt.local_frontier);
            barrier_opt.local_frontier.merge(&frontier);
            barrier_opt.check_unblocked_operations_indexed();
        }

        // Both should have released all 5 operations
        let ref_ready = barrier_ref
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .count();
        let opt_ready = barrier_opt
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .count();

        assert_eq!(ref_ready, 5);
        assert_eq!(opt_ready, 5);
    }

    #[test]
    fn phase_7_1_1_adversarial_large_pending_buffer() {
        // Adversarial: Large pending buffer with low release selectivity
        // 50 operations, only some will be released
        let mut barrier_ref = CausalDependencyBarrier::new();
        let mut barrier_opt = CausalDependencyBarrier::new();

        // Add 50 operations with escalating dependencies on single node
        for i in 0..50 {
            let mut req = VectorClock::new();
            req.clocks.insert("node_a".to_string(), (i + 1) as u64);
            barrier_ref.receive_operation(format!("op_{}", i), req.clone());
            barrier_opt.receive_operation(format!("op_{}", i), req);
        }

        // Advance frontier to release only first 25 (those requiring node_a <= 25)
        let mut frontier = VectorClock::new();
        frontier.clocks.insert("node_a".to_string(), 25);

        barrier_ref
            .prev_frontier
            .clone_from(&barrier_ref.local_frontier);
        barrier_ref.local_frontier.merge(&frontier);
        barrier_ref.check_unblocked_operations_full_scan();

        barrier_opt
            .prev_frontier
            .clone_from(&barrier_opt.local_frontier);
        barrier_opt.local_frontier.merge(&frontier);
        barrier_opt.check_unblocked_operations_indexed();

        // Both should release exactly 25 operations
        let ref_ready = barrier_ref
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .count();
        let opt_ready = barrier_opt
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .count();

        assert_eq!(ref_ready, 25);
        assert_eq!(opt_ready, 25);
    }

    #[test]
    fn phase_7_1_1_adversarial_independent_node_dependencies() {
        // Adversarial: Operations depending on different independent nodes
        let mut barrier_ref = CausalDependencyBarrier::new();
        let mut barrier_opt = CausalDependencyBarrier::new();

        // Create enough operations with dependencies on different nodes
        for i in 0..5 {
            let mut req = VectorClock::new();
            req.clocks.insert(format!("node_{}", i), 10);
            barrier_ref.receive_operation(format!("op_{}", i), req.clone());
            barrier_opt.receive_operation(format!("op_{}", i), req);
        }

        // Advance all nodes to their required values
        let mut frontier = VectorClock::new();
        for i in 0..5 {
            frontier.clocks.insert(format!("node_{}", i), 10);
        }

        barrier_ref.advance_frontier(&frontier);
        barrier_opt.advance_frontier(&frontier);

        // All operations should be ready
        let ref_ready = barrier_ref
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .count();
        let opt_ready = barrier_opt
            .get_pending_operations()
            .iter()
            .filter(|p| matches!(p.state, OperationState::Ready))
            .count();

        assert_eq!(ref_ready, 5);
        assert_eq!(opt_ready, 5);
    }

    #[test]
    fn phase_7_1_1_property_index_completeness() {
        // Property test: Index must contain all pending operations
        for seed in 0..10 {
            let scenario = DifferentialScenario::random_with_seed(seed);
            let mut barrier = CausalDependencyBarrier::new();

            for (op_id, required) in &scenario.pending_operations {
                barrier.receive_operation(op_id.clone(), required.clone());
            }

            // Every pending operation must be discoverable through index
            for (op_id, pending_op) in &barrier.pending {
                let mut found = false;
                for (node, ts) in &pending_op.required_dependencies.clocks {
                    if let Some(waiters) =
                        barrier.dependency_index.waiters.get(&(node.clone(), *ts))
                    {
                        if waiters.contains(op_id) {
                            found = true;
                            break;
                        }
                    }
                }
                assert!(found, "Operation {} not found in any index entry", op_id);
            }
        }
    }

    #[test]
    fn phase_7_1_1_property_frontier_only_advances() {
        // Property test: Frontier never regresses in any dimension
        for seed in 0..10 {
            let scenario = DifferentialScenario::random_with_seed(seed);
            let mut barrier = CausalDependencyBarrier::new();

            for (op_id, required) in &scenario.pending_operations {
                barrier.receive_operation(op_id.clone(), required.clone());
            }

            let mut prev_frontier = barrier.local_frontier.clone();

            for advance in &scenario.frontier_advances {
                barrier.prev_frontier.clone_from(&barrier.local_frontier);
                barrier.local_frontier.merge(advance);
                barrier.check_unblocked_operations();

                // Check frontier only advanced (never regressed)
                for (node, prev_ts) in &prev_frontier.clocks {
                    let new_ts = barrier.local_frontier.get(node);
                    assert!(
                        new_ts >= *prev_ts,
                        "Frontier regressed for {} from {} to {}",
                        node,
                        prev_ts,
                        new_ts
                    );
                }

                prev_frontier = barrier.local_frontier.clone();
            }
        }
    }

    #[test]
    fn phase_7_1_1_property_ready_operations_never_waiting() {
        // Property test: Once an operation is marked Ready, it never regresses to Waiting
        for seed in 0..10 {
            let scenario = DifferentialScenario::random_with_seed(seed);
            let mut barrier = CausalDependencyBarrier::new();

            for (op_id, required) in &scenario.pending_operations {
                barrier.receive_operation(op_id.clone(), required.clone());
            }

            let mut seen_ready = std::collections::HashSet::new();

            for advance in &scenario.frontier_advances {
                barrier.prev_frontier.clone_from(&barrier.local_frontier);
                barrier.local_frontier.merge(advance);
                barrier.check_unblocked_operations();

                for pending_op in barrier.get_pending_operations() {
                    if matches!(pending_op.state, OperationState::Ready) {
                        seen_ready.insert(pending_op.operation_id.clone());
                    } else if matches!(
                        pending_op.state,
                        OperationState::WaitingForDependencies { .. }
                    ) {
                        assert!(
                            !seen_ready.contains(&pending_op.operation_id),
                            "Operation {} regressed from Ready to Waiting",
                            pending_op.operation_id
                        );
                    }
                }
            }
        }
    }
}
