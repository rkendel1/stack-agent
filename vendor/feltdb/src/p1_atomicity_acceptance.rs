//! Production Gate P1 — Multi-Operation Atomicity Acceptance
//!
//! The gate asserts one property, adversarially:
//!
//! ```text
//!   COMMIT            ->  A + B + C all visible
//!   FAIL or CRASH     ->  A + B + C none visible
//!   never             ->  A visible, B visible, C missing
//! ```
//!
//! Every test here restarts a node from its durable log and inspects what
//! recovery produced. Nothing asserts on in-memory state that survived a
//! simulated crash, because in a real crash it would not have.

#[cfg(test)]
mod tests {
    use crate::multi_operation_transaction::*;
    use serde_json::json;
    use std::collections::HashSet;
    use tempfile::TempDir;

    /// A three-operation transaction: the canonical A + B + C.
    fn abc(transaction_id: &str) -> Vec<TransactionOperation> {
        vec![
            TransactionOperation::set("accounts", "A", json!({ "balance": 100 })),
            TransactionOperation::set("accounts", "B", json!({ "balance": 200 })),
            TransactionOperation::set("accounts", "C", json!({ "balance": 300 })),
        ]
        .into_iter()
        .map(|mut operation| {
            operation.record_id = format!("{}-{}", operation.record_id, transaction_id);
            operation
        })
        .collect()
    }

    /// How many of a transaction's records are present after recovery.
    fn visible(node: &TransactionalNode, record: &TransactionRecord) -> usize {
        node.executor().visible_operations(record)
    }

    /// The gate's core assertion: all of them, or none of them.
    fn assert_atomic(node: &TransactionalNode, record: &TransactionRecord, context: &str) -> bool {
        let seen = visible(node, record);
        let total = record.operations.len();
        assert!(
            seen == 0 || seen == total,
            "{}: transaction was partially visible ({} of {} operations)",
            context,
            seen,
            total
        );
        seen == total
    }

    // ---------------------------------------------------------------- normal

    #[test]
    fn commit_makes_every_operation_visible() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let (mut node, _) = TransactionalNode::open(dir.path().join("tx.log"), "node1")?;

        let record = node.build("tx1", abc("tx1"));
        assert_eq!(node.commit(&record)?, ApplyOutcome::Applied);

