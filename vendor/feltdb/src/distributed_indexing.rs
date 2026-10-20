/**
 * Distributed Indexing - Multi-node index replication and synchronization
 *
 * Extends the local indexing layer with distributed capabilities:
 * - Index replication across peers
 * - Consistency protocols based on vector clocks
 * - Remote index state tracking
 * - Query routing and distribution
 * - Index merging strategies
 */
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PeerId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexVectorClock {
    pub clock: BTreeMap<String, u64>,
}

impl IndexVectorClock {
    pub fn new() -> Self {
        IndexVectorClock {
            clock: BTreeMap::new(),
        }
    }

    pub fn increment(&mut self, node_id: &str) {
        *self.clock.entry(node_id.to_string()).or_insert(0) += 1;
    }

    pub fn merge(&mut self, other: &IndexVectorClock) {
        for (node, timestamp) in &other.clock {
            let current = self.clock.entry(node.clone()).or_insert(0);
            *current = (*current).max(*timestamp);
        }
    }

    pub fn happens_before(&self, other: &IndexVectorClock) -> bool {
        let mut less = false;
        for (node, timestamp) in &self.clock {
            let other_ts = other.clock.get(node).copied().unwrap_or(0);
            if timestamp > &other_ts {
                return false;
            }
            if timestamp < &other_ts {
                less = true;
            }
        }
        less
    }

