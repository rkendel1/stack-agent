/// Durable Deduplication Set
///
/// Provides persistent storage for the dedup set (seen envelope IDs).
/// The dedup set prevents duplicate operations from being applied multiple times.
///
/// Invariant: Every envelope ID persisted survives process restart.
use crate::distributed_transactions::EnvelopeId;
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Persistent dedup set (seen envelope IDs)
pub struct DeduplicationSet {
    path: PathBuf,
    file: Option<File>,
}

impl DeduplicationSet {
    /// Create or open a persistent dedup set at the given path
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, std::io::Error> {
        let path = path.as_ref().to_path_buf();

        // Create parent directory if needed
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Open dedup file in append mode
        let file = OpenOptions::new().create(true).append(true).open(&path)?;

        Ok(Self {
            path,
            file: Some(file),
        })
    }

    /// Record an envelope ID as seen (persist immediately with fsync)
    pub fn mark_seen(&mut self, envelope_id: &EnvelopeId) -> Result<(), std::io::Error> {
        if let Some(ref mut file) = self.file {
            // Serialize envelope ID to JSON line
            let json = serde_json::to_string(envelope_id)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

            // Write ID line (newline-delimited)
            writeln!(file, "{}", json)?;

            // Force durable persistence to disk
            file.sync_all()?;

            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "Dedup set file not open",
            ))
        }
    }

    /// Load all seen envelope IDs from persistent storage
    pub fn load_all(&self) -> Result<HashSet<EnvelopeId>, std::io::Error> {
        if !self.path.exists() {
            return Ok(HashSet::new());
        }

        let file = File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut seen = HashSet::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<EnvelopeId>(&line) {
                Ok(envelope_id) => {
                    seen.insert(envelope_id);
                }
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Failed to deserialize envelope ID: {}", e),
                    ))
                }
            }
        }

        Ok(seen)
    }

    /// Get count of persisted envelope IDs
    pub fn count(&self) -> Result<usize, std::io::Error> {
        let seen = self.load_all()?;
        Ok(seen.len())
    }

    /// Clear the dedup set (used for testing)
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
    use crate::distributed_transactions::EnvelopeId;
    use tempfile::TempDir;

    fn create_test_id(node: &str, seq: u64) -> EnvelopeId {
        EnvelopeId::new(node.to_string(), seq)
    }

    #[test]
    fn test_dedup_set_mark_and_load() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let dedup_path = temp_dir.path().join("dedup.set");

        // Mark some IDs as seen
        {
            let mut dedup = DeduplicationSet::new(&dedup_path)?;
            let id1 = create_test_id("node1", 1);
            let id2 = create_test_id("node1", 2);

            dedup.mark_seen(&id1)?;
            dedup.mark_seen(&id2)?;
        }

        // Reload and verify
        {
            let dedup = DeduplicationSet::new(&dedup_path)?;
            let seen = dedup.load_all()?;

            assert_eq!(seen.len(), 2);
            assert!(seen.contains(&create_test_id("node1", 1)));
            assert!(seen.contains(&create_test_id("node1", 2)));
        }

        Ok(())
    }

    #[test]
    fn test_dedup_set_persists_across_restart() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let dedup_path = temp_dir.path().join("dedup.set");

        // Create and populate
        {
            let mut dedup = DeduplicationSet::new(&dedup_path)?;
            for seq in 1..=5 {
                let id = create_test_id("nodeA", seq);
                dedup.mark_seen(&id)?;
            }
        }

        // Restart and verify all are still there
        {
            let dedup = DeduplicationSet::new(&dedup_path)?;
            let seen = dedup.load_all()?;

            assert_eq!(seen.len(), 5);
            for seq in 1..=5 {
                assert!(seen.contains(&create_test_id("nodeA", seq)));
            }
        }

        Ok(())
    }

    #[test]
    fn test_dedup_set_empty_recovery() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let dedup_path = temp_dir.path().join("nonexistent.dedup");

        let dedup = DeduplicationSet::new(&dedup_path)?;
        let seen = dedup.load_all()?;

        assert_eq!(seen.len(), 0);

        Ok(())
    }

    #[test]
    fn test_dedup_set_fsync_semantics() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let dedup_path = temp_dir.path().join("dedup.set");

        let mut dedup = DeduplicationSet::new(&dedup_path)?;

        // Mark many IDs to verify fsync durability
        for seq in 1..=50 {
            let id = create_test_id("node", seq);
            dedup.mark_seen(&id)?;
        }

        // Verify all 50 persisted
        let loaded = dedup.load_all()?;
        assert_eq!(loaded.len(), 50);

        Ok(())
    }

    #[test]
    fn test_dedup_set_count() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = TempDir::new()?;
        let dedup_path = temp_dir.path().join("dedup.set");

        let mut dedup = DeduplicationSet::new(&dedup_path)?;

        for seq in 1..=3 {
            let id = create_test_id("node", seq);
            dedup.mark_seen(&id)?;
        }

        assert_eq!(dedup.count()?, 3);

        Ok(())
    }
}
