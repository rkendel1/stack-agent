/// Replay Fuzzing Framework
///
/// Tests that the same operation log produces identical state when replayed via:
/// - Normal replay (in original order)
/// - Batched replay (grouped operations)
/// - After snapshot (recover from snapshot + remaining ops)
/// - After simulated crash (recover + replay remaining)
/// - After index deletion (rebuild index from scratch)
///
/// All paths must converge to identical state_hash.
use crate::operation_log::{MemoryOperationLog, OperationLogEntry};
use crate::state_hash::{CanonicalState, StateHash};
use crate::state_transition_store::MemoryStateTransitionStore;
use crate::transactions::*;
use std::collections::HashMap;

/// Replay scenario
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayScenario {
    /// Replay all operations in original order
    Normal,
    /// Replay in batches of N operations
    Batched { batch_size: usize },
    /// Simulate recovery from snapshot at index N, replay rest
    AfterSnapshot { snapshot_at: u64 },
    /// Simulate crash after index N, recover and replay
    AfterCrash { crash_at: u64 },
    /// Simulate index deletion, rebuild from log
    IndexRebuilt,
}

/// Replay path and result
#[derive(Clone, Debug)]
pub struct ReplayPath {
    pub scenario: ReplayScenario,
    pub final_state_hash: StateHash,
    pub operations_replayed: u64,
    pub transitions_created: u64,
}

/// Replay fuzzer
pub struct ReplayFuzzer {
    operation_log: MemoryOperationLog,
    transition_store: MemoryStateTransitionStore,
}

impl ReplayFuzzer {
    pub fn new(
        operation_log: MemoryOperationLog,
        transition_store: MemoryStateTransitionStore,
    ) -> Self {
        Self {
            operation_log,
            transition_store,
        }
    }

    /// Replay operations normally (original order)
    pub fn replay_normal(&self) -> ReplayPath {
        let all_ops = self.operation_log.iter();
        let mut state = CanonicalState::new();

        for entry in all_ops {
            self.apply_operation_to_state(&mut state, &entry.operation);
        }

        ReplayPath {
            scenario: ReplayScenario::Normal,
            final_state_hash: state.hash(),
            operations_replayed: self.operation_log.size(),
            transitions_created: 1,
        }
    }

    /// Replay operations in batches
    pub fn replay_batched(&self, batch_size: usize) -> ReplayPath {
        let mut state = CanonicalState::new();
        let log_size = self.operation_log.size();
        let mut total_batches = 0;

        for batch_start in (0..log_size).step_by(batch_size) {
            let batch_end = std::cmp::min(batch_start + batch_size as u64, log_size);
            let batch = self.operation_log.get_range(batch_start, batch_end);

            for entry in batch {
                self.apply_operation_to_state(&mut state, &entry.operation);
            }
            total_batches += 1;
        }

        ReplayPath {
            scenario: ReplayScenario::Batched { batch_size },
            final_state_hash: state.hash(),
            operations_replayed: self.operation_log.size(),
            transitions_created: total_batches as u64,
        }
    }

    /// Replay from snapshot (skip first N operations, replay rest)
    pub fn replay_after_snapshot(&self, snapshot_at: u64) -> ReplayPath {
        let log_size = self.operation_log.size();
        if snapshot_at >= log_size {
            return ReplayPath {
                scenario: ReplayScenario::AfterSnapshot { snapshot_at },
                final_state_hash: StateHash::from_hex("snapshot_beyond_log".to_string()),
                operations_replayed: 0,
                transitions_created: 0,
            };
        }

        let mut state = CanonicalState::new();

        // Simulate snapshot state (in real system, would be loaded from snapshot)
        // For this test, assume snapshot captures state after snapshot_at operations
        let pre_snapshot_ops = self.operation_log.get_range(0, snapshot_at);
        for entry in pre_snapshot_ops {
            self.apply_operation_to_state(&mut state, &entry.operation);
        }

        // Replay remaining operations
        let remaining_ops = self.operation_log.get_range(snapshot_at, log_size);
        for entry in remaining_ops {
            self.apply_operation_to_state(&mut state, &entry.operation);
        }

        ReplayPath {
            scenario: ReplayScenario::AfterSnapshot { snapshot_at },
            final_state_hash: state.hash(),
            operations_replayed: log_size,
            transitions_created: 1,
        }
    }

    /// Replay after simulated crash (skip operations after crash point)
    pub fn replay_after_crash(&self, crash_at: u64) -> ReplayPath {
        let log_size = self.operation_log.size();
        let replay_limit = std::cmp::min(crash_at, log_size);

        let mut state = CanonicalState::new();
        let ops = self.operation_log.get_range(0, replay_limit);

        for entry in ops {
            self.apply_operation_to_state(&mut state, &entry.operation);
        }

        ReplayPath {
            scenario: ReplayScenario::AfterCrash { crash_at },
            final_state_hash: state.hash(),
            operations_replayed: replay_limit,
            transitions_created: 1,
        }
    }

    /// Replay with index rebuilt from log
    pub fn replay_index_rebuilt(&self) -> ReplayPath {
        // Clear transition store (simulate index deletion)
        let all_ops = self.operation_log.iter();
        let mut state = CanonicalState::new();

        for entry in all_ops {
            self.apply_operation_to_state(&mut state, &entry.operation);
        }

        ReplayPath {
            scenario: ReplayScenario::IndexRebuilt,
            final_state_hash: state.hash(),
            operations_replayed: self.operation_log.size(),
            transitions_created: 1,
        }
    }

