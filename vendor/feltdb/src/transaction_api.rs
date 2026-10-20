/// Transaction API Layer
///
/// Public-facing transaction management API that integrates:
/// - TransactionExecutor for execution logic
/// - OperationLog for operation persistence
/// - StateTransitionStore for transition durability
/// - Precondition validation
/// - Read/write dependency tracking
use crate::convergence::VectorClock;
use crate::operation_log::{MemoryOperationLog, OperationLogEntry};
use crate::state_transition_store::{MemoryStateTransitionStore, StateTransitionRecord};
use crate::transactions::*;
use std::sync::{Arc, Mutex};

/// Result type for transaction API operations
pub type TxResult<T> = Result<T, TxError>;

/// Transaction API errors
#[derive(Clone, Debug)]
pub enum TxError {
    PreconditionFailed(String),
    ConflictDetected(String),
    OperationDeduplication(String),
    InvalidState(String),
    PersistenceFailure(String),
}

impl std::fmt::Display for TxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TxError::PreconditionFailed(msg) => write!(f, "Precondition failed: {}", msg),
            TxError::ConflictDetected(msg) => write!(f, "Conflict detected: {}", msg),
            TxError::OperationDeduplication(msg) => {
                write!(f, "Operation already executed: {}", msg)
            }
            TxError::InvalidState(msg) => write!(f, "Invalid state: {}", msg),
            TxError::PersistenceFailure(msg) => write!(f, "Persistence failure: {}", msg),
        }
    }
}

impl std::error::Error for TxError {}

/// Transaction manager combining execution, persistence, and validation
pub struct TransactionManager {
    node_id: String,
    executor: TransactionExecutor,
    operation_log: MemoryOperationLog,
    transition_store: MemoryStateTransitionStore,
    current_version: Arc<Mutex<StateVersion>>,
}

impl TransactionManager {
    /// Create a new transaction manager
    pub fn new(node_id: String, initial_version: StateVersion) -> Self {
        Self {
            node_id: node_id.clone(),
            executor: TransactionExecutor::new(node_id),
            operation_log: MemoryOperationLog::new(),
            transition_store: MemoryStateTransitionStore::new(),
            current_version: Arc::new(Mutex::new(initial_version)),
        }
    }

    /// Get the current state version
    pub fn current_state_version(&self) -> StateVersion {
        self.current_version.lock().unwrap().clone()
    }

    /// Execute a transaction with full persistence integration
    pub fn execute_and_commit(
        &mut self,
        transaction_id: String,
        mut operations: Vec<Operation>,
        consistency_contract: ConsistencyContract,
    ) -> TxResult<StateTransitionRecord> {
        // 1. Validate all operations are new (not duplicated in operation log)
        for op in &operations {
            if self.operation_log.contains_operation_id(&op.operation_id) {
                return Err(TxError::OperationDeduplication(format!(
                    "Operation {} already executed",
                    format!("{}:{}", op.operation_id.node_id, op.operation_id.sequence)
                )));
            }
        }

        // 2. Set transaction_id on all operations
        for op in &mut operations {
            op.transaction_id = Some(transaction_id.clone());
        }

        // 3. Execute transaction
        let current_version = self.current_version.lock().unwrap().clone();
        let transition = self.executor.execute_transaction(
            transaction_id.clone(),
            current_version,
            operations.clone(),
            consistency_contract,
        );

        // 4. Validate transition succeeded
        if !transition.is_successful() {
            return Err(TxError::InvalidState(
                "Transaction execution failed".to_string(),
            ));
        }

        // 5. Persist operations to operation log
        for op in &transition.operations {
            self.operation_log
                .append(op.clone())
                .map_err(|e| TxError::PersistenceFailure(e))?;
        }

        // 6. Persist transition to state transition store
        let store_index = self
            .transition_store
            .record_transition(transition.clone())
            .map_err(|e| TxError::PersistenceFailure(e))?;

        // 7. Update current version
        {
            let mut current = self.current_version.lock().unwrap();
            *current = transition.to_version.clone();
        }

        // 8. Retrieve the recorded transition
        let record = self
            .transition_store
            .get_by_index(store_index)
            .ok_or_else(|| {
                TxError::PersistenceFailure("Failed to retrieve recorded transition".to_string())
            })?;

        Ok(record)
    }

    /// Get a recorded operation by ID
    pub fn get_operation(&self, operation_id: &OperationId) -> Option<OperationLogEntry> {
        self.operation_log.get_by_id(operation_id)
    }

    /// Get a recorded transition by transaction ID
    pub fn get_transition(&self, transaction_id: &str) -> Option<StateTransitionRecord> {
        self.transition_store.get_by_transaction_id(transaction_id)
    }

    /// Get the operation log size
    pub fn operation_log_size(&self) -> u64 {
        self.operation_log.size()
    }

    /// Get the transition store size
    pub fn transition_store_size(&self) -> u64 {
        self.transition_store.size()
    }

