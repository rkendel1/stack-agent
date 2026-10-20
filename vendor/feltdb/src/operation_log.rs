/// Operation Log Persistence
///
/// Append-only log storing all executed operations for durability, replay, and recovery.
/// Operations are immutable once appended; the log is the authoritative source for
/// what has been executed in the system.
use crate::transactions::{Operation, OperationId, StateVersion};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Persisted operation log entry
#[derive(Clone, Debug)]
pub struct OperationLogEntry {
    pub log_index: u64,
    pub operation_id: OperationId,
    pub operation: Operation,
    pub timestamp_ms: u64,
}

/// In-memory operation log (for testing/development)
/// For production, implement FileOperationLog or DatabaseOperationLog
#[derive(Clone)]
pub struct MemoryOperationLog {
    entries: Arc<Mutex<Vec<OperationLogEntry>>>,
    by_id: Arc<Mutex<HashMap<String, u64>>>,
}

impl MemoryOperationLog {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
            by_id: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Append an operation to the log
    /// Returns the log index assigned to this operation
    pub fn append(&self, operation: Operation) -> Result<u64, String> {
        let mut entries = self.entries.lock().unwrap();
        let mut by_id = self.by_id.lock().unwrap();

        let op_id_key = format!(
            "{}:{}",
            operation.operation_id.node_id, operation.operation_id.sequence
        );

        if by_id.contains_key(&op_id_key) {
            return Err(format!("Operation already logged: {}", op_id_key));
        }

        let log_index = entries.len() as u64;
        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let entry = OperationLogEntry {
            log_index,
            operation_id: operation.operation_id.clone(),
            operation: operation.clone(),
            timestamp_ms,
        };

        by_id.insert(op_id_key, log_index);
        entries.push(entry);

        Ok(log_index)
    }

    /// Get an operation by its ID
    pub fn get_by_id(&self, operation_id: &OperationId) -> Option<OperationLogEntry> {
        let by_id = self.by_id.lock().unwrap();
        let op_id_key = format!("{}:{}", operation_id.node_id, operation_id.sequence);

        if let Some(&log_index) = by_id.get(&op_id_key) {
            let entries = self.entries.lock().unwrap();
            entries.get(log_index as usize).cloned()
        } else {
            None
        }
    }

    /// Get an operation by log index
    pub fn get_by_index(&self, log_index: u64) -> Option<OperationLogEntry> {
        let entries = self.entries.lock().unwrap();
        entries.get(log_index as usize).cloned()
    }

    /// Get all operations in range [start_index, end_index)
    pub fn get_range(&self, start_index: u64, end_index: u64) -> Vec<OperationLogEntry> {
        let entries = self.entries.lock().unwrap();
        entries
            .iter()
            .skip(start_index as usize)
            .take((end_index - start_index) as usize)
            .cloned()
            .collect()
    }

    /// Get the current size of the log (number of entries)
    pub fn size(&self) -> u64 {
        let entries = self.entries.lock().unwrap();
        entries.len() as u64
    }

    /// Iterate over all entries in the log
    pub fn iter(&self) -> Vec<OperationLogEntry> {
        let entries = self.entries.lock().unwrap();
        entries.clone()
    }

    /// Check if an operation ID has been logged
    pub fn contains_operation_id(&self, operation_id: &OperationId) -> bool {
        let by_id = self.by_id.lock().unwrap();
        let op_id_key = format!("{}:{}", operation_id.node_id, operation_id.sequence);
        by_id.contains_key(&op_id_key)
    }

    /// Truncate the log at the given index (for recovery/compaction)
    /// Keeps entries with log_index < truncate_at
    pub fn truncate(&self, truncate_at: u64) -> Result<(), String> {
        let mut entries = self.entries.lock().unwrap();
        let mut by_id = self.by_id.lock().unwrap();

        if truncate_at > entries.len() as u64 {
            return Err("Truncate index beyond log size".to_string());
        }

        let removed: Vec<_> = entries.drain(truncate_at as usize..).collect();
        for entry in removed {
            let op_id_key = format!(
                "{}:{}",
                entry.operation_id.node_id, entry.operation_id.sequence
            );
            by_id.remove(&op_id_key);
        }

        Ok(())
    }
}

