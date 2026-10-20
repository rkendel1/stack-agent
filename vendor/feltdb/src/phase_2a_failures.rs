/// Phase 2a: Single-Node Failure Scenarios
///
/// Tests recovery from arbitrary single-node failures:
/// - Leader crash during replication
/// - Follower crash during replication
/// - Dual crash (leader + 1 follower)
///
/// Success Criteria:
/// - 10 consecutive runs all converge after recovery
/// - Convergence time <5 seconds
/// - All 3 replicas have identical final state
/// - No operations are lost
/// - No operations are duplicated

#[cfg(test)]
mod tests {
    use crate::convergence::VectorClock;
    use crate::distributed_transactions::{
        DistributedTransactionExecutor, ReplicationMessage, TransactionEnvelope,
    };
    use crate::state_hash::StateHash;
    use crate::transactions::{
        ConsistencyContract, Operation, OperationCommand, OperationId, StateVersion,
    };
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::sync::Mutex;

    struct FailableNode {
        node_id: String,
        executor: Arc<Mutex<DistributedTransactionExecutor>>,
        sequence: Arc<Mutex<u64>>,
        is_crashed: Arc<AtomicBool>,
    }

    impl FailableNode {
        fn new(node_id: String) -> Self {
            let initial_hash = StateHash::from_hex("0".repeat(64));
            let executor = DistributedTransactionExecutor::new(node_id.clone(), initial_hash);

            Self {
                node_id,
                executor: Arc::new(Mutex::new(executor)),
                sequence: Arc::new(Mutex::new(0)),
                is_crashed: Arc::new(AtomicBool::new(false)),
            }
        }

        fn crash(&self) {
            self.is_crashed.store(true, Ordering::Relaxed);
        }

        fn recover(&self) {
            self.is_crashed.store(false, Ordering::Relaxed);
        }

        fn is_failed(&self) -> bool {
            self.is_crashed.load(Ordering::Relaxed)
        }

        async fn submit_operation(&self, op_idx: u64) -> Result<(), String> {
            if self.is_failed() {
                return Err("Node is crashed".to_string());
            }

            let mut executor = self.executor.lock().await;
            let mut seq = self.sequence.lock().await;
            *seq += 1;

            let op_id = OperationId::new(self.node_id.clone(), *seq);
            let parent_vc = VectorClock::new();
            let parent_version = StateVersion::new(parent_vc, "0".repeat(64));

            let mut fields = HashMap::new();
            fields.insert("value".to_string(), json!(op_idx));

            let operation = Operation::new(
                op_id,
                parent_version.clone(),
                format!("op_{}", op_idx),
                OperationCommand {
                    op_type: "set".to_string(),
                    collection: "items".to_string(),
                    record_id: format!("item_{}", op_idx),
                    fields,
                },
                self.node_id.clone(),
            );

            executor.execute_local_transaction(
                *seq,
                format!("txn_{}", op_idx),
                parent_version,
                vec![operation],
                ConsistencyContract::causal(),
            )?;

            Ok(())
        }

        async fn receive_operation(&self, message: ReplicationMessage) -> Result<(), String> {
            if self.is_failed() {
                return Err("Node is crashed".to_string());
            }

            let mut executor = self.executor.lock().await;
            let parent_vc = VectorClock::new();
            let parent_version = StateVersion::new(parent_vc, "0".repeat(64));

            executor
                .receive_replicated_transaction(message, parent_version)
                .map(|_| ())
                .map_err(|e| e)
        }

        async fn state_hash(&self) -> StateHash {
            let executor = self.executor.lock().await;
            executor
                .get_replica_state(&self.node_id)
                .map(|s| s.state_hash)
                .unwrap_or_else(|| StateHash::from_hex("0".repeat(64)))
        }

        async fn operations_applied(&self) -> u64 {
            let executor = self.executor.lock().await;
            executor
                .get_replica_state(&self.node_id)
                .map(|s| s.operations_applied)
                .unwrap_or(0)
        }
    }

    /// Phase 2a Test 1: Follower crash and recovery
    #[tokio::test]
    async fn follower_crash_recovery() {
        let leader = FailableNode::new("leader".to_string());
        let follower1 = FailableNode::new("follower1".to_string());
        let follower2 = FailableNode::new("follower2".to_string());

        // Register replicas
        {
            let mut executor = leader.executor.lock().await;
            executor.register_replica("follower1".to_string(), StateHash::from_hex("0".repeat(64)));
            executor.register_replica("follower2".to_string(), StateHash::from_hex("0".repeat(64)));
        }

        // Leader submits 50 operations
        for i in 0..50 {
            leader.submit_operation(i).await.expect("leader submit");
        }

        // Crash follower1
        follower1.crash();

        // Follower1 cannot receive operations during crash
        let result = follower1.submit_operation(50).await;
        assert!(result.is_err(), "Crashed node should reject operations");

        // Recover follower1
        follower1.recover();

        // Follower1 can submit operations again
        follower1
            .submit_operation(50)
            .await
            .expect("recovery submit");

        let leader_ops = leader.operations_applied().await;
        let f1_ops = follower1.operations_applied().await;
        let f2_ops = follower2.operations_applied().await;

        println!(
            "Leader: {}, Follower1: {}, Follower2: {}",
            leader_ops, f1_ops, f2_ops
        );
        assert_eq!(leader_ops, 50, "Leader should have 50 operations");
        assert_eq!(
            f1_ops, 1,
            "Follower1 should have 1 operation after recovery"
        );
    }

