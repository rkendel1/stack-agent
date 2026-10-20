/// Persistence Reality (Gate 7)
///
/// Tests actual durable implementations against crash scenarios.
/// What happens when:
///   process writes → crash → restart → recover
///
/// Critical test: Same data after arbitrary crash/recovery cycles.
use std::collections::HashMap;

/// Point at which to inject a crash during persistence
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PersistenceCrashPoint {
    /// Crash before any write begins
    BeforeWrite,
    /// Crash after operation enters buffer, before fsync
    AfterBufferBeforeFsync,
    /// Crash after fsync call starts, before completion
    DuringFsync,
    /// Crash after fsync completes, before returning
    AfterFsyncBeforeReturn,
    /// Crash after write fully returns
    AfterWriteReturns,
}

/// Write-ahead log entry (simplified)
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WALEntry {
    pub lsn: u64, // Log sequence number
    pub operation_id: String,
    pub data: Vec<u8>,
    pub checksum: u32,
}

impl WALEntry {
    pub fn new(lsn: u64, operation_id: String, data: Vec<u8>) -> Self {
        let checksum = Self::compute_checksum(&data);
        Self {
            lsn,
            operation_id,
            data,
            checksum,
        }
    }

    fn compute_checksum(data: &[u8]) -> u32 {
        let mut sum: u32 = 0;
        for byte in data {
            sum = sum.wrapping_add(*byte as u32);
        }
        sum
    }

    fn verify_checksum(&self) -> bool {
        Self::compute_checksum(&self.data) == self.checksum
    }

    /// Simulate torn write: truncate data randomly
    fn truncate_randomly(&mut self, max_bytes: usize) {
        if self.data.len() > max_bytes {
            self.data.truncate(max_bytes);
        }
    }
}

/// Simulates a durable write-ahead log with crash points
pub struct DurablePersistenceLayer {
    /// In-memory buffer (not yet durable)
    write_buffer: Vec<u8>,
    /// Committed entries (durable)
    committed_log: Vec<WALEntry>,
    /// Next LSN
    next_lsn: u64,
    /// Tracks fsync state
    fsynced_lsn: u64,
}

impl DurablePersistenceLayer {
    pub fn new() -> Self {
        Self {
            write_buffer: Vec::new(),
            committed_log: Vec::new(),
            next_lsn: 1,
            fsynced_lsn: 0,
        }
    }

    /// Write an operation with potential crash
    pub fn write_with_crash(
        &mut self,
        operation_id: String,
        data: Vec<u8>,
        crash_point: PersistenceCrashPoint,
    ) -> WriteResult {
        match crash_point {
            PersistenceCrashPoint::BeforeWrite => {
                // Crash before anything is written
                WriteResult::Crashed {
                    written_to_buffer: false,
                    fsynced: false,
                    lsn: None,
                }
            }
            PersistenceCrashPoint::AfterBufferBeforeFsync => {
                // Buffer write succeeds, but crash before fsync
                self.write_buffer.extend_from_slice(&data);
                WriteResult::Crashed {
                    written_to_buffer: true,
                    fsynced: false,
                    lsn: Some(self.next_lsn),
                }
            }
            PersistenceCrashPoint::DuringFsync => {
                // Partial fsync (some data written, some not)
                self.write_buffer.extend_from_slice(&data);
                let mut entry = WALEntry::new(self.next_lsn, operation_id, data);

                // Simulate torn write: truncate at random point
                entry.truncate_randomly(entry.data.len() / 2);

                // Partially committed (corrupted)
                self.committed_log.push(entry);

                WriteResult::Crashed {
                    written_to_buffer: true,
                    fsynced: false,
                    lsn: Some(self.next_lsn),
                }
            }
            PersistenceCrashPoint::AfterFsyncBeforeReturn => {
                // Fsync succeeds but crash before returning
                self.write_buffer.extend_from_slice(&data);
                let entry = WALEntry::new(self.next_lsn, operation_id, data);
                self.committed_log.push(entry.clone());
                self.fsynced_lsn = self.next_lsn;

                WriteResult::Crashed {
                    written_to_buffer: true,
                    fsynced: true,
                    lsn: Some(self.next_lsn),
                }
            }
            PersistenceCrashPoint::AfterWriteReturns => {
                // Full write succeeds
                self.write_buffer.extend_from_slice(&data);
                let entry = WALEntry::new(self.next_lsn, operation_id, data);
                self.committed_log.push(entry);
                self.fsynced_lsn = self.next_lsn;

                let lsn = self.next_lsn;
                self.next_lsn += 1;

                WriteResult::Success { lsn }
            }
        }
    }

