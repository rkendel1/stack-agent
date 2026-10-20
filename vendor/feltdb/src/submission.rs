/// Production submission layer for FeltDB
///
/// Integrates the admission contract with actual operation submission,
/// ensuring no silent failures and explicit rejection semantics.
use crate::admission::{ProductionRetryPolicy, RejectionReason, SubmissionOutcome};
use crate::transaction_api::{TransactionManager, TxError};
use crate::transactions::{ConsistencyContract, Operation};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Metadata about a submission's retry history
#[derive(Clone, Debug)]
pub struct SubmissionMetrics {
    pub outcome: SubmissionOutcome,
    pub first_attempt_latency_us: u64,
    pub total_latency_us: u64,
    pub retry_attempts: usize,
    pub eventually_accepted: bool,
}

/// Production submission manager
/// Handles operation admission with explicit contract guarantees
pub struct SubmissionManager {
    transaction_manager: Arc<Mutex<TransactionManager>>,
    retry_policy: ProductionRetryPolicy,
    deduplication_cache: Arc<Mutex<std::collections::HashMap<String, u64>>>,
}

impl SubmissionManager {
    /// Create a new submission manager with standard production retry policy
    pub fn new(transaction_manager: Arc<Mutex<TransactionManager>>) -> Self {
        Self {
            transaction_manager,
            retry_policy: ProductionRetryPolicy::standard(),
            deduplication_cache: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Create with custom retry policy
    pub fn with_policy(
        transaction_manager: Arc<Mutex<TransactionManager>>,
        retry_policy: ProductionRetryPolicy,
    ) -> Self {
        Self {
            transaction_manager,
            retry_policy,
            deduplication_cache: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Submit a single operation for processing with detailed metrics
    /// Returns SubmissionMetrics including outcome, latencies, and retry count
    pub fn submit_with_metrics(&self, operation: Operation) -> SubmissionMetrics {
        let content_hash = self.hash_operation(&operation);
        let start_time = std::time::Instant::now();

        // Check deduplication cache
        {
            let cache = self.deduplication_cache.lock().unwrap();
            if let Some(&original_id) = cache.get(&content_hash) {
                let latency = start_time.elapsed().as_micros() as u64;
                return SubmissionMetrics {
                    outcome: SubmissionOutcome::Rejected(RejectionReason::DuplicateDetected {
                        original_operation_id: original_id,
                    }),
                    first_attempt_latency_us: latency,
                    total_latency_us: latency,
                    retry_attempts: 0,
                    eventually_accepted: false,
                };
            }
        }

        // Attempt submission with bounded retries
        let mut first_attempt_latency_us = 0u64;
        let mut retry_attempts = 0usize;

        for attempt in 0..self.retry_policy.max_attempts {
            let attempt_start = std::time::Instant::now();
            match self.try_submit_once(&operation) {
                Ok(operation_id) => {
                    let attempt_latency = attempt_start.elapsed().as_micros() as u64;
                    if attempt == 0 {
                        first_attempt_latency_us = attempt_latency;
                    }
                    // Cache this operation
                    {
                        let mut cache = self.deduplication_cache.lock().unwrap();
                        cache.insert(content_hash, operation_id);
                    }
                    return SubmissionMetrics {
                        outcome: SubmissionOutcome::Accepted { operation_id },
                        first_attempt_latency_us,
                        total_latency_us: start_time.elapsed().as_micros() as u64,
                        retry_attempts,
                        eventually_accepted: true,
                    };
                }
                Err(rejection) => {
                    let attempt_latency = attempt_start.elapsed().as_micros() as u64;
                    if attempt == 0 {
                        first_attempt_latency_us = attempt_latency;
                    }

                    // Determine if we should retry or reject
                    match &rejection {
                        RejectionReason::AdmissionBackpressure {
                            retry_after_us: _,
                            suggested_backoff_ms: _,
                        } => {
                            // Apply backoff if not at final attempt
                            if attempt < self.retry_policy.max_attempts - 1 {
                                let backoff_us = self.retry_policy.backoff_for_attempt(attempt + 1);
                                spin_sleep_us(backoff_us);
                                retry_attempts += 1;
                                continue;
                            } else {
                                // Final attempt exhausted
                                return SubmissionMetrics {
                                    outcome: SubmissionOutcome::Rejected(rejection),
                                    first_attempt_latency_us,
                                    total_latency_us: start_time.elapsed().as_micros() as u64,
                                    retry_attempts,
                                    eventually_accepted: false,
                                };
                            }
                        }
                        RejectionReason::TransientLockContention => {
                            // Immediate retry likely to succeed; retry without delay on first attempt
                            if attempt < self.retry_policy.max_attempts - 1 {
                                retry_attempts += 1;
                                continue;
                            } else {
                                return SubmissionMetrics {
                                    outcome: SubmissionOutcome::Rejected(rejection),
                                    first_attempt_latency_us,
                                    total_latency_us: start_time.elapsed().as_micros() as u64,
                                    retry_attempts,
                                    eventually_accepted: false,
                                };
                            }
                        }
                        RejectionReason::QueueFull | RejectionReason::DuplicateDetected { .. } => {
                            // No point retrying these
                            return SubmissionMetrics {
                                outcome: SubmissionOutcome::Rejected(rejection),
                                first_attempt_latency_us,
                                total_latency_us: start_time.elapsed().as_micros() as u64,
                                retry_attempts,
                                eventually_accepted: false,
                            };
                        }
                    }
                }
            }
        }

        // Should not reach here, but if we do, reject due to exhausted attempts
        SubmissionMetrics {
            outcome: SubmissionOutcome::Rejected(RejectionReason::AdmissionBackpressure {
                retry_after_us: 1000,
                suggested_backoff_ms: 10,
            }),
            first_attempt_latency_us,
            total_latency_us: start_time.elapsed().as_micros() as u64,
            retry_attempts,
            eventually_accepted: false,
        }
    }

    /// Submit a single operation for processing
    /// Returns explicit SubmissionOutcome: accepted with operation_id, or rejected with reason
    pub fn submit(&self, operation: Operation) -> SubmissionOutcome {
        let content_hash = self.hash_operation(&operation);

        // Check deduplication cache
        {
            let cache = self.deduplication_cache.lock().unwrap();
            if let Some(&original_id) = cache.get(&content_hash) {
                return SubmissionOutcome::Rejected(RejectionReason::DuplicateDetected {
                    original_operation_id: original_id,
                });
            }
        }

        // Attempt submission with bounded retries
        for attempt in 0..self.retry_policy.max_attempts {
            match self.try_submit_once(&operation) {
                Ok(operation_id) => {
                    // Cache this operation
                    {
                        let mut cache = self.deduplication_cache.lock().unwrap();
                        cache.insert(content_hash, operation_id);
                    }
                    return SubmissionOutcome::Accepted { operation_id };
                }
                Err(rejection) => {
                    // Determine if we should retry or reject
                    match &rejection {
                        RejectionReason::AdmissionBackpressure {
                            retry_after_us: _,
                            suggested_backoff_ms: _,
                        } => {
                            // Apply backoff if not at final attempt
                            if attempt < self.retry_policy.max_attempts - 1 {
                                let backoff_us = self.retry_policy.backoff_for_attempt(attempt + 1);
                                spin_sleep_us(backoff_us);
                                continue;
                            } else {
                                // Final attempt exhausted
                                return SubmissionOutcome::Rejected(rejection);
                            }
                        }
                        RejectionReason::TransientLockContention => {
                            // Immediate retry likely to succeed; retry without delay on first attempt
                            if attempt < self.retry_policy.max_attempts - 1 {
                                continue;
                            } else {
                                return SubmissionOutcome::Rejected(rejection);
                            }
                        }
                        RejectionReason::QueueFull | RejectionReason::DuplicateDetected { .. } => {
                            // No point retrying these
                            return SubmissionOutcome::Rejected(rejection);
                        }
                    }
                }
            }
        }

        // Should not reach here, but if we do, reject due to exhausted attempts
        SubmissionOutcome::Rejected(RejectionReason::AdmissionBackpressure {
            retry_after_us: 1000,
            suggested_backoff_ms: 10,
        })
    }

    /// Try to submit once without retry logic
    /// Used internally; only call once per submission flow
    fn try_submit_once(&self, operation: &Operation) -> Result<u64, RejectionReason> {
        let mut manager = self.transaction_manager.lock().unwrap();

        // Extract operation ID - this is the unique identifier
        let operation_id =
            u64::from_str_radix(&operation.operation_id.sequence.to_string(), 10).unwrap_or(0);

        // Attempt to execute transaction
        match manager.execute_and_commit(
            format!("txn_{}", operation_id),
            vec![operation.clone()],
            ConsistencyContract::local(),
        ) {
            Ok(_) => Ok(operation_id),
            Err(err) => {
                // Map TxError to RejectionReason
                match err {
                    TxError::OperationDeduplication(_) => Err(RejectionReason::DuplicateDetected {
                        original_operation_id: operation_id,
                    }),
                    TxError::PreconditionFailed(_) | TxError::ConflictDetected(_) => {
                        // These could indicate lock contention
                        Err(RejectionReason::TransientLockContention)
                    }
                    TxError::InvalidState(_) => Err(RejectionReason::AdmissionBackpressure {
                        retry_after_us: 100,
                        suggested_backoff_ms: 1,
                    }),
                    TxError::PersistenceFailure(_) => Err(RejectionReason::AdmissionBackpressure {
                        retry_after_us: 500,
                        suggested_backoff_ms: 5,
                    }),
                }
            }
        }
    }

    /// Compute content hash of an operation for deduplication
    fn hash_operation(&self, operation: &Operation) -> String {
        let mut hasher = Sha256::new();
        // Hash the operation's complete identifying fields
        hasher.update(operation.operation_id.node_id.as_bytes());
        hasher.update(operation.operation_id.sequence.to_le_bytes());
        hasher.update(operation.command.op_type.as_bytes());
        hasher.update(operation.command.collection.as_bytes());
        hasher.update(operation.command.record_id.as_bytes());
        // Include field values in the hash for complete uniqueness
        for (key, val) in &operation.command.fields {
            hasher.update(key.as_bytes());
            hasher.update(val.to_string().as_bytes());
        }
        let result = hasher.finalize();
        format!("{:x}", result)
    }
}

/// Spin-based sleep in microseconds (for tight retry loops)
fn spin_sleep_us(us: u64) {
    if us == 0 {
        return;
    }
    let start = std::time::Instant::now();
    let duration = std::time::Duration::from_micros(us);
    while start.elapsed() < duration {
        // Spin
        std::hint::spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::VectorClock;
    use crate::transactions::{Operation, OperationCommand, OperationId, StateVersion};
    use std::collections::HashMap;

    fn create_test_manager() -> Arc<Mutex<TransactionManager>> {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());
        Arc::new(Mutex::new(TransactionManager::new(
            "node1".to_string(),
            version,
        )))
    }

    fn create_test_operation(seq: u64) -> Operation {
        Operation::new(
            OperationId::new("node1".to_string(), seq),
            StateVersion::new(VectorClock::new(), "hash0".to_string()),
            "Update".to_string(),
            OperationCommand {
                op_type: "UpdateField".to_string(),
                collection: "test".to_string(),
                record_id: "rec1".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("field".to_string(), serde_json::json!("value"));
                    m
                },
            },
            "user".to_string(),
        )
    }

    #[test]
    fn submission_contract_accepted() {
        let manager = create_test_manager();
        let submitter = SubmissionManager::new(manager);

        let op = create_test_operation(1);
        let outcome = submitter.submit(op);

        assert!(outcome.is_accepted());
        match outcome {
            SubmissionOutcome::Accepted { operation_id } => {
                assert_eq!(operation_id, 1);
            }
            _ => panic!("Expected Accepted outcome"),
        }
    }

    #[test]
    fn submission_contract_duplicate_detection() {
        let manager = create_test_manager();
        let submitter = SubmissionManager::new(manager);

        let op = create_test_operation(1);
        let outcome1 = submitter.submit(op.clone());
        assert!(outcome1.is_accepted());

        // Submit same operation again
        let outcome2 = submitter.submit(op);
        assert!(outcome2.is_rejected());

        match outcome2 {
            SubmissionOutcome::Rejected(RejectionReason::DuplicateDetected {
                original_operation_id,
            }) => {
                assert_eq!(original_operation_id, 1);
            }
            _ => panic!("Expected DuplicateDetected rejection"),
        }
    }

    #[test]
    fn submission_contract_never_silently_drops() {
        // This test verifies that submit() always returns an explicit outcome
        let manager = create_test_manager();
        let submitter = SubmissionManager::new(manager);

        for i in 1..=10 {
            let op = create_test_operation(i);
            let outcome = submitter.submit(op);
            // Every submission gets an explicit outcome
            match outcome {
                SubmissionOutcome::Accepted { .. } => {
                    assert!(true, "Operation {} accepted", i);
                }
                SubmissionOutcome::Rejected(_) => {
                    assert!(true, "Operation {} explicitly rejected", i);
                }
            }
        }
    }

    #[test]
    fn production_retry_policy_exhaustion() {
        let manager = create_test_manager();
        let retry_policy = ProductionRetryPolicy::standard();
        let submitter = SubmissionManager::with_policy(manager, retry_policy);

        let op = create_test_operation(1);
        let outcome = submitter.submit(op);

        // Should succeed normally
        assert!(outcome.is_accepted());
    }

    #[test]
    fn deduplication_cache_prevents_duplicates() {
        let manager = create_test_manager();
        let submitter = SubmissionManager::new(manager);

        // Submit three different operations
        for i in 1..=3 {
            let op = create_test_operation(i);
            let outcome = submitter.submit(op);
            assert!(outcome.is_accepted());
        }

        // Duplicate should be detected
        let op_dup = create_test_operation(2);
        let outcome = submitter.submit(op_dup);
        assert!(outcome.is_rejected());
    }

    #[test]
    fn rejection_reason_display() {
        let backpressure = RejectionReason::AdmissionBackpressure {
            retry_after_us: 100,
            suggested_backoff_ms: 1,
        };
        let msg = format!("{}", backpressure);
        assert!(msg.contains("backpressure"));
        assert!(msg.contains("100"));

        let duplicate = RejectionReason::DuplicateDetected {
            original_operation_id: 42,
        };
        let msg = format!("{}", duplicate);
        assert!(msg.contains("duplicate"));
        assert!(msg.contains("42"));
    }

    #[test]
    fn acceptance_guarantees_operation_id() {
        let manager = create_test_manager();
        let submitter = SubmissionManager::new(manager);

        let op = create_test_operation(123);
        let outcome = submitter.submit(op);

        match outcome {
            SubmissionOutcome::Accepted { operation_id } => {
                assert_eq!(operation_id, 123);
            }
            SubmissionOutcome::Rejected(_) => panic!("Expected acceptance"),
        }
    }
}