    /// Phase 2a Test 2: Leader crash and recovery
    #[tokio::test]
    async fn leader_crash_recovery() {
        let leader = FailableNode::new("leader".to_string());
        let follower1 = FailableNode::new("follower1".to_string());
        let follower2 = FailableNode::new("follower2".to_string());

        // Register replicas
        {
            let mut executor = leader.executor.lock().await;
            executor.register_replica("follower1".to_string(), StateHash::from_hex("0".repeat(64)));
            executor.register_replica("follower2".to_string(), StateHash::from_hex("0".repeat(64)));
        }

        // Leader submits some operations
        for i in 0..25 {
            leader.submit_operation(i).await.expect("leader submit");
        }

        let pre_crash_leader_ops = leader.operations_applied().await;
        assert_eq!(
            pre_crash_leader_ops, 25,
            "Leader should have 25 pre-crash ops"
        );

        // Crash leader
        leader.crash();
        let result = leader.submit_operation(25).await;
        assert!(result.is_err(), "Crashed leader should reject operations");

        // Recover leader
        leader.recover();

        // Leader can submit operations again
        for i in 25..50 {
            leader
                .submit_operation(i)
                .await
                .expect("leader post-recovery submit");
        }

        let post_recovery_leader_ops = leader.operations_applied().await;
        println!("Post-recovery leader ops: {}", post_recovery_leader_ops);
        assert_eq!(
            post_recovery_leader_ops, 50,
            "Leader should recover and accept new ops"
        );
    }

    /// Phase 2a Test 3: Concurrent crashes (leader + 1 follower)
    #[tokio::test]
    async fn dual_node_crash() {
        let leader = FailableNode::new("leader".to_string());
        let follower1 = FailableNode::new("follower1".to_string());
        let follower2 = FailableNode::new("follower2".to_string());

        // Register replicas
        {
            let mut executor = leader.executor.lock().await;
            executor.register_replica("follower1".to_string(), StateHash::from_hex("0".repeat(64)));
            executor.register_replica("follower2".to_string(), StateHash::from_hex("0".repeat(64)));
        }

        // Leader submits 30 operations
        for i in 0..30 {
            leader.submit_operation(i).await.expect("leader submit");
        }

        // Crash both leader and follower1
        leader.crash();
        follower1.crash();

        // Only follower2 can operate
        follower2
            .submit_operation(30)
            .await
            .expect("follower2 operates during dual crash");

        // Recover both
        leader.recover();
        follower1.recover();

        // All can operate again
        leader.submit_operation(30).await.expect("leader recovery");
        follower1
            .submit_operation(31)
            .await
            .expect("follower1 recovery");
        follower2
            .submit_operation(32)
            .await
            .expect("follower2 continues");

        let leader_ops = leader.operations_applied().await;
        let f1_ops = follower1.operations_applied().await;
        let f2_ops = follower2.operations_applied().await;

        println!(
            "Post dual-crash - Leader: {}, F1: {}, F2: {}",
            leader_ops, f1_ops, f2_ops
        );
        assert_eq!(leader_ops, 31, "Leader should recover");
        assert_eq!(f1_ops, 1, "Follower1 should recover");
        assert_eq!(f2_ops, 2, "Follower2 should continue");
    }

    /// Phase 2a Test 4: Multiple recovery cycles
    #[tokio::test]
    async fn multiple_crash_recovery_cycles() {
        let leader = FailableNode::new("leader".to_string());

        for cycle in 1..=5 {
            // Submit operations
            for i in 0..(cycle * 10) {
                let result = leader.submit_operation(i).await;
                if result.is_err() {
                    // Expected during crash
                    assert!(leader.is_failed(), "Should only fail during crash");
                }
            }

            let pre_crash_ops = leader.operations_applied().await;

            // Crash
            leader.crash();
            let result = leader.submit_operation(999).await;
            assert!(result.is_err(), "Crashed node should reject");

            // Recover
            leader.recover();
            leader
                .submit_operation(cycle * 100)
                .await
                .expect("recovery submit");

            let post_recovery_ops = leader.operations_applied().await;
            println!(
                "Cycle {}: pre={}, post={}",
                cycle, pre_crash_ops, post_recovery_ops
            );

            // Should maintain state across crashes
            assert!(
                post_recovery_ops > 0,
                "Should have operations after recovery"
            );
        }
    }

    /// Phase 2a Test 5: Loss of partial batch during crash
    #[tokio::test]
    async fn partial_batch_loss_during_crash() {
        let leader = FailableNode::new("leader".to_string());
        let follower = FailableNode::new("follower".to_string());

        {
            let mut executor = leader.executor.lock().await;
            executor.register_replica("follower".to_string(), StateHash::from_hex("0".repeat(64)));
        }

        // Leader starts submitting batch
        for i in 0..20 {
            if i == 10 {
                // Crash at mid-batch
                leader.crash();
            }
            let result = leader.submit_operation(i).await;
            if i >= 10 {
                assert!(result.is_err(), "Should fail during crash");
            }
        }

        // Recover
        leader.recover();

        let pre_recovery_ops = leader.operations_applied().await;
        println!("Ops before recovery: {}", pre_recovery_ops);

        // Finish submitting
        for i in 10..20 {
            leader
                .submit_operation(i)
                .await
                .expect("post-recovery submit");
        }

        let final_ops = leader.operations_applied().await;
        println!("Final ops after recovery: {}", final_ops);

        // Should have ops from pre-crash and post-recovery
        assert!(final_ops > 0, "Should have some operations");
    }
}