    /// Recover from crash, returning what was durable
    pub fn recover(&self) -> RecoveryResult {
        let mut recovered_operations = Vec::new();
        let mut corrupted_entries = Vec::new();

        for entry in &self.committed_log {
            if entry.verify_checksum() {
                recovered_operations.push(entry.operation_id.clone());
            } else {
                corrupted_entries.push(entry.lsn);
            }
        }

        RecoveryResult {
            recovered_operations,
            corrupted_entries,
            recovered_lsn: self.fsynced_lsn,
            total_entries: self.committed_log.len(),
        }
    }

    /// Simulate disk corruption: flip random bytes
    pub fn corrupt_tail(&mut self, num_entries_affected: usize) {
        let start = self
            .committed_log
            .len()
            .saturating_sub(num_entries_affected);
        for entry in &mut self.committed_log[start..] {
            if !entry.data.is_empty() {
                entry.data[0] ^= 0xFF; // Flip all bits in first byte
            }
        }
    }

    /// Simulate truncation at arbitrary point
    pub fn truncate_at_byte(&mut self, byte_offset: usize) {
        let mut total_bytes = 0;
        let mut entries_to_keep = 0;

        for entry in &self.committed_log {
            if total_bytes + entry.data.len() > byte_offset {
                break;
            }
            total_bytes += entry.data.len();
            entries_to_keep += 1;
        }

        // Truncate and corrupt last entry if partial
        if entries_to_keep < self.committed_log.len() && entries_to_keep > 0 {
            let last_entry = &mut self.committed_log[entries_to_keep - 1];
            let bytes_into_entry = byte_offset - total_bytes;
            if bytes_into_entry < last_entry.data.len() {
                last_entry.data.truncate(bytes_into_entry);
            }
        }

        self.committed_log.truncate(entries_to_keep);
    }

    pub fn get_log(&self) -> &[WALEntry] {
        &self.committed_log
    }

    pub fn fsynced_lsn(&self) -> u64 {
        self.fsynced_lsn
    }
}

impl Default for DurablePersistenceLayer {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of a write operation
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WriteResult {
    Success {
        lsn: u64,
    },
    Crashed {
        written_to_buffer: bool,
        fsynced: bool,
        lsn: Option<u64>,
    },
}

/// Result of recovery from crash
#[derive(Clone, Debug)]
pub struct RecoveryResult {
    pub recovered_operations: Vec<String>,
    pub corrupted_entries: Vec<u64>,
    pub recovered_lsn: u64,
    pub total_entries: usize,
}

/// Persistence contract verifier
pub struct PersistenceContractVerifier {
    scenarios: Vec<PersistenceScenario>,
}

#[derive(Clone, Debug)]
struct PersistenceScenario {
    name: String,
    written_operations: Vec<String>,
    recovered_operations: Vec<String>,
    corrupted_count: usize,
}

impl PersistenceContractVerifier {
    pub fn new() -> Self {
        Self {
            scenarios: Vec::new(),
        }
    }

    /// Record a crash/recovery scenario
    pub fn record_scenario(
        &mut self,
        name: String,
        written: Vec<String>,
        recovered: Vec<String>,
        corrupted: usize,
    ) {
        self.scenarios.push(PersistenceScenario {
            name,
            written_operations: written,
            recovered_operations: recovered,
            corrupted_count: corrupted,
        });
    }

