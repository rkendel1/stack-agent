use crate::now_ms;
/// Operation log module for durable append-only mutations
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// A durable operation that can be appended to the log
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    /// Unique operation ID (monotonically increasing)
    pub op_id: u64,
    /// Instance ID - unique identifier for the FeltDB instance that created this operation
    pub instance_id: String,
    /// Sequence number - monotonically increasing sequence per instance
    pub sequence: u64,
    /// Type of operation (insert, update, delete)
    pub op_type: OperationType,
    /// The key being operated on (e.g., "users:123")
    pub key: String,
    /// The value (for insert/update operations)
    pub value: Option<Value>,
    /// Timestamp in milliseconds since UNIX epoch
    pub timestamp_ms: u128,
    /// The data type being operated on
    pub rust_type: String,
    /// Capability/namespace
    pub capability: String,
    /// Content hash - deterministic hash of the operation's content for deduplication
    pub content_hash: String,
    /// Vector clock for causal tracking (optional for backward compatibility)
    #[serde(default)]
    pub vector_clock: Option<std::collections::HashMap<String, u64>>,
    /// The historical revision this operation produced on the authority that
    /// created it.
    ///
    /// Carried so a peer can **reconstruct the same revision** rather than mint
    /// a local substitute for the resulting state. Without it a replica can
    /// only reproduce what is current, and two authorities end up agreeing
    /// about what *is* while disagreeing about what *happened*.
    ///
    /// Additive and outside `content_hash`, exactly like `vector_clock`: an
    /// operation from a build that does not send it still verifies, and simply
    /// carries no history.
    #[serde(default)]
    pub revision: Option<RevisionProvenance>,
}

/// The revision an operation produced, as its originating authority recorded it.
///
/// Everything a peer needs to rebuild the identical `StateRevision`. The content
/// is not repeated — it is the operation's own value — so this adds a bounded
/// amount to an operation regardless of record size.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RevisionProvenance {
    /// The resource whose history this revision belongs to.
    pub resource: String,
    /// The revision identity the originating authority committed.
    pub id: String,
    /// Its parent, as the origin recorded it — **not** whatever is current on
    /// the peer. This is what preserves forks.
    pub parent_id: Option<String>,
    /// Its position in the resource's history, preserved rather than reassigned.
    pub sequence: u64,
    /// The content identity, so a peer can check that the value it received is
    /// the value the revision was computed from.
    pub content_id: String,
    /// Who committed the historical fact. Never rewritten to the peer.
    pub authority: String,
}

/// Types of mutations that become durable operations
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OperationType {
    /// Insert a new record
    Insert,
    /// Update an existing record
    Update,
    /// Delete an existing record
    Delete,
}

impl Operation {
    /// Create a new insert operation
    pub fn insert(
        op_id: u64,
        instance_id: String,
        sequence: u64,
        key: String,
        value: Value,
        rust_type: String,
        capability: String,
    ) -> Self {
        let op = Self {
            op_id,
            instance_id,
            sequence,
            op_type: OperationType::Insert,
            key,
            value: Some(value),
            timestamp_ms: now_ms(),
            rust_type,
            capability,
            content_hash: String::new(), // Will be computed below
            vector_clock: None,
            revision: None,
        };
        let mut result = op;
        result.content_hash = result.compute_content_hash();
        result
    }

    /// Create a new update operation
    pub fn update(
        op_id: u64,
        instance_id: String,
        sequence: u64,
        key: String,
        value: Value,
        rust_type: String,
        capability: String,
    ) -> Self {
        let op = Self {
            op_id,
            instance_id,
            sequence,
            op_type: OperationType::Update,
            key,
            value: Some(value),
            timestamp_ms: now_ms(),
            rust_type,
            capability,
            content_hash: String::new(), // Will be computed below
            vector_clock: None,
            revision: None,
        };
        let mut result = op;
        result.content_hash = result.compute_content_hash();
        result
    }

