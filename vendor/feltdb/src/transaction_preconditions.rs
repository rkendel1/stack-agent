//! Transaction-level preconditions: version, epoch and lease.
//!
//! The primitive these tests hold to its contract lets a caller do the thing a
//! shared authority otherwise makes impossible:
//!
//! ```text
//!     read state  ->  decide  ->  conditionally commit several records
//! ```
//!
//! with the condition evaluated by the authority inside the same atomic
//! boundary as the writes. Client-side checking cannot provide this: between a
//! client's check and its write, another writer commits.
//!
//! The invariant, which every test here is an attempt to break:
//!
//! ```text
//!     all preconditions pass  ->  all writes commit
//!     any precondition fails  ->  ZERO writes commit
//! ```
//!
//! There is no third outcome, and in particular no state in which the guarded
//! record is checked while the other staged records commit anyway.
//!
//! ## What "version" means here
//!
//! The document's `__version` -- the field a caller reads back and passes to
//! `updateIfVersion` -- not the row's internal storage sequence. Those are two
//! different numbers that happen to share a name, and fencing on the wrong one
//! would compare a caller's value against something the caller never sees.
//!
//! The authority owns conditional transaction versions: `requireAbsent`
//! creates version 1 and a version-fenced write of N stores N + 1. Both happen
//! inside the same lock as validation. Unconditional writes remain verbatim.

#[cfg(test)]
mod tests {
    use crate::{AtomicMutation, FeltDb, FlowError, PreconditionFailure, RecordPrecondition};
    use serde_json::json;
    use tempfile::TempDir;

    fn db(dir: &TempDir) -> FeltDb {
        FeltDb::open(dir.path().join("app.log")).expect("open")
    }

    /// Storage keys are namespaced `collection:id`; the capability is the
    /// collection. Every helper below takes the bare id and namespaces it, so
    /// the tests read the way a caller thinks.
    fn key(collection: &str, id: &str) -> String {
        format!("{collection}:{id}")
    }

    fn set(collection: &str, id: &str, value: serde_json::Value) -> AtomicMutation {
        AtomicMutation {
            capability: collection.to_string(),
            key: key(collection, id),
            value: Some(value),
        }
    }

    fn delete(collection: &str, id: &str) -> AtomicMutation {
        AtomicMutation {
            capability: collection.to_string(),
            key: key(collection, id),
            value: None,
        }
    }

    fn precondition(collection: &str, id: &str) -> RecordPrecondition {
        RecordPrecondition {
            capability: collection.to_string(),
            key: key(collection, id),
            ..Default::default()
        }
    }

    fn expect_version(collection: &str, id: &str, version: u64) -> RecordPrecondition {
        RecordPrecondition {
            expected_version: Some(version),
            ..precondition(collection, id)
        }
    }

    fn present(db: &FeltDb, collection: &str, id: &str) -> bool {
        db.get_value(&key(collection, id)).expect("read").is_some()
    }

    fn field(db: &FeltDb, collection: &str, id: &str, name: &str) -> serde_json::Value {
        db.get_value(&key(collection, id))
            .expect("read")
            .and_then(|value| value.get(name).cloned())
            .unwrap_or(serde_json::Value::Null)
    }

    fn version_of(db: &FeltDb, collection: &str, id: &str) -> u64 {
        field(db, collection, id, "__version").as_u64().unwrap_or(0)
    }

    /// Seed a record at a chosen version, as an application would hold it.
    fn seed(db: &FeltDb, collection: &str, id: &str, version: u64, extra: serde_json::Value) {
        let mut value = json!({ "id": id, "__version": version });
        if let Some(fields) = extra.as_object() {
            for (name, field) in fields {
                value
                    .as_object_mut()
                    .unwrap()
                    .insert(name.clone(), field.clone());
            }
        }
        db.apply_atomic_transaction(
            &format!("seed-{collection}-{id}-{version}"),
            None,
            &[],
            &[set(collection, id, value)],
            None,
        )
        .expect("seed commits");
    }

    fn guarded(
        db: &FeltDb,
        id: &str,
        preconditions: &[RecordPrecondition],
        mutations: &[AtomicMutation],
    ) -> Result<crate::AtomicCommit, FlowError> {
        db.apply_atomic_transaction_guarded(id, None, None, &[], preconditions, mutations, None)
    }

