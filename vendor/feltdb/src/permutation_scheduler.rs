/// Permutation Scheduler for Convergence Testing
///
/// Generates multiple orderings of the same operation set to test
/// whether independent nodes converge to identical state regardless
/// of message delivery order.
use crate::transactions::Operation;
use rand::seq::SliceRandom;
use rand::SeedableRng;

/// Different scheduling strategies for operation ordering
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScheduleStrategy {
    /// Original order (baseline)
    Original,
    /// Reverse order
    Reversed,
    /// Sorted by operation ID
    SortedById,
    /// Random shuffle with fixed seed for reproducibility
    RandomSeed { seed: u64 },
}

/// A specific ordering of operations
#[derive(Clone, Debug)]
pub struct OperationSchedule {
    pub strategy: ScheduleStrategy,
    pub operations: Vec<Operation>,
}

impl OperationSchedule {
    pub fn new(strategy: ScheduleStrategy, operations: Vec<Operation>) -> Self {
        Self {
            strategy,
            operations,
        }
    }

    pub fn id(&self) -> String {
        format!("{:?}", self.strategy)
    }
}

/// Generates diverse operation schedules for convergence testing
pub struct PermutationScheduler;

impl PermutationScheduler {
    /// Generate all deterministic permutations (original, reversed, sorted)
    /// Use this for small operation sets where all permutations are feasible
    pub fn deterministic_schedules(operations: &[Operation]) -> Vec<OperationSchedule> {
        vec![
            OperationSchedule::new(ScheduleStrategy::Original, operations.to_vec()),
            OperationSchedule::new(ScheduleStrategy::Reversed, {
                let mut ops = operations.to_vec();
                ops.reverse();
                ops
            }),
            OperationSchedule::new(ScheduleStrategy::SortedById, {
                let mut ops = operations.to_vec();
                ops.sort_by(|a, b| a.operation_id.sequence.cmp(&b.operation_id.sequence));
                ops
            }),
        ]
    }

    /// Generate N random permutations with different seeds
    /// Use this for large operation sets where exhaustive permutation is infeasible
    pub fn random_schedules(operations: &[Operation], count: usize) -> Vec<OperationSchedule> {
        (0..count)
            .map(|i| {
                let seed = i as u64;
                let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
                let mut ops = operations.to_vec();
                ops.shuffle(&mut rng);

                OperationSchedule::new(ScheduleStrategy::RandomSeed { seed }, ops)
            })
            .collect()
    }

    /// Generate a comprehensive test set:
    /// - All deterministic permutations
    /// - N random permutations
    pub fn comprehensive_schedules(
        operations: &[Operation],
        random_count: usize,
    ) -> Vec<OperationSchedule> {
        let mut schedules = Self::deterministic_schedules(operations);
        schedules.extend(Self::random_schedules(operations, random_count));
        schedules
    }

    /// Generate minimal test set (fast iteration)
    /// - Original
    /// - Reversed
    /// - 5 random permutations
    pub fn minimal_schedules(operations: &[Operation]) -> Vec<OperationSchedule> {
        Self::comprehensive_schedules(operations, 5)
    }

    /// Generate standard test set (good coverage)
    /// - Original
    /// - Reversed
    /// - Sorted
    /// - 50 random permutations
    pub fn standard_schedules(operations: &[Operation]) -> Vec<OperationSchedule> {
        Self::comprehensive_schedules(operations, 50)
    }

