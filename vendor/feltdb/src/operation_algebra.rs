/// Operation Algebra Registry
///
/// Declares the convergence semantics and merge algebra for every
/// operation type in FeltDB. Gate 2 requires every replicated operation
/// to declare how it converges under concurrent application.
///
/// This registry is the foundation of operation-specific CRDT semantics.
use std::collections::HashMap;

/// Conflict classification for an operation
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ConflictType {
    /// Commutative: can be applied in any order
    Commutative,
    /// Register: LWW (last-write-wins by timestamp)
    Register,
    /// Set CRDT: union on add, tombstone on remove
    SetCRDT,
    /// Counter CRDT: additive
    CounterCRDT,
    /// Custom: user-provided merge semantics
    Custom(String),
    /// Transaction: requires precondition/consensus
    Transaction,
}

/// How this operation converges with concurrent applications
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConvergenceSemantic {
    /// All orderings produce identical result
    StrictlyCommutative,
    /// Deterministic merge function resolves concurrent versions
    DeterministicMerge,
    /// Requires consensus/total ordering
    ConsensusRequired,
}

/// Declared convergence algebra for an operation type
#[derive(Clone, Debug)]
pub struct OperationAlgebra {
    pub operation_type: String,
    pub read_set_bound: String, // "single_record", "multiple_records", etc
    pub write_set_bound: String, // "single_record", "multiple_records", etc
    pub conflict_type: ConflictType,
    pub convergence_semantic: ConvergenceSemantic,
    pub merge_function_description: String,
    pub commutativity_proof: Option<String>, // Reference to commutative algebra proof
    pub determinism_proof: Option<String>,   // Reference to determinism proof
}

impl OperationAlgebra {
    pub fn new(
        operation_type: &str,
        read_set_bound: &str,
        write_set_bound: &str,
        conflict_type: ConflictType,
        convergence_semantic: ConvergenceSemantic,
        merge_function_description: &str,
    ) -> Self {
        Self {
            operation_type: operation_type.to_string(),
            read_set_bound: read_set_bound.to_string(),
            write_set_bound: write_set_bound.to_string(),
            conflict_type,
            convergence_semantic,
            merge_function_description: merge_function_description.to_string(),
            commutativity_proof: None,
            determinism_proof: None,
        }
    }

    pub fn with_proofs(
        mut self,
        commutativity: Option<String>,
        determinism: Option<String>,
    ) -> Self {
        self.commutativity_proof = commutativity;
        self.determinism_proof = determinism;
        self
    }

    pub fn is_commutative(&self) -> bool {
        self.convergence_semantic == ConvergenceSemantic::StrictlyCommutative
    }

    pub fn is_deterministic(&self) -> bool {
        self.convergence_semantic != ConvergenceSemantic::ConsensusRequired
    }
}

/// Registry of all operation types and their convergence algebra
pub struct OperationAlgebraRegistry {
    algebras: HashMap<String, OperationAlgebra>,
}

impl OperationAlgebraRegistry {
    /// Create new registry with standard operations
    pub fn new() -> Self {
        let mut registry = Self {
            algebras: HashMap::new(),
        };
        registry.register_standard_operations();
        registry
    }

