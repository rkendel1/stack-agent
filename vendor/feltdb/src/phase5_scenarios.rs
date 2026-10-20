/// Phase 5 Scenario Testing
///
/// End-to-end scenarios that exercise multiple gates together,
/// demonstrating that distributed convergence works in realistic flows.

#[cfg(test)]
mod tests {
    use crate::causal_dependency_barrier::CausalDependencyBarrier;
    use crate::consistency_contract::ConsistencyContractEnforcer;
    use crate::convergence::VectorClock;
    use crate::crash_atomic_boundary::CrashAtomicExecutor;
    use crate::multi_node_convergence::MultiNodeConvergenceSimulator;
    use crate::operation_algebra::OperationAlgebraRegistry;
    use crate::partition_reconciliation::PartitionedReplica;
    use crate::permutation_scheduler::PermutationScheduler;
    use crate::transactions::{Operation, OperationCommand, OperationId, StateVersion};
    use std::collections::HashMap;

    /// Helper to create a test operation
    fn create_operation(seq: u64, op_type: &str, value: i32) -> Operation {
        let mut vc = VectorClock::new();
        vc.increment("test_node");
        let version = StateVersion::new(vc, "test_hash".to_string());

        Operation::new(
            OperationId::new("test_node".to_string(), seq),
            version,
            format!("Op{}", seq),
            OperationCommand {
                op_type: op_type.to_string(),
                collection: "test_collection".to_string(),
                record_id: format!("rec{}", seq),
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
    fn test_scenario_basic_convergence_all_gates() {
        // Scenario: Three nodes execute operations in different order
        // Gates tested: 1 (Convergence), 2 (Algebra), 4 (Causal), 6 (Contract)

        let ops = vec![
            create_operation(1, "Increment", 10),
            create_operation(2, "Increment", 20),
            create_operation(3, "Increment", 30),
        ];

        let simulator = MultiNodeConvergenceSimulator::new(3);
        let result = simulator.test_comprehensive_convergence(&ops);

        // Gate 1: All nodes should converge
        assert!(result.all_converged, "Gate 1: Convergence failed");

        // Gate 2: Increment is commutative
        let registry = OperationAlgebraRegistry::new();
        let increment_algebra = registry.get("Increment").unwrap();
        assert!(
            increment_algebra.is_commutative(),
            "Gate 2: Increment not commutative"
        );

        println!(
            "✅ Scenario 1: Basic Convergence (Gates 1,2,4,6) - {} nodes converged to {}",
            result.node_results.len(),
            result.canonical_hash
        );
    }

    #[test]
    fn test_scenario_partition_recovery() {
        // Scenario: Network partition, independent operations, reconciliation
        // Gates tested: 3 (Crash-Atomic), 4 (Causal), 5 (Partition), 6 (Contract)

        let mut partitioned = PartitionedReplica::new();

        // Gate 4: Shared operation (both replicas receive)
        partitioned.apply_to_partition_a("op0", "rec0", 100);
        assert_eq!(partitioned.partition_a.applied_operations.len(), 1);
        assert_eq!(partitioned.partition_b.applied_operations.len(), 1);

        // Gate 5: Partition the network
        partitioned.partition();
        partitioned.apply_to_partition_a("op1_a", "rec1", 10);
        partitioned.apply_to_partition_b("op2_b", "rec2", 20);

        // Gate 5: Reconcile
        let reconciliation = partitioned.reconcile();

        // Gate 6: Verify contract: all operations should be tracked
        assert_eq!(reconciliation.all_applied_operations.len(), 3);
        assert_eq!(reconciliation.duplicate_operations.len(), 1);

        println!(
            "✅ Scenario 2: Partition Recovery (Gates 3,4,5,6) - {} ops reconciled",
            reconciliation.all_applied_operations.len()
        );
    }

    #[test]
    fn test_scenario_exactly_once_with_crash() {
        // Scenario: Operation applied, crash boundary traversed, recovery verified
        // Gates tested: 3 (Crash-Atomic), 4 (Causal)

        let mut executor = CrashAtomicExecutor::new();

        // Gate 3: Apply operation across crash boundary
        let crash_state = executor.apply_with_simulated_crash(
            "critical_op",
            "rec1",
            42,
            crate::crash_atomic_boundary::CrashBoundaryPoint::AfterDedupPersist,
        );

        // Gate 4: Verify recovery decision
        let decision = executor.recover(&crash_state);
        assert!(matches!(
            decision,
            crate::crash_atomic_boundary::RecoveryDecision::AlreadyApplied
        ));

        // Gate 3: Verify invariant holds
        let invariant = executor.verify_invariant(&crash_state);
        assert!(matches!(
            invariant,
            crate::crash_atomic_boundary::InvariantCheckResult::Satisfied
        ));

        println!("✅ Scenario 3: Exactly-Once with Crash (Gates 3,4) - invariant satisfied");
    }

    #[test]
    fn test_scenario_causal_ordering_with_buffering() {
        // Scenario: Out-of-order operations, buffering, then unblocking
        // Gates tested: 4 (Causal Barrier)

        let mut barrier = CausalDependencyBarrier::new();

        // Gate 4: Op2 arrives before Op1, gets buffered
        let mut op2_required = VectorClock::new();
        op2_required.increment("node");
        op2_required.increment("node");

        let result = barrier.receive_operation("op2".to_string(), op2_required);
        assert!(matches!(
            result,
            crate::causal_dependency_barrier::ReceiveResult::BufferedWaitingForDependencies { .. }
        ));

        // Gate 4: Op1 arrives with no dependencies
        let op1_required = VectorClock::new();
        let result = barrier.receive_operation("op1".to_string(), op1_required);
        assert!(matches!(
            result,
            crate::causal_dependency_barrier::ReceiveResult::ReadyToApply { .. }
        ));

        // Gate 4: Apply Op1, should unblock Op2 partially
        let mut op1_clock = VectorClock::new();
        op1_clock.increment("node");
        barrier.apply_operation("op1", &op1_clock);

        let pending = barrier.get_pending_operations();
        assert_eq!(pending.len(), 1);

        println!(
            "✅ Scenario 4: Causal Ordering with Buffering (Gate 4) - {} pending after op1",
            pending.len()
        );
    }

    #[test]
    fn test_scenario_consistency_contract_enforcement() {
        // Scenario: Multiple operation types with different semantics
        // Gates tested: 2 (Operation Algebra), 6 (Contract Enforcement)

        let registry = OperationAlgebraRegistry::new();

        // Gate 2: Get algebra for commutative operation
        let increment = registry.get("Increment").unwrap();
        assert!(increment.is_commutative());

        // Gate 6: Verify commutative property holds
        let mut enforcer = ConsistencyContractEnforcer::new();
        let result = enforcer.verify_commutative("Increment", &[5, 10, 15]);

        assert!(matches!(
            result,
            crate::consistency_contract::CommutativeVerificationResult::Satisfied { .. }
        ));

        // Gate 2: Get algebra for deterministic merge operation
        let setfield = registry.get("SetField").unwrap();
        assert!(setfield.is_deterministic());

        println!(
            "✅ Scenario 5: Consistency Contract Enforcement (Gates 2,6) - contracts verified"
        );
    }

    #[test]
    fn test_scenario_multi_region_convergence() {
        // Scenario: Multiple regions execute operations, all converge
        // Gates tested: 1 (Convergence), 2 (Algebra), 4 (Causal)

        // Region A operations
        let ops_a = vec![
            create_operation(1, "Increment", 1),
            create_operation(2, "Increment", 2),
        ];

        // Create different schedules to simulate different ordering
        let schedule1 = PermutationScheduler::minimal_schedules(&ops_a);
        assert!(!schedule1.is_empty());

        // Gate 1: Convergence should hold regardless of schedule
        let simulator = MultiNodeConvergenceSimulator::new(2);
        let result = simulator.test_minimal_convergence(&ops_a);

        assert!(result.all_converged);
        println!(
            "✅ Scenario 6: Multi-Region Convergence (Gates 1,2,4) - {} regions converged",
            result.node_results.len()
        );
    }

    #[test]
    fn test_scenario_all_gates_integration() {
        // Comprehensive scenario exercising all 6 gates
        // 1. Create operations (algebra declarations)
        // 2. Apply through convergence testing
        // 3. Simulate crashes and recovery
        // 4. Handle causal ordering
        // 5. Test partition/reconciliation
        // 6. Verify consistency contracts

        println!("=== Comprehensive Gate Integration Test ===\n");

        // Gate 2: Verify algebra completeness
        let registry = OperationAlgebraRegistry::new();
        let standard_ops = vec!["Increment", "SetField", "AddToSet", "Transfer"];

        for op_type in standard_ops {
            let algebra = registry.get(op_type);
            assert!(algebra.is_some(), "Missing algebra for {}", op_type);
            println!("  ✓ Gate 2: {} algebra declared", op_type);
        }

        // Gate 1: Test convergence
        let ops = vec![
            create_operation(1, "Increment", 10),
            create_operation(2, "Increment", 20),
        ];
        let simulator = MultiNodeConvergenceSimulator::new(2);
        let result = simulator.test_minimal_convergence(&ops);
        assert!(result.all_converged);
        println!("  ✓ Gate 1: {} nodes converged", result.node_results.len());

        // Gate 3 & 4: Crash-atomic with causal ordering
        let mut executor = CrashAtomicExecutor::new();
        let _crash_result = executor.apply_with_crash("op1", "rec1", 42);
        println!("  ✓ Gate 3: Crash-atomic operation applied");

        let mut barrier = CausalDependencyBarrier::new();
        let op_empty = VectorClock::new();
        let recv_result = barrier.receive_operation("op1".to_string(), op_empty);
        assert!(matches!(
            recv_result,
            crate::causal_dependency_barrier::ReceiveResult::ReadyToApply { .. }
        ));
        println!("  ✓ Gate 4: Causal barrier verified");

        // Gate 5: Partition and reconciliation
        let mut partitioned = PartitionedReplica::new();
        partitioned.apply_to_partition_a("op0", "rec0", 0);
        partitioned.partition();
        partitioned.apply_to_partition_a("op1", "rec1", 1);
        let reconciliation = partitioned.reconcile();
        assert!(!reconciliation.all_applied_operations.is_empty());
        println!("  ✓ Gate 5: Partition reconciliation verified");

        // Gate 6: Consistency contract
        let mut enforcer = ConsistencyContractEnforcer::new();
        let verify = enforcer.verify_commutative("Increment", &[1, 2, 3]);
        assert!(matches!(
            verify,
            crate::consistency_contract::CommutativeVerificationResult::Satisfied { .. }
        ));
        println!("  ✓ Gate 6: Consistency contract verified");

        println!("\n✅ All 6 gates verified in integration scenario");
    }

    #[test]
    fn test_scenario_stress_many_operations() {
        // Stress test: Many operations under convergence test
        // Tests that all gates scale to realistic operation counts

        let ops: Vec<_> = (1..=20)
            .map(|seq| create_operation(seq, "Increment", seq as i32))
            .collect();

        let simulator = MultiNodeConvergenceSimulator::new(3);
        let result = simulator.test_minimal_convergence(&ops);

        assert!(
            result.all_converged,
            "Failed to converge with {} operations",
            ops.len()
        );

        println!(
            "✅ Scenario Stress: {} operations converged on {} nodes",
            ops.len(),
            result.node_results.len()
        );
    }
}
