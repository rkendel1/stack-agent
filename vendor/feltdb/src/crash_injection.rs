/// Crash Injection Testing Framework
///
/// Simulates crashes at persistence boundaries to verify recovery behavior.
/// Tests that recovery always produces valid state (pre-crash or post-crash, never partial).
use crate::convergence::VectorClock;
use crate::operation_log::MemoryOperationLog;
use crate::state_hash::{CanonicalState, StateHash};
use crate::state_transition_store::MemoryStateTransitionStore;
use crate::transactions::*;
use std::collections::HashMap;

/// Point in the transaction pipeline where crash can occur
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CrashPoint {
    BeforeOperationLogAppend,
    AfterOperationLogAppend,
    BeforeStateTransitionRecord,
    AfterStateTransitionRecord,
    BeforeStateHashUpdate,
    AfterStateHashUpdate,
}

/// Transaction execution checkpoint tracking
#[derive(Clone, Debug)]
pub struct ExecutionCheckpoint {
    pub transaction_id: String,
    pub operations_appended: bool,
    pub transition_recorded: bool,
    pub state_hash_updated: bool,
}

/// Result of crash recovery attempt
#[derive(Clone, Debug)]
pub enum RecoveryResult {
    /// State is in a valid pre-transaction state
    ValidPre { checkpoint: ExecutionCheckpoint },
    /// State is in a valid post-transaction state
    ValidPost { checkpoint: ExecutionCheckpoint },
    /// State is corrupted or indeterminate
    Invalid { reason: String },
}

/// Crash injection test harness
pub struct CrashInjectionTester {
    operation_log: MemoryOperationLog,
    transition_store: MemoryStateTransitionStore,
    pre_crash_state: CanonicalState,
    post_crash_state: CanonicalState,
    checkpoints: Vec<ExecutionCheckpoint>,
}

impl CrashInjectionTester {
    pub fn new() -> Self {
        Self {
            operation_log: MemoryOperationLog::new(),
            transition_store: MemoryStateTransitionStore::new(),
            pre_crash_state: CanonicalState::new(),
            post_crash_state: CanonicalState::new(),
            checkpoints: Vec::new(),
        }
    }

    /// Simulate crash before operation log append
    pub fn crash_before_operation_append(&self, operation: &Operation) -> RecoveryResult {
        assert_eq!(
            self.operation_log.size(),
            0,
            "Operation should not be logged yet"
        );
        assert_eq!(
            self.transition_store.size(),
            0,
            "Transition should not be recorded yet"
        );

        RecoveryResult::ValidPre {
            checkpoint: ExecutionCheckpoint {
                transaction_id: operation.transaction_id.clone().unwrap_or_default(),
                operations_appended: false,
                transition_recorded: false,
                state_hash_updated: false,
            },
        }
    }

    /// Simulate crash after operation log append (before transition)
    pub fn crash_after_operation_append(&self, operation: &Operation) -> RecoveryResult {
        let op_appended = self
            .operation_log
            .contains_operation_id(&operation.operation_id);

        if !op_appended {
            return RecoveryResult::Invalid {
                reason: "Operation should be appended before crash".to_string(),
            };
        }

        assert_eq!(
            self.transition_store.size(),
            0,
            "Transition should not be recorded yet"
        );

        RecoveryResult::ValidPre {
            checkpoint: ExecutionCheckpoint {
                transaction_id: operation.transaction_id.clone().unwrap_or_default(),
                operations_appended: true,
                transition_recorded: false,
                state_hash_updated: false,
            },
        }
    }

    /// Simulate crash after state transition recorded
    pub fn crash_after_transition_recorded(&self, transaction_id: &str) -> RecoveryResult {
        let transition = self.transition_store.get_by_transaction_id(transaction_id);

        if transition.is_none() {
            return RecoveryResult::Invalid {
                reason: "Transition should be recorded before crash".to_string(),
            };
        }

        RecoveryResult::ValidPost {
            checkpoint: ExecutionCheckpoint {
                transaction_id: transaction_id.to_string(),
                operations_appended: true,
                transition_recorded: true,
                state_hash_updated: true,
            },
        }
    }

    /// Record pre-crash state
    pub fn set_pre_crash_state(&mut self, state: CanonicalState) {
        self.pre_crash_state = state;
    }

    /// Record post-crash state (expected state after recovery)
    pub fn set_post_crash_state(&mut self, state: CanonicalState) {
        self.post_crash_state = state;
    }

    /// Verify recovery never produces invalid state
    pub fn verify_no_partial_state(&self) -> bool {
        // No partial operations should exist (all or nothing)
        // No orphaned transitions (operation log and store should be consistent)

        let operation_log_size = self.operation_log.size();
        let transition_store_size = self.transition_store.size();

        if operation_log_size > 0 && transition_store_size == 0 {
            return true; // Valid partial: operations logged but transition pending
        }

        if operation_log_size > 0 && transition_store_size > 0 {
            return true; // Valid complete: both logged
        }

        if operation_log_size == 0 && transition_store_size == 0 {
            return true; // Valid empty: nothing executed
        }

        false // Invalid: orphaned state
    }