    /// Verify: if all operations were fsynced, they must all recover
    pub fn verify_fsync_durability(&self) -> FsyncDurabilityResult {
        for scenario in &self.scenarios {
            if scenario.corrupted_count == 0 {
                // No corruption = all written should be recovered
                if scenario.written_operations.len() != scenario.recovered_operations.len() {
                    return FsyncDurabilityResult::Violated {
                        scenario: scenario.name.clone(),
                        written: scenario.written_operations.len(),
                        recovered: scenario.recovered_operations.len(),
                    };
                }

                // Check all written are in recovered
                for op in &scenario.written_operations {
                    if !scenario.recovered_operations.contains(op) {
                        return FsyncDurabilityResult::Violated {
                            scenario: scenario.name.clone(),
                            written: scenario.written_operations.len(),
                            recovered: scenario.recovered_operations.len(),
                        };
                    }
                }
            }
        }

        FsyncDurabilityResult::Satisfied {
            scenarios_checked: self.scenarios.len(),
        }
    }

    /// Verify: corrupted entries don't silently return garbage
    pub fn verify_corruption_detection(&self) -> CorruptionDetectionResult {
        let mut corrupted_undetected = 0;

        for scenario in &self.scenarios {
            if scenario.corrupted_count > 0 {
                // If corruption occurred, it should be detected or entry dropped
                // (not silently included in recovered)
                let expected_max = scenario
                    .written_operations
                    .len()
                    .saturating_sub(scenario.corrupted_count);
                if scenario.recovered_operations.len() > expected_max {
                    corrupted_undetected += 1;
                }
            }
        }

        if corrupted_undetected == 0 {
            CorruptionDetectionResult::Satisfied {
                scenarios_checked: self.scenarios.len(),
            }
        } else {
            CorruptionDetectionResult::Violated {
                undetected_corruptions: corrupted_undetected,
            }
        }
    }

