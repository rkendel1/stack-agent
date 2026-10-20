/// Crash Recovery Tests
///
/// Comprehensive tests verifying that the transaction system can recover from
/// partial failures and crashes. Tests scenarios where operations are logged but
/// transitions not persisted, and vice versa.
use crate::convergence::VectorClock;
use crate::operation_log::MemoryOperationLog;
use crate::state_transition_store::MemoryStateTransitionStore;
use crate::transactions::*;
use std::collections::HashMap;

#[cfg(test)]
mod crash_recovery {
    use super::*;

    fn create_test_operation(
        op_id: u64,
        parent_version: StateVersion,
        record_id: &str,
        value: i32,
    ) -> Operation {
        Operation::new(
            OperationId::new("node1".to_string(), op_id),
            parent_version,
            "Update".to_string(),
            OperationCommand {
                op_type: "UpdateField".to_string(),
                collection: "test".to_string(),
                record_id: record_id.to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("value".to_string(), serde_json::json!(value));
                    m
                },
            },
            "user".to_string(),
        )
    }

    fn create_version(sequence: u64, hash: &str) -> StateVersion {
        let mut vc = VectorClock::new();
        for _ in 0..sequence {
            vc.increment("node1");
        }
        StateVersion::new(vc, hash.to_string())
    }

    #[test]
    fn test_recover_from_operation_log_single_operation() {
        let log = MemoryOperationLog::new();
        let base_version = create_version(1, "hash0");

        let op = create_test_operation(1, base_version, "rec1", 100);
        let op_id = op.operation_id.clone();

        log.append(op).unwrap();

        assert_eq!(log.size(), 1);
        let recovered = log.get_by_id(&op_id).unwrap();
        assert_eq!(recovered.operation.intent, "Update");
    }

    #[test]
    fn test_recover_from_operation_log_multiple_operations() {
        let log = MemoryOperationLog::new();
        let base_version = create_version(1, "hash0");

        let op_ids: Vec<_> = (1..=5)
            .map(|i| {
                let op = create_test_operation(
                    i,
                    base_version.clone(),
                    &format!("rec{}", i),
                    i as i32 * 10,
                );
                let op_id = op.operation_id.clone();
                log.append(op).unwrap();
                op_id
            })
            .collect();

        assert_eq!(log.size(), 5);
        for (i, op_id) in op_ids.iter().enumerate() {
            assert!(log.get_by_id(op_id).is_some());
            let entry = log.get_by_index(i as u64).unwrap();
            assert_eq!(entry.log_index, i as u64);
        }
    }

    #[test]
    fn test_partial_write_recovery_operation_logged_only() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();
        let base_version = create_version(1, "hash0");

        let op = create_test_operation(1, base_version, "rec1", 100);
        let op_id = op.operation_id.clone();

        log.append(op.clone()).unwrap();

        assert_eq!(log.size(), 1);
        assert_eq!(store.size(), 0);

        assert!(log.contains_operation_id(&op_id));
        assert!(!store.contains_transaction_id("txn1"));
    }

    #[test]
    fn test_state_transition_chain_recovery() {
        let store = MemoryStateTransitionStore::new();

        let mut versions = vec![create_version(0, "hash0")];
        for i in 1..=3 {
            versions.push(create_version(i as u64, &format!("hash{}", i)));
        }

        for i in 0..3 {
            let ops = vec![create_test_operation(
                (i + 1) as u64,
                versions[i].clone(),
                &format!("rec{}", i),
                (i as i32) * 10,
            )];

            let transition = StateTransition::new(
                versions[i].clone(),
                ops,
                versions[i + 1].clone(),
                ConsistencyContract::local(),
            );

            store.record_transition(transition).unwrap();
        }

        assert_eq!(store.size(), 3);

        let chain = store.get_transition_chain("hash0");
        assert_eq!(chain.len(), 3);

        assert_eq!(chain[0].transition.from_version.state_hash, "hash0");
        assert_eq!(chain[0].transition.to_version.state_hash, "hash1");

        assert_eq!(chain[2].transition.from_version.state_hash, "hash2");
        assert_eq!(chain[2].transition.to_version.state_hash, "hash3");
    }

    #[test]
    fn test_divergence_recovery_concurrent_operations() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();

        let base_version = create_version(1, "hash0");

        let ops_seq1: Vec<_> = (1..=3)
            .map(|i| create_test_operation(i, base_version.clone(), "rec1", i as i32 * 10))
            .collect();

        let ops_seq2: Vec<_> = (4..=6)
            .map(|i| create_test_operation(i, base_version.clone(), "rec2", i as i32 * 10))
            .collect();

        for op in ops_seq1.iter().chain(ops_seq2.iter()) {
            log.append(op.clone()).unwrap();
        }

        assert_eq!(log.size(), 6);

        for op in ops_seq1 {
            log.append(op.clone()).unwrap_err();
        }

        assert_eq!(log.size(), 6);
    }

    #[test]
    fn test_recovery_point_identification() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();

        let base_version = create_version(1, "hash0");

        for i in 1..=10 {
            let op =
                create_test_operation(i, base_version.clone(), &format!("rec{}", i), i as i32 * 10);
            log.append(op).unwrap();
        }

        let range = log.get_range(5, 8);
        assert_eq!(range.len(), 3);

        let store_range = store.get_range(0, 10);
        assert_eq!(store_range.len(), 0);
    }

    #[test]
    fn test_exactly_once_recovery() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();

        let base_version = create_version(1, "hash0");
        let op_id = OperationId::new("node1".to_string(), 1);
        let op = create_test_operation(1, base_version.clone(), "rec1", 100);

        log.append(op.clone()).unwrap();

        assert!(log.contains_operation_id(&op_id));

        let duplicate_result = log.append(op);
        assert!(duplicate_result.is_err());
        assert!(duplicate_result.unwrap_err().contains("already logged"));
    }

    #[test]
    fn test_state_version_lineage_recovery() {
        let store = MemoryStateTransitionStore::new();

        let v0 = create_version(0, "hash0");
        let v1 = create_version(1, "hash1");
        let v2 = create_version(2, "hash2");
        let v3 = create_version(3, "hash3");

        let ops = vec![create_test_operation(1, v0.clone(), "rec1", 10)];
        let t1 = StateTransition::new(v0.clone(), ops, v1.clone(), ConsistencyContract::local());
        store.record_transition(t1).unwrap();

        let ops = vec![create_test_operation(2, v1.clone(), "rec2", 20)];
        let t2 = StateTransition::new(v1.clone(), ops, v2.clone(), ConsistencyContract::local());
        store.record_transition(t2).unwrap();

        let ops = vec![create_test_operation(3, v2.clone(), "rec3", 30)];
        let t3 = StateTransition::new(v2, ops, v3, ConsistencyContract::local());
        store.record_transition(t3).unwrap();

        let chain = store.get_transition_chain("hash0");
        assert_eq!(chain.len(), 3);

        let mut prev_hash = "hash0".to_string();
        for record in chain {
            assert_eq!(record.transition.from_version.state_hash, prev_hash);
            prev_hash = record.transition.to_version.state_hash.clone();
        }
        assert_eq!(prev_hash, "hash3");
    }

    #[test]
    fn test_partial_crash_operation_logged_transition_pending() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();

        let base_version = create_version(1, "hash0");
        let to_version = create_version(2, "hash1");

        let op = create_test_operation(1, base_version.clone(), "rec1", 100);
        let op_id = op.operation_id.clone();

        log.append(op.clone()).unwrap();

        assert_eq!(log.size(), 1);
        assert_eq!(store.size(), 0);

        assert!(log.contains_operation_id(&op_id));

        let ops = vec![create_test_operation(2, base_version.clone(), "rec2", 200)];
        let transition =
            StateTransition::new(base_version, ops, to_version, ConsistencyContract::local());

        store.record_transition(transition).unwrap();
        assert_eq!(store.size(), 1);
    }

    #[test]
    fn test_recovery_completeness_check() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();

        let base_version = create_version(1, "hash0");

        for i in 1..=5 {
            let op =
                create_test_operation(i, base_version.clone(), &format!("rec{}", i), i as i32 * 10);
            log.append(op.clone()).unwrap();

            let to_version = create_version(i as u64 + 1, &format!("hash{}", i));
            let transition = StateTransition::new(
                base_version.clone(),
                vec![op],
                to_version,
                ConsistencyContract::local(),
            );
            store.record_transition(transition).unwrap();
        }

        assert_eq!(log.size(), 5);
        assert_eq!(store.size(), 5);

        for i in 1..=5 {
            let op_id = OperationId::new("node1".to_string(), i);
            assert!(log.contains_operation_id(&op_id));
        }
    }

    #[test]
    fn test_truncation_recovery_point() {
        let log = MemoryOperationLog::new();

        let base_version = create_version(1, "hash0");

        for i in 1..=10 {
            let op =
                create_test_operation(i, base_version.clone(), &format!("rec{}", i), i as i32 * 10);
            log.append(op).unwrap();
        }

        assert_eq!(log.size(), 10);

        log.truncate(5).unwrap();
        assert_eq!(log.size(), 5);

        for i in 1..=5 {
            let op_id = OperationId::new("node1".to_string(), i);
            assert!(log.contains_operation_id(&op_id));
        }

        for i in 6..=10 {
            let op_id = OperationId::new("node1".to_string(), i);
            assert!(!log.contains_operation_id(&op_id));
        }
    }

    #[test]
    fn test_concurrent_crash_scenarios() {
        let log = MemoryOperationLog::new();
        let store = MemoryStateTransitionStore::new();

        let base_version = create_version(1, "hash0");

        let op1 = create_test_operation(1, base_version.clone(), "rec1", 100);
        let op2 = create_test_operation(2, base_version.clone(), "rec2", 200);

        log.append(op1.clone()).unwrap();

        let to_version1 = create_version(2, "hash1");
        let transition1 = StateTransition::new(
            base_version.clone(),
            vec![op1],
            to_version1,
            ConsistencyContract::local(),
        );
        store.record_transition(transition1).unwrap();

        log.append(op2.clone()).unwrap();

        let to_version2 = create_version(2, "hash2");
        let transition2 = StateTransition::new(
            base_version,
            vec![op2],
            to_version2,
            ConsistencyContract::local(),
        );
        store.record_transition(transition2).unwrap();

        assert_eq!(log.size(), 2);
        assert_eq!(store.size(), 2);
    }
}
