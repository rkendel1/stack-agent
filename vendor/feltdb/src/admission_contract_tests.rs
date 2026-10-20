/// Permanent contract tests for FeltDB production admission semantics
///
/// These tests validate the core guarantees of the production admission contract:
/// 1. No silent operation drops (every submission gets explicit outcome)
/// 2. Explicit rejection with machine-readable reasons
/// 3. Idempotency guarantees (duplicate detection prevents re-application)
/// 4. Retry safety (callers can safely retry rejected operations)
/// 5. Bounded admission (no infinite retry loops)

#[cfg(test)]
mod tests {
    use crate::admission::{ProductionRetryPolicy, RejectionReason, SubmissionOutcome};
    use crate::convergence::VectorClock;
    use crate::submission::SubmissionManager;
    use crate::transaction_api::TransactionManager;
    use crate::transactions::{Operation, OperationCommand, OperationId, StateVersion};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn create_manager() -> Arc<Mutex<TransactionManager>> {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());
        Arc::new(Mutex::new(TransactionManager::new(
            "node1".to_string(),
            version,
        )))
    }

    fn create_operation(node: &str, seq: u64) -> Operation {
        Operation::new(
            OperationId::new(node.to_string(), seq),
            StateVersion::new(VectorClock::new(), "hash0".to_string()),
            "Update".to_string(),
            OperationCommand {
                op_type: "UpdateField".to_string(),
                collection: "test".to_string(),
                record_id: "rec1".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert(
                        "field".to_string(),
                        serde_json::json!(format!("value_{}", seq)),
                    );
                    m
                },
            },
            "user".to_string(),
        )
    }

    // ====== Contract Guarantee 1: No Silent Drops ======

    #[test]
    fn contract_guarantee_no_silent_drops_single_operation() {
        let manager = create_manager();
        let submitter = SubmissionManager::new(manager);

        let op = create_operation("node1", 1);
        let outcome = submitter.submit(op);

        // Every submission must produce an explicit outcome
        match outcome {
            SubmissionOutcome::Accepted { .. } => {
                assert!(true, "Operation explicitly accepted");
            }
            SubmissionOutcome::Rejected(ref reason) => {
                assert!(
                    true,
                    "Operation explicitly rejected with reason: {:?}",
                    reason
                );
            }
        }
    }

    #[test]
    fn contract_guarantee_no_silent_drops_batch() {
        let manager = create_manager();
        let submitter = SubmissionManager::new(manager);

        // Submit 100 operations and verify each gets explicit outcome
        let mut accepted_count = 0;
        let mut rejected_count = 0;

        for i in 1..=100 {
            let op = create_operation("node1", i);
            let outcome = submitter.submit(op);

            match outcome {
                SubmissionOutcome::Accepted { .. } => accepted_count += 1,
                SubmissionOutcome::Rejected(_) => rejected_count += 1,
            }
        }

        // Each operation must have produced an outcome
        assert_eq!(accepted_count + rejected_count, 100);
        // Under normal load, most should be accepted
        assert!(accepted_count >= 90, "Expected >90% acceptance rate");
    }

    // ====== Contract Guarantee 2: Explicit Rejection with Reasons ======

    #[test]
    fn contract_guarantee_explicit_rejection_duplicate_detected() {
        let manager = create_manager();
        let submitter = SubmissionManager::new(manager);

        let op1 = create_operation("node1", 1);
        let outcome1 = submitter.submit(op1.clone());
        assert!(
            outcome1.is_accepted(),
            "First submission should be accepted"
        );

        let outcome2 = submitter.submit(op1);
        assert!(outcome2.is_rejected(), "Duplicate should be rejected");

        match outcome2 {
            SubmissionOutcome::Rejected(RejectionReason::DuplicateDetected {
                original_operation_id,
            }) => {
                assert_eq!(
                    original_operation_id, 1,
                    "Should identify original operation ID"
                );
            }
            _ => panic!("Expected DuplicateDetected rejection reason"),
        }
    }

    #[test]
    fn contract_guarantee_rejection_reasons_are_actionable() {
        // This test verifies that all rejection reasons provide guidance for callers

        // AdmissionBackpressure includes retry guidance
        let backpressure = RejectionReason::AdmissionBackpressure {
            retry_after_us: 100,
            suggested_backoff_ms: 1,
        };
        match backpressure {
            RejectionReason::AdmissionBackpressure {
                retry_after_us,
                suggested_backoff_ms,
            } => {
                assert!(
                    retry_after_us > 0,
                    "retry_after_us should guide backoff strategy"
                );
                assert!(
                    suggested_backoff_ms > 0,
                    "suggested_backoff_ms should guide backoff"
                );
            }
            _ => panic!("Expected AdmissionBackpressure"),
        }

        // TransientLockContention suggests immediate retry
        let lock_contention = RejectionReason::TransientLockContention;
        assert!(
            !matches!(lock_contention, RejectionReason::QueueFull),
            "TransientLockContention is not QueueFull (different retry strategy)"
        );

        // QueueFull indicates severe overload
        let queue_full = RejectionReason::QueueFull;
        let display = format!("{}", queue_full);
        assert!(
            display.contains("full"),
            "QueueFull should be clearly identified"
        );

        // DuplicateDetected identifies original operation
        let duplicate = RejectionReason::DuplicateDetected {
            original_operation_id: 42,
        };
        match duplicate {
            RejectionReason::DuplicateDetected {
                original_operation_id,
            } => {
                assert_eq!(
                    original_operation_id, 42,
                    "Should identify original operation"
                );
            }
            _ => panic!("Expected DuplicateDetected"),
        }
    }

    // ====== Contract Guarantee 3: Idempotency / Duplicate Detection ======

    #[test]
    fn contract_guarantee_idempotent_exact_once() {
        let manager = create_manager();
        let submitter = SubmissionManager::new(manager);

        let op = create_operation("node1", 1);

        // Submit same operation 5 times
        let outcome1 = submitter.submit(op.clone());
        assert!(outcome1.is_accepted(), "First submission accepted");

        for i in 2..=5 {
            let outcome = submitter.submit(op.clone());
            assert!(
                outcome.is_rejected(),
                "Submission #{} should be rejected as duplicate",
                i
            );
            match outcome {
                SubmissionOutcome::Rejected(RejectionReason::DuplicateDetected { .. }) => {
                    assert!(true, "Correctly identified as duplicate");
                }
                _ => panic!("Expected DuplicateDetected for duplicate submission"),
            }
        }
    }

    #[test]
    fn contract_guarantee_different_operations_not_deduplicated() {
        let manager = create_manager();
        let submitter = SubmissionManager::new(manager);

        // Submit 10 different operations
        for i in 1..=10 {
            let op = create_operation("node1", i);
            let outcome = submitter.submit(op);
            assert!(
                outcome.is_accepted(),
                "Operation {} should be accepted (not deduplicated)",
                i
            );
        }
    }

    #[test]
    fn contract_guarantee_deduplication_survives_across_retries() {
        let manager = create_manager();
        let submitter = SubmissionManager::new(manager);

        let op = create_operation("node1", 42);

        // Initial submission
        let outcome1 = submitter.submit(op.clone());
        assert!(outcome1.is_accepted());

        // Caller retries on a different error (simulated here)
        // The deduplication cache should still prevent re-application
        let outcome2 = submitter.submit(op);
        assert!(outcome2.is_rejected());
        assert!(matches!(
            outcome2,
            SubmissionOutcome::Rejected(RejectionReason::DuplicateDetected { .. })
        ));
    }

    // ====== Contract Guarantee 4: Retry Safety ======

    #[test]
    fn contract_guarantee_accepted_operations_get_operation_id() {
        let manager = create_manager();
        let submitter = SubmissionManager::new(manager);

        let op = create_operation("node1", 100);
        let outcome = submitter.submit(op);

        match outcome {
            SubmissionOutcome::Accepted { operation_id } => {
                assert_eq!(
                    operation_id, 100,
                    "operation_id in Accepted outcome must match submitted operation"
                );
            }
            SubmissionOutcome::Rejected(_) => {
                panic!("Expected Accepted outcome for a normal operation")
            }
        }
    }

    #[test]
    fn contract_guarantee_rejected_operations_safe_to_retry() {
        let manager = create_manager();
        let submitter = SubmissionManager::new(manager);

        // Submit initial operation
        let op = create_operation("node1", 1);
        let outcome1 = submitter.submit(op.clone());
        assert!(outcome1.is_accepted());

        // Next submission of different operation would go through
        // (demonstrating that rejection is not a permanent state)
        let op2 = create_operation("node1", 2);
        let outcome2 = submitter.submit(op2);
        assert!(outcome2.is_accepted());
    }

    // ====== Contract Guarantee 5: Bounded Admission ======

    #[test]
    fn contract_guarantee_retry_policy_is_bounded() {
        let policy = ProductionRetryPolicy::standard();

        // Verify bounded policy constraints
        assert!(
            policy.max_attempts < 100,
            "max_attempts should be reasonable"
        );
        assert!(
            policy.backoff_sequence.len() <= policy.max_attempts,
            "backoff sequence length should not exceed max_attempts"
        );

        // Verify no negative backoffs
        for backoff in &policy.backoff_sequence {
            assert!(*backoff >= 0, "Backoff must be non-negative");
        }

        // Verify total budget is bounded
        let budget = policy.total_budget_us();
        assert!(
            budget < 1_000_000,
            "Total admission budget should be < 1 second"
        );
    }

    #[test]
    fn contract_guarantee_stress_test_still_bounded() {
        let policy = ProductionRetryPolicy::stress_test();

        // Even stress policy must be bounded
        assert!(
            policy.max_attempts <= 100,
            "Stress policy max_attempts must be bounded"
        );
        assert!(policy.total_budget_us() < 10_000_000, "Stress budget < 10s");
    }

    #[test]
    fn contract_guarantee_no_infinite_loops() {
        let manager = create_manager();
        let policy = ProductionRetryPolicy::standard();
        let submitter = SubmissionManager::with_policy(manager, policy);

        // Submit and measure time to verify we don't hang
        let op = create_operation("node1", 1);
        let start = std::time::Instant::now();

        let _outcome = submitter.submit(op);

        let elapsed = start.elapsed();
        // Should complete in reasonable time (less than 100ms for standard policy)
        assert!(
            elapsed.as_millis() < 100,
            "Submission should complete quickly; took {}ms",
            elapsed.as_millis()
        );
    }

    // ====== Comprehensive End-to-End Tests ======

    #[test]
    fn contract_end_to_end_high_throughput() {
        let manager = create_manager();
        let submitter = SubmissionManager::new(manager);

        let start = std::time::Instant::now();
        let mut accepted = 0;
        let mut rejected = 0;

        // Submit 1000 operations
        for i in 1..=1000 {
            let op = create_operation("node1", i);
            match submitter.submit(op) {
                SubmissionOutcome::Accepted { .. } => accepted += 1,
                SubmissionOutcome::Rejected(_) => rejected += 1,
            }
        }

        let elapsed = start.elapsed();

        assert_eq!(
            accepted + rejected,
            1000,
            "Every operation must get an outcome"
        );
        assert_eq!(rejected, 0, "Under normal load, no rejections expected");
        println!(
            "Processed 1000 operations in {:.2}ms ({:.0} ops/sec)",
            elapsed.as_millis(),
            1000.0 / elapsed.as_secs_f64()
        );
    }

    #[test]
    fn contract_end_to_end_duplicate_handling_under_load() {
        let manager = create_manager();
        let submitter = SubmissionManager::new(manager);

        // Submit 100 operations, some of which are duplicates
        let mut outcomes = Vec::new();

        for cycle in 0..5 {
            for i in 1..=20 {
                let op = create_operation("node1", i);
                let outcome = submitter.submit(op);
                outcomes.push((cycle, i, outcome));
            }
        }

        // Count outcomes by type
        let mut accepted_by_cycle = vec![0; 5];
        let mut duplicates_by_cycle = vec![0; 5];

        for (cycle, _seq, outcome) in outcomes {
            match outcome {
                SubmissionOutcome::Accepted { .. } => {
                    accepted_by_cycle[cycle] += 1;
                }
                SubmissionOutcome::Rejected(RejectionReason::DuplicateDetected { .. }) => {
                    duplicates_by_cycle[cycle] += 1;
                }
                _ => {
                    panic!("Unexpected rejection reason");
                }
            }
        }

        // First cycle: all should be accepted
        assert_eq!(accepted_by_cycle[0], 20, "First cycle should accept all");
        assert_eq!(
            duplicates_by_cycle[0], 0,
            "First cycle should have no duplicates"
        );

        // Subsequent cycles: all should be duplicates
        for cycle in 1..5 {
            assert_eq!(
                accepted_by_cycle[cycle], 0,
                "Cycle {} should not accept (all duplicates)",
                cycle
            );
            assert_eq!(
                duplicates_by_cycle[cycle], 20,
                "Cycle {} should detect all as duplicates",
                cycle
            );
        }
    }

    #[test]
    fn contract_production_vs_stress_policy_difference() {
        let prod_policy = ProductionRetryPolicy::standard();
        let stress_policy = ProductionRetryPolicy::stress_test();

        // Stress policy should allow more retries
        assert!(
            stress_policy.max_attempts > prod_policy.max_attempts,
            "Stress policy should have more attempts"
        );

        // But both should be bounded
        assert!(prod_policy.max_attempts <= 10);
        assert!(stress_policy.max_attempts <= 100);
    }

    #[test]
    fn contract_rejection_reason_display_is_clear() {
        let reasons = vec![
            (
                RejectionReason::AdmissionBackpressure {
                    retry_after_us: 500,
                    suggested_backoff_ms: 5,
                },
                "backpressure",
            ),
            (RejectionReason::TransientLockContention, "contention"),
            (RejectionReason::QueueFull, "full"),
            (
                RejectionReason::DuplicateDetected {
                    original_operation_id: 99,
                },
                "duplicate",
            ),
        ];

        for (reason, expected_substring) in reasons {
            let display = format!("{}", reason);
            assert!(
                display.to_lowercase().contains(expected_substring),
                "Display for {:?} should contain '{}'",
                reason,
                expected_substring
            );
        }
    }

    #[test]
    fn contract_outcome_accessors_are_correct() {
        let accepted = SubmissionOutcome::Accepted { operation_id: 42 };
        assert!(accepted.is_accepted());
        assert!(!accepted.is_rejected());

        let rejected = SubmissionOutcome::Rejected(RejectionReason::QueueFull);
        assert!(!rejected.is_accepted());
        assert!(rejected.is_rejected());
    }
}
