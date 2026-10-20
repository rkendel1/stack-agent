use crate::Operation;
/// Execution Engine module for event-driven execution runtime
///
/// Execution represents a trigger being fired and a capability being executed.
/// Executions are durable, identifiable, and resumable.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Execution status enumeration
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExecutionStatus {
    /// Waiting to be executed
    Pending,
    /// Currently executing
    Running,
    /// Completed successfully
    Succeeded,
    /// Failed and may be retried
    Failed,
    /// Scheduled for retry
    RetryScheduled,
    /// Cannot be retried
    DeadLettered,
}

/// A durable execution record
///
/// Tracks the execution of a capability triggered by an operation.
/// Provides idempotency and recoverability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Execution {
    /// Unique execution identifier
    pub execution_id: String,

    /// The trigger that caused this execution
    pub trigger_id: String,

    /// Source operation that triggered this execution
    pub source_operation: Operation,

    /// Capability to execute
    pub capability: String,

    /// Attempt number (1-based)
    pub attempt: u32,

    /// Current status
    pub status: ExecutionStatus,

    /// Optional result value
    pub result: Option<Value>,

    /// Optional error message
    pub error: Option<String>,

    /// Timestamp when execution was created (ms since UNIX epoch)
    pub created_ms: u128,

    /// Timestamp when execution started (ms since UNIX epoch)
    pub started_ms: Option<u128>,

    /// Timestamp when execution completed (ms since UNIX epoch)
    pub completed_ms: Option<u128>,

    /// Claim token for single-owner execution semantics
    pub claimed_by: Option<String>,

    /// Retry policy settings
    pub retry_policy: RetryPolicy,
}

/// Retry policy for executions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts
    pub max_attempts: u32,

    /// Initial backoff in milliseconds
    pub initial_backoff_ms: u64,

    /// Maximum backoff in milliseconds
    pub max_backoff_ms: u64,

    /// Backoff multiplier for exponential backoff
    pub backoff_multiplier: f64,

    /// Timeout per attempt in milliseconds
    pub timeout_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff_ms: 100,
            max_backoff_ms: 30000,
            backoff_multiplier: 2.0,
            timeout_ms: 30000,
        }
    }
}

impl Execution {
    /// Create a new execution
    pub fn new(
        execution_id: String,
        trigger_id: String,
        source_operation: Operation,
        capability: String,
    ) -> Self {
        let now_ms = crate::now_ms();
        Self {
            execution_id,
            trigger_id,
            source_operation,
            capability,
            attempt: 1,
            status: ExecutionStatus::Pending,
            result: None,
            error: None,
            created_ms: now_ms,
            started_ms: None,
            completed_ms: None,
            claimed_by: None,
            retry_policy: RetryPolicy::default(),
        }
    }

    /// Mark execution as running
    pub fn mark_running(&mut self, claimed_by: String) {
        self.status = ExecutionStatus::Running;
        self.started_ms = Some(crate::now_ms());
        self.claimed_by = Some(claimed_by);
    }

    /// Mark execution as succeeded
    pub fn mark_succeeded(&mut self, result: Value) {
        self.status = ExecutionStatus::Succeeded;
        self.result = Some(result);
        self.completed_ms = Some(crate::now_ms());
    }

    /// Mark execution as failed
    pub fn mark_failed(&mut self, error: String) {
        self.status = ExecutionStatus::Failed;
        self.error = Some(error);
        self.completed_ms = Some(crate::now_ms());
    }

    /// Mark execution for retry
    pub fn mark_retry_scheduled(&mut self) {
        if self.attempt < self.retry_policy.max_attempts {
            self.attempt += 1;
            self.status = ExecutionStatus::RetryScheduled;
            self.claimed_by = None;
        } else {
            self.status = ExecutionStatus::DeadLettered;
        }
    }

    /// Get backoff time for next retry attempt
    pub fn get_backoff_ms(&self) -> u64 {
        let backoff = (self.retry_policy.initial_backoff_ms as f64
            * self
                .retry_policy
                .backoff_multiplier
                .powi((self.attempt - 1) as i32)) as u64;
        backoff.min(self.retry_policy.max_backoff_ms)
    }

    /// Check if execution can be retried
    pub fn can_retry(&self) -> bool {
        matches!(
            self.status,
            ExecutionStatus::Failed | ExecutionStatus::RetryScheduled
        ) && self.attempt < self.retry_policy.max_attempts
    }

    /// Check if execution is terminal (won't change state)
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            ExecutionStatus::Succeeded | ExecutionStatus::DeadLettered
        )
    }
}

/// Execution queue - tracks all executions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionQueue {
    /// All executions by execution_id
    #[serde(default)]
    executions: HashMap<String, Execution>,

    /// Pending executions queue (FIFO)
    #[serde(default)]
    pending_queue: Vec<String>,

    /// Idempotency map: (instance_id, sequence) -> execution_id
    /// Prevents executing the same operation multiple times
    /// Serialized as a vector of tuples for JSON compatibility
    #[serde(default, rename = "idempotency_map")]
    idempotency_map_vec: Vec<((String, u64), String)>,

    #[serde(skip)]
    idempotency_map: HashMap<(String, u64), String>,
}

impl ExecutionQueue {
    /// Create a new execution queue
    pub fn new() -> Self {
        Self {
            executions: HashMap::new(),
            pending_queue: Vec::new(),
            idempotency_map_vec: Vec::new(),
            idempotency_map: HashMap::new(),
        }
    }
}

