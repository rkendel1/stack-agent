/// Cryptographic State Hashing
///
/// Produces SHA-256 hashes of canonical state serializations.
/// Invariant: same state → same hash, different state → different hash (except negligible crypto collision)
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Canonical state representation for hashing
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalState {
    /// Records indexed by (collection, record_id)
    pub records: BTreeMap<(String, String), Value>,
}

impl CanonicalState {
    pub fn new() -> Self {
        Self {
            records: BTreeMap::new(),
        }
    }

    /// Add or update a record
    pub fn set_record(&mut self, collection: String, record_id: String, data: Value) {
        self.records.insert((collection, record_id), data);
    }

    /// Get a record
    pub fn get_record(&self, collection: &str, record_id: &str) -> Option<&Value> {
        self.records
            .get(&(collection.to_string(), record_id.to_string()))
    }

    /// Canonical serialization for hashing (deterministic ordering)
    pub fn to_canonical_json(&self) -> Value {
        let mut state_map = serde_json::Map::new();

        for ((collection, record_id), data) in &self.records {
            let key = format!("{}:{}", collection, record_id);
            state_map.insert(key, data.clone());
        }

        Value::Object(state_map)
    }

    /// Compute cryptographic hash of canonical state
    pub fn hash(&self) -> StateHash {
        let canonical = self.to_canonical_json();
        let json_str = canonical.to_string();
        StateHash::compute(&json_str)
    }

    /// Get the size of the canonical state (for metrics)
    pub fn size_bytes(&self) -> usize {
        self.to_canonical_json().to_string().len()
    }
}

impl Default for CanonicalState {
    fn default() -> Self {
        Self::new()
    }
}

/// SHA-256 hash of state
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StateHash(String);

impl StateHash {
    /// Compute hash from canonical JSON string
    pub fn compute(canonical_json: &str) -> Self {
        use sha2::{Digest, Sha256};

        let mut hasher = Sha256::new();
        hasher.update(canonical_json.as_bytes());
        let result = hasher.finalize();

        let hash_hex = format!("{:x}", result);
        StateHash(hash_hex)
    }

    /// Create from string (for testing/recovery)
    pub fn from_hex(hex: String) -> Self {
        StateHash(hex)
    }

    /// Get hex representation
    pub fn as_hex(&self) -> &str {
        &self.0
    }

    /// Verify a canonical state produces this hash
    pub fn verify(&self, canonical_json: &str) -> bool {
        Self::compute(canonical_json) == *self
    }
}

impl std::fmt::Display for StateHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_canonical_state_creation() {
        let mut state = CanonicalState::new();
        state.set_record(
            "users".to_string(),
            "user1".to_string(),
            json!({"name": "Alice", "balance": 100}),
        );

        assert_eq!(state.records.len(), 1);
        let record = state.get_record("users", "user1");
        assert!(record.is_some());
    }

    #[test]
    fn test_state_hash_deterministic() {
        let mut state1 = CanonicalState::new();
        state1.set_record(
            "users".to_string(),
            "user1".to_string(),
            json!({"name": "Alice", "balance": 100}),
        );

        let mut state2 = CanonicalState::new();
        state2.set_record(
            "users".to_string(),
            "user1".to_string(),
            json!({"name": "Alice", "balance": 100}),
        );

        let hash1 = state1.hash();
        let hash2 = state2.hash();

        assert_eq!(hash1, hash2, "Same state must produce same hash");
    }

    #[test]
    fn test_state_hash_collision_resistant() {
        let mut state1 = CanonicalState::new();
        state1.set_record(
            "users".to_string(),
            "user1".to_string(),
            json!({"name": "Alice", "balance": 100}),
        );

        let mut state2 = CanonicalState::new();
        state2.set_record(
            "users".to_string(),
            "user1".to_string(),
            json!({"name": "Alice", "balance": 101}),
        );

        let hash1 = state1.hash();
        let hash2 = state2.hash();

        assert_ne!(hash1, hash2, "Different state must produce different hash");
    }

    #[test]
    fn test_state_hash_field_order_invariant() {
        let mut state1 = CanonicalState::new();
        state1.set_record(
            "users".to_string(),
            "user1".to_string(),
            json!({"name": "Alice", "balance": 100}),
        );

        let mut state2 = CanonicalState::new();
        state2.set_record(
            "users".to_string(),
            "user1".to_string(),
            json!({"balance": 100, "name": "Alice"}),
        );

        let hash1 = state1.hash();
        let hash2 = state2.hash();

        assert_eq!(hash1, hash2, "Field order must not affect hash");
    }

    #[test]
    fn test_state_hash_multiple_records() {
        let mut state = CanonicalState::new();
        state.set_record(
            "users".to_string(),
            "user1".to_string(),
            json!({"name": "Alice"}),
        );
        state.set_record(
            "users".to_string(),
            "user2".to_string(),
            json!({"name": "Bob"}),
        );
        state.set_record(
            "accounts".to_string(),
            "acc1".to_string(),
            json!({"balance": 1000}),
        );

        let hash = state.hash();

        assert_eq!(hash.as_hex().len(), 64, "SHA-256 hex is 64 chars");
        assert!(hash.verify(&state.to_canonical_json().to_string()));
    }

    #[test]
    fn test_state_hash_record_update() {
        let mut state = CanonicalState::new();
        state.set_record(
            "users".to_string(),
            "user1".to_string(),
            json!({"balance": 100}),
        );

        let hash_before = state.hash();

        state.set_record(
            "users".to_string(),
            "user1".to_string(),
            json!({"balance": 200}),
        );

        let hash_after = state.hash();

        assert_ne!(hash_before, hash_after, "Hash changes when state updates");
    }

    #[test]
    fn test_state_hash_empty_state() {
        let state = CanonicalState::new();
        let hash = state.hash();

        assert_eq!(hash.as_hex().len(), 64);
        assert!(hash.verify(&state.to_canonical_json().to_string()));
    }

    #[test]
    fn test_state_hash_verify() {
        let mut state = CanonicalState::new();
        state.set_record(
            "users".to_string(),
            "user1".to_string(),
            json!({"name": "Alice"}),
        );

        let hash = state.hash();
        let canonical_json = state.to_canonical_json().to_string();

        assert!(hash.verify(&canonical_json));
    }

    #[test]
    fn test_canonical_json_ordering() {
        let mut state1 = CanonicalState::new();
        state1.set_record("a".to_string(), "1".to_string(), json!(1));
        state1.set_record("c".to_string(), "3".to_string(), json!(3));
        state1.set_record("b".to_string(), "2".to_string(), json!(2));

        let mut state2 = CanonicalState::new();
        state2.set_record("c".to_string(), "3".to_string(), json!(3));
        state2.set_record("a".to_string(), "1".to_string(), json!(1));
        state2.set_record("b".to_string(), "2".to_string(), json!(2));

        let json1 = state1.to_canonical_json().to_string();
        let json2 = state2.to_canonical_json().to_string();

        assert_eq!(
            json1, json2,
            "Insertion order must not affect canonical JSON"
        );
    }

    #[test]
    fn test_state_size_bytes() {
        let mut state = CanonicalState::new();
        state.set_record(
            "users".to_string(),
            "user1".to_string(),
            json!({"name": "Alice", "balance": 100}),
        );

        let size = state.size_bytes();
        assert!(size > 0);
    }
}
