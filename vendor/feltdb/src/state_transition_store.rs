/// State Transition Store
///
/// Persistent storage of executed state transitions.
/// Transitions are immutable once committed; this is the authoritative record of
/// what state changes have occurred and in what order.
use crate::transactions::{StateTransition, StateVersion};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Persisted state transition record
#[derive(Clone, Debug)]
pub struct StateTransitionRecord {
    pub store_index: u64,
    pub transition: StateTransition,
    pub timestamp_ms: u64,
}

/// In-memory state transition store (for testing/development)
/// For production, implement FileStateTransitionStore or DatabaseStateTransitionStore
#[derive(Clone)]
pub struct MemoryStateTransitionStore {
    records: Arc<Mutex<Vec<StateTransitionRecord>>>,
    by_transaction_id: Arc<Mutex<HashMap<String, u64>>>,
    by_state_version: Arc<Mutex<HashMap<String, u64>>>,
}

impl MemoryStateTransitionStore {
    pub fn new() -> Self {
        Self {
            records: Arc::new(Mutex::new(Vec::new())),
            by_transaction_id: Arc::new(Mutex::new(HashMap::new())),
            by_state_version: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Record a committed state transition
    /// Returns the store index assigned to this transition
    pub fn record_transition(&self, transition: StateTransition) -> Result<u64, String> {
        let mut records = self.records.lock().unwrap();
        let mut by_txn = self.by_transaction_id.lock().unwrap();
        let mut by_version = self.by_state_version.lock().unwrap();

        // Ensure transition is successful before recording
        if !transition.is_successful() {
            return Err("Cannot record unsuccessful transition".to_string());
        }

        // Index by transaction_id if present
        if let Some(ref txn_id) = transition.transaction_id {
            if by_txn.contains_key(txn_id) {
                return Err(format!("Transaction already recorded: {}", txn_id));
            }
        }

        let store_index = records.len() as u64;
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let record = StateTransitionRecord {
            store_index,
            transition: transition.clone(),
            timestamp_ms,
        };

        // Index by transaction_id
        if let Some(ref txn_id) = transition.transaction_id {
            by_txn.insert(txn_id.clone(), store_index);
        }

        // Index by to_version (state_hash for quick lookup)
        by_version.insert(transition.to_version.state_hash.clone(), store_index);

        records.push(record);

        Ok(store_index)
    }

    /// Get a transition by transaction ID
    pub fn get_by_transaction_id(&self, transaction_id: &str) -> Option<StateTransitionRecord> {
        let by_txn = self.by_transaction_id.lock().unwrap();
        if let Some(&store_index) = by_txn.get(transaction_id) {
            let records = self.records.lock().unwrap();
            records.get(store_index as usize).cloned()
        } else {
            None
        }
    }

    /// Get a transition by store index
    pub fn get_by_index(&self, store_index: u64) -> Option<StateTransitionRecord> {
        let records = self.records.lock().unwrap();
        records.get(store_index as usize).cloned()
    }

    /// Get transitions in range [start_index, end_index)
    pub fn get_range(&self, start_index: u64, end_index: u64) -> Vec<StateTransitionRecord> {
        let records = self.records.lock().unwrap();
        records
            .iter()
            .skip(start_index as usize)
            .take((end_index - start_index) as usize)
            .cloned()
            .collect()
    }

    /// Find transitions that led from a specific state version
    pub fn get_from_version(&self, state_hash: &str) -> Vec<StateTransitionRecord> {
        let records = self.records.lock().unwrap();
        records
            .iter()
            .filter(|r| r.transition.from_version.state_hash == state_hash)
            .cloned()
            .collect()
    }

    /// Find the transition that led to a specific state version
    pub fn get_to_version(&self, state_hash: &str) -> Option<StateTransitionRecord> {
        let by_version = self.by_state_version.lock().unwrap();
        if let Some(&store_index) = by_version.get(state_hash) {
            let records = self.records.lock().unwrap();
            records.get(store_index as usize).cloned()
        } else {
            None
        }
    }

    /// Get the current size of the store (number of transitions)
    pub fn size(&self) -> u64 {
        let records = self.records.lock().unwrap();
        records.len() as u64
    }

    /// Iterate over all transitions in the store
    pub fn iter(&self) -> Vec<StateTransitionRecord> {
        let records = self.records.lock().unwrap();
        records.clone()
    }

    /// Check if a transaction has been recorded
    pub fn contains_transaction_id(&self, transaction_id: &str) -> bool {
        let by_txn = self.by_transaction_id.lock().unwrap();
        by_txn.contains_key(transaction_id)
    }

    /// Get the chain of transitions from a starting state to the current state
    /// Returns transitions in order
    pub fn get_transition_chain(&self, from_state_hash: &str) -> Vec<StateTransitionRecord> {
        let records = self.records.lock().unwrap();
        let mut chain = Vec::new();
        let mut current_hash = from_state_hash.to_string();

        for record in records.iter() {
            if record.transition.from_version.state_hash == current_hash {
                current_hash = record.transition.to_version.state_hash.clone();
                chain.push(record.clone());
            }
        }

        chain
    }

    /// Truncate the store at the given index (for recovery/compaction)
    /// Keeps records with store_index < truncate_at
    pub fn truncate(&self, truncate_at: u64) -> Result<(), String> {
        let mut records = self.records.lock().unwrap();
        let mut by_txn = self.by_transaction_id.lock().unwrap();
        let mut by_version = self.by_state_version.lock().unwrap();

        if truncate_at > records.len() as u64 {
            return Err("Truncate index beyond store size".to_string());
        }

        let removed: Vec<_> = records.drain(truncate_at as usize..).collect();
        for record in removed {
            if let Some(ref txn_id) = record.transition.transaction_id {
                by_txn.remove(txn_id);
            }
            by_version.remove(&record.transition.to_version.state_hash);
        }

        Ok(())
    }
}

impl Default for MemoryStateTransitionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convergence::VectorClock;
    use crate::transactions::{ConsistencyContract, Operation, OperationCommand, OperationId};
    use std::collections::HashMap;

    fn create_test_transition(
        from_hash: &str,
        to_hash: &str,
        txn_id: Option<&str>,
    ) -> StateTransition {
        let mut from_vc = VectorClock::new();
        from_vc.increment("node1");
        let from_version = StateVersion::new(from_vc, from_hash.to_string());

        let mut to_vc = VectorClock::new();
        to_vc.increment("node1");
        to_vc.increment("node1");
        let to_version = StateVersion::new(to_vc, to_hash.to_string());

        let mut op = Operation::new(
            OperationId::new("node1".to_string(), 1),
            from_version.clone(),
            "Test".to_string(),
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

        if let Some(txn_id_str) = txn_id {
            op = op.with_transaction_id(txn_id_str.to_string());
        }

        StateTransition::new(
            from_version,
            vec![op],
            to_version,
            ConsistencyContract::causal(),
        )
    }

    #[test]
    fn test_record_and_retrieve() {
        let store = MemoryStateTransitionStore::new();
        let transition = create_test_transition("hash0", "hash1", Some("txn1"));

        let idx = store.record_transition(transition).unwrap();

        assert_eq!(idx, 0);
        assert!(store.get_by_transaction_id("txn1").is_some());
        assert_eq!(store.size(), 1);
    }

    #[test]
    fn test_duplicate_transaction_rejection() {
        let store = MemoryStateTransitionStore::new();
        let transition1 = create_test_transition("hash0", "hash1", Some("txn1"));
        let transition2 = create_test_transition("hash1", "hash2", Some("txn1"));

        let _ = store.record_transition(transition1).unwrap();
        let result = store.record_transition(transition2);

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already recorded"));
    }

    #[test]
    fn test_get_by_index() {
        let store = MemoryStateTransitionStore::new();
        let transition1 = create_test_transition("hash0", "hash1", Some("txn1"));
        let transition2 = create_test_transition("hash1", "hash2", Some("txn2"));

        store.record_transition(transition1).unwrap();
        store.record_transition(transition2).unwrap();

        let record1 = store.get_by_index(0).unwrap();
        assert_eq!(record1.store_index, 0);

        let record2 = store.get_by_index(1).unwrap();
        assert_eq!(record2.store_index, 1);
    }

    #[test]
    fn test_get_from_version() {
        let store = MemoryStateTransitionStore::new();
        let transition1 = create_test_transition("hash0", "hash1", Some("txn1"));
        let transition2 = create_test_transition("hash0", "hash2", Some("txn2"));
        let transition3 = create_test_transition("hash1", "hash3", Some("txn3"));

        store.record_transition(transition1).unwrap();
        store.record_transition(transition2).unwrap();
        store.record_transition(transition3).unwrap();

        let from_hash0 = store.get_from_version("hash0");
        assert_eq!(from_hash0.len(), 2);

        let from_hash1 = store.get_from_version("hash1");
        assert_eq!(from_hash1.len(), 1);
    }

    #[test]
    fn test_get_to_version() {
        let store = MemoryStateTransitionStore::new();
        let transition = create_test_transition("hash0", "hash1", Some("txn1"));

        store.record_transition(transition).unwrap();

        let record = store.get_to_version("hash1").unwrap();
        assert_eq!(record.transition.to_version.state_hash, "hash1");
    }

    #[test]
    fn test_get_transition_chain() {
        let store = MemoryStateTransitionStore::new();
        let t1 = create_test_transition("hash0", "hash1", Some("txn1"));
        let t2 = create_test_transition("hash1", "hash2", Some("txn2"));
        let t3 = create_test_transition("hash2", "hash3", Some("txn3"));

        store.record_transition(t1).unwrap();
        store.record_transition(t2).unwrap();
        store.record_transition(t3).unwrap();

        let chain = store.get_transition_chain("hash0");
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].transition.to_version.state_hash, "hash1");
        assert_eq!(chain[1].transition.to_version.state_hash, "hash2");
        assert_eq!(chain[2].transition.to_version.state_hash, "hash3");
    }