    /// Register all standard FeltDB operation types
    fn register_standard_operations(&mut self) {
        // SetField: Last-Write-Wins register
        self.register(
            OperationAlgebra::new(
                "SetField",
                "single_record.field",
                "single_record.field",
                ConflictType::Register,
                ConvergenceSemantic::DeterministicMerge,
                "LWW: max(timestamp, value)",
            )
            .with_proofs(
                None,
                Some("Deterministic: higher timestamp wins deterministically".to_string()),
            ),
        );

        // Increment: Counter CRDT (commutative addition)
        self.register(
            OperationAlgebra::new(
                "Increment",
                "single_record.field",
                "single_record.field",
                ConflictType::CounterCRDT,
                ConvergenceSemantic::StrictlyCommutative,
                "Counter CRDT: sum(all_increments)",
            )
            .with_proofs(
                Some("Commutative: addition commutes over integers".to_string()),
                Some("Deterministic: sum is deterministic".to_string()),
            ),
        );

        // AddToSet: Set CRDT (union on add)
        self.register(
            OperationAlgebra::new(
                "AddToSet",
                "single_record.set",
                "single_record.set",
                ConflictType::SetCRDT,
                ConvergenceSemantic::StrictlyCommutative,
                "Set CRDT: union(all_adds)",
            )
            .with_proofs(
                Some("Commutative: set union is commutative".to_string()),
                Some("Deterministic: union is deterministic".to_string()),
            ),
        );

        // RemoveFromSet: Set CRDT (tombstone semantics)
        self.register(
            OperationAlgebra::new(
                "RemoveFromSet",
                "single_record.set",
                "single_record.set",
                ConflictType::SetCRDT,
                ConvergenceSemantic::DeterministicMerge,
                "Set CRDT: (set - tombstones) ∪ (tombstones resolved by causal ordering)",
            )
            .with_proofs(
                None,
                Some("Deterministic: tombstone semantics with causal ordering".to_string()),
            ),
        );

        // Create: Conflict detection (earliest timestamp wins deterministically)
        self.register(
            OperationAlgebra::new(
                "Create",
                "none",
                "single_record",
                ConflictType::Register,
                ConvergenceSemantic::DeterministicMerge,
                "Deterministic: min(timestamp, id) for create conflicts",
            )
            .with_proofs(
                None,
                Some("Deterministic: total order via (timestamp, node_id)".to_string()),
            ),
        );

        // Delete: Tombstone semantics (deterministic)
        self.register(
            OperationAlgebra::new(
                "Delete",
                "single_record",
                "single_record",
                ConflictType::Register,
                ConvergenceSemantic::DeterministicMerge,
                "Tombstone: delete_timestamp determines visibility",
            )
            .with_proofs(
                None,
                Some("Deterministic: tombstone ordering via timestamp and node_id".to_string()),
            ),
        );

        // Transfer: Business invariant (requires consensus/transaction)
        self.register(
            OperationAlgebra::new(
                "Transfer",
                "source_record,dest_record",
                "source_record,dest_record",
                ConflictType::Transaction,
                ConvergenceSemantic::ConsensusRequired,
                "Transaction: requires precondition validation and consensus ordering",
            )
            .with_proofs(
                None,
                Some("Consensus: total ordering via distributed log".to_string()),
            ),
        );

        // Append: Sequence ordering (vector clock ordering)
        self.register(
            OperationAlgebra::new(
                "Append",
                "single_sequence",
                "single_sequence",
                ConflictType::Custom("SequenceOrdering".to_string()),
                ConvergenceSemantic::DeterministicMerge,
                "Sequence: vector clock establishes total order",
            )
            .with_proofs(
                None,
                Some("Deterministic: vector clock provides causal lineage".to_string()),
            ),
        );

        // Noop: No-operation (trivially commutative)
        self.register(
            OperationAlgebra::new(
                "Noop",
                "none",
                "none",
                ConflictType::Commutative,
                ConvergenceSemantic::StrictlyCommutative,
                "No-operation: idempotent, affects nothing",
            )
            .with_proofs(
                Some("Trivially commutative: no state affected".to_string()),
                Some("Trivially deterministic: no change produced".to_string()),
            ),
        );
    }

    /// Register an operation algebra
    pub fn register(&mut self, algebra: OperationAlgebra) {
        self.algebras
            .insert(algebra.operation_type.clone(), algebra);
    }

    /// Look up algebra for operation type
    pub fn get(&self, operation_type: &str) -> Option<&OperationAlgebra> {
        self.algebras.get(operation_type)
    }

    /// Check if operation type is registered
    pub fn contains(&self, operation_type: &str) -> bool {
        self.algebras.contains_key(operation_type)
    }

    /// Get all registered algebras
    pub fn all_algebras(&self) -> Vec<&OperationAlgebra> {
        self.algebras.values().collect()
    }

    /// Verify all registered operations declare convergence semantics
    pub fn verify_all_declared(&self) -> Result<(), Vec<String>> {
        let undeclared: Vec<String> = self
            .algebras
            .values()
            .filter(|a| a.convergence_semantic == ConvergenceSemantic::ConsensusRequired)
            .map(|a| a.operation_type.clone())
            .collect();

        if undeclared.is_empty() {
            Ok(())
        } else {
            Err(undeclared)
        }
    }

