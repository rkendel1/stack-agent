//! Read-basis fences: commit only if the state a decision read is unchanged.
//!
//! ```text
//!     basis = read_basis(records, predicates)       -- one token, under the lock
//!     ... decide ...
//!     commit fenced by basis                        -- re-read under the commit lock
//!         unchanged  ->  commit
//!         changed    ->  PreconditionFailed(Fence), ZERO writes
//! ```
//!
//! The fence reads stored rows, not bookkeeping a writer maintains, so it holds
//! whichever write path changed them. Only the named records and the named
//! collections' matching rows are read: everything else is outside the fence
//! and never conflicts.

#[cfg(test)]
mod tests {
    use crate::{
        AtomicMutation, FeltDb, FlowError, PreconditionFailure, ReadBasis, ReadCondition,
        ReadLayout, ReadPredicate, ReadRecord, ReadSet,
    };
    use serde_json::json;
    use tempfile::TempDir;

    fn db(dir: &TempDir) -> FeltDb {
        FeltDb::open(dir.path().join("app.log")).expect("open")
    }

    fn set(collection: &str, id: &str, value: serde_json::Value) -> AtomicMutation {
        AtomicMutation {
            capability: collection.to_string(),
            key: format!("{collection}:{id}"),
            value: Some(value),
        }
    }

    fn delete(collection: &str, id: &str) -> AtomicMutation {
        AtomicMutation {
            capability: collection.to_string(),
            key: format!("{collection}:{id}"),
            value: None,
        }
    }

    fn write(db: &FeltDb, id: &str, mutations: &[AtomicMutation]) {
        db.apply_atomic_transaction(id, None, &[], mutations, None)
            .expect("write commits");
    }

    fn reads() -> ReadSet {
        ReadSet {
            records: vec![ReadRecord {
                collection: "A".into(),
                id: "a1".into(),
            }],
            predicates: vec![ReadPredicate {
                collection: "B".into(),
                conditions: vec![ReadCondition {
                    field: "a".into(),
                    eq: json!("a1"),
                }],
            }],
        }
    }

    fn seed(db: &FeltDb) {
        write(
            db,
            "seed",
            &[
                set("A", "a1", json!({ "id": "a1", "n": 1 })),
                set("B", "b1", json!({ "id": "b1", "a": "a1" })),
                set("B", "b2", json!({ "id": "b2", "a": "a2" })),
            ],
        );
    }

    fn fenced(db: &FeltDb, id: &str, basis: &ReadBasis) -> Result<crate::AtomicCommit, FlowError> {
        db.apply_atomic_transaction_fenced(
            id,
            None,
            None,
            &[],
            &[],
            std::slice::from_ref(basis),
            &[set("Z", id, json!({ "id": id }))],
            None,
        )
    }

    fn is_fence_conflict(result: Result<crate::AtomicCommit, FlowError>) -> bool {
        matches!(result, Err(FlowError::PreconditionFailed(failure)) if matches!(*failure, PreconditionFailure::Fence { .. }))
    }

