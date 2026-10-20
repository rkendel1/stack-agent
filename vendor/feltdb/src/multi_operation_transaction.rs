//! Multi-Operation Atomicity (Production Gate P1)
//!
//! A transaction groups several operations and guarantees that a reader sees
//! all of them or none of them — never a subset — across commit, crash,
//! restart, replay, concurrent writers, and replication.
//!
//! # How the guarantee is obtained
//!
//! The durable log is the *only* source of truth for committed state. A
//! transaction becomes durable when, and only when, its complete record has
//! been written and fsynced as a single log entry. Observable state is derived
//! by replaying committed records.
//!
//! ```text
//!   transaction { A, B, C }
//!            |
//!            v
//!   preflight every precondition        <- no state has been touched yet
//!            |
//!            v
//!   one record, one write, one fsync    <- the atomic commit point
//!            |
//!            v
//!   apply A, B, C to derived state      <- cannot fail; preflight proved it
//! ```
//!
//! A crash before the fsync leaves no complete record, so recovery replays
//! nothing. A crash after the fsync leaves a complete record, so recovery
//! replays all of A, B, and C. A crash *during* the write leaves a torn tail,
//! which is not a complete record and is discarded. There is no interleaving
//! that produces a partial transaction, because partial application is never
//! durable in the first place.
//!
//! Two properties do the real work:
//!
//! 1. **Preflight before commit.** Every precondition is evaluated against the
//!    current state before anything is written. A transaction that would fail
//!    part-way through is rejected before it can be committed, so the apply
//!    step after commit cannot fail.
//! 2. **Integrity per record.** Each record carries a checksum over its body.
//!    A record that does not parse, or whose checksum does not match, was not
//!    completely written and therefore never committed.
//!
//! # Boundary
//!
//! This guarantee holds because derived state is rebuilt from the log. If a
//! checkpointing layer is added later, it must checkpoint only at transaction
//! boundaries; a checkpoint taken mid-apply would reintroduce exactly the
//! partial-visibility problem this module removes.

use crate::state_hash::CanonicalState;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// What a single operation does to one record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionOp {
    /// Create or replace the record with this value.
    Set(Value),
    /// Remove the record.
    Delete,
}

/// A condition that must hold before the transaction is allowed to commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Precondition {
    /// The record must currently exist.
    MustExist,
    /// The record must not currently exist.
    MustNotExist,
    /// The record must currently hold exactly this value.
    ValueEquals(Value),
}

/// One operation within a transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionOperation {
    pub collection: String,
    pub record_id: String,
    pub op: TransactionOp,
    pub precondition: Option<Precondition>,
}

impl TransactionOperation {
    pub fn set(collection: &str, record_id: &str, value: Value) -> Self {
        Self {
            collection: collection.to_string(),
            record_id: record_id.to_string(),
            op: TransactionOp::Set(value),
            precondition: None,
        }
    }

    pub fn delete(collection: &str, record_id: &str) -> Self {
        Self {
            collection: collection.to_string(),
            record_id: record_id.to_string(),
            op: TransactionOp::Delete,
            precondition: None,
        }
    }

    pub fn requiring(mut self, precondition: Precondition) -> Self {
        self.precondition = Some(precondition);
        self
    }

    fn key(&self) -> (String, String) {
        (self.collection.clone(), self.record_id.clone())
    }
}

/// The durable unit of atomicity.
///
/// A record is committed if and only if it appears in the log completely and
/// its checksum verifies. Identity is `transaction_id`, which is what makes
/// replay idempotent across restart and replication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionRecord {
    pub transaction_id: String,
    pub node_id: String,
    pub sequence: u64,
    pub operations: Vec<TransactionOperation>,
    /// Checksum over the canonical body. Detects a partially written record.
    pub checksum: String,
}

impl TransactionRecord {
    pub fn new(
        transaction_id: &str,
        node_id: &str,
        sequence: u64,
        operations: Vec<TransactionOperation>,
    ) -> Self {
        let mut record = Self {
            transaction_id: transaction_id.to_string(),
            node_id: node_id.to_string(),
            sequence,
            operations,
            checksum: String::new(),
        };
        record.checksum = record.compute_checksum();
        record
    }