    pub fn concurrent_with(&self, other: &IndexVectorClock) -> bool {
        !self.happens_before(other) && !other.happens_before(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexReplicationState {
    pub index_name: String,
    pub local_version: IndexVectorClock,
    pub peer_versions: HashMap<String, IndexVectorClock>,
    pub replicated_peers: HashSet<String>,
    pub pending_sync: Vec<IndexOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IndexOperation {
    CreateIndex {
        name: String,
        field: String,
        index_type: String,
    },
    UpdateIndex {
        name: String,
        record_id: String,
        value: String,
    },
    DeleteIndex {
        name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexSyncMessage {
    pub source_peer: String,
    pub target_peer: String,
    pub index_name: String,
    pub version: IndexVectorClock,
    pub operations: Vec<IndexOperation>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteIndexState {
    pub peer_id: String,
    pub indexes: HashMap<String, RemoteIndexMetadata>,
    pub last_sync: u64,
    pub reachable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteIndexMetadata {
    pub name: String,
    pub version: IndexVectorClock,
    pub entry_count: usize,
    pub avg_query_time_ms: f64,
    pub last_updated: u64,
}

/// Distributed index manager - coordinates indexes across peers
pub struct DistributedIndexManager {
    local_peer_id: String,
    replication_states: HashMap<String, IndexReplicationState>,
    peer_states: HashMap<String, RemoteIndexState>,
    pending_operations: Vec<IndexSyncMessage>,
    consistency_protocol: ConsistencyProtocol,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConsistencyProtocol {
    EventualConsistency,
    StrongConsistency,
    CausalConsistency,
}

impl DistributedIndexManager {
    pub fn new(local_peer_id: String) -> Self {
        DistributedIndexManager {
            local_peer_id,
            replication_states: HashMap::new(),
            peer_states: HashMap::new(),
            pending_operations: Vec::new(),
            consistency_protocol: ConsistencyProtocol::CausalConsistency,
        }
    }

    /// Create a local index and prepare for replication
    pub fn create_replicated_index(
        &mut self,
        name: String,
        field: String,
        index_type: String,
    ) -> Result<(), String> {
        let mut version = IndexVectorClock::new();
        version.increment(&self.local_peer_id);

        let replication_state = IndexReplicationState {
            index_name: name.clone(),
            local_version: version,
            peer_versions: HashMap::new(),
            replicated_peers: HashSet::new(),
            pending_sync: vec![IndexOperation::CreateIndex {
                name: name.clone(),
                field,
                index_type,
            }],
        };

        self.replication_states.insert(name, replication_state);
        Ok(())
    }

    /// Track a remote peer's index state
    pub fn register_peer(&mut self, peer_id: String) {
        self.peer_states.insert(
            peer_id.clone(),
            RemoteIndexState {
                peer_id,
                indexes: HashMap::new(),
                last_sync: current_time_ms(),
                reachable: true,
            },
        );
    }

    /// Update remote peer's index metadata after sync
    pub fn update_peer_index_metadata(
        &mut self,
        peer_id: &str,
        index_name: String,
        metadata: RemoteIndexMetadata,
    ) -> Result<(), String> {
        if let Some(peer_state) = self.peer_states.get_mut(peer_id) {
            peer_state.indexes.insert(index_name.clone(), metadata);
            peer_state.last_sync = current_time_ms();
            Ok(())
        } else {
            Err(format!("Peer {} not found", peer_id))
        }
    }

    /// Enqueue a sync message to a peer
    pub fn enqueue_sync(&mut self, message: IndexSyncMessage) {
        self.pending_operations.push(message);
    }

    /// Get pending operations for a peer
    pub fn get_pending_for_peer(&self, peer_id: &str) -> Vec<IndexSyncMessage> {
        self.pending_operations
            .iter()
            .filter(|msg| msg.target_peer == peer_id)
            .cloned()
            .collect()
    }

    /// Acknowledge sync from peer
    pub fn acknowledge_sync(&mut self, peer_id: &str, index_name: &str) -> Result<(), String> {
        if let Some(replication_state) = self.replication_states.get_mut(index_name) {
            replication_state
                .replicated_peers
                .insert(peer_id.to_string());
            Ok(())
        } else {
            Err(format!("Index {} not found", index_name))
        }
    }

    /// Check replication status
    pub fn get_replication_status(&self, index_name: &str) -> Option<ReplicationStatus> {
        self.replication_states.get(index_name).map(|state| {
            let replicated_count = state.replicated_peers.len();
            let total_peers = self.peer_states.len();
            let reachable_peers = self.peer_states.values().filter(|p| p.reachable).count();

            ReplicationStatus {
                index_name: index_name.to_string(),
                replicated_to: replicated_count,
                total_peers,
                reachable_peers,
                pending_syncs: state.pending_sync.len(),
                consistency_level: self.calculate_consistency_level(index_name),
            }
        })
    }

    /// Calculate current consistency level for an index
    fn calculate_consistency_level(&self, index_name: &str) -> ConsistencyLevel {
        if let Some(replication_state) = self.replication_states.get(index_name) {
            let total = self.peer_states.len();
            let replicated = replication_state.replicated_peers.len();

            if replicated == total {
                ConsistencyLevel::Strong
            } else if replicated > total / 2 {
                ConsistencyLevel::Causal
            } else {
                ConsistencyLevel::Eventual
            }
        } else {
            ConsistencyLevel::Local
        }
    }

    /// Get list of peers where index needs replication
    pub fn get_replication_targets(&self, index_name: &str) -> Vec<String> {
        if let Some(replication_state) = self.replication_states.get(index_name) {
            self.peer_states
                .keys()
                .filter(|peer_id| {
                    peer_id != &&self.local_peer_id
                        && !replication_state.replicated_peers.contains(*peer_id)
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Detect conflicts between peer versions
    pub fn detect_conflicts(&self, index_name: &str) -> Vec<VersionConflict> {
        let mut conflicts = Vec::new();

        if let Some(replication_state) = self.replication_states.get(index_name) {
            for (peer_id, peer_version) in &replication_state.peer_versions {
                if replication_state
                    .local_version
                    .concurrent_with(peer_version)
                {
                    conflicts.push(VersionConflict {
                        index_name: index_name.to_string(),
                        local_version: replication_state.local_version.clone(),
                        remote_peer: peer_id.clone(),
                        remote_version: peer_version.clone(),
                    });
                }
            }
        }

        conflicts
    }

    /// Merge concurrent versions using CRDT strategy
    pub fn merge_versions(&mut self, index_name: &str) -> Result<(), String> {
        if let Some(replication_state) = self.replication_states.get_mut(index_name) {
            for peer_version in replication_state.peer_versions.values() {
                replication_state.local_version.merge(peer_version);
            }
            Ok(())
        } else {
            Err(format!("Index {} not found", index_name))
        }
    }

    /// Set peer reachability
    pub fn set_peer_reachable(&mut self, peer_id: &str, reachable: bool) {
        if let Some(peer_state) = self.peer_states.get_mut(peer_id) {
            peer_state.reachable = reachable;
        }
    }

    /// Get synchronization status across all indexes
    pub fn get_sync_status(&self) -> DistributedSyncStatus {
        let total_indexes = self.replication_states.len();
        let fully_replicated = self
            .replication_states
            .values()
            .filter(|state| state.replicated_peers.len() == self.peer_states.len())
            .count();

        let pending_total: usize = self
            .replication_states
            .values()
            .map(|state| state.pending_sync.len())
            .sum();

        DistributedSyncStatus {
            total_indexes,
            fully_replicated,
            partially_replicated: total_indexes - fully_replicated,
            pending_operations: pending_total,
            unreachable_peers: self.peer_states.values().filter(|p| !p.reachable).count(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConsistencyLevel {
    Local,
    Eventual,
    Causal,
    Strong,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicationStatus {
    pub index_name: String,
    pub replicated_to: usize,
    pub total_peers: usize,
    pub reachable_peers: usize,
    pub pending_syncs: usize,
    pub consistency_level: ConsistencyLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionConflict {
    pub index_name: String,
    pub local_version: IndexVectorClock,
    pub remote_peer: String,
    pub remote_version: IndexVectorClock,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributedSyncStatus {
    pub total_indexes: usize,
    pub fully_replicated: usize,
    pub partially_replicated: usize,
    pub pending_operations: usize,
    pub unreachable_peers: usize,
}

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vector_clock_increment() {
        let mut clock = IndexVectorClock::new();
        clock.increment("node1");
        assert_eq!(clock.clock.get("node1"), Some(&1));
    }

    #[test]
    fn test_vector_clock_merge() {
        let mut clock1 = IndexVectorClock::new();
        clock1.increment("node1");
        clock1.increment("node1");

        let mut clock2 = IndexVectorClock::new();
        clock2.increment("node2");
        clock2.increment("node2");

        clock1.merge(&clock2);
        assert_eq!(clock1.clock.get("node1"), Some(&2));
        assert_eq!(clock1.clock.get("node2"), Some(&2));
    }

    #[test]
    fn test_vector_clock_happens_before() {
        let mut clock1 = IndexVectorClock::new();
        clock1.increment("node1");

        let mut clock2 = IndexVectorClock::new();
        clock2.increment("node1");
        clock2.increment("node1");

        assert!(clock1.happens_before(&clock2));
        assert!(!clock2.happens_before(&clock1));
    }

    #[test]
    fn test_concurrent_detection() {
        let mut clock1 = IndexVectorClock::new();
        clock1.increment("node1");

        let mut clock2 = IndexVectorClock::new();
        clock2.increment("node2");

        assert!(clock1.concurrent_with(&clock2));
    }

    #[test]
    fn test_distributed_manager_creation() {
        let mut manager = DistributedIndexManager::new("node1".to_string());
        manager.register_peer("node2".to_string());

        assert!(manager
            .create_replicated_index(
                "status_idx".to_string(),
                "status".to_string(),
                "hash".to_string()
            )
            .is_ok());

        let status = manager.get_replication_status("status_idx");
        assert!(status.is_some());
    }

    #[test]
    fn test_replication_targets() {
        let mut manager = DistributedIndexManager::new("node1".to_string());
        manager.register_peer("node2".to_string());
        manager.register_peer("node3".to_string());

        manager
            .create_replicated_index("idx1".to_string(), "field".to_string(), "hash".to_string())
            .ok();

        let targets = manager.get_replication_targets("idx1");
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn test_sync_status() {
        let mut manager = DistributedIndexManager::new("node1".to_string());
        manager.register_peer("node2".to_string());

        manager
            .create_replicated_index("idx1".to_string(), "field".to_string(), "hash".to_string())
            .ok();
        manager
            .create_replicated_index("idx2".to_string(), "field".to_string(), "hash".to_string())
            .ok();

        let status = manager.get_sync_status();
        assert_eq!(status.total_indexes, 2);
    }
}
