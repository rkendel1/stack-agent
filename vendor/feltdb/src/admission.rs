/// Production admission contract for FeltDB
///
/// Defines explicit, machine-readable submission semantics.
/// Never silently drops operations. Rejection is explicit and retryable.
use serde::{Deserialize, Serialize};
use std::fmt;

/// Outcome of a submission attempt
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubmissionOutcome {
    /// Operation accepted for processing
    /// Operations are accepted in order but may not yet be committed.
    /// Caller should wait for CommitAck before assuming durability.
    Accepted { operation_id: u64 },

    /// Operation rejected due to backpressure
    /// Caller should retry with backoff
    Rejected(RejectionReason),
}

impl SubmissionOutcome {
    pub fn is_accepted(&self) -> bool {
        matches!(self, SubmissionOutcome::Accepted { .. })
    }

    pub fn is_rejected(&self) -> bool {
        matches!(self, SubmissionOutcome::Rejected(_))
    }
}

/// Explicit reason for rejection
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectionReason {
    /// Admission path saturated due to lock contention
    /// Safe to retry. Caller should apply exponential backoff.
    AdmissionBackpressure {
        /// Microseconds to wait before retry
        retry_after_us: u64,
        /// Suggested backoff for caller's exponential strategy (milliseconds)
        suggested_backoff_ms: u32,
    },

    /// Transient lock contention at submission time
    /// Immediate retry likely to succeed
    TransientLockContention,

    /// Operation queue at capacity
    /// Rare in normal operation; indicates severe overload
    QueueFull,

    /// Deduplication rejected duplicate (same content_hash already processed)
    /// Not an error; operation will be applied exactly once
    DuplicateDetected {
        /// The original operation_id that processed this content
        original_operation_id: u64,
    },
}

impl fmt::Display for RejectionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RejectionReason::AdmissionBackpressure {
                retry_after_us,
                suggested_backoff_ms,
            } => write!(
                f,
                "admission backpressure (retry after {}µs, suggested backoff {}ms)",
                retry_after_us, suggested_backoff_ms
            ),
            RejectionReason::TransientLockContention => {
                write!(f, "transient lock contention")
            }
            RejectionReason::QueueFull => write!(f, "admission queue full"),
            RejectionReason::DuplicateDetected {
                original_operation_id,
            } => write!(
                f,
                "duplicate detected (original op_id: {})",
                original_operation_id
            ),
        }
    }
}

/// Production retry/backoff policy for admission
/// Bounded, non-infinite, based on measured contention curve
pub struct ProductionRetryPolicy {
    /// Maximum number of lock acquisition attempts
    pub max_attempts: usize,
    /// Backoff durations (microseconds) for each attempt
    pub backoff_sequence: Vec<u64>,
}

impl ProductionRetryPolicy {
    /// Create the standard production policy
    /// Based on Gate 14A-SYNC measurements:
    /// - 3 retries → ~1% rejection at 8 writers
    /// - 4 retries → near-zero rejection at intended operating envelope
    pub fn standard() -> Self {
        ProductionRetryPolicy {
            max_attempts: 4,
            backoff_sequence: vec![
                0,   // Attempt 1: no backoff (immediate)
                100, // Attempt 2: 100µs
                200, // Attempt 3: 200µs
                400, // Attempt 4: 400µs
                     // Total: 700µs of bounded admission attempts
            ],
        }
    }

    /// Create a policy for testing/stress scenarios
    /// Higher attempts for high-concurrency testing
    pub fn stress_test() -> Self {
        ProductionRetryPolicy {
            max_attempts: 10,
            backoff_sequence: vec![0, 50, 100, 150, 200, 250, 300, 350, 400, 450],
        }
    }

    /// Get suggested backoff for a given attempt number
    pub fn backoff_for_attempt(&self, attempt: usize) -> u64 {
        if attempt == 0 {
            0
        } else if attempt <= self.backoff_sequence.len() {
            self.backoff_sequence[attempt - 1]
        } else {
            // Beyond configured attempts, suggest exponential backoff to caller
            self.backoff_sequence.last().copied().unwrap_or(400) * 2
        }
    }

    /// Total admission time budget (sum of all backoffs)
    pub fn total_budget_us(&self) -> u64 {
        self.backoff_sequence.iter().sum()
    }
}

impl Default for ProductionRetryPolicy {
    fn default() -> Self {
        Self::standard()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_policy_is_bounded() {
        let policy = ProductionRetryPolicy::standard();
        assert_eq!(policy.max_attempts, 4);
        assert_eq!(policy.backoff_sequence.len(), 4);
        assert_eq!(policy.total_budget_us(), 700);
    }

    #[test]
    fn stress_policy_has_higher_budget() {
        let prod = ProductionRetryPolicy::standard();
        let stress = ProductionRetryPolicy::stress_test();
        assert!(stress.total_budget_us() > prod.total_budget_us());
    }

    #[test]
    fn outcome_accessors_work() {
        let accepted = SubmissionOutcome::Accepted { operation_id: 123 };
        assert!(accepted.is_accepted());
        assert!(!accepted.is_rejected());

        let rejected = SubmissionOutcome::Rejected(RejectionReason::TransientLockContention);
        assert!(!rejected.is_accepted());
        assert!(rejected.is_rejected());
    }

    #[test]
    fn rejection_reason_display() {
        let backpressure = RejectionReason::AdmissionBackpressure {
            retry_after_us: 1000,
            suggested_backoff_ms: 10,
        };
        let msg = format!("{}", backpressure);
        assert!(msg.contains("backpressure"));
        assert!(msg.contains("1000µs"));
    }
}