    fn compute_checksum(&self) -> String {
        // The checksum covers everything except itself, in a deterministic
        // encoding, so any truncation or mutation of the body is detectable.
        let body = serde_json::json!({
            "transaction_id": self.transaction_id,
            "node_id": self.node_id,
            "sequence": self.sequence,
            "operations": self.operations,
        });
        let mut hasher = Sha256::new();
        hasher.update(body.to_string().as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Whether this record was written completely and has not been altered.
    pub fn verify_checksum(&self) -> bool {
        self.checksum == self.compute_checksum()
    }

    /// Records touched by this transaction, for conflict detection.
    pub fn touched_keys(&self) -> HashSet<(String, String)> {
        self.operations
            .iter()
            .map(|operation| operation.key())
            .collect()
    }
}

/// Why a transaction was refused. No state is modified in any of these cases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RejectionReason {
    /// A precondition did not hold. Carries the operation index and a reason.
    PreconditionFailed {
        operation_index: usize,
        detail: String,
    },
    /// The transaction contained no operations.
    Empty,
    /// The transaction writes the same record twice, so its outcome would
    /// depend on operation order rather than on the transaction as a whole.
    ConflictingWritesWithinTransaction {
        collection: String,
        record_id: String,
    },
}

/// The outcome of applying a record to derived state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Every operation was applied.
    Applied,
    /// This transaction id was already applied; applying it again is a no-op.
    AlreadyApplied,
    /// Nothing was applied.
    Rejected(RejectionReason),
}

/// A log entry that could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogDefect {
    /// The final entry was incomplete: the process died while writing it.
    /// This transaction never committed and is discarded.
    TornTail { bytes: usize },
    /// An entry before the end is unreadable. This is corruption, not a torn
    /// write, and recovery refuses to guess.
    InteriorCorruption { line_number: usize, detail: String },
}

/// What recovery found in the log.
#[derive(Clone, Debug)]
pub struct RecoveryReport {
    pub committed: Vec<TransactionRecord>,
    /// Present when an incomplete final entry was discarded and truncated.
    pub discarded_tail: Option<LogDefect>,
}

/// Where to stop when simulating a crash. Used by the crash-injection harness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CrashPoint {
    /// Die before any byte reaches the log.
    BeforeWrite,
    /// Die having written only part of the record: a torn tail.
    DuringWrite { keep_percent: u8 },
    /// Die after the bytes were written but before fsync.
    AfterWriteBeforeFsync,
    /// Die after the record is durable but before it was applied to state.
    AfterFsyncBeforeApply,
    /// Die part-way through applying operations to in-memory state.
    DuringApply { after_operations: usize },
    /// Die after everything completed.
    AfterApply,
}

/// Append-only transaction log with per-record integrity.
pub struct AtomicTransactionLog {
    path: PathBuf,
    file: File,
}