    #[test]
    fn test_contains_transaction_id() {
        let store = MemoryStateTransitionStore::new();
        let transition = create_test_transition("hash0", "hash1", Some("txn1"));

        store.record_transition(transition).unwrap();

        assert!(store.contains_transaction_id("txn1"));
        assert!(!store.contains_transaction_id("txn999"));
    }

    #[test]
    fn test_truncate() {
        let store = MemoryStateTransitionStore::new();
        let t1 = create_test_transition("hash0", "hash1", Some("txn1"));
        let t2 = create_test_transition("hash1", "hash2", Some("txn2"));
        let t3 = create_test_transition("hash2", "hash3", Some("txn3"));

        store.record_transition(t1).unwrap();
        store.record_transition(t2).unwrap();
        store.record_transition(t3).unwrap();

        assert_eq!(store.size(), 3);
        store.truncate(2).unwrap();
        assert_eq!(store.size(), 2);

        assert!(store.contains_transaction_id("txn1"));
        assert!(store.contains_transaction_id("txn2"));
        assert!(!store.contains_transaction_id("txn3"));
    }

    #[test]
    fn test_range_query() {
        let store = MemoryStateTransitionStore::new();
        for i in 0..5 {
            let t = create_test_transition(
                &format!("hash{}", i),
                &format!("hash{}", i + 1),
                Some(&format!("txn{}", i)),
            );
            store.record_transition(t).unwrap();
        }

        let range = store.get_range(1, 4);
        assert_eq!(range.len(), 3);
        assert_eq!(range[0].store_index, 1);
        assert_eq!(range[2].store_index, 3);
    }
}