    /// Create a new delete operation
    pub fn delete(
        op_id: u64,
        instance_id: String,
        sequence: u64,
        key: String,
        rust_type: String,
        capability: String,
    ) -> Self {
        let op = Self {
            op_id,
            instance_id,
            sequence,
            op_type: OperationType::Delete,
            key,
            value: None,
            timestamp_ms: now_ms(),
            rust_type,
            capability,
            content_hash: String::new(), // Will be computed below
            vector_clock: None,
            revision: None,
        };
        let mut result = op;
        result.content_hash = result.compute_content_hash();
        result
    }

    /// Compute deterministic content hash for this operation
    /// This is used for deduplication across peers
    pub fn compute_content_hash(&self) -> String {
        let mut hasher = Sha256::new();

        // Hash the operation type
        hasher.update(format!("{:?}", self.op_type).as_bytes());

        // Hash the key
        hasher.update(self.key.as_bytes());

        // Hash the value if present
        if let Some(val) = &self.value {
            hasher.update(val.to_string().as_bytes());
        }

        // Hash the rust type
        hasher.update(self.rust_type.as_bytes());

        // Hash the capability
        hasher.update(self.capability.as_bytes());

        let result = hasher.finalize();
        format!("{:x}", result)
    }

    /// Set the vector clock for this operation
    pub fn with_vector_clock(
        mut self,
        vector_clock: std::collections::HashMap<String, u64>,
    ) -> Self {
        self.vector_clock = Some(vector_clock);
        self
    }

    /// Get the vector clock for this operation
    pub fn get_vector_clock(&self) -> Option<&std::collections::HashMap<String, u64>> {
        self.vector_clock.as_ref()
    }
}

/// Mutation ordering specification for FeltDB
/// This defines the order in which mutation side effects occur
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationOrder {
    /// Conservative: Apply locally first, then persist in background
    /// Mutation → Validate → Apply state transition → Emit diff → Persist
    /// Good for: Interactive feel, optimistic updates
    ApplyFirst,

    /// Durable: Persist first, then apply and emit
    /// Mutation → Validate → Persist → Apply state transition → Emit diff
    /// Good for: Durability guarantee, no data loss
    PersistFirst,
}

/// The default mutation ordering for FeltDB
/// We choose PersistFirst to ensure durability guarantees
pub const DEFAULT_MUTATION_ORDER: MutationOrder = MutationOrder::PersistFirst;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_insert_creates_insert_op() {
        let op = Operation::insert(
            1,
            "inst1".to_string(),
            1,
            "users:123".to_string(),
            serde_json::json!({"name": "Alice"}),
            "User".to_string(),
            "users".to_string(),
        );

        assert_eq!(op.op_id, 1);
        assert_eq!(op.instance_id, "inst1");
        assert_eq!(op.sequence, 1);
        assert_eq!(op.op_type, OperationType::Insert);
        assert_eq!(op.key, "users:123");
        assert!(op.value.is_some());
    }

    #[test]
    fn operation_delete_has_no_value() {
        let op = Operation::delete(
            2,
            "inst1".to_string(),
            2,
            "users:456".to_string(),
            "User".to_string(),
            "users".to_string(),
        );

        assert_eq!(op.op_type, OperationType::Delete);
        assert!(op.value.is_none());
        assert_eq!(op.instance_id, "inst1");
        assert_eq!(op.sequence, 2);
    }

    #[test]
    fn operations_are_serializable() {
        let op = Operation::update(
            1,
            "inst1".to_string(),
            1,
            "users:789".to_string(),
            serde_json::json!({"status": "active"}),
            "User".to_string(),
            "users".to_string(),
        );

        let json = serde_json::to_string(&op).expect("serialize");
        let deserialized: Operation = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.op_id, op.op_id);
        assert_eq!(deserialized.op_type, op.op_type);
        assert_eq!(deserialized.instance_id, op.instance_id);
        assert_eq!(deserialized.sequence, op.sequence);
    }
}
