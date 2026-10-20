use crate::Operation;
/// Sync Engine module for peer-to-peer synchronization
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

/// A change log that tracks pending operations for synchronization
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChangeLog {
    /// Operations that have been applied locally but not yet acknowledged by peers
    pub pending: VecDeque<Operation>,
    /// Last sequence number we've applied from each peer
    pub peer_versions: HashMap<String, u64>,
    /// Contiguous high-water mark observed from every operation origin.
    #[serde(default)]
    pub observed_versions: HashMap<String, u64>,
    /// Per-peer acknowledgement vector, keyed by operation origin.
    #[serde(default)]
    pub peer_acknowledgements: HashMap<String, HashMap<String, u64>>,
}

impl ChangeLog {
    /// Earliest retained sequence for each operation origin.
    pub fn retained_from(&self) -> HashMap<String, u64> {
        let mut floors: HashMap<String, u64> = HashMap::new();
        for operation in &self.pending {
            floors
                .entry(operation.instance_id.clone())
                .and_modify(|sequence| *sequence = (*sequence).min(operation.sequence))
                .or_insert(operation.sequence);
        }
        floors
    }

    /// Create a new empty change log
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an operation to the pending queue
    pub fn add_operation(&mut self, op: Operation) {
        if !self.has_operation(&op.instance_id, op.sequence) {
            self.observed_versions
                .entry(op.instance_id.clone())
                .and_modify(|sequence| *sequence = (*sequence).max(op.sequence))
                .or_insert(op.sequence);
            self.pending.push_back(op);
        }
    }

    pub fn has_operation(&self, instance_id: &str, sequence: u64) -> bool {
        self.observed_versions
            .get(instance_id)
            .copied()
            .unwrap_or(0)
            >= sequence
    }

    /// Return the operations missing from a caller's per-origin version vector.
    pub fn operations_since(&self, versions: &HashMap<String, u64>) -> Vec<Operation> {
        self.pending
            .iter()
            .filter(|op| op.sequence > versions.get(&op.instance_id).copied().unwrap_or(0))
            .cloned()
            .collect()
    }

    /// Highest operation sequence observed for every originating instance.
    pub fn versions(&self) -> HashMap<String, u64> {
        self.observed_versions.clone()
    }

    /// Get all pending operations
    pub fn pending_operations(&self) -> Vec<Operation> {
        self.pending.iter().cloned().collect()
    }

    /// Acknowledge operations up to a given sequence number from a peer
    pub fn acknowledge_peer(&mut self, peer_id: String, sequence: u64) {
        self.peer_versions.insert(peer_id, sequence);
    }

    pub fn acknowledge_versions(&mut self, peer_id: String, versions: HashMap<String, u64>) {
        let acknowledgements = self.peer_acknowledgements.entry(peer_id).or_default();
        for (origin, sequence) in versions {
            acknowledgements
                .entry(origin)
                .and_modify(|current| *current = (*current).max(sequence))
                .or_insert(sequence);
        }
    }

    pub fn acknowledged_versions(&self, peer_id: &str) -> HashMap<String, u64> {
        self.peer_acknowledgements
            .get(peer_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn acknowledged_operation_count(&self, active_peers: &[String]) -> usize {
        if active_peers.is_empty() {
            return 0;
        }
        self.pending
            .iter()
            .filter(|operation| {
                active_peers.iter().all(|peer| {
                    self.peer_acknowledgements
                        .get(peer)
                        .and_then(|versions| versions.get(&operation.instance_id))
                        .copied()
                        .unwrap_or(0)
                        >= operation.sequence
                })
            })
            .count()
    }

    pub fn prune_acknowledged(&mut self, active_peers: &[String]) -> usize {
        if active_peers.is_empty() {
            return 0;
        }
        let acknowledgements = &self.peer_acknowledgements;
        let before = self.pending.len();
        self.pending.retain(|operation| {
            !active_peers.iter().all(|peer| {
                acknowledgements
                    .get(peer)
                    .and_then(|versions| versions.get(&operation.instance_id))
                    .copied()
                    .unwrap_or(0)
                    >= operation.sequence
            })
        });
        before - self.pending.len()
    }

    /// Get the last acknowledged sequence for a peer
    pub fn last_peer_sequence(&self, peer_id: &str) -> u64 {
        *self.peer_versions.get(peer_id).unwrap_or(&0)
    }

    /// Remove operations that have been acknowledged by all peers
    pub fn cleanup_acknowledged(&mut self, _active_peers: &[String]) {
        // For now, we keep all operations for replay
        // In production, we'd remove operations that all peers have acknowledged
    }
}

/// State of a peer's synchronization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerState {
    /// Unique peer identifier
    pub peer_id: String,
    /// Last version we received from this peer
    pub last_received_version: u64,
    /// Operations pending delivery to this peer
    pub pending_for_delivery: VecDeque<Operation>,
    /// Whether we're currently syncing with this peer
    pub is_syncing: bool,
    /// Last sync timestamp
    pub last_sync_ms: u128,
}

impl PeerState {
    /// Create a new peer state
    pub fn new(peer_id: String) -> Self {
        Self {
            peer_id,
            last_received_version: 0,
            pending_for_delivery: VecDeque::new(),
            is_syncing: false,
            last_sync_ms: 0,
        }
    }

