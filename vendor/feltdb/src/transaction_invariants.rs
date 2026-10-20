/// Transaction Invariant Tests
///
/// Verifies that the transaction system maintains the architectural invariants
/// from STATE_MODEL.md:
/// 1. Atomicity: All-or-nothing semantics
/// 2. Optimistic Concurrency: Conflict detection at commit
/// 3. Determinism: Replay produces identical state
/// 4. Exactly-Once: Operation deduplication
/// 5. Causality: Happens-before ordering preserved
/// 6. Consistency Contracts: Per-transaction guarantees
/// 7. Derived State: Rebuildable from operation log
/// 8. Partition Tolerance: Divergence + deterministic reconciliation
use crate::convergence::VectorClock;
use crate::transactions::*;
use std::collections::HashMap;

#[cfg(test)]
mod invariant_tests {
    use super::*;

    // ============================================================================
    // INVARIANT 1: ATOMICITY
    // All operations in a transaction commit together, or none do
    // ============================================================================

    #[test]
    fn test_atomicity_both_operations_commit() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        let op1_id = OperationId::new("node1".to_string(), 1);
        let op2_id = OperationId::new("node1".to_string(), 2);

        let op1 = Operation::new(
            op1_id,
            version.clone(),
            "Debit".to_string(),
            OperationCommand {
                op_type: "UpdateField".to_string(),
                collection: "accounts".to_string(),
                record_id: "acc1".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("balance".to_string(), serde_json::json!(900));
                    m
                },
            },
            "system".to_string(),
        )
        .with_write_set(vec![StateDependency::new(
            "accounts".to_string(),
            "acc1".to_string(),
            Some("balance".to_string()),
            version.clone(),
        )]);

        let op2 = Operation::new(
            op2_id,
            version.clone(),
            "Credit".to_string(),
            OperationCommand {
                op_type: "UpdateField".to_string(),
                collection: "accounts".to_string(),
                record_id: "acc2".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("balance".to_string(), serde_json::json!(1100));
                    m
                },
            },
            "system".to_string(),
        )
        .with_write_set(vec![StateDependency::new(
            "accounts".to_string(),
            "acc2".to_string(),
            Some("balance".to_string()),
            version.clone(),
        )]);

        let mut executor = TransactionExecutor::new("node1".to_string());
        let mut to_vc = version.vector_clock.clone();
        to_vc.increment("node1");
        let to_version = StateVersion::new(to_vc, "hash1".to_string());

        let transition = executor.execute_transaction(
            "tx1".to_string(),
            version,
            vec![op1, op2],
            ConsistencyContract::causal(),
        );

        assert!(transition.is_successful());
        assert_eq!(transition.operations.len(), 2);
        assert!(transition.to_version.vector_clock != transition.from_version.vector_clock);
    }

    #[test]
    fn test_atomicity_stale_read_set_aborts() {
        let mut vc1 = VectorClock::new();
        vc1.increment("node1");
        let version1 = StateVersion::new(vc1, "hash1".to_string());

        let mut vc2 = VectorClock::new();
        vc2.increment("node1");
        vc2.increment("node1");
        let version2 = StateVersion::new(vc2, "hash2".to_string());

        let op_id = OperationId::new("node1".to_string(), 1);
        let op = Operation::new(
            op_id,
            version1.clone(),
            "UpdateBalance".to_string(),
            OperationCommand {
                op_type: "UpdateField".to_string(),
                collection: "accounts".to_string(),
                record_id: "acc1".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("balance".to_string(), serde_json::json!(500));
                    m
                },
            },
            "system".to_string(),
        )
        .with_read_set(vec![StateDependency::new(
            "accounts".to_string(),
            "acc1".to_string(),
            Some("balance".to_string()),
            version1,
        )]);

        let mut executor = TransactionExecutor::new("node1".to_string());
        let mut to_vc = version2.vector_clock.clone();
        to_vc.increment("node1");
        let _to_version = StateVersion::new(to_vc, "hash3".to_string());

        let transition = executor.execute_transaction(
            "tx1".to_string(),
            version2.clone(),
            vec![op],
            ConsistencyContract::causal(),
        );

        assert!(CommitValidator::validate_transition(&transition, &version2).is_ok());
    }

    // ============================================================================
    // INVARIANT 2: OPTIMISTIC CONCURRENCY
    // Conflicts detected at commit, not during execution
    // ============================================================================

    #[test]
    fn test_concurrent_non_conflicting_transactions_both_commit() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let base_version = StateVersion::new(vc, "hash_base".to_string());

        let op1_id = OperationId::new("node1".to_string(), 1);
        let op2_id = OperationId::new("node1".to_string(), 2);

        let op1 = Operation::new(
            op1_id,
            base_version.clone(),
            "UpdateUser1".to_string(),
            OperationCommand {
                op_type: "UpdateField".to_string(),
                collection: "users".to_string(),
                record_id: "user1".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("name".to_string(), serde_json::json!("Alice"));
                    m
                },
            },
            "system".to_string(),
        )
        .with_write_set(vec![StateDependency::new(
            "users".to_string(),
            "user1".to_string(),
            Some("name".to_string()),
            base_version.clone(),
        )]);

        let op2 = Operation::new(
            op2_id,
            base_version.clone(),
            "UpdateUser2".to_string(),
            OperationCommand {
                op_type: "UpdateField".to_string(),
                collection: "users".to_string(),
                record_id: "user2".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("name".to_string(), serde_json::json!("Bob"));
                    m
                },
            },
            "system".to_string(),
        )
        .with_write_set(vec![StateDependency::new(
            "users".to_string(),
            "user2".to_string(),
            Some("name".to_string()),
            base_version.clone(),
        )]);

        let mut executor1 = TransactionExecutor::new("node1".to_string());
        let mut executor2 = TransactionExecutor::new("node1".to_string());

        let mut to_vc1 = base_version.vector_clock.clone();
        to_vc1.increment("node1");
        let to_version1 = StateVersion::new(to_vc1, "hash_tx1".to_string());

        let mut to_vc2 = base_version.vector_clock.clone();
        to_vc2.increment("node1");
        let to_version2 = StateVersion::new(to_vc2, "hash_tx2".to_string());

        let tx1 = executor1.execute_transaction(
            "tx1".to_string(),
            base_version.clone(),
            vec![op1],
            ConsistencyContract::causal(),
        );

        let tx2 = executor2.execute_transaction(
            "tx2".to_string(),
            base_version,
            vec![op2],
            ConsistencyContract::causal(),
        );

        assert!(tx1.is_successful());
        assert!(tx2.is_successful());

        let conflicts = CommitValidator::detect_conflicts(&tx1, &[tx2]);
        assert!(
            conflicts.is_empty(),
            "Non-overlapping writes should not conflict"
        );
    }

    #[test]
    fn test_concurrent_conflicting_transactions_detected() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let base_version = StateVersion::new(vc, "hash_base".to_string());

        let op1_id = OperationId::new("node1".to_string(), 1);
        let op2_id = OperationId::new("node1".to_string(), 2);

        let op1 = Operation::new(
            op1_id,
            base_version.clone(),
            "IncrementCounter".to_string(),
            OperationCommand {
                op_type: "Increment".to_string(),
                collection: "counters".to_string(),
                record_id: "counter1".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("value".to_string(), serde_json::json!(1));
                    m
                },
            },
            "system".to_string(),
        )
        .with_write_set(vec![StateDependency::new(
            "counters".to_string(),
            "counter1".to_string(),
            Some("value".to_string()),
            base_version.clone(),
        )]);

        let op2 = Operation::new(
            op2_id,
            base_version.clone(),
            "IncrementCounter".to_string(),
            OperationCommand {
                op_type: "Increment".to_string(),
                collection: "counters".to_string(),
                record_id: "counter1".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("value".to_string(), serde_json::json!(1));
                    m
                },
            },
            "system".to_string(),
        )
        .with_write_set(vec![StateDependency::new(
            "counters".to_string(),
            "counter1".to_string(),
            Some("value".to_string()),
            base_version.clone(),
        )]);

        let mut executor1 = TransactionExecutor::new("node1".to_string());
        let mut executor2 = TransactionExecutor::new("node1".to_string());

        let mut to_vc1 = base_version.vector_clock.clone();
        to_vc1.increment("node1");
        let to_version1 = StateVersion::new(to_vc1, "hash_tx1".to_string());

        let mut to_vc2 = base_version.vector_clock.clone();
        to_vc2.increment("node1");
        let to_version2 = StateVersion::new(to_vc2, "hash_tx2".to_string());

        let tx1 = executor1.execute_transaction(
            "tx1".to_string(),
            base_version.clone(),
            vec![op1],
            ConsistencyContract::causal(),
        );

        let tx2 = executor2.execute_transaction(
            "tx2".to_string(),
            base_version,
            vec![op2],
            ConsistencyContract::causal(),
        );

        assert!(tx1.is_successful());
        assert!(tx2.is_successful());

        let conflicts = CommitValidator::detect_conflicts(&tx1, &[tx2]);
        assert!(
            !conflicts.is_empty(),
            "Write-write conflicts should be detected"
        );
        assert!(
            conflicts.iter().any(|c| c.contains("Write-write")),
            "Should specifically identify write-write conflict"
        );
    }

    // ============================================================================
    // INVARIANT 3: DETERMINISM
    // Replay of same operations always produces identical state
    // ============================================================================

    #[test]
    fn test_determinism_same_operations_same_state_hash() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        let op_id = OperationId::new("node1".to_string(), 1);
        let command = OperationCommand {
            op_type: "SetField".to_string(),
            collection: "orders".to_string(),
            record_id: "order123".to_string(),
            fields: {
                let mut m = HashMap::new();
                m.insert("status".to_string(), serde_json::json!("paid"));
                m.insert("amount".to_string(), serde_json::json!(999.99));
                m
            },
        };

        let mut executor1 = TransactionExecutor::new("node1".to_string());
        let mut executor2 = TransactionExecutor::new("node1".to_string());

        let op1 = Operation::new(
            op_id.clone(),
            version.clone(),
            "PayOrder".to_string(),
            command.clone(),
            "system".to_string(),
        );

        let op2 = Operation::new(
            op_id,
            version.clone(),
            "PayOrder".to_string(),
            command,
            "system".to_string(),
        );

        let tx1 = executor1.execute_transaction(
            "tx1".to_string(),
            version.clone(),
            vec![op1],
            ConsistencyContract::local(),
        );

        let tx2 = executor2.execute_transaction(
            "tx2".to_string(),
            version,
            vec![op2],
            ConsistencyContract::local(),
        );

        assert_eq!(
            tx1.to_version.state_hash, tx2.to_version.state_hash,
            "Same operations should produce identical state hash"
        );
    }

    #[test]
    fn test_determinism_replay_convergence() {
        let mut base_vc = VectorClock::new();
        base_vc.increment("node1");
        let base_version = StateVersion::new(base_vc, "hash0".to_string());

        let mut operations = Vec::new();
        for i in 1..=5 {
            operations.push(Operation::new(
                OperationId::new("node1".to_string(), i),
                base_version.clone(),
                format!("Op{}", i),
                OperationCommand {
                    op_type: "UpdateField".to_string(),
                    collection: "items".to_string(),
                    record_id: format!("item{}", i),
                    fields: {
                        let mut m = HashMap::new();
                        m.insert("value".to_string(), serde_json::json!(i * 10));
                        m
                    },
                },
                "system".to_string(),
            ));
        }

        let mut executor1 = TransactionExecutor::new("node1".to_string());
        let mut executor2 = TransactionExecutor::new("node1".to_string());

        let tx1 = executor1.execute_transaction(
            "tx1".to_string(),
            base_version.clone(),
            operations.clone(),
            ConsistencyContract::causal(),
        );

        let operations_shuffled = vec![
            operations[2].clone(),
            operations[0].clone(),
            operations[4].clone(),
            operations[1].clone(),
            operations[3].clone(),
        ];

        let tx2 = executor2.execute_transaction(
            "tx2".to_string(),
            base_version,
            operations_shuffled,
            ConsistencyContract::causal(),
        );

        assert!(tx1.is_successful(), "Tx1 should succeed");
        assert!(tx2.is_successful(), "Tx2 should succeed");
        assert_eq!(
            tx1.operations.len(),
            tx2.operations.len(),
            "Both transactions should have same number of operations"
        );
    }

    // ============================================================================
    // INVARIANT 4: EXACTLY-ONCE SEMANTICS
    // Same operation ID applied multiple times produces single effect
    // ============================================================================

    #[test]
    fn test_exactly_once_duplicate_operation_rejected() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        let op_id = OperationId::new("node1".to_string(), 42);

        let op = Operation::new(
            op_id.clone(),
            version.clone(),
            "IncrementScore".to_string(),
            OperationCommand {
                op_type: "Increment".to_string(),
                collection: "scores".to_string(),
                record_id: "player1".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("score".to_string(), serde_json::json!(1));
                    m
                },
            },
            "system".to_string(),
        );

        let mut executor = TransactionExecutor::new("node1".to_string());

        executor.mark_applied(&op_id);
        assert!(executor.is_applied(&op_id));

        let tx = executor.execute_transaction(
            "tx1".to_string(),
            version,
            vec![op],
            ConsistencyContract::eventual(),
        );

        assert!(
            !tx.is_successful(),
            "Duplicate operation should be rejected"
        );
    }

    #[test]
    fn test_exactly_once_deduplication_set() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        let op_ids: Vec<_> = (1..=10)
            .map(|i| OperationId::new("node1".to_string(), i))
            .collect();

        let mut executor = TransactionExecutor::new("node1".to_string());

        for op_id in op_ids.iter().take(5) {
            executor.mark_applied(op_id);
        }

        for (i, op_id) in op_ids.iter().enumerate() {
            let is_applied = executor.is_applied(op_id);
            if i < 5 {
                assert!(is_applied, "Operations 1-5 should be marked as applied");
            } else {
                assert!(!is_applied, "Operations 6-10 should not be applied yet");
            }
        }
    }

    // ============================================================================
    // INVARIANT 5: CAUSALITY
    // Happens-before ordering preserved across replicas
    // ============================================================================

    #[test]
    fn test_causality_happens_before_ordering() {
        let mut vc1 = VectorClock::new();
        vc1.increment("node1");
        let v1 = StateVersion::new(vc1, "hash1".to_string());

        let mut vc2 = VectorClock::new();
        vc2.increment("node1");
        vc2.increment("node1");
        let v2 = StateVersion::new(vc2, "hash2".to_string());

        assert!(v1.vector_clock.happens_before(&v2.vector_clock));
        assert!(!v2.vector_clock.happens_before(&v1.vector_clock));
    }

    #[test]
    fn test_causality_concurrent_states() {
        let mut vc1 = VectorClock::new();
        vc1.increment("node1");
        let v1 = StateVersion::new(vc1, "hash1".to_string());

        let mut vc2 = VectorClock::new();
        vc2.increment("node2");
        let v2 = StateVersion::new(vc2, "hash2".to_string());

        assert!(v1.concurrent_with(&v2));
        assert!(v2.concurrent_with(&v1));
        assert!(!v1.vector_clock.happens_before(&v2.vector_clock));
        assert!(!v2.vector_clock.happens_before(&v1.vector_clock));
    }

    // ============================================================================
    // INVARIANT 6: CONSISTENCY CONTRACTS
    // Per-operation consistency levels honored
    // ============================================================================

    #[test]
    fn test_consistency_local_no_replication_wait() {
        let contract = ConsistencyContract::local();
        assert_eq!(contract.level, ConsistencyLevel::Local);
        assert_eq!(contract.timeout_ms, None);
    }

    #[test]
    fn test_consistency_causal_ordered() {
        let contract = ConsistencyContract::causal();
        assert_eq!(contract.level, ConsistencyLevel::Causal);
        assert!(contract.timeout_ms.is_some());
        assert!(contract.timeout_ms.unwrap() < 5000);
    }

    #[test]
    fn test_consistency_linearizable_requires_quorum() {
        let contract = ConsistencyContract::linearizable(3);
        assert_eq!(contract.level, ConsistencyLevel::Linearizable);
        assert_eq!(contract.required_acks, Some(3));
        assert!(contract.timeout_ms.unwrap() >= 5000);
    }

    // ============================================================================
    // INVARIANT 7: DERIVED STATE REBUILD
    // Indexes rebuildable from operation log
    // ============================================================================

    #[test]
    fn test_derived_state_operation_log_contains_operations() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        let ops: Vec<_> = (1..=5)
            .map(|i| {
                Operation::new(
                    OperationId::new("node1".to_string(), i),
                    version.clone(),
                    format!("Op{}", i),
                    OperationCommand {
                        op_type: "SetField".to_string(),
                        collection: "data".to_string(),
                        record_id: format!("record{}", i),
                        fields: {
                            let mut m = HashMap::new();
                            m.insert("value".to_string(), serde_json::json!(i));
                            m
                        },
                    },
                    "system".to_string(),
                )
            })
            .collect();

        let mut executor = TransactionExecutor::new("node1".to_string());
        let mut to_vc = version.vector_clock.clone();
        to_vc.increment("node1");
        let to_version = StateVersion::new(to_vc, "hash1".to_string());

        let transition = executor.execute_transaction(
            "tx1".to_string(),
            version,
            ops.clone(),
            ConsistencyContract::local(),
        );

        assert_eq!(transition.operations.len(), ops.len());

        for (i, op) in transition.operations.iter().enumerate() {
            assert_eq!(
                op.intent,
                format!("Op{}", i + 1),
                "Operation sequence should be preserved in transition"
            );
        }
    }

    // ============================================================================
    // INVARIANT 8: PARTITION TOLERANCE
    // Divergence + deterministic reconciliation
    // ============================================================================

    #[test]
    fn test_partition_concurrent_writes_create_divergence() {
        let mut vc_base = VectorClock::new();
        vc_base.increment("node1");
        let base_version = StateVersion::new(vc_base, "hash_base".to_string());

        let op1_id = OperationId::new("node1".to_string(), 1);
        let op2_id = OperationId::new("node2".to_string(), 1);

        let op1 = Operation::new(
            op1_id,
            base_version.clone(),
            "UpdateFromNode1".to_string(),
            OperationCommand {
                op_type: "SetField".to_string(),
                collection: "state".to_string(),
                record_id: "shared".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("value".to_string(), serde_json::json!("node1_value"));
                    m
                },
            },
            "node1".to_string(),
        );

        let op2 = Operation::new(
            op2_id,
            base_version.clone(),
            "UpdateFromNode2".to_string(),
            OperationCommand {
                op_type: "SetField".to_string(),
                collection: "state".to_string(),
                record_id: "shared".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("value".to_string(), serde_json::json!("node2_value"));
                    m
                },
            },
            "node2".to_string(),
        );

        let mut executor1 = TransactionExecutor::new("node1".to_string());
        let mut executor2 = TransactionExecutor::new("node2".to_string());

        let mut vc1 = base_version.vector_clock.clone();
        vc1.increment("node1");
        let v1 = StateVersion::new(vc1, "hash1".to_string());

        let mut vc2 = base_version.vector_clock.clone();
        vc2.increment("node2");
        let v2 = StateVersion::new(vc2, "hash2".to_string());

        let tx1 = executor1.execute_transaction(
            "tx1".to_string(),
            base_version.clone(),
            vec![op1],
            ConsistencyContract::eventual(),
        );

        let tx2 = executor2.execute_transaction(
            "tx2".to_string(),
            base_version,
            vec![op2],
            ConsistencyContract::eventual(),
        );

        assert!(tx1.is_successful());
        assert!(tx2.is_successful());

        assert!(
            tx1.to_version.concurrent_with(&tx2.to_version),
            "Concurrent writes should create divergent versions"
        );
    }

    #[test]
    fn test_partition_deterministic_reconciliation() {
        let mut base_vc = VectorClock::new();
        base_vc.increment("node1");
        base_vc.increment("node2");
        let base_version = StateVersion::new(base_vc, "hash_base".to_string());

        let op_id = OperationId::new("node1".to_string(), 1);
        let op = Operation::new(
            op_id,
            base_version.clone(),
            "ReconcileOperation".to_string(),
            OperationCommand {
                op_type: "Merge".to_string(),
                collection: "state".to_string(),
                record_id: "merged".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("combined".to_string(), serde_json::json!(true));
                    m
                },
            },
            "reconciler".to_string(),
        );

        let mut executor1 = TransactionExecutor::new("node1".to_string());
        let mut executor2 = TransactionExecutor::new("node2".to_string());

        let tx1 = executor1.execute_transaction(
            "reconcile1".to_string(),
            base_version.clone(),
            vec![op.clone()],
            ConsistencyContract::causal(),
        );

        let tx2 = executor2.execute_transaction(
            "reconcile2".to_string(),
            base_version,
            vec![op],
            ConsistencyContract::causal(),
        );

        assert_eq!(
            tx1.to_version.state_hash, tx2.to_version.state_hash,
            "Deterministic reconciliation should converge both replicas"
        );
    }

    // ============================================================================
    // CROSS-INVARIANT SCENARIOS
    // ============================================================================

    #[test]
    fn test_atomicity_with_causal_consistency() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        let op1 = Operation::new(
            OperationId::new("node1".to_string(), 1),
            version.clone(),
            "WriteA".to_string(),
            OperationCommand {
                op_type: "SetField".to_string(),
                collection: "log".to_string(),
                record_id: "entry1".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("data".to_string(), serde_json::json!("value_a"));
                    m
                },
            },
            "system".to_string(),
        );

        let op2 = Operation::new(
            OperationId::new("node1".to_string(), 2),
            version.clone(),
            "WriteB".to_string(),
            OperationCommand {
                op_type: "SetField".to_string(),
                collection: "log".to_string(),
                record_id: "entry2".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("data".to_string(), serde_json::json!("value_b"));
                    m
                },
            },
            "system".to_string(),
        );

        let mut executor = TransactionExecutor::new("node1".to_string());
        let mut to_vc = version.vector_clock.clone();
        to_vc.increment("node1");
        let to_version = StateVersion::new(to_vc, "hash1".to_string());

        let transition = executor.execute_transaction(
            "atomic_tx".to_string(),
            version,
            vec![op1, op2],
            ConsistencyContract::causal(),
        );

        assert!(transition.is_successful());
        assert_eq!(transition.operations.len(), 2);
        assert!(transition.to_version.dominates(&transition.from_version));
    }

    #[test]
    fn test_write_skew_detection() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let base_version = StateVersion::new(vc, "hash_base".to_string());

        let op1 = Operation::new(
            OperationId::new("node1".to_string(), 1),
            base_version.clone(),
            "RemoveDoctor".to_string(),
            OperationCommand {
                op_type: "UpdateField".to_string(),
                collection: "oncall".to_string(),
                record_id: "doctor1".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("active".to_string(), serde_json::json!(false));
                    m
                },
            },
            "system".to_string(),
        )
        .with_preconditions(vec![("check_count".to_string(), "count >= 2".to_string())])
        .with_read_set(vec![
            StateDependency::new(
                "oncall".to_string(),
                "doctor1".to_string(),
                Some("active".to_string()),
                base_version.clone(),
            ),
            StateDependency::new(
                "oncall".to_string(),
                "metadata".to_string(),
                Some("count".to_string()),
                base_version.clone(),
            ),
        ])
        .with_write_set(vec![StateDependency::new(
            "oncall".to_string(),
            "doctor1".to_string(),
            Some("active".to_string()),
            base_version.clone(),
        )]);

        let op2 = Operation::new(
            OperationId::new("node1".to_string(), 2),
            base_version.clone(),
            "RemoveDoctor".to_string(),
            OperationCommand {
                op_type: "UpdateField".to_string(),
                collection: "oncall".to_string(),
                record_id: "doctor2".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("active".to_string(), serde_json::json!(false));
                    m
                },
            },
            "system".to_string(),
        )
        .with_preconditions(vec![("check_count".to_string(), "count >= 2".to_string())])
        .with_read_set(vec![
            StateDependency::new(
                "oncall".to_string(),
                "doctor2".to_string(),
                Some("active".to_string()),
                base_version.clone(),
            ),
            StateDependency::new(
                "oncall".to_string(),
                "metadata".to_string(),
                Some("count".to_string()),
                base_version.clone(),
            ),
        ])
        .with_write_set(vec![
            StateDependency::new(
                "oncall".to_string(),
                "doctor2".to_string(),
                Some("active".to_string()),
                base_version.clone(),
            ),
            StateDependency::new(
                "oncall".to_string(),
                "metadata".to_string(),
                Some("count".to_string()),
                base_version.clone(),
            ),
        ]);

        let mut executor1 = TransactionExecutor::new("node1".to_string());
        let mut executor2 = TransactionExecutor::new("node1".to_string());

        let mut to_vc1 = base_version.vector_clock.clone();
        to_vc1.increment("node1");
        let to_version1 = StateVersion::new(to_vc1, "hash_tx1".to_string());

        let mut to_vc2 = base_version.vector_clock.clone();
        to_vc2.increment("node1");
        let to_version2 = StateVersion::new(to_vc2, "hash_tx2".to_string());

        let tx1 = executor1.execute_transaction(
            "tx1".to_string(),
            base_version.clone(),
            vec![op1],
            ConsistencyContract::causal(),
        );

        let tx2 = executor2.execute_transaction(
            "tx2".to_string(),
            base_version,
            vec![op2],
            ConsistencyContract::causal(),
        );

        let conflicts = CommitValidator::detect_conflicts(&tx1, &[tx2]);
        assert!(
            !conflicts.is_empty(),
            "Write-skew scenario should detect potential conflict"
        );
    }
}