    fn failure(error: FlowError) -> PreconditionFailure {
        match error {
            FlowError::PreconditionFailed(failure) => *failure,
            other => panic!("expected a structured precondition failure, got {other:?}"),
        }
    }

    // -- the acceptance invariant -----------------------------------------

    #[test]
    fn a_satisfied_precondition_commits_every_staged_write() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "sessions", "s1", 7, json!({}));
        seed(&db, "outbox", "o1", 1, json!({}));

        let commit = guarded(
            &db,
            "tx-ok",
            &[expect_version("sessions", "s1", 7)],
            &[
                set(
                    "sessions",
                    "s1",
                    json!({ "id": "s1", "__version": 8, "state": "active" }),
                ),
                set("changes", "c1", json!({ "id": "c1" })),
                delete("outbox", "o1"),
            ],
        )
        .expect("the precondition holds, so the transaction commits");

        assert_eq!(commit.rows.len(), 3);
        assert_eq!(version_of(&db, "sessions", "s1"), 8);
        assert!(present(&db, "changes", "c1"));
        assert!(
            !present(&db, "outbox", "o1"),
            "the staged delete applied too"
        );
    }

    #[test]
    fn a_stale_version_commits_nothing_at_all() {
        // The failure the whole primitive exists to prevent: A is checked while
        // B and C commit anyway.
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "records", "a", 7, json!({}));
        seed(&db, "records", "b", 1, json!({ "state": "original" }));

        // A moves to 8 before the transaction is evaluated.
        seed(&db, "records", "a", 8, json!({}));
        assert_eq!(version_of(&db, "records", "a"), 8);

        let error = guarded(
            &db,
            "tx-stale",
            &[expect_version("records", "a", 7)],
            &[
                set(
                    "records",
                    "a",
                    json!({ "id": "a", "__version": 8, "written": true }),
                ),
                set("records", "b", json!({ "id": "b", "state": "overwritten" })),
                set("records", "c", json!({ "id": "c" })),
            ],
        )
        .expect_err("a stale version must reject");

        assert_eq!(
            failure(error),
            PreconditionFailure::Version {
                collection: "records".into(),
                key: "records:a".into(),
                expected: 7,
                actual: 8,
            },
            "and it names the predicate that failed, so a caller can tell a lost race \
             from a broken deployment"
        );

        assert_eq!(
            version_of(&db, "records", "a"),
            8,
            "the guarded record is untouched"
        );
        assert_eq!(
            field(&db, "records", "b", "state"),
            "original",
            "and so is every other staged record"
        );
        assert!(
            !present(&db, "records", "c"),
            "a staged write to a new record did not appear"
        );
    }

    #[test]
    fn multi_record_ownership_fencing() {
        // The Session Kernel's shape. Owner 1 read generation 12; owner 2 took
        // 13 before owner 1 committed. Owner 1 must write nothing -- not the
        // session, not the change record, not the outbox row.
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "sessions", "session", 12, json!({ "generation": 12 }));

        seed(&db, "sessions", "session", 13, json!({ "generation": 13 }));

        let error = guarded(
            &db,
            "owner-1-commit",
            &[expect_version("sessions", "session", 12)],
            &[
                set(
                    "sessions",
                    "session",
                    json!({ "id": "session", "__version": 13, "generation": 12 }),
                ),
                set("changes", "change-1", json!({ "id": "change-1" })),
                set("outbox", "outbox-1", json!({ "id": "outbox-1" })),
            ],
        )
        .expect_err("the losing owner must be refused");

        assert!(matches!(
            failure(error),
            PreconditionFailure::Version {
                expected: 12,
                actual: 13,
                ..
            }
        ));
        assert_eq!(
            field(&db, "sessions", "session", "generation"),
            13,
            "the winning owner's generation stands"
        );
        assert!(!present(&db, "changes", "change-1"), "no change record");
        assert!(!present(&db, "outbox", "outbox-1"), "no outbox record");
    }

    // -- epoch and lease ---------------------------------------------------

    #[test]
    fn a_matching_epoch_commits_and_a_stale_one_rejects() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "cells", "c", 1, json!({ "authority": { "epoch": 4 } }));

        guarded(
            &db,
            "tx-epoch-ok",
            &[RecordPrecondition {
                expected_epoch: Some(4),
                ..precondition("cells", "c")
            }],
            &[set("cells", "written", json!({ "id": "written" }))],
        )
        .expect("a matching epoch commits");
        assert!(present(&db, "cells", "written"));

        let error = guarded(
            &db,
            "tx-epoch-stale",
            &[RecordPrecondition {
                expected_epoch: Some(3),
                ..precondition("cells", "c")
            }],
            &[set("cells", "never", json!({ "id": "never" }))],
        )
        .expect_err("a stale epoch rejects");

        assert_eq!(
            failure(error),
            PreconditionFailure::Epoch {
                collection: "cells".into(),
                key: "cells:c".into(),
                expected: 3,
                actual: 4,
            }
        );
        assert!(!present(&db, "cells", "never"), "and writes nothing");
    }

    #[test]
    fn a_held_lease_commits_and_a_stale_or_expired_one_rejects() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        let future = crate::now_ms() as u64 + 600_000;
        seed(
            &db,
            "cells",
            "c",
            1,
            json!({ "lease": { "leaseId": "lease-a", "expiresAt": future } }),
        );

        let hold = |lease: &str| RecordPrecondition {
            expected_lease_id: Some(lease.to_string()),
            ..precondition("cells", "c")
        };

        guarded(
            &db,
            "tx-lease-ok",
            &[hold("lease-a")],
            &[set("cells", "w1", json!({ "id": "w1" }))],
        )
        .expect("the held lease commits");
        assert!(present(&db, "cells", "w1"));

        let error = guarded(
            &db,
            "tx-lease-stale",
            &[hold("lease-b")],
            &[set("cells", "w2", json!({ "id": "w2" }))],
        )
        .expect_err("a different lease rejects");
        assert_eq!(
            failure(error),
            PreconditionFailure::Lease {
                collection: "cells".into(),
                key: "cells:c".into(),
                expected: "lease-b".into(),
                actual: Some("lease-a".into()),
            }
        );
        assert!(!present(&db, "cells", "w2"));

        // An expired lease is not held, so naming it is a conflict: the caller
        // believes it owns something it no longer does.
        seed(
            &db,
            "cells",
            "expired",
            1,
            json!({ "lease": { "leaseId": "lease-a", "expiresAt": 1u64 } }),
        );
        let error = guarded(
            &db,
            "tx-lease-expired",
            &[RecordPrecondition {
                expected_lease_id: Some("lease-a".into()),
                ..precondition("cells", "expired")
            }],
            &[set("cells", "w3", json!({ "id": "w3" }))],
        )
        .expect_err("an expired lease is not held");
        assert!(matches!(failure(error), PreconditionFailure::Lease { .. }));
        assert!(!present(&db, "cells", "w3"));
    }

    #[test]
    fn a_transaction_rejects_if_any_predicate_of_several_fails() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "cells", "c", 5, json!({ "authority": { "epoch": 2 } }));

        // Version right, epoch wrong.
        let error = guarded(
            &db,
            "tx-mixed",
            &[RecordPrecondition {
                expected_version: Some(5),
                expected_epoch: Some(9),
                ..precondition("cells", "c")
            }],
            &[set("cells", "w", json!({ "id": "w" }))],
        )
        .expect_err("one failing predicate refuses the whole transaction");
        assert!(matches!(
            failure(error),
            PreconditionFailure::Epoch {
                expected: 9,
                actual: 2,
                ..
            }
        ));
        assert!(!present(&db, "cells", "w"));

        // Several records, one of them stale.
        seed(&db, "cells", "d", 1, json!({}));
        let error = guarded(
            &db,
            "tx-mixed-2",
            &[
                expect_version("cells", "c", 5),
                expect_version("cells", "d", 99),
            ],
            &[set("cells", "w", json!({ "id": "w" }))],
        )
        .expect_err("a failing predicate on any record refuses the transaction");
        assert!(matches!(
            failure(error),
            PreconditionFailure::Version { expected: 99, .. }
        ));
        assert!(!present(&db, "cells", "w"));
    }

    // -- shapes that must not be mistaken for protection -------------------

    #[test]
    fn a_precondition_on_a_missing_record_is_a_conflict_not_a_pass() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        let error = guarded(
            &db,
            "tx-missing",
            &[expect_version("cells", "absent", 1)],
            &[set("cells", "w", json!({ "id": "w" }))],
        )
        .expect_err("expecting a version of a record that does not exist is a conflict");
        assert_eq!(
            failure(error),
            PreconditionFailure::Missing {
                collection: "cells".into(),
                key: "cells:absent".into()
            }
        );
        assert!(!present(&db, "cells", "w"));
    }

    #[test]
    fn an_empty_precondition_is_refused_rather_than_satisfied() {
        // A precondition that constrains nothing would read as protection and
        // provide none, so it is a caller error rather than a pass.
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        let error = guarded(
            &db,
            "tx-empty",
            &[precondition("cells", "c")],
            &[set("cells", "w", json!({ "id": "w" }))],
        )
        .expect_err("an empty precondition is refused");
        assert!(format!("{error}").contains("EMPTY_PRECONDITION"));
        assert!(!present(&db, "cells", "w"));
    }

    #[test]
    fn require_absent_still_means_absent() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        let absent = |id: &str| RecordPrecondition {
            require_absent: true,
            ..precondition("cells", id)
        };

        guarded(
            &db,
            "tx-new",
            &[absent("fresh")],
            &[set("cells", "fresh", json!({ "id": "fresh" }))],
        )
        .expect("absent record commits");
        assert!(present(&db, "cells", "fresh"));
        assert_eq!(version_of(&db, "cells", "fresh"), 1);

        let error = guarded(
            &db,
            "tx-again",
            &[absent("fresh")],
            &[set("cells", "other", json!({ "id": "other" }))],
        )
        .expect_err("an existing record refuses a require-absent transaction");
        assert_eq!(
            failure(error),
            PreconditionFailure::Present {
                collection: "cells".into(),
                key: "cells:fresh".into()
            }
        );
        assert!(!present(&db, "cells", "other"));
    }

    // -- replay ------------------------------------------------------------

    #[test]
    fn a_committed_guarded_transaction_replays_as_a_duplicate() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "cells", "c", 3, json!({}));

        let first = guarded(
            &db,
            "tx-replay",
            &[expect_version("cells", "c", 3)],
            &[set("cells", "w", json!({ "id": "w", "n": 1 }))],
        )
        .expect("commits");
        assert!(!first.duplicate);

        // The response was lost; the caller retries the same id. The
        // precondition is no longer satisfiable -- but the transaction already
        // committed, so replay returns the original outcome rather than
        // re-evaluating it.
        seed(&db, "cells", "c", 4, json!({}));
        let retry = guarded(
            &db,
            "tx-replay",
            &[expect_version("cells", "c", 3)],
            &[set("cells", "w", json!({ "id": "w", "n": 2 }))],
        )
        .expect("a committed id replays rather than failing");
        assert!(
            retry.duplicate,
            "the retry is recognised as the same transaction"
        );
        assert_eq!(
            field(&db, "cells", "w", "n"),
            1,
            "and nothing is written twice"
        );
    }

    #[test]
    fn a_failed_precondition_does_not_consume_the_transaction_id() {
        // Stated explicitly rather than left ambiguous: a refused transaction
        // never committed, so its id is unused. A caller may retry the same id
        // after re-reading state, or use a new one for a new decision. Both
        // work, and neither is poisoned by the earlier refusal.
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "cells", "c", 1, json!({}));

        guarded(
            &db,
            "tx-decision",
            &[expect_version("cells", "c", 99)],
            &[set("cells", "w", json!({ "id": "w" }))],
        )
        .expect_err("refused");
        assert!(!present(&db, "cells", "w"));

        // Same id, corrected expectation.
        let commit = guarded(
            &db,
            "tx-decision",
            &[expect_version("cells", "c", 1)],
            &[set("cells", "w", json!({ "id": "w" }))],
        )
        .expect("the same id is still usable after a refusal");
        assert!(!commit.duplicate, "it is a first commit, not a replay");
        assert!(present(&db, "cells", "w"));
    }

    #[test]
    fn a_guarded_transaction_survives_restart() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.log");
        {
            let db = FeltDb::open(&path).expect("open");
            seed(&db, "cells", "c", 2, json!({}));
            guarded(
                &db,
                "tx-durable",
                &[expect_version("cells", "c", 2)],
                &[set("cells", "w", json!({ "id": "w" }))],
            )
            .expect("commits");
        }
        let reopened = FeltDb::open(&path).expect("reopen");
        assert!(
            present(&reopened, "cells", "w"),
            "a guarded commit is as durable as any other"
        );
        let retry = reopened
            .apply_atomic_transaction_guarded(
                "tx-durable",
                None,
                None,
                &[],
                &[expect_version("cells", "c", 2)],
                &[set("cells", "w", json!({ "id": "w", "n": 2 }))],
                None,
            )
            .expect("replay after restart");
        assert!(retry.duplicate, "transaction identity survives the restart");
    }

    // -- concurrency -------------------------------------------------------

    #[test]
    fn exactly_one_of_two_competing_transactions_commits() {
        // Sequential tests cannot show that the check and the write share a
        // boundary. Two threads race on the same expected version; the
        // authority must admit exactly one, and the loser must write neither of
        // its records.
        use std::sync::Arc;
        let dir = TempDir::new().unwrap();
        let db = Arc::new(db(&dir));
        seed(&db, "cells", "shared", 1, json!({}));

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for (id, second) in [("t1", "b"), ("t2", "c")] {
            let db = Arc::clone(&db);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                db.apply_atomic_transaction_guarded(
                    id,
                    None,
                    None,
                    &[],
                    &[RecordPrecondition {
                        expected_version: Some(1),
                        ..RecordPrecondition {
                            capability: "cells".into(),
                            key: "cells:shared".into(),
                            ..Default::default()
                        }
                    }],
                    &[
                        set(
                            "cells",
                            "shared",
                            json!({ "id": "shared", "__version": 2, "by": id }),
                        ),
                        set("cells", second, json!({ "id": second })),
                    ],
                    None,
                )
                .map(|_| id)
            }));
        }

        let outcomes: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        let winners: Vec<_> = outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref().ok())
            .collect();
        let losers: Vec<_> = outcomes
            .iter()
            .filter_map(|outcome| outcome.as_ref().err())
            .collect();

        assert_eq!(winners.len(), 1, "exactly one transaction may commit");
        assert_eq!(losers.len(), 1);
        assert!(
            matches!(losers[0], FlowError::PreconditionFailed(_)),
            "the loser receives a structured conflict, not an infrastructure error: {:?}",
            losers[0]
        );

        let winner = *winners[0];
        assert_eq!(version_of(&db, "cells", "shared"), 2);
        assert_eq!(field(&db, "cells", "shared", "by"), winner);

        let loser_record = if winner == "t1" { "c" } else { "b" };
        let winner_record = if winner == "t1" { "b" } else { "c" };
        assert!(
            present(&db, "cells", winner_record),
            "the winner's second write committed"
        );
        assert!(
            !present(&db, "cells", loser_record),
            "the loser wrote neither of its records"
        );
    }

    // -- the boundary that used to exist ----------------------------------

    #[test]
    fn a_fenced_write_is_fenced_against_its_own_successor() {
        // This test previously asserted the opposite, and recorded it as a
        // known limitation: because a transaction write stored the caller's
        // value verbatim, two transactions fencing on the same version both
        // committed, and `ifVersion` was a check that permitted its own
        // successor rather than a fence.
        //
        // The authority now advances the version of a fenced write, so the
        // second is refused. The test is kept and inverted rather than deleted,
        // because the gap is the reason the contract exists.
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "cells", "c", 1, json!({}));

        guarded(
            &db,
            "tx-a",
            &[expect_version("cells", "c", 1)],
            &[set("cells", "c", json!({ "id": "c", "written": "a" }))],
        )
        .expect("first commits");
        assert_eq!(version_of(&db, "cells", "c"), 2, "and advances the version");

        let second = guarded(
            &db,
            "tx-b",
            &[expect_version("cells", "c", 1)],
            &[set("cells", "c", json!({ "id": "c", "written": "b" }))],
        );
        assert!(
            second.is_err(),
            "a caller no longer has to advance __version by hand to be fenced against itself"
        );
        assert_eq!(field(&db, "cells", "c", "written"), "a");
    }

    // -- version advancement ----------------------------------------------
    //
    // docs/architecture/transaction-version-contract.md: a write fenced by
    // expectedVersion is a conditional replacement, and the authority advances
    // the version so `ifVersion` is a fence rather than a check that permits
    // its own successor.

    #[test]
    fn a_version_fenced_write_advances_the_version() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "cells", "c", 1, json!({}));

        guarded(
            &db,
            "tx-advance",
            &[expect_version("cells", "c", 1)],
            &[set("cells", "c", json!({ "id": "c", "state": "written" }))],
        )
        .expect("commits");

        assert_eq!(
            version_of(&db, "cells", "c"),
            2,
            "validated at 1, written at 2"
        );
        assert_eq!(field(&db, "cells", "c", "state"), "written");
    }

    #[test]
    fn the_authority_owns_the_transition_and_replaces_a_supplied_version() {
        // A caller that computes a different next version must not be able to
        // install it, exactly as updateIfVersion strips __version from updates.
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "cells", "c", 4, json!({}));

        guarded(
            &db,
            "tx-owned",
            &[expect_version("cells", "c", 4)],
            &[set("cells", "c", json!({ "id": "c", "__version": 99 }))],
        )
        .expect("commits");
        assert_eq!(
            version_of(&db, "cells", "c"),
            5,
            "the authority's transition, not the caller's"
        );
    }

    #[test]
    fn a_second_transaction_on_the_same_version_now_fails() {
        // The gap this closes: before advancement, both of these committed.
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "cells", "c", 7, json!({}));

        guarded(
            &db,
            "tx-first",
            &[expect_version("cells", "c", 7)],
            &[set("cells", "c", json!({ "id": "c", "by": "first" }))],
        )
        .expect("the first commits");

        let second = guarded(
            &db,
            "tx-second",
            &[expect_version("cells", "c", 7)],
            &[set("cells", "c", json!({ "id": "c", "by": "second" }))],
        );
        assert!(
            second.is_err(),
            "ifVersion is now a fence: the successor is refused"
        );
        assert!(matches!(
            failure(second.unwrap_err()),
            PreconditionFailure::Version {
                expected: 7,
                actual: 8,
                ..
            }
        ));
        assert_eq!(field(&db, "cells", "c", "by"), "first");
    }

    #[test]
    fn sequential_transactions_each_advancing_both_succeed() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "cells", "c", 1, json!({}));

        guarded(
            &db,
            "s1",
            &[expect_version("cells", "c", 1)],
            &[set("cells", "c", json!({ "id": "c" }))],
        )
        .expect("first");
        guarded(
            &db,
            "s2",
            &[expect_version("cells", "c", 2)],
            &[set("cells", "c", json!({ "id": "c" }))],
        )
        .expect("second, on the advanced version");
        assert_eq!(version_of(&db, "cells", "c"), 3);
    }

    #[test]
    fn each_versioned_record_in_one_transaction_advances_independently() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "cells", "a", 1, json!({}));
        seed(&db, "cells", "b", 5, json!({}));

        guarded(
            &db,
            "tx-multi",
            &[
                expect_version("cells", "a", 1),
                expect_version("cells", "b", 5),
            ],
            &[
                set("cells", "a", json!({ "id": "a" })),
                set("cells", "b", json!({ "id": "b" })),
                set("cells", "c", json!({ "id": "c" })),
            ],
        )
        .expect("commits");

        assert_eq!(version_of(&db, "cells", "a"), 2);
        assert_eq!(version_of(&db, "cells", "b"), 6);
        assert_eq!(
            version_of(&db, "cells", "c"),
            0,
            "an unfenced write in the same transaction is stored verbatim and gains no version"
        );
    }

    #[test]
    fn a_guard_only_record_is_not_advanced() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "sessions", "s", 3, json!({}));

        guarded(
            &db,
            "tx-guard-only",
            &[expect_version("sessions", "s", 3)],
            &[set("changes", "c1", json!({ "id": "c1" }))],
        )
        .expect("commits");

        assert_eq!(
            version_of(&db, "sessions", "s"),
            3,
            "a guard is read, not written: its version is untouched"
        );
        assert!(present(&db, "changes", "c1"));
    }

    #[test]
    fn an_unconditional_write_is_stored_verbatim() {
        // What preserves every existing transaction caller.
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "cells", "c", 4, json!({}));

        db.apply_atomic_transaction(
            "tx-plain",
            None,
            &[],
            &[set(
                "cells",
                "c",
                json!({ "id": "c", "__version": 4, "state": "same" }),
            )],
            None,
        )
        .expect("commits");
        assert_eq!(
            version_of(&db, "cells", "c"),
            4,
            "unchanged, because nothing fenced it"
        );

        db.apply_atomic_transaction(
            "tx-plain-2",
            None,
            &[],
            &[set("cells", "unversioned", json!({ "id": "unversioned" }))],
            None,
        )
        .expect("commits");
        assert_eq!(
            version_of(&db, "cells", "unversioned"),
            0,
            "and the authority assigns no version to a record that has none"
        );
    }

    #[test]
    fn a_fenced_delete_validates_and_removes_without_a_version_transition() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "cells", "c", 2, json!({}));

        let stale = guarded(
            &db,
            "tx-del-stale",
            &[expect_version("cells", "c", 1)],
            &[delete("cells", "c")],
        );
        assert!(stale.is_err(), "a stale fence refuses the delete");
        assert!(present(&db, "cells", "c"));

        guarded(
            &db,
            "tx-del",
            &[expect_version("cells", "c", 2)],
            &[delete("cells", "c")],
        )
        .expect("the current fence permits it");
        assert!(
            !present(&db, "cells", "c"),
            "fence, then delete; there is no record left to version"
        );
    }

    #[test]
    fn a_transaction_create_gets_authority_version_one_and_can_be_fenced_immediately() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);

        guarded(
            &db,
            "tx-insert",
            &[RecordPrecondition {
                require_absent: true,
                ..precondition("cells", "new")
            }],
            &[set(
                "cells",
                "new",
                json!({ "id": "new", "__version": 999, "state": "created" }),
            )],
        )
        .expect("commits");
        assert_eq!(
            version_of(&db, "cells", "new"),
            1,
            "the authority owns the initial version"
        );

        guarded(
            &db,
            "tx-update-new",
            &[expect_version("cells", "new", 1)],
            &[set(
                "cells",
                "new",
                json!({ "id": "new", "state": "updated" }),
            )],
        )
        .expect("the newly created version can be fenced immediately");
        assert_eq!(version_of(&db, "cells", "new"), 2);

        let missing = guarded(
            &db,
            "tx-fence-absent",
            &[expect_version("cells", "absent", 1)],
            &[set("cells", "absent", json!({ "id": "absent" }))],
        );
        assert!(
            matches!(
                failure(missing.unwrap_err()),
                PreconditionFailure::Missing { .. }
            ),
            "a version-fenced write against an absent record is a conflict, not an insert"
        );
    }

    #[test]
    fn replayed_conditional_create_does_not_create_or_advance_twice() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        let absent = RecordPrecondition {
            require_absent: true,
            ..precondition("cells", "new")
        };

        let first = guarded(
            &db,
            "tx-create-once",
            &[absent.clone()],
            &[set("cells", "new", json!({ "id": "new", "delivery": 1 }))],
        )
        .expect("first delivery commits");
        assert!(!first.duplicate);

        let replay = guarded(
            &db,
            "tx-create-once",
            &[absent],
            &[set("cells", "new", json!({ "id": "new", "delivery": 2 }))],
        )
        .expect("replay returns the original transaction result");
        assert!(replay.duplicate);
        assert_eq!(version_of(&db, "cells", "new"), 1);
        assert_eq!(field(&db, "cells", "new", "delivery"), 1);
    }

    #[test]
    fn replay_of_a_fenced_transaction_does_not_advance_twice() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db, "cells", "c", 1, json!({}));

        guarded(
            &db,
            "tx-once",
            &[expect_version("cells", "c", 1)],
            &[set("cells", "c", json!({ "id": "c" }))],
        )
        .expect("commits");
        assert_eq!(version_of(&db, "cells", "c"), 2);

        let replay = guarded(
            &db,
            "tx-once",
            &[expect_version("cells", "c", 1)],
            &[set("cells", "c", json!({ "id": "c" }))],
        )
        .expect("a committed id replays");
        assert!(replay.duplicate);
        assert_eq!(
            version_of(&db, "cells", "c"),
            2,
            "a replay cannot advance the version again"
        );
    }

    #[test]
    fn competing_fenced_transactions_leave_the_version_at_exactly_one_advance() {
        use std::sync::Arc;
        let dir = TempDir::new().unwrap();
        let db = Arc::new(db(&dir));
        seed(&db, "cells", "race", 1, json!({}));

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for id in ["r1", "r2"] {
            let db = Arc::clone(&db);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                db.apply_atomic_transaction_guarded(
                    id,
                    None,
                    None,
                    &[],
                    &[RecordPrecondition {
                        expected_version: Some(1),
                        ..RecordPrecondition {
                            capability: "cells".into(),
                            key: "cells:race".into(),
                            ..Default::default()
                        }
                    }],
                    &[set("cells", "race", json!({ "id": "race", "by": id }))],
                    None,
                )
                .map(|_| id)
            }));
        }
        let outcomes: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            version_of(&db, "cells", "race"),
            2,
            "exactly one advance, whichever transaction won"
        );
    }
}
