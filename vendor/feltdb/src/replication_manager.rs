/// Replication Manager
///
/// Coordinates message routing and delivery between nodes in a distributed cluster.
/// Handles:
/// - Tracking pending operations and their delivery status
/// - Routing messages from leader to followers
/// - Acknowledgment tracking
/// - Retry logic for lost messages
use crate::distributed_transactions::ReplicationMessage;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Message delivery status
#[derive(Clone, Debug, PartialEq)]
pub enum DeliveryStatus {
    Pending,
    Delivered,
    Acknowledged,
    Failed,
}

/// Tracked message for replication
#[derive(Clone)]
pub struct TrackedMessage {
    pub message: ReplicationMessage,
    pub status: DeliveryStatus,
    pub send_count: u64,
    pub timestamp_ms: u64,
}

impl TrackedMessage {
    pub fn new(message: ReplicationMessage) -> Self {
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        Self {
            message,
            status: DeliveryStatus::Pending,
            send_count: 0,
            timestamp_ms,
        }
    }
}

/// Tracks pending messages to be replicated to a specific peer
pub struct PeerReplicationQueue {
    peer_id: String,
    messages: VecDeque<TrackedMessage>,
    next_seq: Arc<AtomicU64>,
}

impl PeerReplicationQueue {
    pub fn new(peer_id: String) -> Self {
        Self {
            peer_id,
            messages: VecDeque::new(),
            next_seq: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn enqueue(&mut self, message: ReplicationMessage) {
        self.messages.push_back(TrackedMessage::new(message));
    }

    pub fn pending_count(&self) -> usize {
        self.messages
            .iter()
            .filter(|m| m.status == DeliveryStatus::Pending)
            .count()
    }

    pub fn next_pending(&mut self) -> Option<TrackedMessage> {
        for msg in &mut self.messages {
            if msg.status == DeliveryStatus::Pending {
                msg.send_count += 1;
                return Some(msg.clone());
            }
        }
        None
    }

    pub fn mark_delivered(&mut self, seq: u64) {
        if seq > 0 && seq as usize <= self.messages.len() {
            if let Some(msg) = self.messages.get_mut((seq - 1) as usize) {
                msg.status = DeliveryStatus::Delivered;
            }
        }
    }
}

/// Replication manager for a cluster
pub struct ReplicationManager {
    peer_queues: HashMap<String, PeerReplicationQueue>,
}

impl ReplicationManager {
    pub fn new() -> Self {
        Self {
            peer_queues: HashMap::new(),
        }
    }

    /// Register a peer to receive replication messages
    pub fn register_peer(&mut self, peer_id: String) {
        self.peer_queues
            .insert(peer_id.clone(), PeerReplicationQueue::new(peer_id));
    }

    /// Enqueue a message for replication to all peers
    pub fn replicate_to_all(&mut self, message: ReplicationMessage) {
        for queue in self.peer_queues.values_mut() {
            queue.enqueue(message.clone());
        }
    }

    /// Enqueue a message for replication to a specific peer
    pub fn replicate_to_peer(&mut self, peer_id: &str, message: ReplicationMessage) {
        if let Some(queue) = self.peer_queues.get_mut(peer_id) {
            queue.enqueue(message);
        }
    }

    /// Get pending messages for a peer
    pub fn get_pending_for_peer(&mut self, peer_id: &str) -> Vec<TrackedMessage> {
        let mut result = Vec::new();
        if let Some(queue) = self.peer_queues.get_mut(peer_id) {
            while let Some(msg) = queue.next_pending() {
                result.push(msg);
            }
        }
        result
    }

    /// Mark a message as delivered to a peer
    pub fn mark_peer_delivered(&mut self, peer_id: &str, seq: u64) {
        if let Some(queue) = self.peer_queues.get_mut(peer_id) {
            queue.mark_delivered(seq);
        }
    }

    /// Get replication statistics
    pub fn get_pending_count(&self) -> usize {
        self.peer_queues.values().map(|q| q.pending_count()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replication_manager_basic() {
        let mut mgr = ReplicationManager::new();
        mgr.register_peer("peer1".to_string());

        // Verify peer registered
        assert_eq!(mgr.peer_queues.len(), 1);
        assert_eq!(mgr.get_pending_count(), 0);
    }
}