impl AtomicTransactionLog {
    /// Open a log, recovering it first.
    ///
    /// An incomplete final entry is truncated away before the log accepts new
    /// writes. Without this, the next append would splice onto a torn record
    /// and turn a recoverable tail into interior corruption.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<(Self, RecoveryReport), std::io::Error> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let report = Self::read_log(&path)?;
        if let Some(LogDefect::TornTail { bytes }) = &report.discarded_tail {
            let file = OpenOptions::new().write(true).open(&path)?;
            let length = file.metadata()?.len();
            file.set_len(length.saturating_sub(*bytes as u64))?;
            file.sync_all()?;
        }

        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok((Self { path, file }, report))
    }

    /// Read the log without modifying it.
    pub fn read_log(path: &Path) -> Result<RecoveryReport, std::io::Error> {
        if !path.exists() {
            return Ok(RecoveryReport {
                committed: Vec::new(),
                discarded_tail: None,
            });
        }

        let reader = BufReader::new(File::open(path)?);
        let mut committed = Vec::new();
        let mut defect = None;

        // A line is only trustworthy once we know it is terminated, so entries
        // are collected with their raw bytes and the final one is judged last.
        let mut entries: Vec<(usize, String, bool)> = Vec::new();
        let mut buffer = Vec::new();
        let mut line_number = 0usize;
        let mut reader = reader;
        loop {
            buffer.clear();
            let read = reader.read_until(b'\n', &mut buffer)?;
            if read == 0 {
                break;
            }
            line_number += 1;
            let terminated = buffer.ends_with(b"\n");
            let text = String::from_utf8_lossy(&buffer)
                .trim_end_matches('\n')
                .to_string();
            entries.push((line_number, text, terminated));
        }

        let last_index = entries.len().saturating_sub(1);
        for (index, (number, text, terminated)) in entries.into_iter().enumerate() {
            if text.trim().is_empty() {
                continue;
            }
            let is_last = index == last_index;
            let parsed = serde_json::from_str::<TransactionRecord>(&text);
            let usable = match &parsed {
                Ok(record) => terminated && record.verify_checksum(),
                Err(_) => false,
            };

            if usable {
                committed.push(parsed.expect("checked above"));
                continue;
            }

            if is_last {
                // An unterminated, unparseable, or checksum-failing final entry
                // is a transaction that was still being written when the
                // process died. It never committed.
                defect = Some(LogDefect::TornTail {
                    bytes: text.len() + usize::from(terminated),
                });
            } else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "transaction log corrupted at line {}: {}",
                        number,
                        parsed
                            .err()
                            .map(|error| error.to_string())
                            .unwrap_or_else(|| "checksum mismatch".to_string())
                    ),
                ));
            }
        }

        Ok(RecoveryReport {
            committed,
            discarded_tail: defect,
        })
    }

    /// Commit a record: one write, one fsync. This is the atomic commit point.
    pub fn commit(&mut self, record: &TransactionRecord) -> Result<(), std::io::Error> {
        let json = serde_json::to_string(record)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        writeln!(self.file, "{}", json)?;
        self.file.sync_all()?;
        Ok(())
    }

    /// Commit while simulating a crash, for the crash-injection harness.
    ///
    /// Returns whether the record reached durable storage intact.
    pub fn commit_with_crash(
        &mut self,
        record: &TransactionRecord,
        crash: &CrashPoint,
    ) -> Result<bool, std::io::Error> {
        let json = serde_json::to_string(record)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;

        match crash {
            CrashPoint::BeforeWrite => Ok(false),
            CrashPoint::DuringWrite { keep_percent } => {
                // Write only part of the record and never terminate it: exactly
                // what a process death mid-write leaves behind.
                let cut = json.len() * (*keep_percent).min(99) as usize / 100;
                // Cut on a character boundary; the record is ASCII JSON, but a
                // value could carry multibyte text.
                let mut cut = cut.max(1).min(json.len().saturating_sub(1)).max(1);
                while cut > 1 && !json.is_char_boundary(cut) {
                    cut -= 1;
                }
                let partial = &json[..cut];
                self.file.write_all(partial.as_bytes())?;
                self.file.sync_all()?;
                Ok(false)
            }
            CrashPoint::AfterWriteBeforeFsync => {
                writeln!(self.file, "{}", json)?;
                self.file.flush()?;
                // Deliberately no sync_all. The record is complete, so whether
                // it survives is the operating system's choice; either way it
                // is all-or-nothing, never partial.
                Ok(true)
            }
            _ => {
                writeln!(self.file, "{}", json)?;
                self.file.sync_all()?;
                Ok(true)
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes currently in the log, for harness assertions.
    pub fn size_bytes(&self) -> Result<u64, std::io::Error> {
        Ok(self.file.metadata()?.len())
    }

    /// Append raw bytes. Used only to simulate damage in tests.
    pub fn append_raw(&mut self, bytes: &[u8]) -> Result<(), std::io::Error> {
        self.file.write_all(bytes)?;
        self.file.sync_all()?;
        Ok(())
    }

    /// Overwrite bytes at an offset. Used only to simulate corruption in tests.
    pub fn corrupt_at(&mut self, offset: u64, bytes: &[u8]) -> Result<(), std::io::Error> {
        let mut file = OpenOptions::new().write(true).open(&self.path)?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    }
}

/// Applies committed transactions to derived state.
///
/// The executor never applies a transaction part-way. Preconditions are
/// evaluated for every operation before any mutation is staged, and the staged
/// mutations are then written together.
pub struct TransactionExecutor {
    state: CanonicalState,
    applied: HashSet<String>,
    applied_order: Vec<String>,
}

impl TransactionExecutor {
    pub fn new() -> Self {
        Self {
            state: CanonicalState::new(),
            applied: HashSet::new(),
            applied_order: Vec::new(),
        }
    }

    pub fn state(&self) -> &CanonicalState {
        &self.state
    }

    pub fn applied_transactions(&self) -> &HashSet<String> {
        &self.applied
    }

    pub fn applied_order(&self) -> &[String] {
        &self.applied_order
    }

    /// Evaluate every precondition without mutating anything.
    ///
    /// This runs before the commit, which is what allows the post-commit apply
    /// to be infallible.
    pub fn preflight(&self, record: &TransactionRecord) -> Result<(), RejectionReason> {
        if record.operations.is_empty() {
            return Err(RejectionReason::Empty);
        }

        let mut seen: HashSet<(String, String)> = HashSet::new();
        for operation in &record.operations {
            if !seen.insert(operation.key()) {
                return Err(RejectionReason::ConflictingWritesWithinTransaction {
                    collection: operation.collection.clone(),
                    record_id: operation.record_id.clone(),
                });
            }
        }

        for (index, operation) in record.operations.iter().enumerate() {
            let Some(precondition) = &operation.precondition else {
                continue;
            };
            let current = self
                .state
                .get_record(&operation.collection, &operation.record_id);
            let failure = match (precondition, current) {
                (Precondition::MustExist, None) => Some("record does not exist".to_string()),
                (Precondition::MustNotExist, Some(_)) => Some("record already exists".to_string()),
                (Precondition::ValueEquals(expected), Some(actual)) if actual != expected => {
                    Some("record does not hold the expected value".to_string())
                }
                (Precondition::ValueEquals(_), None) => Some("record does not exist".to_string()),
                _ => None,
            };
            if let Some(detail) = failure {
                return Err(RejectionReason::PreconditionFailed {
                    operation_index: index,
                    detail,
                });
            }
        }

        Ok(())
    }

    /// Apply a committed record. All operations, or none.
    pub fn apply(&mut self, record: &TransactionRecord) -> ApplyOutcome {
        if self.applied.contains(&record.transaction_id) {
            return ApplyOutcome::AlreadyApplied;
        }
        if let Err(reason) = self.preflight(record) {
            return ApplyOutcome::Rejected(reason);
        }

        // Preflight succeeded, so every mutation below is known to be legal.
        // Staging them first keeps the mutation phase free of any decision that
        // could fail half-way through.
        let staged: Vec<(&TransactionOperation, ())> = record
            .operations
            .iter()
            .map(|operation| (operation, ()))
            .collect();
        for (operation, ()) in staged {
            match &operation.op {
                TransactionOp::Set(value) => self.state.set_record(
                    operation.collection.clone(),
                    operation.record_id.clone(),
                    value.clone(),
                ),
                TransactionOp::Delete => {
                    self.state
                        .records
                        .remove(&(operation.collection.clone(), operation.record_id.clone()));
                }
            }
        }

        self.applied.insert(record.transaction_id.clone());
        self.applied_order.push(record.transaction_id.clone());
        ApplyOutcome::Applied
    }

    /// Rebuild state by replaying committed records in order.
    ///
    /// Replay is idempotent: a record already applied is skipped, so recovery
    /// can run repeatedly and converge on the same state.
    pub fn recover(&mut self, records: &[TransactionRecord]) -> Vec<ApplyOutcome> {
        records.iter().map(|record| self.apply(record)).collect()
    }

    /// Whether any operation of this transaction is visible in state.
    ///
    /// The atomicity assertion in the harness is that this is either all of
    /// them or none of them.
    pub fn visible_operations(&self, record: &TransactionRecord) -> usize {
        record
            .operations
            .iter()
            .filter(|operation| match &operation.op {
                TransactionOp::Set(value) => {
                    self.state
                        .get_record(&operation.collection, &operation.record_id)
                        == Some(value)
                }
                TransactionOp::Delete => self
                    .state
                    .get_record(&operation.collection, &operation.record_id)
                    .is_none(),
            })
            .count()
    }
}

impl Default for TransactionExecutor {
    fn default() -> Self {
        Self::new()
    }
}

/// A node that commits transactions durably and derives state from the log.
///
/// This is the unit the crash harness kills and restarts.
pub struct TransactionalNode {
    log: AtomicTransactionLog,
    executor: TransactionExecutor,
    node_id: String,
    next_sequence: u64,
}

impl TransactionalNode {
    /// Start a node, recovering derived state from the durable log.
    pub fn open<P: AsRef<Path>>(
        path: P,
        node_id: &str,
    ) -> Result<(Self, RecoveryReport), std::io::Error> {
        let (log, report) = AtomicTransactionLog::open(path)?;
        let mut executor = TransactionExecutor::new();
        executor.recover(&report.committed);
        let next_sequence = report
            .committed
            .iter()
            .map(|record| record.sequence)
            .max()
            .unwrap_or(0)
            + 1;
        Ok((
            Self {
                log,
                executor,
                node_id: node_id.to_string(),
                next_sequence,
            },
            report,
        ))
    }

    pub fn state(&self) -> &CanonicalState {
        self.executor.state()
    }

    pub fn executor(&self) -> &TransactionExecutor {
        &self.executor
    }

    pub fn log(&mut self) -> &mut AtomicTransactionLog {
        &mut self.log
    }

    pub fn build(
        &mut self,
        transaction_id: &str,
        operations: Vec<TransactionOperation>,
    ) -> TransactionRecord {
        let record = TransactionRecord::new(
            transaction_id,
            &self.node_id,
            self.next_sequence,
            operations,
        );
        self.next_sequence += 1;
        record
    }

    /// Commit a transaction: preflight, durably record, then apply.
    ///
    /// A rejected transaction is never written to the log, so a failed
    /// precondition leaves no trace in durable state.
    pub fn commit(&mut self, record: &TransactionRecord) -> Result<ApplyOutcome, std::io::Error> {
        if self
            .executor
            .applied_transactions()
            .contains(&record.transaction_id)
        {
            return Ok(ApplyOutcome::AlreadyApplied);
        }
        if let Err(reason) = self.executor.preflight(record) {
            return Ok(ApplyOutcome::Rejected(reason));
        }
        self.log.commit(record)?;
        Ok(self.executor.apply(record))
    }

    /// Commit while simulating a crash at the given point.
    ///
    /// Returns what the in-memory executor saw before the process "died". What
    /// matters is not this value but what a restarted node recovers.
    pub fn commit_with_crash(
        &mut self,
        record: &TransactionRecord,
        crash: &CrashPoint,
    ) -> Result<(), std::io::Error> {
        if self.executor.preflight(record).is_err() {
            return Ok(());
        }
        let durable = self.log.commit_with_crash(record, crash)?;
        if !durable {
            return Ok(());
        }
        match crash {
            CrashPoint::AfterFsyncBeforeApply => Ok(()),
            CrashPoint::DuringApply { after_operations } => {
                // Deliberately apply only a prefix, in memory, to model a
                // process that died mid-apply. This state is never persisted;
                // recovery rebuilds from the log.
                for operation in record.operations.iter().take(*after_operations) {
                    if let TransactionOp::Set(value) = &operation.op {
                        self.executor.state.set_record(
                            operation.collection.clone(),
                            operation.record_id.clone(),
                            value.clone(),
                        );
                    }
                }
                Ok(())
            }
            _ => {
                self.executor.apply(record);
                Ok(())
            }
        }
    }

    /// Accept a transaction replicated from another node.
    ///
    /// Applying a replicated transaction is the same all-or-nothing path, and
    /// a transaction already applied here is ignored, so duplicate delivery is
    /// safe.
    pub fn accept_replicated(
        &mut self,
        record: &TransactionRecord,
    ) -> Result<ApplyOutcome, std::io::Error> {
        if !record.verify_checksum() {
            return Ok(ApplyOutcome::Rejected(
                RejectionReason::PreconditionFailed {
                    operation_index: 0,
                    detail: "replicated record failed its integrity check".to_string(),
                },
            ));
        }
        if self
            .executor
            .applied_transactions()
            .contains(&record.transaction_id)
        {
            return Ok(ApplyOutcome::AlreadyApplied);
        }
        if let Err(reason) = self.executor.preflight(record) {
            return Ok(ApplyOutcome::Rejected(reason));
        }
        self.log.commit(record)?;
        Ok(self.executor.apply(record))
    }
}
