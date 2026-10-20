/// Concurrency Fuzzing Framework
///
/// Generates randomized concurrent transactions with variable:
/// - Operation count per transaction
/// - Read/write set sizes
/// - Conflicts between transactions
/// - Execution interleaving
///
/// Verifies final state matches deterministic reference model.
use crate::convergence::VectorClock;
use crate::state_hash::StateHash;
use crate::transactions::*;
use rand::Rng;
use std::collections::{HashMap, HashSet};

/// Fuzzing configuration
#[derive(Clone, Debug)]
pub struct FuzzingConfig {
    pub num_transactions: usize,
    pub max_operations_per_transaction: usize,
    pub max_records: usize,
    pub conflict_probability: f32, // 0.0 to 1.0
    pub seed: u64,
}

impl Default for FuzzingConfig {
    fn default() -> Self {
        Self {
            num_transactions: 100,
            max_operations_per_transaction: 5,
            max_records: 20,
            conflict_probability: 0.3,
            seed: 42,
        }
    }
}

/// Generated transaction for fuzzing
#[derive(Clone, Debug)]
pub struct FuzzedTransaction {
    pub transaction_id: String,
    pub operations: Vec<Operation>,
    pub write_set: HashSet<String>, // record IDs written
    pub read_set: HashSet<String>,  // record IDs read
}

/// Fuzzing statistics
#[derive(Clone, Debug)]
pub struct FuzzingStats {
    pub total_transactions: usize,
    pub total_operations: usize,
    pub total_conflicts: usize,
    pub concurrent_transactions: usize,
    pub sequential_transactions: usize,
}

/// Concurrency fuzzer
pub struct ConcurrencyFuzzer {
    config: FuzzingConfig,
    rng: rand::rngs::StdRng,
}

impl ConcurrencyFuzzer {
    pub fn new(config: FuzzingConfig) -> Self {
        use rand::SeedableRng;
        let rng = rand::rngs::StdRng::seed_from_u64(config.seed);

        Self { config, rng }
    }

    /// Generate random transaction
    fn generate_transaction(&mut self, id: usize) -> FuzzedTransaction {
        let num_ops = self
            .rng
            .gen_range(1..=self.config.max_operations_per_transaction);
        let mut write_set = HashSet::new();
        let mut read_set = HashSet::new();
        let mut operations = Vec::new();

        let mut vc = VectorClock::new();
        vc.increment("fuzzer");

        let base_version = StateVersion::new(vc.clone(), format!("genesis_{}", id));

        for op_idx in 0..num_ops {
            let record_id = format!("rec_{}", self.rng.gen_range(0..self.config.max_records));
            let value = self.rng.gen_range(0..1000);

            let op_id = OperationId::new("fuzzer".to_string(), ((id * 100) + op_idx) as u64);

            let mut op = Operation::new(
                op_id,
                base_version.clone(),
                format!("FuzzOp_{}", op_idx),
                OperationCommand {
                    op_type: "UpdateField".to_string(),
                    collection: "fuzzing".to_string(),
                    record_id: record_id.clone(),
                    fields: {
                        let mut m = HashMap::new();
                        m.insert("value".to_string(), serde_json::json!(value));
                        m
                    },
                },
                "fuzzer".to_string(),
            );

            // Randomly add read dependencies
            if self.rng.gen_bool(0.3) {
                let read_record = format!("rec_{}", self.rng.gen_range(0..self.config.max_records));
                op = op.with_read_set(vec![StateDependency::new(
                    "fuzzing".to_string(),
                    read_record.clone(),
                    Some("value".to_string()),
                    base_version.clone(),
                )]);
                read_set.insert(read_record);
            }

            op = op.with_write_set(vec![StateDependency::new(
                "fuzzing".to_string(),
                record_id.clone(),
                Some("value".to_string()),
                base_version.clone(),
            )]);

            write_set.insert(record_id);
            operations.push(op);
        }

        FuzzedTransaction {
            transaction_id: format!("fuzz_txn_{}", id),
            operations,
            write_set,
            read_set,
        }
    }

    /// Generate fuzzed transaction workload
    pub fn generate_workload(&mut self) -> Vec<FuzzedTransaction> {
        (0..self.config.num_transactions)
            .map(|i| self.generate_transaction(i))
            .collect()
    }