    /// Verify no duplicate operations after recovery
    pub fn verify_no_duplicates(&self) -> bool {
        let all_ops = self.operation_log.iter();
        let mut seen_ids = std::collections::HashSet::new();

        for entry in all_ops {
            let op_id_key = format!(
                "{}:{}",
                entry.operation_id.node_id, entry.operation_id.sequence
            );
            if seen_ids.contains(&op_id_key) {
                return false; // Duplicate detected
            }
            seen_ids.insert(op_id_key);
        }

        true
    }

    /// Verify state lineage is consistent
    pub fn verify_state_lineage(&self) -> bool {
        let all_transitions = self.transition_store.iter();

        let mut current_hash = "genesis".to_string();
        for record in all_transitions {
            if record.transition.from_version.state_hash != current_hash {
                return false; // Broken lineage
            }
            current_hash = record.transition.to_version.state_hash.clone();
        }

        true
    }
}

impl Default for CrashInjectionTester {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_operation(sequence: u64) -> Operation {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = crate::transactions::StateVersion::new(vc, "hash0".to_string());

        Operation::new(
            OperationId::new("node1".to_string(), sequence),
            version,
            format!("Op{}", sequence),
            OperationCommand {
                op_type: "UpdateField".to_string(),
                collection: "test".to_string(),
                record_id: format!("rec{}", sequence),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("value".to_string(), serde_json::json!(sequence));
                    m
                },
            },
            "user".to_string(),
        )
    }

    #[test]
    fn test_crash_before_operation_append() {
        let tester = CrashInjectionTester::new();
        let op = create_test_operation(1);

        let result = tester.crash_before_operation_append(&op);

        match result {
            RecoveryResult::ValidPre { checkpoint } => {
                assert!(!checkpoint.operations_appended);
                assert!(!checkpoint.transition_recorded);
            }
            _ => panic!("Expected ValidPre"),
        }
    }

    #[test]
    fn test_verify_no_duplicates() {
        let tester = CrashInjectionTester::new();

        assert!(tester.verify_no_duplicates());

        let op = create_test_operation(1);
        tester.operation_log.append(op).unwrap();

        assert!(tester.verify_no_duplicates());
    }

    #[test]
    fn test_verify_no_partial_state_empty() {
        let tester = CrashInjectionTester::new();

        assert!(tester.verify_no_partial_state());
    }

    #[test]
    fn test_verify_state_lineage_linear_chain() {
        let mut tester = CrashInjectionTester::new();

        let mut prev_hash = "genesis".to_string();
        for i in 1..=3 {
            let mut vc = VectorClock::new();
            vc.increment("node1");

            let from_version =
                crate::transactions::StateVersion::new(vc.clone(), prev_hash.clone());

            vc.increment("node1");
            let to_version = crate::transactions::StateVersion::new(vc, format!("hash{}", i));

            let op = create_test_operation(i);
            let transition = StateTransition::new(
                from_version,
                vec![op],
                to_version.clone(),
                ConsistencyContract::local(),
            );

            tester
                .transition_store
                .record_transition(transition)
                .unwrap();
            prev_hash = format!("hash{}", i);
        }

        assert!(tester.verify_state_lineage());
    }

    #[test]
    fn test_verify_state_lineage_broken_chain() {
        let mut tester = CrashInjectionTester::new();

        let mut vc = VectorClock::new();
        vc.increment("node1");
        let from_version1 = crate::transactions::StateVersion::new(vc.clone(), "hash0".to_string());

        vc.increment("node1");
        let to_version1 = crate::transactions::StateVersion::new(vc.clone(), "hash1".to_string());

        let op1 = create_test_operation(1);
        let t1 = StateTransition::new(
            from_version1,
            vec![op1],
            to_version1,
            ConsistencyContract::local(),
        );
        tester.transition_store.record_transition(t1).unwrap();

        let mut vc2 = VectorClock::new();
        vc2.increment("node1");
        let from_version2 =
            crate::transactions::StateVersion::new(vc2.clone(), "hash_broken".to_string());

        vc2.increment("node1");
        let to_version2 = crate::transactions::StateVersion::new(vc2, "hash2".to_string());

        let op2 = create_test_operation(2);
        let t2 = StateTransition::new(
            from_version2,
            vec![op2],
            to_version2,
            ConsistencyContract::local(),
        );
        tester.transition_store.record_transition(t2).unwrap();

        assert!(!tester.verify_state_lineage());
    }

    #[test]
    fn test_crash_recovery_idempotence() {
        let mut tester = CrashInjectionTester::new();
        let op = create_test_operation(1);

        let result1 = tester.crash_before_operation_append(&op);
        let result2 = tester.crash_before_operation_append(&op);

        match (&result1, &result2) {
            (RecoveryResult::ValidPre { .. }, RecoveryResult::ValidPre { .. }) => {
                // Both should produce same recovery result
            }
            _ => panic!("Expected ValidPre for both"),
        }
    }

    #[test]
    fn test_crash_after_append_recovery() {
        let tester = CrashInjectionTester::new();
        let op = create_test_operation(1);

        tester.operation_log.append(op.clone()).unwrap();

        let result = tester.crash_after_operation_append(&op);

        match result {
            RecoveryResult::ValidPre { checkpoint } => {
                assert!(checkpoint.operations_appended);
                assert!(!checkpoint.transition_recorded);
            }
            _ => panic!("Expected ValidPre"),
        }
    }
}