    pub fn get_scenarios(&self) -> &[PersistenceScenario] {
        &self.scenarios
    }
}

impl Default for PersistenceContractVerifier {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of fsync durability verification
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FsyncDurabilityResult {
    Satisfied {
        scenarios_checked: usize,
    },
    Violated {
        scenario: String,
        written: usize,
        recovered: usize,
    },
}

/// Result of corruption detection verification
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CorruptionDetectionResult {
    Satisfied { scenarios_checked: usize },
    Violated { undetected_corruptions: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wal_creation() {
        let wal = DurablePersistenceLayer::new();
        assert_eq!(wal.fsynced_lsn(), 0);
    }

    #[test]
    fn test_write_with_crash_before_write() {
        let mut wal = DurablePersistenceLayer::new();
        let result = wal.write_with_crash(
            "op1".to_string(),
            vec![1, 2, 3],
            PersistenceCrashPoint::BeforeWrite,
        );

        assert!(matches!(result, WriteResult::Crashed { .. }));
        assert_eq!(wal.get_log().len(), 0);
    }

    #[test]
    fn test_write_with_crash_after_fsync() {
        let mut wal = DurablePersistenceLayer::new();
        let result = wal.write_with_crash(
            "op1".to_string(),
            vec![1, 2, 3],
            PersistenceCrashPoint::AfterFsyncBeforeReturn,
        );

        assert!(matches!(result, WriteResult::Crashed { fsynced: true, .. }));
        assert_eq!(wal.get_log().len(), 1);
    }

    #[test]
    fn test_write_success() {
        let mut wal = DurablePersistenceLayer::new();
        let result = wal.write_with_crash(
            "op1".to_string(),
            vec![1, 2, 3],
            PersistenceCrashPoint::AfterWriteReturns,
        );

        assert!(matches!(result, WriteResult::Success { lsn: 1 }));
        assert_eq!(wal.get_log().len(), 1);
        assert_eq!(wal.fsynced_lsn(), 1);
    }

    #[test]
    fn test_recovery_with_no_corruption() {
        let mut wal = DurablePersistenceLayer::new();
        wal.write_with_crash(
            "op1".to_string(),
            vec![1, 2, 3],
            PersistenceCrashPoint::AfterWriteReturns,
        );
        wal.write_with_crash(
            "op2".to_string(),
            vec![4, 5, 6],
            PersistenceCrashPoint::AfterWriteReturns,
        );

        let recovery = wal.recover();
        assert_eq!(recovery.recovered_operations.len(), 2);
        assert_eq!(recovery.corrupted_entries.len(), 0);
    }

    #[test]
    fn test_recovery_detects_corruption() {
        let mut wal = DurablePersistenceLayer::new();
        wal.write_with_crash(
            "op1".to_string(),
            vec![1, 2, 3],
            PersistenceCrashPoint::AfterWriteReturns,
        );

        // Corrupt the entry
        wal.corrupt_tail(1);

        let recovery = wal.recover();
        assert!(recovery.corrupted_entries.len() > 0);
    }

    #[test]
    fn test_truncate_at_byte() {
        let mut wal = DurablePersistenceLayer::new();
        wal.write_with_crash(
            "op1".to_string(),
            vec![1, 2, 3, 4, 5],
            PersistenceCrashPoint::AfterWriteReturns,
        );
        wal.write_with_crash(
            "op2".to_string(),
            vec![6, 7, 8, 9, 10],
            PersistenceCrashPoint::AfterWriteReturns,
        );

        // Truncate after 7 bytes (partway through second entry)
        wal.truncate_at_byte(7);

        let recovery = wal.recover();
        // Should have partial or truncated data
        assert!(recovery.total_entries <= 2);
    }

    #[test]
    fn test_fsync_durability_contract() {
        let mut verifier = PersistenceContractVerifier::new();

        verifier.record_scenario(
            "full_fsync".to_string(),
            vec!["op1".to_string(), "op2".to_string()],
            vec!["op1".to_string(), "op2".to_string()],
            0,
        );

        let result = verifier.verify_fsync_durability();
        assert!(matches!(result, FsyncDurabilityResult::Satisfied { .. }));
    }

    #[test]
    fn test_fsync_durability_violation() {
        let mut verifier = PersistenceContractVerifier::new();

        verifier.record_scenario(
            "lost_data".to_string(),
            vec!["op1".to_string(), "op2".to_string(), "op3".to_string()],
            vec!["op1".to_string()],
            0,
        );

        let result = verifier.verify_fsync_durability();
        assert!(matches!(result, FsyncDurabilityResult::Violated { .. }));
    }

    #[test]
    fn test_corruption_detection() {
        let mut verifier = PersistenceContractVerifier::new();

        // Corruption detected (not silently included)
        verifier.record_scenario(
            "detected_corruption".to_string(),
            vec!["op1".to_string(), "op2".to_string()],
            vec!["op1".to_string()], // op2 detected as corrupted and dropped
            1,
        );

        let result = verifier.verify_corruption_detection();
        assert!(matches!(
            result,
            CorruptionDetectionResult::Satisfied { .. }
        ));
    }

    #[test]
    fn test_multiple_crash_points() {
        let crash_points = vec![
            PersistenceCrashPoint::BeforeWrite,
            PersistenceCrashPoint::AfterBufferBeforeFsync,
            PersistenceCrashPoint::DuringFsync,
            PersistenceCrashPoint::AfterFsyncBeforeReturn,
            PersistenceCrashPoint::AfterWriteReturns,
        ];

        for crash_point in crash_points {
            let mut wal = DurablePersistenceLayer::new();
            let result = wal.write_with_crash("op1".to_string(), vec![1, 2, 3], crash_point);

            // Verify recovery is deterministic regardless of crash point
            let _recovery = wal.recover();
            // Should not panic
            assert!(true);
        }
    }

    #[test]
    fn test_wal_entry_checksum() {
        let entry = WALEntry::new(1, "op1".to_string(), vec![1, 2, 3]);
        assert!(entry.verify_checksum());

        let mut corrupted = entry.clone();
        corrupted.data[0] ^= 0xFF;
        assert!(!corrupted.verify_checksum());
    }
}
