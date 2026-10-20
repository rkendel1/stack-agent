use crate::{FlowError, Result, StoredRow};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Trait defining the storage abstraction for FeltDB.
///
/// This allows FeltDB to work with different storage backends:
/// - Memory (for testing)
/// - Native files (for desktop/server)
/// - Browser storage (OPFS/IndexedDB via WASM)
pub trait Storage: Send + Sync {
    /// Append a new operation to the log
    fn append(&mut self, row: &StoredRow) -> Result<()>;

    /// Read all operations from storage
    fn read_all(&self) -> Result<Vec<StoredRow>>;

    /// Create a checkpoint of the current state
    fn checkpoint(&mut self, state: &CheckpointData) -> Result<()>;

    /// Read the latest checkpoint if it exists
    fn read_checkpoint(&self) -> Result<Option<CheckpointData>>;

    /// Compact the storage by merging checkpoint and delta operations
    fn compact(&mut self) -> Result<()>;

    /// Get the storage backend type name (for debugging/runtime info)
    fn storage_type(&self) -> &'static str;
}

/// Data structure for checkpoints
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CheckpointData {
    /// All rows at checkpoint time
    pub rows: HashMap<String, HashMap<String, StoredRow>>,
    /// Key counters at checkpoint time
    pub key_counters: HashMap<String, u64>,
    /// Operations since last checkpoint
    pub delta_operations: Vec<StoredRow>,
}

/// In-memory storage backend for testing
pub struct MemoryStorage {
    operations: Vec<StoredRow>,
    checkpoint: Option<CheckpointData>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self {
            operations: Vec::new(),
            checkpoint: None,
        }
    }
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl Storage for MemoryStorage {
    fn append(&mut self, row: &StoredRow) -> Result<()> {
        self.operations.push(row.clone());
        Ok(())
    }

    fn read_all(&self) -> Result<Vec<StoredRow>> {
        Ok(self.operations.clone())
    }

    fn checkpoint(&mut self, state: &CheckpointData) -> Result<()> {
        self.checkpoint = Some(state.clone());
        self.operations.clear();
        Ok(())
    }

    fn read_checkpoint(&self) -> Result<Option<CheckpointData>> {
        Ok(self.checkpoint.clone())
    }

    fn compact(&mut self) -> Result<()> {
        // For in-memory, compaction is just checkpoint + clear deltas
        if let Some(checkpoint) = &self.checkpoint {
            self.operations = checkpoint.delta_operations.clone();
        }
        Ok(())
    }

    fn storage_type(&self) -> &'static str {
        "memory"
    }
}

/// File-based storage backend for native file operations
pub struct FileStorage {
    path: PathBuf,
    checkpoint_path: PathBuf,
}

impl FileStorage {
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            OpenOptions::new().create(true).append(true).open(&path)?;
        }

        let checkpoint_path = path.with_extension("checkpoint");
        Ok(Self {
            path,
            checkpoint_path,
        })
    }
}

impl Storage for FileStorage {
    fn append(&mut self, row: &StoredRow) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let mut line = serde_json::to_string(row)?;
        line.push('\n');
        file.write_all(line.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    fn read_all(&self) -> Result<Vec<StoredRow>> {
        let mut operations = Vec::new();

        // First, try to read checkpoint if it exists
        if let Ok(checkpoint_data) = self.read_checkpoint() {
            if checkpoint_data.is_some() {
                // Get all rows from checkpoint
                if let Some(cp) = checkpoint_data {
                    for bucket in cp.rows.values() {
                        for row in bucket.values() {
                            operations.push(row.clone());
                        }
                    }
                    // Add delta operations
                    operations.extend(cp.delta_operations.clone());
                    return Ok(operations);
                }
            }
        }

        // If no checkpoint, read from main log file
        if self.path.exists() {
            let file = OpenOptions::new().read(true).open(&self.path)?;
            let reader = BufReader::new(file);

            for (index, line) in reader.lines().enumerate() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let row: StoredRow = serde_json::from_str(&line).map_err(|_| {
                    FlowError::CorruptLogLine(Box::new(crate::LogCorruption {
                        line_number: index + 1,
                        byte_offset: 0,
                        reason: "not a durable record this build understands".to_string(),
                        excerpt: line.chars().take(120).collect(),
                    }))
                })?;
                operations.push(row);
            }
        }

        Ok(operations)
    }

    fn checkpoint(&mut self, state: &CheckpointData) -> Result<()> {
        let checkpoint_json = serde_json::to_string(state)?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.checkpoint_path)?;
        file.write_all(checkpoint_json.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    fn read_checkpoint(&self) -> Result<Option<CheckpointData>> {
        if !self.checkpoint_path.exists() {
            return Ok(None);
        }

        let content = fs::read_to_string(&self.checkpoint_path)?;
        match serde_json::from_str(&content) {
            Ok(data) => Ok(Some(data)),
            Err(_) => Ok(None),
        }
    }

    fn compact(&mut self) -> Result<()> {
        // Compaction for file storage:
        // 1. Create checkpoint
        // 2. Truncate main log to only contain delta operations
        // 3. On next read, we'll load from checkpoint + deltas

        // This is handled automatically by read_all() which checks for checkpoint first
        Ok(())
    }

    fn storage_type(&self) -> &'static str {
        "file"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_storage_append_and_read() {
        let mut storage = MemoryStorage::new();
        let row = StoredRow {
            capability: "test".to_string(),
            key: "test:1".to_string(),
            rust_type: "String".to_string(),
            value: serde_json::json!("value"),
            unix_ms: 1000,
            content_hash: None,
            flow_ref: None,
            deleted: false,
            operation: None,
        };

        storage.append(&row).expect("append should succeed");
        let operations = storage.read_all().expect("read_all should succeed");
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].key, "test:1");
    }

    #[test]
    fn memory_storage_checkpoint() {
        let mut storage = MemoryStorage::new();
        let row = StoredRow {
            capability: "test".to_string(),
            key: "test:1".to_string(),
            rust_type: "String".to_string(),
            value: serde_json::json!("value"),
            unix_ms: 1000,
            content_hash: None,
            flow_ref: None,
            deleted: false,
            operation: None,
        };

        storage.append(&row).expect("append");

        let checkpoint = CheckpointData {
            rows: Default::default(),
            key_counters: Default::default(),
            delta_operations: vec![row],
        };

        storage
            .checkpoint(&checkpoint)
            .expect("checkpoint should succeed");
        let operations = storage.read_all().expect("read_all should succeed");
        assert_eq!(operations.len(), 0); // Cleared after checkpoint
    }
}
