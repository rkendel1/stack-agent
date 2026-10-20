use crate::convergence::VectorClock;
use serde::{Deserialize, Serialize};
/// Transaction system - Verified state transitions with atomicity guarantees
///
/// Implements the STATE_MODEL.md specification:
/// - Optimistic concurrency with commit-time conflict detection
/// - Deterministic operation execution
/// - Read/write dependency tracking
/// - Configurable consistency contracts
/// - Authoritative operation log with derived indexes
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OperationId {
    pub node_id: String,
    pub sequence: u64,
}

impl OperationId {
    pub fn new(node_id: String, sequence: u64) -> Self {
        OperationId { node_id, sequence }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateVersion {
    pub vector_clock: VectorClock,
    pub state_hash: String,
}

impl StateVersion {
    pub fn new(vector_clock: VectorClock, state_hash: String) -> Self {
        StateVersion {
            vector_clock,
            state_hash,
        }
    }

    pub fn dominates(&self, other: &StateVersion) -> bool {
        self.vector_clock != other.vector_clock
            && other.vector_clock.happens_before(&self.vector_clock)
    }

    pub fn concurrent_with(&self, other: &StateVersion) -> bool {
        self.vector_clock.is_concurrent(&other.vector_clock)
    }

    pub fn equal_to(&self, other: &StateVersion) -> bool {
        self.vector_clock == other.vector_clock && self.state_hash == other.state_hash
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitId {
    pub consensus_group_id: String,
    pub commit_index: u64,
    pub transaction_id: String,
}

impl CommitId {
    pub fn new(consensus_group_id: String, commit_index: u64, transaction_id: String) -> Self {
        CommitId {
            consensus_group_id,
            commit_index,
            transaction_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsistencyLevel {
    Local,
    Eventual,
    Causal,
    Linearizable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConsistencyContract {
    pub level: ConsistencyLevel,
    pub timeout_ms: Option<u64>,
    pub required_acks: Option<usize>,
}

impl ConsistencyContract {
    pub fn local() -> Self {
        ConsistencyContract {
            level: ConsistencyLevel::Local,
            timeout_ms: None,
            required_acks: None,
        }
    }

    pub fn eventual() -> Self {
        ConsistencyContract {
            level: ConsistencyLevel::Eventual,
            timeout_ms: Some(5000),
            required_acks: None,
        }
    }

    pub fn causal() -> Self {
        ConsistencyContract {
            level: ConsistencyLevel::Causal,
            timeout_ms: Some(1000),
            required_acks: None,
        }
    }

    pub fn linearizable(required_acks: usize) -> Self {
        ConsistencyContract {
            level: ConsistencyLevel::Linearizable,
            timeout_ms: Some(10000),
            required_acks: Some(required_acks),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDependency {
    pub collection: String,
    pub record_id: String,
    pub field: Option<String>,
    pub version: StateVersion,
}

impl StateDependency {
    pub fn new(
        collection: String,
        record_id: String,
        field: Option<String>,
        version: StateVersion,
    ) -> Self {
        StateDependency {
            collection,
            record_id,
            field,
            version,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationCommand {
    pub op_type: String,
    pub collection: String,
    pub record_id: String,
    pub fields: HashMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Operation {
    pub operation_id: OperationId,
    pub parent_version: StateVersion,
    pub transaction_id: Option<String>,
    pub intent: String,
    pub preconditions: Vec<(String, String)>,
    pub read_set: Vec<StateDependency>,
    pub write_set: Vec<StateDependency>,
    pub command: OperationCommand,
    pub timestamp: u64,
    pub initiator: String,
    pub source: String,
    pub evidence: Option<HashMap<String, serde_json::Value>>,
}

impl Operation {
    pub fn new(
        operation_id: OperationId,
        parent_version: StateVersion,
        intent: String,
        command: OperationCommand,
        initiator: String,
    ) -> Self {
        Operation {
            operation_id,
            parent_version,
            transaction_id: None,
            intent,
            preconditions: Vec::new(),
            read_set: Vec::new(),
            write_set: Vec::new(),
            command,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
            initiator,
            source: "local".to_string(),
            evidence: None,
        }
    }

    pub fn with_preconditions(mut self, preconditions: Vec<(String, String)>) -> Self {
        self.preconditions = preconditions;
        self
    }

    pub fn with_read_set(mut self, read_set: Vec<StateDependency>) -> Self {
        self.read_set = read_set;
        self
    }

    pub fn with_write_set(mut self, write_set: Vec<StateDependency>) -> Self {
        self.write_set = write_set;
        self
    }

    pub fn with_transaction_id(mut self, transaction_id: String) -> Self {
        self.transaction_id = Some(transaction_id);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransitionResult {
    Success,
    Aborted { reason: String },
    Failed { reason: String },
}

#[derive(Clone, Debug)]
pub struct StateTransition {
    pub from_version: StateVersion,
    pub operations: Vec<Operation>,
    pub transaction_id: Option<String>,
    pub result: TransitionResult,
    pub to_version: StateVersion,
    pub commit_id: Option<CommitId>,
    pub consistency_contract: ConsistencyContract,
}

impl StateTransition {
    pub fn new(
        from_version: StateVersion,
        operations: Vec<Operation>,
        to_version: StateVersion,
        consistency_contract: ConsistencyContract,
    ) -> Self {
        StateTransition {
            from_version,
            transaction_id: operations.first().and_then(|o| o.transaction_id.clone()),
            result: TransitionResult::Success,
            operations,
            to_version,
            commit_id: None,
            consistency_contract,
        }
    }

    pub fn aborted(
        from_version: StateVersion,
        reason: String,
        consistency_contract: ConsistencyContract,
    ) -> Self {
        StateTransition {
            from_version: from_version.clone(),
            operations: Vec::new(),
            transaction_id: None,
            result: TransitionResult::Aborted { reason },
            to_version: from_version,
            commit_id: None,
            consistency_contract,
        }
    }

    pub fn is_successful(&self) -> bool {
        matches!(self.result, TransitionResult::Success)
    }
}

pub struct TransactionExecutor {
    node_id: String,
    local_clock: u64,
    applied_operations: HashSet<OperationId>,
}

impl TransactionExecutor {
    pub fn new(node_id: String) -> Self {
        TransactionExecutor {
            node_id,
            local_clock: 0,
            applied_operations: HashSet::new(),
        }
    }

    pub fn execute_transaction(
        &mut self,
        _transaction_id: String,
        parent_version: StateVersion,
        operations: Vec<Operation>,
        consistency_contract: ConsistencyContract,
    ) -> StateTransition {
        for op in &operations {
            if self.applied_operations.contains(&op.operation_id) {
                return StateTransition::aborted(
                    parent_version,
                    format!("Operation {:?} already applied", op.operation_id),
                    consistency_contract,
                );
            }
        }

        let mut to_version = parent_version.clone();
        self.local_clock += 1;

        for op in &operations {
            self.applied_operations.insert(op.operation_id.clone());

            to_version.vector_clock.increment(&self.node_id);

            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            use std::hash::{Hash, Hasher};
            format!("{}{:?}", to_version.state_hash, op.command).hash(&mut hasher);
            to_version.state_hash = format!("{:x}", hasher.finish());
        }

        StateTransition::new(parent_version, operations, to_version, consistency_contract)
    }

    pub fn mark_applied(&mut self, operation_id: &OperationId) {
        self.applied_operations.insert(operation_id.clone());
    }

    pub fn is_applied(&self, operation_id: &OperationId) -> bool {
        self.applied_operations.contains(operation_id)
    }
}

pub struct CommitValidator;

impl CommitValidator {
    pub fn validate_transition(
        transition: &StateTransition,
        current_state: &StateVersion,
    ) -> Result<(), String> {
        match transition.result {
            TransitionResult::Success => {
                if transition.from_version.equal_to(current_state)
                    || transition.from_version.dominates(current_state)
                {
                    Ok(())
                } else if transition.from_version.concurrent_with(current_state) {
                    Err(format!(
                        "State version mismatch: concurrent states detected"
                    ))
                } else {
                    Err(format!(
                        "State version stale: transition from {:?} but current is {:?}",
                        transition.from_version, current_state
                    ))
                }
            }
            TransitionResult::Aborted { ref reason } => {
                Err(format!("Transaction aborted: {}", reason))
            }
            TransitionResult::Failed { ref reason } => {
                Err(format!("Transaction failed: {}", reason))
            }
        }
    }

    pub fn detect_conflicts(
        transition: &StateTransition,
        concurrent_transitions: &[StateTransition],
    ) -> Vec<String> {
        let mut conflicts = Vec::new();

        for other in concurrent_transitions {
            if !transition.is_successful() || !other.is_successful() {
                continue;
            }

            for op in &transition.operations {
                for other_op in &other.operations {
                    if op.operation_id == other_op.operation_id {
                        conflicts.push(format!("Duplicate operation: {:?}", op.operation_id));
                        continue;
                    }

                    let op_writes: HashSet<_> = op
                        .write_set
                        .iter()
                        .map(|d| (d.collection.clone(), d.record_id.clone()))
                        .collect();
                    let other_writes: HashSet<_> = other_op
                        .write_set
                        .iter()
                        .map(|d| (d.collection.clone(), d.record_id.clone()))
                        .collect();

                    if !op_writes.is_disjoint(&other_writes) {
                        conflicts.push(format!(
                            "Write-write conflict: {:?} and {:?}",
                            op.intent, other_op.intent
                        ));
                    }

                    let op_reads: HashSet<_> = op
                        .read_set
                        .iter()
                        .map(|d| (d.collection.clone(), d.record_id.clone()))
                        .collect();
                    let other_reads: HashSet<_> = other_op
                        .read_set
                        .iter()
                        .map(|d| (d.collection.clone(), d.record_id.clone()))
                        .collect();

                    if !op_reads.is_disjoint(&other_writes) {
                        conflicts.push(format!(
                            "Read-after-write conflict: {:?} reads what {:?} writes",
                            op.intent, other_op.intent
                        ));
                    }

                    if !op_writes.is_disjoint(&other_reads) {
                        conflicts.push(format!(
                            "Write-after-read conflict: {:?} writes what {:?} reads",
                            op.intent, other_op.intent
                        ));
                    }
                }
            }
        }

        conflicts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_operation_id_deduplication() {
        let mut executor = TransactionExecutor::new("node1".to_string());
        let op_id = OperationId::new("node1".to_string(), 1);

        assert!(!executor.is_applied(&op_id));
        executor.mark_applied(&op_id);
        assert!(executor.is_applied(&op_id));
    }

    #[test]
    fn test_state_version_domination() {
        let mut vc1 = VectorClock::new();
        vc1.increment("node1");
        vc1.increment("node1");
        let v1 = StateVersion::new(vc1, "hash1".to_string());

        let mut vc2 = VectorClock::new();
        vc2.increment("node1");
        let v2 = StateVersion::new(vc2, "hash2".to_string());

        assert!(v1.dominates(&v2));
        assert!(!v2.dominates(&v1));
    }

    #[test]
    fn test_concurrent_state_versions() {
        let mut vc1 = VectorClock::new();
        vc1.increment("node1");
        let v1 = StateVersion::new(vc1, "hash1".to_string());

        let mut vc2 = VectorClock::new();
        vc2.increment("node2");
        let v2 = StateVersion::new(vc2, "hash2".to_string());

        assert!(v1.concurrent_with(&v2));
        assert!(v2.concurrent_with(&v1));
    }

    #[test]
    fn test_exactly_once_semantics() {
        let mut executor = TransactionExecutor::new("node1".to_string());
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "initial_hash".to_string());

        let op_id = OperationId::new("node1".to_string(), 1);
        let command = OperationCommand {
            op_type: "SetField".to_string(),
            collection: "orders".to_string(),
            record_id: "123".to_string(),
            fields: {
                let mut m = HashMap::new();
                m.insert("status".to_string(), serde_json::json!("paid"));
                m
            },
        };

        let op = Operation::new(
            op_id.clone(),
            version.clone(),
            "MarkOrderPaid".to_string(),
            command,
            "system".to_string(),
        );

        let transition1 = executor.execute_transaction(
            "tx1".to_string(),
            version.clone(),
            vec![op.clone()],
            ConsistencyContract::local(),
        );

        assert!(transition1.is_successful());
        assert_eq!(transition1.operations.len(), 1);

        let transition2 = executor.execute_transaction(
            "tx2".to_string(),
            version,
            vec![op],
            ConsistencyContract::local(),
        );

        assert!(!transition2.is_successful());
    }

    #[test]
    fn test_conflict_detection() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "initial_hash".to_string());

        let mut cmd1 = HashMap::new();
        cmd1.insert("amount".to_string(), serde_json::json!(100));

        let op1 = Operation::new(
            OperationId::new("node1".to_string(), 1),
            version.clone(),
            "Debit".to_string(),
            OperationCommand {
                op_type: "UpdateField".to_string(),
                collection: "accounts".to_string(),
                record_id: "acc1".to_string(),
                fields: cmd1,
            },
            "system".to_string(),
        )
        .with_write_set(vec![StateDependency::new(
            "accounts".to_string(),
            "acc1".to_string(),
            Some("amount".to_string()),
            version.clone(),
        )]);

        let mut cmd2 = HashMap::new();
        cmd2.insert("amount".to_string(), serde_json::json!(50));

        let op2 = Operation::new(
            OperationId::new("node1".to_string(), 2),
            version.clone(),
            "Credit".to_string(),
            OperationCommand {
                op_type: "UpdateField".to_string(),
                collection: "accounts".to_string(),
                record_id: "acc1".to_string(),
                fields: cmd2,
            },
            "system".to_string(),
        )
        .with_write_set(vec![StateDependency::new(
            "accounts".to_string(),
            "acc1".to_string(),
            Some("amount".to_string()),
            version.clone(),
        )]);

        let mut to_vc = version.vector_clock.clone();
        to_vc.increment("node1");
        let to_version = StateVersion::new(to_vc, "new_hash".to_string());

        let transition1 = StateTransition::new(
            version.clone(),
            vec![op1],
            to_version.clone(),
            ConsistencyContract::causal(),
        );

        let transition2 = StateTransition::new(
            version,
            vec![op2],
            to_version,
            ConsistencyContract::causal(),
        );

        let conflicts = CommitValidator::detect_conflicts(&transition1, &[transition2]);
        assert!(!conflicts.is_empty());
        assert!(conflicts.iter().any(|c| c.contains("Write-write conflict")));
    }

    #[test]
    fn test_consistency_contract_creation() {
        let local = ConsistencyContract::local();
        assert_eq!(local.level, ConsistencyLevel::Local);
        assert_eq!(local.timeout_ms, None);

        let causal = ConsistencyContract::causal();
        assert_eq!(causal.level, ConsistencyLevel::Causal);
        assert_eq!(causal.timeout_ms, Some(1000));

        let linearizable = ConsistencyContract::linearizable(3);
        assert_eq!(linearizable.level, ConsistencyLevel::Linearizable);
        assert_eq!(linearizable.required_acks, Some(3));
    }

    #[test]
    fn test_transition_validation() {
        let mut vc = VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash".to_string());

        let command = OperationCommand {
            op_type: "SetField".to_string(),
            collection: "test".to_string(),
            record_id: "1".to_string(),
            fields: HashMap::new(),
        };

        let op = Operation::new(
            OperationId::new("node1".to_string(), 1),
            version.clone(),
            "Test".to_string(),
            command,
            "system".to_string(),
        );

        let mut to_vc = version.vector_clock.clone();
        to_vc.increment("node1");
        let to_version = StateVersion::new(to_vc, "new_hash".to_string());

        let transition = StateTransition::new(
            version.clone(),
            vec![op],
            to_version,
            ConsistencyContract::local(),
        );

        assert!(CommitValidator::validate_transition(&transition, &version).is_ok());
    }
}
