/// Multi-Node Convergence Testing
///
/// Tests whether independent nodes executing the same operations
/// (in potentially different orders) converge to identical state_hash.
///
/// This is the core Gate 1 test: does operation-specific merge
/// semantics guarantee convergence despite arbitrary delivery order?
use crate::operation_log::MemoryOperationLog;
use crate::permutation_scheduler::{OperationSchedule, PermutationScheduler};
use crate::replay_fuzzing::ReplayFuzzer;
use crate::state_hash::StateHash;
use crate::state_transition_store::MemoryStateTransitionStore;
use crate::transactions::Operation;
use std::collections::HashMap;

/// Result of executing operations on a single simulated node
#[derive(Clone, Debug)]
pub struct NodeExecutionResult {
    pub node_id: String,
    pub schedule_strategy: String,
    pub final_state_hash: StateHash,
    pub operations_applied: u64,
}

/// Comparison of convergence across multiple nodes
#[derive(Clone, Debug)]
pub struct ConvergenceResult {
    pub all_converged: bool,
    pub canonical_hash: StateHash,
    pub node_results: Vec<NodeExecutionResult>,
    pub divergent_nodes: Vec<(String, StateHash)>,
}

impl ConvergenceResult {
    pub fn summary(&self) -> String {
        if self.all_converged {
            format!(
                "✓ Convergence successful: {} nodes → {}",
                self.node_results.len(),
                self.canonical_hash
            )
        } else {
            let divergent_str = self
                .divergent_nodes
                .iter()
                .map(|(node, hash)| format!("{}:{}", node, hash))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "✗ Convergence failed: {} canonical, divergent: [{}]",
                self.canonical_hash, divergent_str
            )
        }
    }
}

/// Multi-node convergence simulator
pub struct MultiNodeConvergenceSimulator {
    node_count: usize,
}

impl MultiNodeConvergenceSimulator {
    pub fn new(node_count: usize) -> Self {
        Self { node_count }
    }

    /// Execute operations on a single simulated node
    fn execute_on_node(schedule: &OperationSchedule, node_id: &str) -> NodeExecutionResult {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();

        // Append operations to log in the schedule order
        for op in &schedule.operations {
            let _ = log.append(op.clone());
        }

        // Execute via replay fuzzer (deterministic replay)
        let fuzzer = ReplayFuzzer::new(log, store);
        let result = fuzzer.replay_normal();

        NodeExecutionResult {
            node_id: node_id.to_string(),
            schedule_strategy: schedule.id(),
            final_state_hash: result.final_state_hash,
            operations_applied: result.operations_replayed,
        }
    }

    /// Execute same operations across multiple nodes with different schedules
    pub fn converge_with_schedules(&self, schedules: Vec<OperationSchedule>) -> ConvergenceResult {
        let mut results = Vec::new();

        for (idx, schedule) in schedules.iter().enumerate() {
            let node_id = format!("node{}", idx);
            let result = Self::execute_on_node(schedule, &node_id);
            results.push(result);
        }

        // Extract unique hashes
        let hashes: Vec<_> = results.iter().map(|r| r.final_state_hash.clone()).collect();
        let unique_hashes: std::collections::HashSet<_> = hashes.iter().cloned().collect();

        let (all_converged, canonical_hash, divergent_nodes) = if unique_hashes.len() == 1 {
            let canonical = results[0].final_state_hash.clone();
            (true, canonical, Vec::new())
        } else {
            // Multiple different hashes = divergence
            let canonical = results[0].final_state_hash.clone();
            let divergent: Vec<(String, StateHash)> = results
                .iter()
                .skip(1)
                .filter(|r| r.final_state_hash != canonical)
                .map(|r| (r.node_id.clone(), r.final_state_hash.clone()))
                .collect();
            (false, canonical, divergent)
        };

        ConvergenceResult {
            all_converged,
            canonical_hash,
            node_results: results,
            divergent_nodes,
        }
    }

    /// Test convergence with all deterministic schedules
    pub fn test_deterministic_convergence(&self, operations: &[Operation]) -> ConvergenceResult {
        let schedules = PermutationScheduler::deterministic_schedules(operations);
        self.converge_with_schedules(schedules)
    }

    /// Test convergence with minimal permutations (fast)
    pub fn test_minimal_convergence(&self, operations: &[Operation]) -> ConvergenceResult {
        let schedules = PermutationScheduler::minimal_schedules(operations);
        self.converge_with_schedules(schedules)
    }

    /// Test convergence with comprehensive permutations
    pub fn test_comprehensive_convergence(&self, operations: &[Operation]) -> ConvergenceResult {
        let schedules = PermutationScheduler::comprehensive_schedules(operations, 50);
        self.converge_with_schedules(schedules)
    }