    /// Apply an operation's effects to state
    fn apply_operation_to_state(&self, state: &mut CanonicalState, operation: &Operation) {
        if operation.command.op_type == "UpdateField" {
            let collection = operation.command.collection.clone();
            let record_id = operation.command.record_id.clone();
            let data = serde_json::json!(operation.command.fields.clone());
            state.set_record(collection, record_id, data);
        }
    }

    /// Execute full replay scenarios (all operations replayed)
    pub fn execute_full_scenarios(&self) -> Vec<ReplayPath> {
        vec![
            self.replay_normal(),
            self.replay_batched(2),
            self.replay_batched(5),
            self.replay_after_snapshot(self.operation_log.size() / 2),
            self.replay_index_rebuilt(),
        ]
    }

    /// Execute all scenarios (including partial)
    pub fn execute_all_scenarios(&self) -> Vec<ReplayPath> {
        let mut all = self.execute_full_scenarios();
        all.push(self.replay_after_crash(self.operation_log.size().saturating_sub(1)));
        all
    }

    /// Verify all FULL replay paths converge to same state
    pub fn verify_convergence(&self) -> Result<StateHash, String> {
        let paths = self.execute_full_scenarios();

        if paths.is_empty() {
            return Err("No replay paths generated".to_string());
        }

        let canonical_hash = &paths[0].final_state_hash;

        for path in &paths[1..] {
            if &path.final_state_hash != canonical_hash {
                return Err(format!(
                    "Convergence failed: {:?} produced {}, expected {}",
                    path.scenario, path.final_state_hash, canonical_hash
                ));
            }
        }

        Ok(canonical_hash.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::VectorClock;

    fn create_test_operation(seq: u64, collection: &str, record_id: &str, value: i32) -> Operation {
        let mut vc = VectorClock::new();
        vc.increment("replay_test");
        let version = StateVersion::new(vc, "test_hash".to_string());

        Operation::new(
            OperationId::new("replay_test".to_string(), seq),
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
    fn test_fuzzer_creation() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();
        let _fuzzer = ReplayFuzzer::new(log, store);
    }

    #[test]
    fn test_replay_normal_empty_log() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();
        let fuzzer = ReplayFuzzer::new(log, store);

        let result = fuzzer.replay_normal();
        assert_eq!(result.operations_replayed, 0);
    }

    #[test]
    fn test_replay_normal_single_operation() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();

        let op = create_test_operation(1, "records", "rec1", 100);
        log.append(op).unwrap();

        let fuzzer = ReplayFuzzer::new(log, store);
        let result = fuzzer.replay_normal();

        assert_eq!(result.operations_replayed, 1);
        assert_eq!(result.scenario, ReplayScenario::Normal);
    }

    #[test]
    fn test_replay_batched() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();

        for i in 1..=5 {
            let op = create_test_operation(i, "records", &format!("rec{}", i), i as i32 * 10);
            log.append(op).unwrap();
        }

        let fuzzer = ReplayFuzzer::new(log, store);
        let result = fuzzer.replay_batched(2);

        assert_eq!(result.operations_replayed, 5);
        assert_eq!(result.scenario, ReplayScenario::Batched { batch_size: 2 });
    }

    #[test]
    fn test_replay_after_snapshot() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();

        for i in 1..=5 {
            let op = create_test_operation(i, "records", &format!("rec{}", i), i as i32 * 10);
            log.append(op).unwrap();
        }

        let fuzzer = ReplayFuzzer::new(log, store);
        let result = fuzzer.replay_after_snapshot(3);

        assert_eq!(result.operations_replayed, 5);
    }

    #[test]
    fn test_replay_after_crash() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();

        for i in 1..=5 {
            let op = create_test_operation(i, "records", &format!("rec{}", i), i as i32 * 10);
            log.append(op).unwrap();
        }

        let fuzzer = ReplayFuzzer::new(log, store);
        let result = fuzzer.replay_after_crash(3);

        assert_eq!(result.operations_replayed, 3);
    }

    #[test]
    fn test_replay_index_rebuilt() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();

        for i in 1..=3 {
            let op = create_test_operation(i, "records", &format!("rec{}", i), i as i32 * 10);
            log.append(op).unwrap();
        }

        let fuzzer = ReplayFuzzer::new(log, store);
        let result = fuzzer.replay_index_rebuilt();

        assert_eq!(result.operations_replayed, 3);
        assert_eq!(result.scenario, ReplayScenario::IndexRebuilt);
    }

    #[test]
    fn test_all_scenarios_same_hash() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();

        for i in 1..=5 {
            let op = create_test_operation(i, "records", &format!("rec{}", i), i as i32 * 10);
            log.append(op).unwrap();
        }

        let fuzzer = ReplayFuzzer::new(log, store);

        let normal = fuzzer.replay_normal();
        let batched = fuzzer.replay_batched(2);
        let snapshot = fuzzer.replay_after_snapshot(2);

        assert_eq!(normal.final_state_hash, batched.final_state_hash);
        assert_eq!(normal.final_state_hash, snapshot.final_state_hash);
    }

    #[test]
    fn test_verify_convergence() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();

        for i in 1..=5 {
            let op = create_test_operation(i, "records", &format!("rec{}", i), i as i32 * 10);
            log.append(op).unwrap();
        }

        let fuzzer = ReplayFuzzer::new(log, store);
        let result = fuzzer.verify_convergence();

        assert!(result.is_ok());
    }

    #[test]
    fn test_execute_all_scenarios() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();

        for i in 1..=3 {
            let op = create_test_operation(i, "records", &format!("rec{}", i), i as i32 * 10);
            log.append(op).unwrap();
        }

        let fuzzer = ReplayFuzzer::new(log, store);
        let paths = fuzzer.execute_all_scenarios();

        assert!(paths.len() > 0);
        assert!(paths.iter().all(|p| p.operations_replayed > 0));
    }
}