    /// Mark operations for delivery to this peer
    pub fn add_pending(&mut self, op: Operation) {
        self.pending_for_delivery.push_back(op);
    }

    /// Get operations pending delivery
    pub fn pending_for_delivery(&self) -> Vec<Operation> {
        self.pending_for_delivery.iter().cloned().collect()
    }

    /// Clear pending deliveries after successful sync
    pub fn clear_pending(&mut self) {
        self.pending_for_delivery.clear();
    }
}

/// Represents a conflict between local and remote operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conflict {
    /// The entity that has conflicting changes
    pub entity_key: String,
    /// The local operation
    pub local_op: Operation,
    /// The remote operation
    pub remote_op: Operation,
    /// Timestamp when conflict was detected
    pub detected_at_ms: u128,
    /// Whether this conflict has been resolved
    pub resolved: bool,
}

impl Conflict {
    /// Create a new conflict
    pub fn new(entity_key: String, local_op: Operation, remote_op: Operation) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);

        Self {
            entity_key,
            local_op,
            remote_op,
            detected_at_ms: now_ms,
            resolved: false,
        }
    }
}

/// Conflict detector for identifying conflicts between operations
#[derive(Debug, Default)]
pub struct ConflictDetector {
    /// Detected conflicts
    conflicts: Vec<Conflict>,
}

impl ConflictDetector {
    /// Create a new conflict detector
    pub fn new() -> Self {
        Self::default()
    }

    /// Detect conflicts between local and remote operations
    pub fn detect_conflict(
        &mut self,
        local_op: &Operation,
        remote_op: &Operation,
    ) -> Option<Conflict> {
        // Detect write-write conflicts (both operations modify the same entity)
        if local_op.key == remote_op.key
            && local_op.capability == remote_op.capability
            && local_op.op_type != crate::operation::OperationType::Delete
            && remote_op.op_type != crate::operation::OperationType::Delete
        {
            let conflict = Conflict::new(local_op.key.clone(), local_op.clone(), remote_op.clone());
            self.conflicts.push(conflict.clone());
            return Some(conflict);
        }

        None
    }

    /// Get all detected conflicts
    pub fn get_conflicts(&self) -> Vec<Conflict> {
        self.conflicts.clone()
    }

    /// Clear resolved conflicts
    pub fn clear_resolved(&mut self) {
        self.conflicts.retain(|c| !c.resolved);
    }
}

/// Sync protocol messages for peer-to-peer synchronization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SyncMessage {
    /// Handshake to establish connection
    Handshake {
        instance_id: String,
        version: String,
    },
    /// Advertise current state version
    StateVersion { instance_id: String, sequence: u64 },
    /// Request missing operations
    MissingOperations {
        instance_id: String,
        since_sequence: u64,
    },
    /// Transfer operations to peer
    OperationTransfer {
        instance_id: String,
        operations: Vec<Operation>,
    },
    /// Acknowledge receipt of operations
    Acknowledgement {
        instance_id: String,
        up_to_sequence: u64,
    },
}

/// Overall sync state and metrics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncState {
    /// This instance's ID
    pub instance_id: String,
    /// Current sequence number for this instance
    pub sequence: u64,
    /// Connected peers
    pub connected_peers: Vec<String>,
    /// Pending operations waiting to be synced
    pub pending_operations: u64,
    /// Operations sent to peers
    pub operations_sent: u64,
    /// Operations received from peers
    pub operations_received: u64,
    /// Conflicts detected
    pub conflicts_detected: u64,
    /// Last sync timestamp
    pub last_sync_ms: u128,
    /// Is currently connected to network
    pub is_connected: bool,
    /// Vector clock for causal tracking (optional for backward compatibility)
    #[serde(default)]
    pub vector_clock: Option<std::collections::HashMap<String, u64>>,
}

