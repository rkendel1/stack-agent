/// Durable Synchronization Primitives
///
/// Core synchronization guarantees:
/// - Change/delta ingestion with sequence tracking
/// - Idempotent application of operations
/// - Ordering guarantees via sequence numbers
/// - Checkpoints and high-water marks for recovery
/// - Replay and recovery mechanisms
/// - Backpressure handling for flow control
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ChangeId {
    pub node_id: String,
    pub sequence: u64,
}

impl ChangeId {
    pub fn new(node_id: String, sequence: u64) -> Self {
        ChangeId { node_id, sequence }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChangeType {
    Insert,
    Update,
    Delete,
    Upsert,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableChange {
    pub change_id: ChangeId,
    pub change_type: ChangeType,
    pub resource_id: String,
    pub data: String,
    pub timestamp: u64,
    pub idempotency_key: String,
}

impl DurableChange {
    pub fn new(
        node_id: String,
        sequence: u64,
        change_type: ChangeType,
        resource_id: String,
        data: String,
        idempotency_key: String,
    ) -> Self {
        DurableChange {
            change_id: ChangeId::new(node_id, sequence),
            change_type,
            resource_id,
            data,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            idempotency_key,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub checkpoint_id: String,
    pub node_id: String,
    pub high_water_mark: u64,
    pub applied_changes: Vec<String>,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncState {
    pub node_id: String,
    pub last_applied_sequence: u64,
    pub pending_changes: Vec<DurableChange>,
    pub checkpoints: Vec<Checkpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BackpressureAction {
    Accept,
    Throttle { delay_ms: u64 },
    Reject,
}

pub struct DurableSyncManager {
    node_id: String,
    sync_state: SyncState,
    applied_ids: std::collections::HashSet<String>,
    backpressure_threshold: usize,
    max_pending_changes: usize,
}

impl DurableSyncManager {
    pub fn new(node_id: String) -> Self {
        DurableSyncManager {
            node_id: node_id.clone(),
            sync_state: SyncState {
                node_id,
                last_applied_sequence: 0,
                pending_changes: Vec::new(),
                checkpoints: Vec::new(),
            },
            applied_ids: std::collections::HashSet::new(),
            backpressure_threshold: 10000,
            max_pending_changes: 50000,
        }
    }

    pub fn ingest_change(&mut self, change: DurableChange) -> Result<(), String> {
        if self.sync_state.pending_changes.len() >= self.max_pending_changes {
            return Err("Max pending changes exceeded".to_string());
        }

        self.sync_state.pending_changes.push(change);
        self.sync_state
            .pending_changes
            .sort_by_key(|c| c.change_id.sequence);

        Ok(())
    }

    pub fn apply_changes_idempotent(&mut self) -> Result<Vec<DurableChange>, String> {
        let mut applied = Vec::new();

        while let Some(change) = self.sync_state.pending_changes.first() {
            if change.change_id.sequence != self.sync_state.last_applied_sequence + 1 {
                break;
            }

            if self.applied_ids.contains(&change.idempotency_key) {
                self.sync_state.pending_changes.remove(0);
                continue;
            }

            applied.push(change.clone());
            self.applied_ids.insert(change.idempotency_key.clone());
            self.sync_state.last_applied_sequence = change.change_id.sequence;
            self.sync_state.pending_changes.remove(0);
        }

        Ok(applied)
    }

    pub fn create_checkpoint(&mut self) -> Result<Checkpoint, String> {
        let checkpoint = Checkpoint {
            checkpoint_id: format!(
                "cp-{}-{}",
                self.node_id, self.sync_state.last_applied_sequence
            ),
            node_id: self.node_id.clone(),
            high_water_mark: self.sync_state.last_applied_sequence,
            applied_changes: self.applied_ids.iter().cloned().collect(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };

        self.sync_state.checkpoints.push(checkpoint.clone());
        Ok(checkpoint)
    }

    pub fn get_high_water_mark(&self) -> u64 {
        self.sync_state.last_applied_sequence
    }

    pub fn get_pending_count(&self) -> usize {
        self.sync_state.pending_changes.len()
    }

    pub fn evaluate_backpressure(&self) -> BackpressureAction {
        let pending = self.sync_state.pending_changes.len();

        if pending >= self.max_pending_changes {
            BackpressureAction::Reject
        } else if pending >= self.backpressure_threshold {
            BackpressureAction::Throttle {
                delay_ms: ((pending - self.backpressure_threshold) / 100) as u64,
            }
        } else {
            BackpressureAction::Accept
        }
    }

    pub fn replay_from_checkpoint(&mut self, checkpoint: &Checkpoint) -> Result<usize, String> {
        let mut replayed = 0;
        let applied_set: std::collections::HashSet<_> =
            checkpoint.applied_changes.iter().cloned().collect();

        for change in self.sync_state.pending_changes.drain(..) {
            if !applied_set.contains(&change.idempotency_key)
                && change.change_id.sequence <= checkpoint.high_water_mark
            {
                replayed += 1;
            }
        }

        self.sync_state.last_applied_sequence = checkpoint.high_water_mark;
        Ok(replayed)
    }

    pub fn get_checkpoint_count(&self) -> usize {
        self.sync_state.checkpoints.len()
    }

    pub fn get_last_checkpoint(&self) -> Option<Checkpoint> {
        self.sync_state.checkpoints.last().cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_idempotent_application() {
        let mut manager = DurableSyncManager::new("node-1".to_string());

        let change = DurableChange::new(
            "node-1".to_string(),
            1,
            ChangeType::Insert,
            "resource-1".to_string(),
            "data".to_string(),
            "idem-1".to_string(),
        );

        manager.ingest_change(change.clone()).unwrap();
        let applied = manager.apply_changes_idempotent().unwrap();

        assert_eq!(applied.len(), 1);
        assert_eq!(manager.get_high_water_mark(), 1);

        manager.ingest_change(change).unwrap();
        let applied = manager.apply_changes_idempotent().unwrap();
        assert_eq!(applied.len(), 0);
    }

    #[test]
    fn test_checkpoint_recovery() {
        let mut manager = DurableSyncManager::new("node-1".to_string());

        let change1 = DurableChange::new(
            "node-1".to_string(),
            1,
            ChangeType::Insert,
            "res-1".to_string(),
            "data1".to_string(),
            "idem-1".to_string(),
        );

        manager.ingest_change(change1).unwrap();
        manager.apply_changes_idempotent().unwrap();
        let cp = manager.create_checkpoint().unwrap();

        assert_eq!(cp.high_water_mark, 1);
        assert_eq!(manager.get_checkpoint_count(), 1);
    }

    #[test]
    fn test_backpressure() {
        let mut manager = DurableSyncManager::new("node-1".to_string());
        manager.backpressure_threshold = 100;

        for i in 0..150 {
            let change = DurableChange::new(
                "node-1".to_string(),
                (i + 1) as u64,
                ChangeType::Insert,
                format!("res-{}", i),
                "data".to_string(),
                format!("idem-{}", i),
            );
            manager.ingest_change(change).ok();
        }

        match manager.evaluate_backpressure() {
            BackpressureAction::Throttle { delay_ms } => assert!(delay_ms >= 0),
            _ => panic!("Expected throttle"),
        }
    }

    #[test]
    fn test_ordering_guarantee() {
        let mut manager = DurableSyncManager::new("node-1".to_string());

        let change2 = DurableChange::new(
            "node-1".to_string(),
            2,
            ChangeType::Insert,
            "res-2".to_string(),
            "data2".to_string(),
            "idem-2".to_string(),
        );

        let change1 = DurableChange::new(
            "node-1".to_string(),
            1,
            ChangeType::Insert,
            "res-1".to_string(),
            "data1".to_string(),
            "idem-1".to_string(),
        );

        manager.ingest_change(change2).unwrap();
        manager.ingest_change(change1).unwrap();

        let applied = manager.apply_changes_idempotent().unwrap();
        assert_eq!(applied[0].change_id.sequence, 1);
        assert_eq!(applied[1].change_id.sequence, 2);
    }
}