impl Default for ExecutionQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl ExecutionQueue {
    /// Sync the vec-based storage with the hashmap for operations
    fn sync_map_from_vec(&mut self) {
        self.idempotency_map.clear();
        for ((instance_id, sequence), exec_id) in &self.idempotency_map_vec {
            self.idempotency_map
                .insert((instance_id.clone(), *sequence), exec_id.clone());
        }
    }

    /// Sync the vec-based storage with the hashmap for serialization
    fn sync_vec_from_map(&mut self) {
        self.idempotency_map_vec = self
            .idempotency_map
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
    }

    /// Add an execution to the queue
    pub fn add_execution(&mut self, execution: Execution) -> Result<String, String> {
        // Sync map from vec in case we were deserialized
        self.sync_map_from_vec();

        let execution_id = execution.execution_id.clone();

        // Check if this operation has already been executed
        let op_key = (
            execution.source_operation.instance_id.clone(),
            execution.source_operation.sequence,
        );

        if let Some(existing_id) = self.idempotency_map.get(&op_key) {
            // Operation already has an execution - return existing execution_id
            return Ok(existing_id.clone());
        }

        // Record in idempotency map
        self.idempotency_map.insert(op_key, execution_id.clone());

        // Add to executions
        self.executions.insert(execution_id.clone(), execution);

        // Add to pending queue
        self.pending_queue.push(execution_id.clone());

        // Sync vec for serialization
        self.sync_vec_from_map();

        Ok(execution_id)
    }

    /// Get an execution by ID
    pub fn get_execution(&self, execution_id: &str) -> Option<Execution> {
        self.executions.get(execution_id).cloned()
    }

    /// Update an execution status
    pub fn update_execution(
        &mut self,
        execution_id: &str,
        execution: Execution,
    ) -> Result<(), String> {
        if self.executions.contains_key(execution_id) {
            self.executions.insert(execution_id.to_string(), execution);
            Ok(())
        } else {
            Err(format!("Execution not found: {}", execution_id))
        }
    }

    /// Get all pending executions
    pub fn get_pending(&self) -> Vec<Execution> {
        self.pending_queue
            .iter()
            .filter_map(|id| self.executions.get(id).cloned())
            .filter(|e| matches!(e.status, ExecutionStatus::Pending))
            .collect()
    }

    /// Get all executions
    pub fn all(&self) -> Vec<Execution> {
        self.executions.values().cloned().collect()
    }

    /// Check if an operation has already triggered an execution
    pub fn has_execution_for_operation(&self, instance_id: &str, sequence: u64) -> bool {
        self.idempotency_map
            .contains_key(&(instance_id.to_string(), sequence))
    }

    /// Get execution ID for an operation (if it exists)
    pub fn get_execution_for_operation(&self, instance_id: &str, sequence: u64) -> Option<String> {
        self.idempotency_map
            .get(&(instance_id.to_string(), sequence))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_lifecycle() {
        let mut exec = Execution::new(
            "exec-1".to_string(),
            "trigger-1".to_string(),
            Operation::insert(
                1,
                "inst-a".to_string(),
                1,
                "orders:123".to_string(),
                serde_json::json!({"total": 100}),
                "Order".to_string(),
                "orders".to_string(),
            ),
            "process_order".to_string(),
        );

        assert_eq!(exec.status, ExecutionStatus::Pending);
        assert_eq!(exec.attempt, 1);

        exec.mark_running("inst-a".to_string());
        assert_eq!(exec.status, ExecutionStatus::Running);

        exec.mark_succeeded(serde_json::json!({"processed": true}));
        assert_eq!(exec.status, ExecutionStatus::Succeeded);
        assert!(exec.result.is_some());
        assert!(exec.is_terminal());
    }

    #[test]
    fn test_execution_idempotency() {
        let mut queue = ExecutionQueue::new();

        let op = Operation::insert(
            1,
            "inst-a".to_string(),
            1,
            "orders:123".to_string(),
            serde_json::json!({"total": 100}),
            "Order".to_string(),
            "orders".to_string(),
        );

        let exec1 = Execution::new(
            "exec-1".to_string(),
            "trigger-1".to_string(),
            op.clone(),
            "process_order".to_string(),
        );

        let id1 = queue.add_execution(exec1).unwrap();

        // Try to add same operation again
        let exec2 = Execution::new(
            "exec-2".to_string(),
            "trigger-1".to_string(),
            op.clone(),
            "process_order".to_string(),
        );

        let id2 = queue.add_execution(exec2).unwrap();

        // Should return the same execution ID (idempotency)
        assert_eq!(id1, id2);
        assert_eq!(queue.all().len(), 1);
    }

    #[test]
    fn test_retry_backoff() {
        let mut exec = Execution::new(
            "exec-1".to_string(),
            "trigger-1".to_string(),
            Operation::insert(
                1,
                "inst-a".to_string(),
                1,
                "orders:123".to_string(),
                serde_json::json!({"total": 100}),
                "Order".to_string(),
                "orders".to_string(),
            ),
            "process_order".to_string(),
        );

        assert_eq!(exec.get_backoff_ms(), 100); // initial backoff

        exec.mark_failed("Test error".to_string());
        exec.mark_retry_scheduled();
        assert_eq!(exec.attempt, 2);
        assert_eq!(exec.get_backoff_ms(), 200); // exponential backoff

        exec.mark_retry_scheduled();
        assert_eq!(exec.attempt, 3);
        assert_eq!(exec.get_backoff_ms(), 400);
    }
}