impl SyncState {
    /// Create a new sync state
    pub fn new(instance_id: String) -> Self {
        Self {
            instance_id,
            sequence: 0,
            connected_peers: Vec::new(),
            pending_operations: 0,
            operations_sent: 0,
            operations_received: 0,
            conflicts_detected: 0,
            last_sync_ms: 0,
            is_connected: false,
            vector_clock: None,
        }
    }

    /// Add a connected peer
    pub fn add_peer(&mut self, peer_id: String) {
        if !self.connected_peers.contains(&peer_id) {
            self.connected_peers.push(peer_id);
            self.is_connected = true;
        }
    }

    /// Remove a connected peer
    pub fn remove_peer(&mut self, peer_id: &str) {
        self.connected_peers.retain(|p| p != peer_id);
        if self.connected_peers.is_empty() {
            self.is_connected = false;
        }
    }

    /// Increment operations sent
    pub fn increment_sent(&mut self, count: u64) {
        self.operations_sent += count;
    }

    /// Increment operations received
    pub fn increment_received(&mut self, count: u64) {
        self.operations_received += count;
    }

    /// Increment conflicts detected
    pub fn increment_conflicts(&mut self) {
        self.conflicts_detected += 1;
    }

    /// Initialize or get the vector clock for this sync state
    pub fn get_or_init_vector_clock(&mut self) -> &mut std::collections::HashMap<String, u64> {
        if self.vector_clock.is_none() {
            self.vector_clock = Some(std::collections::HashMap::new());
        }
        self.vector_clock.as_mut().unwrap()
    }

    /// Increment the vector clock for this instance
    pub fn increment_vector_clock(&mut self) {
        let instance_id = self.instance_id.clone();
        let clock = self.get_or_init_vector_clock();
        let entry = clock.entry(instance_id).or_insert(0);
        *entry += 1;
    }

    /// Merge another vector clock into ours (takes maximum)
    pub fn merge_vector_clock(&mut self, other: &std::collections::HashMap<String, u64>) {
        let clock = self.get_or_init_vector_clock();
        for (instance_id, other_value) in other {
            let entry = clock.entry(instance_id.clone()).or_insert(0);
            *entry = (*entry).max(*other_value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_log_tracks_operations() {
        let mut log = ChangeLog::new();
        let op = Operation::insert(
            1,
            "inst1".to_string(),
            1,
            "users:123".to_string(),
            serde_json::json!({"name": "Alice"}),
            "User".to_string(),
            "users".to_string(),
        );

        log.add_operation(op.clone());
        assert_eq!(log.pending_operations().len(), 1);
    }

    #[test]
    fn peer_state_tracks_pending() {
        let mut peer = PeerState::new("peer1".to_string());
        let op = Operation::insert(
            1,
            "inst1".to_string(),
            1,
            "users:123".to_string(),
            serde_json::json!({"name": "Alice"}),
            "User".to_string(),
            "users".to_string(),
        );

        peer.add_pending(op);
        assert_eq!(peer.pending_for_delivery().len(), 1);
        peer.clear_pending();
        assert_eq!(peer.pending_for_delivery().len(), 0);
    }

    #[test]
    fn conflict_detector_finds_write_conflicts() {
        let mut detector = ConflictDetector::new();
        let local_op = Operation::update(
            1,
            "inst1".to_string(),
            1,
            "tasks:123".to_string(),
            serde_json::json!({"status": "completed"}),
            "Task".to_string(),
            "tasks".to_string(),
        );

        let remote_op = Operation::update(
            2,
            "inst2".to_string(),
            1,
            "tasks:123".to_string(),
            serde_json::json!({"status": "blocked"}),
            "Task".to_string(),
            "tasks".to_string(),
        );

        let conflict = detector.detect_conflict(&local_op, &remote_op);
        assert!(conflict.is_some());
        assert_eq!(detector.get_conflicts().len(), 1);
    }

    #[test]
    fn sync_state_tracks_peers() {
        let mut state = SyncState::new("inst1".to_string());
        assert!(!state.is_connected);

        state.add_peer("peer1".to_string());
        assert!(state.is_connected);
        assert_eq!(state.connected_peers.len(), 1);

        state.add_peer("peer2".to_string());
        assert_eq!(state.connected_peers.len(), 2);

        state.remove_peer("peer1");
        assert_eq!(state.connected_peers.len(), 1);
        assert!(state.is_connected);

        state.remove_peer("peer2");
        assert!(!state.is_connected);
    }

    #[test]
    fn sync_message_serializes() {
        let msg = SyncMessage::Handshake {
            instance_id: "inst1".to_string(),
            version: "0.1.0".to_string(),
        };

        let json = serde_json::to_string(&msg).expect("serialize");
        let _deserialized: SyncMessage = serde_json::from_str(&json).expect("deserialize");
    }
}