    /// Rebuild state from operation log (for recovery)
    /// Executes all operations in order to verify state integrity
    pub fn rebuild_from_operation_log(&mut self) -> TxResult<StateVersion> {
        let all_ops = self.operation_log.iter();

        if all_ops.is_empty() {
            return Ok(self.current_state_version());
        }

        let mut current_version = StateVersion::new(VectorClock::new(), "genesis".to_string());

        for entry in all_ops {
            let transition = self.executor.execute_transaction(
                entry.operation.transaction_id.clone().unwrap_or_default(),
                current_version.clone(),
                vec![entry.operation.clone()],
                ConsistencyContract::local(),
            );

            if !transition.is_successful() {
                return Err(TxError::InvalidState(
                    "Failed to rebuild state from operation log".to_string(),
                ));
            }

            current_version = transition.to_version;
        }

        {
            let mut current = self.current_version.lock().unwrap();
            *current = current_version.clone();
        }

        Ok(current_version)
    }

    /// Get the chain of state transitions from a starting version
    pub fn get_transition_chain_from(&self, from_state_hash: &str) -> Vec<StateTransitionRecord> {
        self.transition_store.get_transition_chain(from_state_hash)
    }

    /// Validate preconditions for an operation
    /// (Simple string matching - production would have actual condition evaluation)
    pub fn validate_preconditions(&self, operation: &Operation) -> TxResult<()> {
        if operation.preconditions.is_empty() {
            return Ok(());
        }

        // Placeholder: In production, would evaluate actual state conditions
        // For now, just verify preconditions are syntactically valid
        for (condition_name, condition) in &operation.preconditions {
            if condition.is_empty() {
                return Err(TxError::PreconditionFailed(format!(
                    "Empty condition: {}",
                    condition_name
                )));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transactions::OperationCommand;
    use std::collections::HashMap;

    #[test]
    fn test_create_transaction_manager() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        let manager = TransactionManager::new("node1".to_string(), version);

        assert_eq!(manager.operation_log_size(), 0);
        assert_eq!(manager.transition_store_size(), 0);
    }

    #[test]
    fn test_execute_and_commit_single_operation() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        let mut manager = TransactionManager::new("node1".to_string(), version);

        let op = Operation::new(
            OperationId::new("node1".to_string(), 1),
            manager.current_state_version(),
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
        );

        let result =
            manager.execute_and_commit("txn1".to_string(), vec![op], ConsistencyContract::local());

        assert!(result.is_ok());
        assert_eq!(manager.operation_log_size(), 1);
        assert_eq!(manager.transition_store_size(), 1);
    }

    #[test]
    fn test_duplicate_operation_rejection() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        let mut manager = TransactionManager::new("node1".to_string(), version);

        let op = Operation::new(
            OperationId::new("node1".to_string(), 1),
            manager.current_state_version(),
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
        );

        let _ = manager.execute_and_commit(
            "txn1".to_string(),
            vec![op.clone()],
            ConsistencyContract::local(),
        );

        let result =
            manager.execute_and_commit("txn2".to_string(), vec![op], ConsistencyContract::local());

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already executed"));
    }

    #[test]
    fn test_get_operation_after_commit() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        let mut manager = TransactionManager::new("node1".to_string(), version);

        let op_id = OperationId::new("node1".to_string(), 1);
        let op = Operation::new(
            op_id.clone(),
            manager.current_state_version(),
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
        );

        let _ =
            manager.execute_and_commit("txn1".to_string(), vec![op], ConsistencyContract::local());

        let retrieved = manager.get_operation(&op_id);
        assert!(retrieved.is_some());
    }

    #[test]
    fn test_get_transition_after_commit() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        let mut manager = TransactionManager::new("node1".to_string(), version);

        let op = Operation::new(
            OperationId::new("node1".to_string(), 1),
            manager.current_state_version(),
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
        );

        let _ =
            manager.execute_and_commit("txn1".to_string(), vec![op], ConsistencyContract::local());

        let transition = manager.get_transition("txn1");
        assert!(transition.is_some());
    }

    #[test]
    fn test_precondition_validation() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        let manager = TransactionManager::new("node1".to_string(), version);

        let mut op = Operation::new(
            OperationId::new("node1".to_string(), 1),
            manager.current_state_version(),
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
        );

        op = op.with_preconditions(vec![("count".to_string(), "count >= 2".to_string())]);

        let result = manager.validate_preconditions(&op);
        assert!(result.is_ok());

        let mut bad_op = Operation::new(
            OperationId::new("node1".to_string(), 2),
            manager.current_state_version(),
            "Update".to_string(),
            OperationCommand {
                op_type: "UpdateField".to_string(),
                collection: "test".to_string(),
                record_id: "rec2".to_string(),
                fields: {
                    let mut m = HashMap::new();
                    m.insert("field".to_string(), serde_json::json!("value"));
                    m
                },
            },
            "user".to_string(),
        );

        bad_op = bad_op.with_preconditions(vec![("invalid".to_string(), "".to_string())]);

        let bad_result = manager.validate_preconditions(&bad_op);
        assert!(bad_result.is_err());
    }

    #[test]
    fn test_current_state_version_tracking() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let initial_version = StateVersion::new(vc, "hash0".to_string());
        let initial_hash = initial_version.state_hash.clone();

        let mut manager = TransactionManager::new("node1".to_string(), initial_version);

        assert_eq!(manager.current_state_version().state_hash, initial_hash);

        let op = Operation::new(
            OperationId::new("node1".to_string(), 1),
            manager.current_state_version(),
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
        );

        let _ =
            manager.execute_and_commit("txn1".to_string(), vec![op], ConsistencyContract::local());

        let new_version = manager.current_state_version();
        assert_ne!(new_version.state_hash, initial_hash);
    }
}