    /// Test convergence with thorough permutations
    pub fn test_thorough_convergence(&self, operations: &[Operation]) -> ConvergenceResult {
        let schedules = PermutationScheduler::thorough_schedules(operations);
        self.converge_with_schedules(schedules)
    }

    /// Aggregate convergence results across multiple test runs
    pub fn aggregate_convergence_runs(
        &self,
        operations: &[Operation],
        run_count: usize,
    ) -> ConvergenceAggregation {
        let mut run_results = Vec::new();
        let mut all_passed = true;

        for _ in 0..run_count {
            let result = self.test_minimal_convergence(operations);
            if !result.all_converged {
                all_passed = false;
            }
            run_results.push(result);
        }

        ConvergenceAggregation {
            all_runs_passed: all_passed,
            run_results,
            total_operations_tested: operations.len(),
        }
    }
}

/// Aggregated results from multiple convergence test runs
#[derive(Clone, Debug)]
pub struct ConvergenceAggregation {
    pub all_runs_passed: bool,
    pub run_results: Vec<ConvergenceResult>,
    pub total_operations_tested: usize,
}

impl ConvergenceAggregation {
    pub fn summary(&self) -> String {
        let passed = self.run_results.iter().filter(|r| r.all_converged).count();
        let total = self.run_results.len();

        format!(
            "Convergence Test Summary: {}/{} runs passed, {} operations tested",
            passed, total, self.total_operations_tested
        )
    }

    pub fn all_passed(&self) -> bool {
        self.all_runs_passed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::VectorClock;
    use crate::transactions::{OperationCommand, OperationId, StateVersion};
    use std::collections::HashMap;

    fn create_test_operation(seq: u64, collection: &str, record_id: &str, value: i32) -> Operation {
        let mut vc = VectorClock::new();
        vc.increment("test_node");
        let version = StateVersion::new(vc, "test_hash".to_string());

        Operation::new(
            OperationId::new("test_node".to_string(), seq),
            version,
            format!("Op{}", seq),
            OperationCommand {
                op_type: "UpdateField".to_string(),
                collection: collection.to_string(),
                record_id: record_id.to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("value".to_string(), serde_json::json!(value));
                    m
                },
            },
            "test".to_string(),
        )
    }

    #[test]
    fn test_simulator_creation() {
        let simulator = MultiNodeConvergenceSimulator::new(3);
        assert_eq!(simulator.node_count, 3);
    }

    #[test]
    fn test_single_operation_convergence() {
        let ops = vec![create_test_operation(1, "records", "rec1", 10)];

        let simulator = MultiNodeConvergenceSimulator::new(3);
        let result = simulator.test_deterministic_convergence(&ops);

        assert!(result.all_converged);
        assert_eq!(result.node_results.len(), 3); // original, reversed, sorted
    }

    #[test]
    fn test_multiple_operations_convergence() {
        let ops = vec![
            create_test_operation(1, "records", "rec1", 10),
            create_test_operation(2, "records", "rec2", 20),
            create_test_operation(3, "records", "rec3", 30),
        ];

        let simulator = MultiNodeConvergenceSimulator::new(3);
        let result = simulator.test_minimal_convergence(&ops);

        // All permutations should produce same hash
        assert!(result.all_converged);
        assert!(result.node_results.len() > 1);
    }

    #[test]
    fn test_convergence_result_summary() {
        let ops = vec![create_test_operation(1, "records", "rec1", 10)];

        let simulator = MultiNodeConvergenceSimulator::new(3);
        let result = simulator.test_deterministic_convergence(&ops);

        let summary = result.summary();
        assert!(summary.contains("Convergence"));
    }

    #[test]
    fn test_aggregation_summary() {
        let ops = vec![
            create_test_operation(1, "records", "rec1", 10),
            create_test_operation(2, "records", "rec2", 20),
        ];

        let simulator = MultiNodeConvergenceSimulator::new(3);
        let agg = simulator.aggregate_convergence_runs(&ops, 3);

        let summary = agg.summary();
        assert!(summary.contains("Summary"));
        assert_eq!(agg.total_operations_tested, 2);
    }

    #[test]
    fn test_deterministic_vs_minimal_vs_comprehensive() {
        let ops = vec![
            create_test_operation(1, "records", "rec1", 10),
            create_test_operation(2, "records", "rec2", 20),
        ];

        let simulator = MultiNodeConvergenceSimulator::new(3);

        let det = simulator.test_deterministic_convergence(&ops);
        let min = simulator.test_minimal_convergence(&ops);
        let comp = simulator.test_comprehensive_convergence(&ops);

        // deterministic should have fewer results than minimal/comprehensive
        assert!(det.node_results.len() <= min.node_results.len());
        assert!(min.node_results.len() <= comp.node_results.len());

        // All should converge
        assert!(det.all_converged);
        assert!(min.all_converged);
        assert!(comp.all_converged);
    }
}