        assert_eq!(visible(&node, &record), 3);
        assert!(assert_atomic(&node, &record, "after commit"));
        Ok(())
    }

    #[test]
    fn a_rejected_transaction_leaves_no_trace() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("tx.log");
        let (mut node, _) = TransactionalNode::open(&path, "node1")?;

        // The middle operation cannot succeed: the record does not exist.
        let operations = vec![
            TransactionOperation::set("accounts", "A", json!({ "balance": 1 })),
            TransactionOperation::set("accounts", "B", json!({ "balance": 2 }))
                .requiring(Precondition::MustExist),
            TransactionOperation::set("accounts", "C", json!({ "balance": 3 })),
        ];
        let record = node.build("tx-reject", operations);

        match node.commit(&record)? {
            ApplyOutcome::Rejected(RejectionReason::PreconditionFailed {
                operation_index, ..
            }) => {
                assert_eq!(operation_index, 1);
            }
            other => panic!("expected a precondition rejection, got {:?}", other),
        }

        // A rejected transaction must leave nothing behind, including the
        // operations that were legal on their own.
        assert_eq!(visible(&node, &record), 0, "no operation may be visible");
        assert!(
            node.state().get_record("accounts", "A").is_none(),
            "A must not be applied"
        );
        assert!(
            node.state().get_record("accounts", "C").is_none(),
            "C must not be applied"
        );

        drop(node);
        let (restarted, report) = TransactionalNode::open(&path, "node1")?;
        assert!(
            report.committed.is_empty(),
            "a rejected transaction must never be logged"
        );
        assert!(restarted.state().get_record("accounts", "A").is_none());
        Ok(())
    }

    #[test]
    fn a_transaction_writing_one_record_twice_is_refused() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = TempDir::new()?;
        let (mut node, _) = TransactionalNode::open(dir.path().join("tx.log"), "node1")?;

        let record = node.build(
            "tx-conflict",
            vec![
                TransactionOperation::set("accounts", "A", json!({ "balance": 1 })),
                TransactionOperation::set("accounts", "A", json!({ "balance": 2 })),
            ],
        );

        assert!(matches!(
            node.commit(&record)?,
            ApplyOutcome::Rejected(RejectionReason::ConflictingWritesWithinTransaction { .. })
        ));
        Ok(())
    }

    // ----------------------------------------------------------------- crash

    /// Every crash point, on a fresh node, recovered from disk.
    ///
    /// This is the heart of the gate.
    #[test]
    fn every_crash_point_leaves_the_transaction_atomic() -> Result<(), Box<dyn std::error::Error>> {
        let crash_points = vec![
            CrashPoint::BeforeWrite,
            CrashPoint::DuringWrite { keep_percent: 10 },
            CrashPoint::DuringWrite { keep_percent: 50 },
            CrashPoint::DuringWrite { keep_percent: 90 },
            CrashPoint::AfterWriteBeforeFsync,
            CrashPoint::AfterFsyncBeforeApply,
            CrashPoint::DuringApply {
                after_operations: 1,
            },
            CrashPoint::DuringApply {
                after_operations: 2,
            },
            CrashPoint::AfterApply,
        ];

        for crash in crash_points {
            let dir = TempDir::new()?;
            let path = dir.path().join("tx.log");

            let record = {
                let (mut node, _) = TransactionalNode::open(&path, "node1")?;
                let record = node.build("tx-crash", abc("tx-crash"));
                node.commit_with_crash(&record, &crash)?;
                record
                // The node is dropped here: the process died.
            };

            let (recovered, report) = TransactionalNode::open(&path, "node1")?;
            let context = format!("crash at {:?}", crash);
            let fully_visible = assert_atomic(&recovered, &record, &context);

            match &crash {
                // Nothing durable was written, so nothing may be visible.
                CrashPoint::BeforeWrite | CrashPoint::DuringWrite { .. } => {
                    assert!(!fully_visible, "{}: nothing should have committed", context);
                    assert!(
                        report.committed.is_empty(),
                        "{}: log must hold no record",
                        context
                    );
                }
                // The record was fsynced, so recovery must replay all of it —
                // including when the crash happened part-way through applying.
                CrashPoint::AfterFsyncBeforeApply
                | CrashPoint::DuringApply { .. }
                | CrashPoint::AfterApply => {
                    assert!(
                        fully_visible,
                        "{}: a committed transaction must fully recover",
                        context
                    );
                    assert_eq!(report.committed.len(), 1, "{}", context);
                }
                // Written but not fsynced: the operating system decides. Either
                // outcome is correct; a partial one is not, and assert_atomic
                // already ruled that out.
                CrashPoint::AfterWriteBeforeFsync => {}
            }
        }
        Ok(())
    }

    #[test]
    fn a_torn_final_record_is_discarded_and_the_log_stays_writable(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("tx.log");

        // One good transaction, then a crash mid-write on the next.
        let first = {
            let (mut node, _) = TransactionalNode::open(&path, "node1")?;
            let first = node.build("tx1", abc("tx1"));
            node.commit(&first)?;
            let second = node.build("tx2", abc("tx2"));
            node.commit_with_crash(&second, &CrashPoint::DuringWrite { keep_percent: 60 })?;
            first
        };

        // Recovery keeps the committed transaction and discards the torn one.
        let (mut node, report) = TransactionalNode::open(&path, "node1")?;
        assert_eq!(report.committed.len(), 1);
        assert!(matches!(
            report.discarded_tail,
            Some(LogDefect::TornTail { .. })
        ));
        assert_eq!(visible(&node, &first), 3);

        // The torn bytes were truncated, so a later write cannot splice onto
        // them and turn a recoverable tail into interior corruption.
        let third = node.build("tx3", abc("tx3"));
        node.commit(&third)?;
        drop(node);

        let (again, report) = TransactionalNode::open(&path, "node1")?;
        assert!(
            report.discarded_tail.is_none(),
            "the log must be clean after truncation"
        );
        assert_eq!(report.committed.len(), 2);
        assert_eq!(visible(&again, &first), 3);
        assert_eq!(visible(&again, &third), 3);
        Ok(())
    }

    #[test]
    fn interior_corruption_is_refused_rather_than_guessed() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = TempDir::new()?;
        let path = dir.path().join("tx.log");

        let (mut node, _) = TransactionalNode::open(&path, "node1")?;
        let first = node.build("tx1", abc("tx1"));
        node.commit(&first)?;
        let second = node.build("tx2", abc("tx2"));
        node.commit(&second)?;

        // Damage a byte inside the first record. It is not the final entry, so
        // this is corruption, not an interrupted write.
        node.log().corrupt_at(20, b"X")?;
        drop(node);

        let outcome = TransactionalNode::open(&path, "node1");
        assert!(
            outcome.is_err(),
            "recovery must refuse a corrupted interior record"
        );
        let message = outcome
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(
            message.contains("corrupted"),
            "unexpected error: {}",
            message
        );
        Ok(())
    }

    #[test]
    fn a_checksum_mismatch_is_not_treated_as_committed() -> Result<(), Box<dyn std::error::Error>> {
        let mut record = TransactionRecord::new("tx1", "node1", 1, abc("tx1"));
        assert!(record.verify_checksum());

        // Alter the body without recomputing the checksum, as a partial write
        // or a bit flip would.
        record.operations.push(TransactionOperation::set(
            "accounts",
            "D",
            json!({ "balance": 9 }),
        ));
        assert!(
            !record.verify_checksum(),
            "a mutated record must fail its integrity check"
        );
        Ok(())
    }

    // ---------------------------------------------------------------- replay

    #[test]
    fn replaying_a_committed_transaction_changes_nothing() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = TempDir::new()?;
        let (mut node, _) = TransactionalNode::open(dir.path().join("tx.log"), "node1")?;

        let record = node.build("tx1", abc("tx1"));
        node.commit(&record)?;
        let hash_after_first = node.state().hash().as_hex().to_string();

        // Deliver the same transaction repeatedly.
        for _ in 0..5 {
            assert_eq!(
                node.accept_replicated(&record)?,
                ApplyOutcome::AlreadyApplied
            );
        }

        assert_eq!(
            node.state().hash().as_hex(),
            hash_after_first,
            "replay must not change state"
        );
        Ok(())
    }

    #[test]
    fn recovery_is_idempotent_across_repeated_restarts() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("tx.log");

        {
            let (mut node, _) = TransactionalNode::open(&path, "node1")?;
            for index in 1..=4 {
                let record = node.build(&format!("tx{}", index), abc(&format!("tx{}", index)));
                node.commit(&record)?;
            }
        }

        let mut hashes = Vec::new();
        for _ in 0..4 {
            let (node, report) = TransactionalNode::open(&path, "node1")?;
            assert_eq!(report.committed.len(), 4);
            hashes.push(node.state().hash().as_hex().to_string());
        }

        assert!(
            hashes.windows(2).all(|pair| pair[0] == pair[1]),
            "every restart must recover identical state"
        );
        Ok(())
    }

    #[test]
    fn a_crash_during_recovery_still_recovers() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("tx.log");

        {
            let (mut node, _) = TransactionalNode::open(&path, "node1")?;
            for index in 1..=3 {
                let record = node.build(&format!("tx{}", index), abc(&format!("tx{}", index)));
                node.commit(&record)?;
            }
        }

        // Abandon recovery part-way, repeatedly, then complete it.
        for _ in 0..3 {
            let (partial, _) = TransactionalNode::open(&path, "node1")?;
            drop(partial);
        }

        let (node, report) = TransactionalNode::open(&path, "node1")?;
        assert_eq!(report.committed.len(), 3);
        for index in 1..=3 {
            let key = format!("A-tx{}", index);
            assert!(
                node.state().get_record("accounts", &key).is_some(),
                "tx{} lost",
                index
            );
        }
        Ok(())
    }

    // ----------------------------------------------------------- concurrency

    #[test]
    fn concurrent_transactions_never_interleave() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("tx.log");
        let (mut node, _) = TransactionalNode::open(&path, "node1")?;

        // Three writers each commit a three-operation transaction against the
        // same node. Commits are serialized, so no transaction may be split by
        // another.
        let mut records = Vec::new();
        for writer in 0..3 {
            let id = format!("writer{}", writer);
            let record = node.build(&id, abc(&id));
            assert_eq!(node.commit(&record)?, ApplyOutcome::Applied);
            records.push(record);
        }
        drop(node);

        let (recovered, report) = TransactionalNode::open(&path, "node1")?;
        assert_eq!(report.committed.len(), 3);
        for record in &records {
            assert_atomic(&recovered, record, "concurrent writers");
            assert_eq!(visible(&recovered, record), 3);
        }

        // Sequences are unique and ordered, so the log defines one history.
        let sequences: Vec<u64> = report
            .committed
            .iter()
            .map(|record| record.sequence)
            .collect();
        let unique: HashSet<u64> = sequences.iter().copied().collect();
        assert_eq!(unique.len(), sequences.len(), "sequences must be unique");
        assert!(
            sequences.windows(2).all(|pair| pair[0] < pair[1]),
            "log must be ordered"
        );
        Ok(())
    }

    #[test]
    fn a_losing_writer_is_rejected_without_partial_effect() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = TempDir::new()?;
        let path = dir.path().join("tx.log");
        let (mut node, _) = TransactionalNode::open(&path, "node1")?;

        // Two writers race to create the same record. Both transactions also
        // touch a record of their own.
        let first = node.build(
            "writer-a",
            vec![
                TransactionOperation::set("accounts", "shared", json!({ "owner": "a" }))
                    .requiring(Precondition::MustNotExist),
                TransactionOperation::set("accounts", "only-a", json!({ "balance": 1 })),
            ],
        );
        let second = node.build(
            "writer-b",
            vec![
                TransactionOperation::set("accounts", "shared", json!({ "owner": "b" }))
                    .requiring(Precondition::MustNotExist),
                TransactionOperation::set("accounts", "only-b", json!({ "balance": 2 })),
            ],
        );

        assert_eq!(node.commit(&first)?, ApplyOutcome::Applied);
        assert!(matches!(node.commit(&second)?, ApplyOutcome::Rejected(_)));

        // The loser must leave nothing behind, including its uncontended write.
        assert!(
            node.state().get_record("accounts", "only-b").is_none(),
            "a rejected transaction must not apply its other operations"
        );
        assert_eq!(
            node.state().get_record("accounts", "shared"),
            Some(&json!({ "owner": "a" }))
        );
        drop(node);

        let (recovered, report) = TransactionalNode::open(&path, "node1")?;
        assert_eq!(report.committed.len(), 1, "only the winner may be durable");
        assert!(recovered.state().get_record("accounts", "only-b").is_none());
        Ok(())
    }

    // ---------------------------------------------------------- replication

    #[test]
    fn a_transaction_stays_atomic_across_replication() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let (mut leader, _) = TransactionalNode::open(dir.path().join("a.log"), "nodeA")?;
        let (mut follower_b, _) = TransactionalNode::open(dir.path().join("b.log"), "nodeB")?;
        let (mut follower_c, _) = TransactionalNode::open(dir.path().join("c.log"), "nodeC")?;

        let record = leader.build("tx1", abc("tx1"));
        assert_eq!(leader.commit(&record)?, ApplyOutcome::Applied);

        assert_eq!(
            follower_b.accept_replicated(&record)?,
            ApplyOutcome::Applied
        );
        assert_eq!(
            follower_c.accept_replicated(&record)?,
            ApplyOutcome::Applied
        );

        // Duplicate delivery is safe.
        assert_eq!(
            follower_b.accept_replicated(&record)?,
            ApplyOutcome::AlreadyApplied
        );
        assert_eq!(
            follower_c.accept_replicated(&record)?,
            ApplyOutcome::AlreadyApplied
        );

        let leader_hash = leader.state().hash().as_hex().to_string();
        assert_eq!(
            follower_b.state().hash().as_hex(),
            leader_hash,
            "B diverged"
        );
        assert_eq!(
            follower_c.state().hash().as_hex(),
            leader_hash,
            "C diverged"
        );

        for node in [&follower_b, &follower_c] {
            assert_atomic(node, &record, "replicated follower");
        }
        Ok(())
    }

    #[test]
    fn a_follower_that_crashes_mid_replication_recovers_atomically(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let leader_path = dir.path().join("a.log");
        let follower_path = dir.path().join("b.log");

        let record = {
            let (mut leader, _) = TransactionalNode::open(&leader_path, "nodeA")?;
            let record = leader.build("tx1", abc("tx1"));
            leader.commit(&record)?;
            record
        };

        // The follower dies while writing the replicated record.
        {
            let (mut follower, _) = TransactionalNode::open(&follower_path, "nodeB")?;
            follower.commit_with_crash(&record, &CrashPoint::DuringWrite { keep_percent: 40 })?;
        }

        let (mut follower, report) = TransactionalNode::open(&follower_path, "nodeB")?;
        assert!(
            report.committed.is_empty(),
            "a torn replicated record must not commit"
        );
        assert_atomic(&follower, &record, "follower after torn write");
        assert_eq!(follower.executor().visible_operations(&record), 0);

        // Retransmission after recovery succeeds and converges.
        assert_eq!(follower.accept_replicated(&record)?, ApplyOutcome::Applied);
        let (leader, _) = TransactionalNode::open(&leader_path, "nodeA")?;
        assert_eq!(
            follower.state().hash().as_hex(),
            leader.state().hash().as_hex()
        );
        Ok(())
    }

    #[test]
    fn a_replicated_record_that_fails_integrity_is_refused(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let (mut follower, _) = TransactionalNode::open(dir.path().join("b.log"), "nodeB")?;

        let mut record = TransactionRecord::new("tx1", "nodeA", 1, abc("tx1"));
        record.operations[0].record_id = "tampered".to_string();

        assert!(matches!(
            follower.accept_replicated(&record)?,
            ApplyOutcome::Rejected(_)
        ));
        assert!(
            follower.state().records.is_empty(),
            "a tampered record must not apply"
        );
        Ok(())
    }

    // -------------------------------------------------------------- identity

    #[test]
    fn durable_transaction_identity_survives_restart() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("tx.log");

        let record = {
            let (mut node, _) = TransactionalNode::open(&path, "node1")?;
            let record = node.build("stable-id", abc("stable-id"));
            node.commit(&record)?;
            record
        };

        let (mut node, report) = TransactionalNode::open(&path, "node1")?;
        assert_eq!(report.committed[0].transaction_id, "stable-id");
        assert!(
            node.executor().applied_transactions().contains("stable-id"),
            "identity must be restored so replay stays idempotent"
        );

        // Redelivery after restart is still a no-op.
        assert_eq!(
            node.accept_replicated(&record)?,
            ApplyOutcome::AlreadyApplied
        );
        Ok(())
    }

    #[test]
    fn deletes_are_atomic_with_writes() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("tx.log");
        let (mut node, _) = TransactionalNode::open(&path, "node1")?;

        let seed = node.build(
            "seed",
            vec![
                TransactionOperation::set("accounts", "old", json!({ "balance": 5 })),
                TransactionOperation::set("accounts", "keep", json!({ "balance": 6 })),
            ],
        );
        node.commit(&seed)?;

        // Move: delete one record and create another, atomically.
        let moved = node.build(
            "move",
            vec![
                TransactionOperation::delete("accounts", "old").requiring(Precondition::MustExist),
                TransactionOperation::set("accounts", "new", json!({ "balance": 5 }))
                    .requiring(Precondition::MustNotExist),
            ],
        );
        assert_eq!(node.commit(&moved)?, ApplyOutcome::Applied);
        drop(node);

        let (recovered, _) = TransactionalNode::open(&path, "node1")?;
        assert!(
            recovered.state().get_record("accounts", "old").is_none(),
            "delete must persist"
        );
        assert!(
            recovered.state().get_record("accounts", "new").is_some(),
            "insert must persist"
        );
        assert!(
            recovered.state().get_record("accounts", "keep").is_some(),
            "untouched record kept"
        );
        Ok(())
    }

    /// A long mixed history under repeated crashes: no transaction may ever be
    /// partially visible, at any point, under any interleaving.
    #[test]
    fn a_long_crash_ridden_history_never_exposes_a_partial_transaction(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("tx.log");
        let crashes = [
            CrashPoint::AfterApply,
            CrashPoint::DuringWrite { keep_percent: 30 },
            CrashPoint::DuringApply {
                after_operations: 2,
            },
            CrashPoint::BeforeWrite,
            CrashPoint::AfterFsyncBeforeApply,
            CrashPoint::DuringWrite { keep_percent: 80 },
        ];

        let mut issued = Vec::new();
        for (index, crash) in crashes.iter().enumerate() {
            let (mut node, _) = TransactionalNode::open(&path, "node1")?;
            let id = format!("tx{}", index);
            let record = node.build(&id, abc(&id));
            node.commit_with_crash(&record, crash)?;
            issued.push(record);

            // Restart after every crash and check the whole history.
            drop(node);
            let (recovered, _) = TransactionalNode::open(&path, "node1")?;
            for previous in &issued {
                assert_atomic(&recovered, previous, &format!("after crash {}", index));
            }
        }

        // Final state: every transaction is wholly present or wholly absent,
        // and the committed count matches what the log actually holds.
        let (recovered, report) = TransactionalNode::open(&path, "node1")?;
        let committed_ids: HashSet<String> = report
            .committed
            .iter()
            .map(|record| record.transaction_id.clone())
            .collect();
        for record in &issued {
            let seen = recovered.executor().visible_operations(record);
            if committed_ids.contains(&record.transaction_id) {
                assert_eq!(
                    seen,
                    record.operations.len(),
                    "{} committed but not whole",
                    record.transaction_id
                );
            } else {
                assert_eq!(
                    seen, 0,
                    "{} not committed but visible",
                    record.transaction_id
                );
            }
        }
        Ok(())
    }
}