    /// Generate thorough test set (exhaustive)
    /// - Original
    /// - Reversed
    /// - Sorted
    /// - 1000 random permutations
    pub fn thorough_schedules(operations: &[Operation]) -> Vec<OperationSchedule> {
        Self::comprehensive_schedules(operations, 1000)
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
    fn test_deterministic_schedules() {
        let ops = vec![
            create_test_operation(1, "records", "rec1", 10),
            create_test_operation(2, "records", "rec2", 20),
            create_test_operation(3, "records", "rec3", 30),
        ];

        let schedules = PermutationScheduler::deterministic_schedules(&ops);
        assert_eq!(schedules.len(), 3); // original, reversed, sorted
    }

    #[test]
    fn test_random_schedules() {
        let ops = vec![
            create_test_operation(1, "records", "rec1", 10),
            create_test_operation(2, "records", "rec2", 20),
        ];

        let schedules = PermutationScheduler::random_schedules(&ops, 10);
        assert_eq!(schedules.len(), 10);

        // All should be RandomSeed variants
        for schedule in &schedules {
            assert!(matches!(
                schedule.strategy,
                ScheduleStrategy::RandomSeed { .. }
            ));
        }
    }

    #[test]
    fn test_random_schedules_deterministic() {
        let ops = vec![
            create_test_operation(1, "records", "rec1", 10),
            create_test_operation(2, "records", "rec2", 20),
        ];

        let schedules1 = PermutationScheduler::random_schedules(&ops, 3);
        let schedules2 = PermutationScheduler::random_schedules(&ops, 3);

        // Same seed should produce same order
        for i in 0..3 {
            let ops1: Vec<u64> = schedules1[i]
                .operations
                .iter()
                .map(|o| o.operation_id.sequence)
                .collect();
            let ops2: Vec<u64> = schedules2[i]
                .operations
                .iter()
                .map(|o| o.operation_id.sequence)
                .collect();
            assert_eq!(ops1, ops2);
        }
    }

    #[test]
    fn test_comprehensive_schedules() {
        let ops = vec![
            create_test_operation(1, "records", "rec1", 10),
            create_test_operation(2, "records", "rec2", 20),
            create_test_operation(3, "records", "rec3", 30),
        ];

        let schedules = PermutationScheduler::comprehensive_schedules(&ops, 5);
        assert_eq!(schedules.len(), 3 + 5); // 3 deterministic + 5 random
    }

    #[test]
    fn test_minimal_schedules() {
        let ops = vec![
            create_test_operation(1, "records", "rec1", 10),
            create_test_operation(2, "records", "rec2", 20),
        ];

        let schedules = PermutationScheduler::minimal_schedules(&ops);
        assert_eq!(schedules.len(), 3 + 5); // deterministic + 5 random
    }

    #[test]
    fn test_schedule_preserves_all_operations() {
        let ops = vec![
            create_test_operation(1, "records", "rec1", 10),
            create_test_operation(2, "records", "rec2", 20),
            create_test_operation(3, "records", "rec3", 30),
        ];

        let schedules = PermutationScheduler::comprehensive_schedules(&ops, 10);

        for schedule in &schedules {
            assert_eq!(schedule.operations.len(), 3);
            let seq: Vec<u64> = schedule
                .operations
                .iter()
                .map(|o| o.operation_id.sequence)
                .collect();
            assert!(seq.contains(&1));
            assert!(seq.contains(&2));
            assert!(seq.contains(&3));
        }
    }

    #[test]
    fn test_original_schedule_preserves_order() {
        let ops = vec![
            create_test_operation(1, "records", "rec1", 10),
            create_test_operation(2, "records", "rec2", 20),
            create_test_operation(3, "records", "rec3", 30),
        ];

        let schedules = PermutationScheduler::deterministic_schedules(&ops);
        let original = &schedules[0];

        let seq: Vec<u64> = original
            .operations
            .iter()
            .map(|o| o.operation_id.sequence)
            .collect();
        assert_eq!(seq, vec![1, 2, 3]);
    }

    #[test]
    fn test_reversed_schedule() {
        let ops = vec![
            create_test_operation(1, "records", "rec1", 10),
            create_test_operation(2, "records", "rec2", 20),
            create_test_operation(3, "records", "rec3", 30),
        ];

        let schedules = PermutationScheduler::deterministic_schedules(&ops);
        let reversed = &schedules[1];

        let seq: Vec<u64> = reversed
            .operations
            .iter()
            .map(|o| o.operation_id.sequence)
            .collect();
        assert_eq!(seq, vec![3, 2, 1]);
    }
}