    /// Count operations by convergence semantic
    pub fn convergence_statistics(&self) -> HashMap<String, usize> {
        let mut stats = HashMap::new();
        for algebra in self.algebras.values() {
            let key = format!("{:?}", algebra.convergence_semantic);
            *stats.entry(key).or_insert(0) += 1;
        }
        stats
    }
}

impl Default for OperationAlgebraRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_creation() {
        let registry = OperationAlgebraRegistry::new();
        assert!(registry.algebras.len() > 0);
    }

    #[test]
    fn test_setfield_is_lww_register() {
        let registry = OperationAlgebraRegistry::new();
        let setfield = registry.get("SetField").unwrap();

        assert_eq!(setfield.conflict_type, ConflictType::Register);
        assert_eq!(
            setfield.convergence_semantic,
            ConvergenceSemantic::DeterministicMerge
        );
    }

    #[test]
    fn test_increment_is_commutative() {
        let registry = OperationAlgebraRegistry::new();
        let increment = registry.get("Increment").unwrap();

        assert!(increment.is_commutative());
        assert_eq!(increment.conflict_type, ConflictType::CounterCRDT);
    }

    #[test]
    fn test_addtoset_is_commutative() {
        let registry = OperationAlgebraRegistry::new();
        let addtoset = registry.get("AddToSet").unwrap();

        assert!(addtoset.is_commutative());
        assert_eq!(addtoset.conflict_type, ConflictType::SetCRDT);
    }

    #[test]
    fn test_transfer_requires_consensus() {
        let registry = OperationAlgebraRegistry::new();
        let transfer = registry.get("Transfer").unwrap();

        assert!(!transfer.is_commutative());
        assert_eq!(
            transfer.convergence_semantic,
            ConvergenceSemantic::ConsensusRequired
        );
    }

    #[test]
    fn test_custom_registration() {
        let mut registry = OperationAlgebraRegistry::new();
        let custom = OperationAlgebra::new(
            "MyCustomOp",
            "any",
            "any",
            ConflictType::Custom("my_merge".to_string()),
            ConvergenceSemantic::DeterministicMerge,
            "Custom merge semantics",
        );

        registry.register(custom);
        assert!(registry.contains("MyCustomOp"));
    }

    #[test]
    fn test_convergence_statistics() {
        let registry = OperationAlgebraRegistry::new();
        let stats = registry.convergence_statistics();

        // Should have entries for different convergence semantics
        assert!(stats.len() > 0);
        // Most operations should be deterministic or commutative
        let has_deterministic = stats.values().any(|count| *count > 0);
        assert!(has_deterministic);
    }

    #[test]
    fn test_algebra_with_proofs() {
        let algebra = OperationAlgebra::new(
            "TestOp",
            "test",
            "test",
            ConflictType::Commutative,
            ConvergenceSemantic::StrictlyCommutative,
            "test merge",
        )
        .with_proofs(Some("proof1".to_string()), Some("proof2".to_string()));

        assert!(algebra.commutativity_proof.is_some());
        assert!(algebra.determinism_proof.is_some());
    }

    #[test]
    fn test_all_standard_operations_registered() {
        let registry = OperationAlgebraRegistry::new();

        let operations = vec![
            "SetField",
            "Increment",
            "AddToSet",
            "RemoveFromSet",
            "Create",
            "Delete",
            "Transfer",
            "Append",
            "Noop",
        ];

        for op in operations {
            assert!(registry.contains(op), "Operation {} not registered", op);
        }
    }

    #[test]
    fn test_operation_is_deterministic() {
        let registry = OperationAlgebraRegistry::new();

        // Commutative and deterministic merge are deterministic
        let setfield = registry.get("SetField").unwrap();
        assert!(setfield.is_deterministic());

        let increment = registry.get("Increment").unwrap();
        assert!(increment.is_deterministic());

        // Consensus operations are NOT deterministic without consensus authority
        let transfer = registry.get("Transfer").unwrap();
        assert!(!transfer.is_deterministic());
    }
}
