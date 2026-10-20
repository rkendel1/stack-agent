/// Durable Operation Log
///
/// Provides persistent storage for operation envelopes with fsync durability.
/// The log is the source of truth for recovery; replica state and dedup sets are derived from it.
///
/// Invariant: Every successfully persisted operation can be recovered after process restart.
use crate::distributed_transactions::TransactionEnvelope;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Durable operation log with fsync semantics
pub struct OperationLog {
    path: PathBuf,
    file: Option<File>,
}

impl OperationLog {
    /// Create or open a durable log at the given path
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, std::io::Error> {
        let path = path.as_ref().to_path_buf();

        // Create parent directory if needed
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Open log file in append mode
        let file = OpenOptions::new().create(true).append(true).open(&path)?;

        Ok(Self {
            path,
            file: Some(file),
        })
    }

    /// Append and durably persist an operation envelope
    /// Returns error if write or fsync fails
    pub fn append(&mut self, envelope: &TransactionEnvelope) -> Result<(), std::io::Error> {
        if let Some(ref mut file) = self.file {
            // Serialize envelope to JSON line
            let json = serde_json::to_string(envelope)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

            // Write JSON line (newline-delimited)
            writeln!(file, "{}", json)?;

            // Force durable persistence to disk
            file.sync_all()?;

            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "Operation log file not open",
            ))
        }
    }

    /// Load all persisted envelopes from log
    /// Returns envelopes in order they were persisted
    pub fn load_all(&self) -> Result<Vec<TransactionEnvelope>, std::io::Error> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut envelopes = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<TransactionEnvelope>(&line) {
                Ok(envelope) => envelopes.push(envelope),
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Failed to deserialize envelope: {}", e),
                    ))
                }
            }
        }

        Ok(envelopes)
    }

    /// Get the number of persisted operations
    pub fn count(&self) -> Result<usize, std::io::Error> {
        let envelopes = self.load_all()?;
        Ok(envelopes.len())
    }

    /// Clear the log (used for testing)
    pub fn clear(&mut self) -> Result<(), std::io::Error> {
        if let Some(file) = self.file.take() {
            drop(file);
        }

        std::fs::remove_file(&self.path)?;

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;

        self.file = Some(file);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transactions::{ConsistencyContract, ConsistencyLevel};
    use tempfile::TempDir;

    fn create_test_envelope(sequence: u64) -> TransactionEnvelope {
        TransactionEnvelope::new(
            "node1".to_string(),
            sequence,
            format!("tx{}", sequence),
            Vec::new(), // Empty operations for test
            ConsistencyContract {
                level: ConsistencyLevel::Causal,
                timeout_ms: Some(1000),
                required_acks: None,
            },
        )
    }

    #[test]
    fn test_operation_log_append() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let log_path = temp_dir.path().join("test.log");

        let mut log = OperationLog::new(&log_path)?;
        let envelope = create_test_envelope(1);

        log.append(&envelope)?;

        assert!(log_path.exists());
        Ok(())
    }

    #[test]
    fn test_operation_log_load() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let log_path = temp_dir.path().join("test.log");

        let mut log = OperationLog::new(&log_path)?;
        let envelope1 = create_test_envelope(1);
        let envelope2 = create_test_envelope(2);

        log.append(&envelope1)?;
        log.append(&envelope2)?;

        let loaded = log.load_all()?;
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].envelope_id.sequence, 1);
        assert_eq!(loaded[1].envelope_id.sequence, 2);

        Ok(())
    }

    #[test]
    fn test_operation_log_persistence() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let log_path = temp_dir.path().join("test.log");

        // Write to log
        {
            let mut log = OperationLog::new(&log_path)?;
            let envelope = create_test_envelope(1);
            log.append(&envelope)?;
        }

        // Reload from disk
        {
            let log = OperationLog::new(&log_path)?;
            let loaded = log.load_all()?;
            assert_eq!(loaded.len(), 1);
            assert_eq!(loaded[0].envelope_id.sequence, 1);
        }

        Ok(())
    }

    #[test]
    fn test_operation_log_empty() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let log_path = temp_dir.path().join("nonexistent.log");

        let log = OperationLog::new(&log_path)?;
        let loaded = log.load_all()?;
        assert_eq!(loaded.len(), 0);

        Ok(())
    }
}