impl Default for MemoryOperationLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transactions::{OperationCommand, OperationId};
    use std::collections::HashMap;

    #[test]
    fn test_append_and_retrieve() {
        let log = MemoryOperationLog::new();
        let mut vc = crate::convergence::VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        let op = Operation::new(
            OperationId::new("node1".to_string(), 1),
            version.clone(),
            "Update".to_string(),
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

        let op_id = op.operation_id.clone();
        let idx = log.append(op).unwrap();

        assert_eq!(idx, 0);
        assert!(log.get_by_id(&op_id).is_some());
        assert_eq!(log.size(), 1);
    }

    #[test]
    fn test_duplicate_rejection() {
        let log = MemoryOperationLog::new();
        let mut vc = crate::convergence::VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        let op = Operation::new(
            OperationId::new("node1".to_string(), 1),
            version.clone(),
            "Update".to_string(),
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

        let _ = log.append(op.clone()).unwrap();
        let result = log.append(op);

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already logged"));
    }

    #[test]
    fn test_range_query() {
        let log = MemoryOperationLog::new();
        let mut vc = crate::convergence::VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        for i in 0..5 {
            let op = Operation::new(
                OperationId::new("node1".to_string(), i + 1),
                version.clone(),
                format!("Update{}", i),
                OperationCommand {
                    op_type: "UpdateField".to_string(),
                    collection: "test".to_string(),
                    record_id: format!("rec{}", i),
                    fields: {
                        let mut m = HashMap::new();
                        m.insert("field".to_string(), serde_json::json!(i));
                        m
                    },
                },
                "user".to_string(),
            );
            let _ = log.append(op);
        }

        let range = log.get_range(1, 4);
        assert_eq!(range.len(), 3);
        assert_eq!(range[0].log_index, 1);
        assert_eq!(range[2].log_index, 3);
    }

    #[test]
    fn test_contains_operation_id() {
        let log = MemoryOperationLog::new();
        let mut vc = crate::convergence::VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        let op_id = OperationId::new("node1".to_string(), 1);
        let op = Operation::new(
            op_id.clone(),
            version.clone(),
            "Update".to_string(),
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

        let _ = log.append(op);

        assert!(log.contains_operation_id(&op_id));

        let other_id = OperationId::new("node1".to_string(), 999);
        assert!(!log.contains_operation_id(&other_id));
    }

    #[test]
    fn test_truncate() {
        let log = MemoryOperationLog::new();
        let mut vc = crate::convergence::VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        for i in 0..5 {
            let op = Operation::new(
                OperationId::new("node1".to_string(), i + 1),
                version.clone(),
                format!("Update{}", i),
                OperationCommand {
                    op_type: "UpdateField".to_string(),
                    collection: "test".to_string(),
                    record_id: format!("rec{}", i),
                    fields: {
                        let mut m = HashMap::new();
                        m.insert("field".to_string(), serde_json::json!(i));
                        m
                    },
                },
                "user".to_string(),
            );
            let _ = log.append(op);
        }

        assert_eq!(log.size(), 5);
        let _ = log.truncate(3).unwrap();
        assert_eq!(log.size(), 3);

        let op_id = OperationId::new("node1".to_string(), 4);
        assert!(!log.contains_operation_id(&op_id));
    }

    #[test]
    fn test_iteration() {
        let log = MemoryOperationLog::new();
        let mut vc = crate::convergence::VectorClock::new();
        vc.increment("node1");
        let version = StateVersion::new(vc, "hash0".to_string());

        for i in 0..3 {
            let op = Operation::new(
                OperationId::new("node1".to_string(), i + 1),
                version.clone(),
                format!("Update{}", i),
                OperationCommand {
                    op_type: "UpdateField".to_string(),
                    collection: "test".to_string(),
                    record_id: format!("rec{}", i),
                    fields: {
                        let mut m = HashMap::new();
                        m.insert("field".to_string(), serde_json::json!(i));
                        m
                    },
                },
                "user".to_string(),
            );
            let _ = log.append(op);
        }

        let all = log.iter();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].log_index, 0);
        assert_eq!(all[2].log_index, 2);
    }
}