    #[test]
    fn an_unchanged_basis_commits_and_a_changed_one_commits_nothing() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db);
        let basis = db.read_basis(&reads()).unwrap();
        assert!(fenced(&db, "z1", &basis).is_ok());
        write(&db, "raw", &[set("A", "a1", json!({ "id": "a1", "n": 2 }))]);
        assert!(is_fence_conflict(fenced(&db, "z2", &basis)));
        assert!(
            db.get_value("Z:z2").unwrap().is_none(),
            "a refused transaction writes nothing"
        );
    }

    #[test]
    fn records_entering_or_leaving_a_predicate_conflict_and_others_do_not() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db);
        let basis = db.read_basis(&reads()).unwrap();
        write(
            &db,
            "outside",
            &[
                set("B", "b3", json!({ "id": "b3", "a": "a9" })),
                set("C", "c1", json!({ "id": "c1" })),
            ],
        );
        assert!(
            fenced(&db, "z1", &basis).is_ok(),
            "writes outside the read set never conflict"
        );

        write(
            &db,
            "enter",
            &[set("B", "b4", json!({ "id": "b4", "a": "a1" }))],
        );
        assert!(is_fence_conflict(fenced(&db, "z2", &basis)));

        let basis = db.read_basis(&reads()).unwrap();
        write(
            &db,
            "leave",
            &[set("B", "b1", json!({ "id": "b1", "a": "a2" }))],
        );
        assert!(is_fence_conflict(fenced(&db, "z3", &basis)));
    }

    #[test]
    fn delete_and_recreate_with_different_content_conflicts() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db);
        let basis = db.read_basis(&reads()).unwrap();
        write(&db, "delete", &[delete("A", "a1")]);
        write(
            &db,
            "recreate",
            &[set("A", "a1", json!({ "id": "a1", "n": 7 }))],
        );
        assert!(is_fence_conflict(fenced(&db, "z1", &basis)));
    }

    #[test]
    fn tokens_are_deterministic_and_survive_restart() {
        let dir = TempDir::new().unwrap();
        let basis = {
            let db = db(&dir);
            seed(&db);
            let first = db.read_basis(&reads()).unwrap();
            let mut shuffled = reads();
            shuffled.records.push(ReadRecord {
                collection: "A".into(),
                id: "a1".into(),
            });
            assert_eq!(
                first,
                db.read_basis(&shuffled).unwrap(),
                "normalised: duplicates do not matter"
            );
            first
        };
        let db = db(&dir);
        assert_eq!(db.read_basis(&reads()).unwrap().token, basis.token);
        assert!(fenced(&db, "z1", &basis).is_ok());
    }

    #[test]
    fn invalid_and_foreign_bases_are_refused_as_invalid_not_as_conflicts() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        seed(&db);
        assert!(db.read_basis(&ReadSet::default()).is_err());
        let mut foreign = db.read_basis(&reads()).unwrap();
        foreign.token = format!("fb1:{}", "0".repeat(64));
        let result = fenced(&db, "z1", &foreign);
        assert!(
            matches!(result, Err(FlowError::CapabilityError(ref message)) if message.starts_with("INVALID_READ_SET:"))
        );
    }

    #[test]
    fn namespace_bases_read_namespace_rows_and_are_scoped_to_their_namespace() {
        let dir = TempDir::new().unwrap();
        let db = db(&dir);
        let ns = ReadLayout::Namespace("tenant-a".into());
        let row = |collection: &str, id: &str, value: serde_json::Value| AtomicMutation {
            capability: format!("tenant-a:{collection}"),
            key: id.to_string(),
            value: Some(value),
        };
        write(
            &db,
            "seed",
            &[
                row("A", "a1", json!({ "id": "a1", "n": 1 })),
                row("B", "b1", json!({ "id": "b1", "a": "a1" })),
            ],
        );
        let basis = db.read_basis_in(&reads(), &ns).unwrap();
        assert!(basis.token.starts_with(&ns.token_prefix()));
        let commit = |id: &str, layout: &ReadLayout, basis: &ReadBasis| {
            db.apply_atomic_transaction_fenced_in(
                id,
                None,
                None,
                &[],
                &[],
                layout,
                std::slice::from_ref(basis),
                &[row("Z", id, json!({ "id": id }))],
                None,
            )
        };
        let invalid = |result: Result<crate::AtomicCommit, FlowError>| matches!(result, Err(FlowError::CapabilityError(ref message)) if message.starts_with("INVALID_READ_SET:"));
        // A token from another scope is invalid, never compared.
        assert!(invalid(commit("z0", &ReadLayout::Direct, &basis)));
        assert!(invalid(commit(
            "z0",
            &ReadLayout::Namespace("tenant-b".into()),
            &basis
        )));
        let direct = db.read_basis(&reads()).unwrap();
        assert!(invalid(commit("z0", &ns, &direct)));
        // Direct-layout writes to the same names are outside the namespace.
        write(
            &db,
            "direct",
            &[set("A", "a1", json!({ "id": "a1", "n": 9 }))],
        );
        assert!(commit("z1", &ns, &basis).is_ok());
        // A namespace write the basis read refuses the commit.
        write(
            &db,
            "raw",
            &[row("B", "b9", json!({ "id": "b9", "a": "a1" }))],
        );
        let result = commit("z2", &ns, &basis);
        assert!(is_fence_conflict(result));
    }
}