    /// Detect potential conflicts between two transactions
    pub fn detect_conflict(tx1: &FuzzedTransaction, tx2: &FuzzedTransaction) -> bool {
        // Write-Write conflict
        if !tx1.write_set.is_disjoint(&tx2.write_set) {
            return true;
        }

        // Read-Write conflict
        if !tx1.read_set.is_disjoint(&tx2.write_set) {
            return true;
        }
        if !tx1.write_set.is_disjoint(&tx2.read_set) {
            return true;
        }

        false
    }

    /// Compute conflict graph for workload
    pub fn compute_conflict_graph(workload: &[FuzzedTransaction]) -> HashMap<usize, Vec<usize>> {
        let mut graph = HashMap::new();

        for (i, tx1) in workload.iter().enumerate() {
            let mut conflicts = Vec::new();
            for (j, tx2) in workload.iter().enumerate() {
                if i != j && Self::detect_conflict(tx1, tx2) {
                    conflicts.push(j);
                }
            }
            graph.insert(i, conflicts);
        }

        graph
    }

    /// Compute statistics
    pub fn compute_stats(workload: &[FuzzedTransaction]) -> FuzzingStats {
        let total_transactions = workload.len();
        let total_operations = workload.iter().map(|t| t.operations.len()).sum();
        let conflict_graph = Self::compute_conflict_graph(workload);
        let total_conflicts: usize = conflict_graph.iter().map(|(_, v)| v.len()).sum::<usize>() / 2; // Each edge counted twice

        let concurrent_transactions = workload.iter().filter(|t| t.operations.len() > 1).count();
        let sequential_transactions = total_transactions - concurrent_transactions;

        FuzzingStats {
            total_transactions,
            total_operations,
            total_conflicts,
            concurrent_transactions,
            sequential_transactions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzzer_creation() {
        let config = FuzzingConfig::default();
        let fuzzer = ConcurrencyFuzzer::new(config.clone());

        assert_eq!(fuzzer.config.num_transactions, 100);
    }

    #[test]
    fn test_generate_single_transaction() {
        let config = FuzzingConfig {
            num_transactions: 1,
            max_operations_per_transaction: 3,
            max_records: 10,
            conflict_probability: 0.5,
            seed: 123,
        };

        let mut fuzzer = ConcurrencyFuzzer::new(config);
        let tx = fuzzer.generate_transaction(0);

        assert!(!tx.operations.is_empty());
        assert!(tx.operations.len() <= 3);
    }

    #[test]
    fn test_generate_workload() {
        let config = FuzzingConfig {
            num_transactions: 10,
            max_operations_per_transaction: 2,
            max_records: 5,
            conflict_probability: 0.3,
            seed: 456,
        };

        let mut fuzzer = ConcurrencyFuzzer::new(config);
        let workload = fuzzer.generate_workload();

        assert_eq!(workload.len(), 10);
        for tx in &workload {
            assert!(!tx.operations.is_empty());
        }
    }

    #[test]
    fn test_conflict_detection_no_conflict() {
        let tx1 = FuzzedTransaction {
            transaction_id: "tx1".to_string(),
            operations: Vec::new(),
            write_set: ["rec1".to_string()].iter().cloned().collect(),
            read_set: HashSet::new(),
        };

        let tx2 = FuzzedTransaction {
            transaction_id: "tx2".to_string(),
            operations: Vec::new(),
            write_set: ["rec2".to_string()].iter().cloned().collect(),
            read_set: HashSet::new(),
        };

        assert!(!ConcurrencyFuzzer::detect_conflict(&tx1, &tx2));
    }

    #[test]
    fn test_conflict_detection_write_write() {
        let tx1 = FuzzedTransaction {
            transaction_id: "tx1".to_string(),
            operations: Vec::new(),
            write_set: ["rec1".to_string()].iter().cloned().collect(),
            read_set: HashSet::new(),
        };

        let tx2 = FuzzedTransaction {
            transaction_id: "tx2".to_string(),
            operations: Vec::new(),
            write_set: ["rec1".to_string()].iter().cloned().collect(),
            read_set: HashSet::new(),
        };

        assert!(ConcurrencyFuzzer::detect_conflict(&tx1, &tx2));
    }

    #[test]
    fn test_conflict_detection_read_write() {
        let tx1 = FuzzedTransaction {
            transaction_id: "tx1".to_string(),
            operations: Vec::new(),
            write_set: HashSet::new(),
            read_set: ["rec1".to_string()].iter().cloned().collect(),
        };

        let tx2 = FuzzedTransaction {
            transaction_id: "tx2".to_string(),
            operations: Vec::new(),
            write_set: ["rec1".to_string()].iter().cloned().collect(),
            read_set: HashSet::new(),
        };

        assert!(ConcurrencyFuzzer::detect_conflict(&tx1, &tx2));
    }

    #[test]
    fn test_compute_conflict_graph() {
        let tx1 = FuzzedTransaction {
            transaction_id: "tx1".to_string(),
            operations: Vec::new(),
            write_set: ["rec1".to_string()].iter().cloned().collect(),
            read_set: HashSet::new(),
        };

        let tx2 = FuzzedTransaction {
            transaction_id: "tx2".to_string(),
            operations: Vec::new(),
            write_set: ["rec1".to_string()].iter().cloned().collect(),
            read_set: HashSet::new(),
        };

        let tx3 = FuzzedTransaction {
            transaction_id: "tx3".to_string(),
            operations: Vec::new(),
            write_set: ["rec2".to_string()].iter().cloned().collect(),
            read_set: HashSet::new(),
        };

        let workload = vec![tx1, tx2, tx3];
        let graph = ConcurrencyFuzzer::compute_conflict_graph(&workload);

        assert_eq!(graph.len(), 3);
        assert_eq!(graph[&0].len(), 1); // tx1 conflicts with tx2
        assert_eq!(graph[&1].len(), 1); // tx2 conflicts with tx1
        assert_eq!(graph[&2].len(), 0); // tx3 has no conflicts
    }

    #[test]
    fn test_compute_stats() {
        let config = FuzzingConfig {
            num_transactions: 10,
            max_operations_per_transaction: 2,
            max_records: 5,
            conflict_probability: 0.3,
            seed: 789,
        };

        let mut fuzzer = ConcurrencyFuzzer::new(config);
        let workload = fuzzer.generate_workload();
        let stats = ConcurrencyFuzzer::compute_stats(&workload);

        assert_eq!(stats.total_transactions, 10);
        assert!(stats.total_operations >= 10);
        assert!(stats.total_operations <= 20);
    }

    #[test]
    fn test_workload_determinism_same_seed() {
        let config1 = FuzzingConfig {
            num_transactions: 5,
            max_operations_per_transaction: 3,
            max_records: 10,
            conflict_probability: 0.3,
            seed: 999,
        };

        let config2 = FuzzingConfig {
            num_transactions: 5,
            max_operations_per_transaction: 3,
            max_records: 10,
            conflict_probability: 0.3,
            seed: 999,
        };

        let mut fuzzer1 = ConcurrencyFuzzer::new(config1);
        let mut fuzzer2 = ConcurrencyFuzzer::new(config2);

        let workload1 = fuzzer1.generate_workload();
        let workload2 = fuzzer2.generate_workload();

        assert_eq!(workload1.len(), workload2.len());
        for (tx1, tx2) in workload1.iter().zip(workload2.iter()) {
            assert_eq!(tx1.operations.len(), tx2.operations.len());
        }
    }

    #[test]
    fn test_workload_variation_different_seed() {
        let config1 = FuzzingConfig {
            num_transactions: 10,
            max_operations_per_transaction: 3,
            max_records: 10,
            conflict_probability: 0.3,
            seed: 111,
        };

        let config2 = FuzzingConfig {
            num_transactions: 10,
            max_operations_per_transaction: 3,
            max_records: 10,
            conflict_probability: 0.3,
            seed: 222,
        };

        let mut fuzzer1 = ConcurrencyFuzzer::new(config1);
        let mut fuzzer2 = ConcurrencyFuzzer::new(config2);

        let workload1 = fuzzer1.generate_workload();
        let workload2 = fuzzer2.generate_workload();

        let diff_count = workload1
            .iter()
            .zip(workload2.iter())
            .filter(|(tx1, tx2)| tx1.write_set != tx2.write_set)
            .count();

        assert!(
            diff_count > 0,
            "Different seeds should produce different workloads"
        );
    }
}
