//! Production Gate P1 — application-facing atomicity.
//!
//! `apply_atomic_transaction` writes one fsynced transaction record, and
//! `replay_log` restores every row of that record together. These tests hold
//! the *application* store to the same all-or-nothing contract the transaction
//! primitive guarantees, including the case that matters most in practice:
//! the process died while the transaction record was being written.

#[cfg(test)]
mod tests {
    use crate::{AtomicMutation, AtomicPrecondition, FeltDb};
    use serde_json::json;
    use std::io::{Seek, SeekFrom, Write};
    use tempfile::TempDir;

    fn mutation(capability: &str, key: &str, value: serde_json::Value) -> AtomicMutation {
        AtomicMutation {
            capability: capability.to_string(),
            key: key.to_string(),
            value: Some(value),
        }
    }

    /// A cross-collection transaction, as an application would issue it.
    fn user_order_audit() -> Vec<AtomicMutation> {
        vec![
            mutation("users", "users:u1", json!({ "id": "u1", "name": "Ada" })),
            mutation(
                "orders",
                "orders:o1",
                json!({ "id": "o1", "user": "u1", "total": 42 }),
            ),
            mutation(
                "audit",
                "audit:a1",
                json!({ "id": "a1", "event": "order.created" }),
            ),
        ]
    }

    fn present(db: &FeltDb, key: &str) -> bool {
        db.get_value(key).expect("read must succeed").is_some()
    }

    fn visible_count(db: &FeltDb) -> usize {
        ["users:u1", "orders:o1", "audit:a1"]
            .iter()
            .filter(|key| present(db, key))
            .count()
    }

    #[test]
    fn a_cross_collection_transaction_commits_every_operation(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("app.log");
        let db = FeltDb::open(&path)?;

        let commit = db.apply_atomic_transaction("tx-1", None, &[], &user_order_audit(), None)?;
        assert_eq!(commit.rows.len(), 3);
        assert!(!commit.duplicate);
        assert_eq!(visible_count(&db), 3);
        Ok(())
    }

    #[test]
    fn a_cross_collection_transaction_survives_restart() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("app.log");

        {
            let db = FeltDb::open(&path)?;
            db.apply_atomic_transaction("tx-1", None, &[], &user_order_audit(), None)?;
        }

        let reopened = FeltDb::open(&path)?;
        assert_eq!(
            visible_count(&reopened),
            3,
            "every row must recover together"
        );
        Ok(())
    }

    #[test]
    fn a_failed_precondition_leaves_no_operation_applied() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = TempDir::new()?;
        let path = dir.path().join("app.log");
        let db = FeltDb::open(&path)?;

        // The middle key must already exist at a given version; it does not.
        let preconditions = vec![AtomicPrecondition {
            capability: "orders".to_string(),
            key: "orders:o1".to_string(),
            expected_version: Some(7),
        }];

        let outcome =
            db.apply_atomic_transaction("tx-bad", None, &preconditions, &user_order_audit(), None);
        assert!(
            outcome.is_err(),
            "a failed precondition must reject the transaction"
        );

        assert_eq!(visible_count(&db), 0, "no operation may be applied");
        drop(db);

        let reopened = FeltDb::open(&path)?;
        assert_eq!(visible_count(&reopened), 0, "and nothing may be durable");
        Ok(())
    }

    #[test]
    fn replaying_a_transaction_id_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("app.log");
        let db = FeltDb::open(&path)?;

        let first = db.apply_atomic_transaction("tx-1", None, &[], &user_order_audit(), None)?;
        assert!(!first.duplicate);
        let second = db.apply_atomic_transaction("tx-1", None, &[], &user_order_audit(), None)?;
        assert!(
            second.duplicate,
            "the same transaction id must not apply twice"
        );
        assert_eq!(visible_count(&db), 3);
        Ok(())
    }

    #[test]
    fn transaction_identity_survives_restart() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("app.log");

        {
            let db = FeltDb::open(&path)?;
            db.apply_atomic_transaction("tx-stable", None, &[], &user_order_audit(), None)?;
        }

        let reopened = FeltDb::open(&path)?;
        let replayed =
            reopened.apply_atomic_transaction("tx-stable", None, &[], &user_order_audit(), None)?;
        assert!(
            replayed.duplicate,
            "identity must be restored so redelivery stays idempotent"
        );
        assert_eq!(visible_count(&reopened), 3);
        Ok(())
    }

    /// The case that matters in production: the process died while the
    /// transaction record was being written.
    ///
    /// The half-written record is not a committed transaction, so recovery must
    /// discard it and open the store with the previously committed history
    /// intact. Refusing to open at all would turn a normal crash into an
    /// outage.
    #[test]
    fn a_torn_transaction_record_is_discarded_and_the_store_still_opens(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("app.log");

        {
            let db = FeltDb::open(&path)?;
            db.apply_atomic_transaction("tx-1", None, &[], &user_order_audit(), None)?;
        }

        // Append a partial transaction record with no terminating newline,
        // exactly as a crash mid-write would leave behind.
        let complete = std::fs::read_to_string(&path)?;
        let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
        let torn = &complete[..complete.len() / 2];
        file.write_all(torn.as_bytes())?;
        file.flush()?;
        drop(file);

        let recovered = FeltDb::open(&path).map_err(|error| {
            format!("a torn tail must not stop the store from opening: {error}")
        })?;
        assert_eq!(
            visible_count(&recovered),
            3,
            "committed history must survive"
        );
        Ok(())
    }

    /// A transaction that was still being written never committed, so none of
    /// its rows may appear.
    #[test]
    fn a_transaction_torn_mid_write_applies_nothing() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("app.log");

        // Write a complete transaction, then a partial second one.
        {
            let db = FeltDb::open(&path)?;
            db.apply_atomic_transaction("tx-1", None, &[], &user_order_audit(), None)?;
        }
        let after_first = std::fs::read_to_string(&path)?;

        {
            let db = FeltDb::open(&path)?;
            db.apply_atomic_transaction(
                "tx-2",
                None,
                &[],
                &[mutation(
                    "users",
                    "users:u2",
                    json!({ "id": "u2", "name": "Grace" }),
                )],
                None,
            )?;
        }

        // Truncate the second record part-way through.
        let full = std::fs::read_to_string(&path)?;
        let second = &full[after_first.len()..];
        let keep = after_first.len() + second.len() / 2;
        let file = std::fs::OpenOptions::new().write(true).open(&path)?;
        file.set_len(keep as u64)?;
        file.sync_all()?;
        drop(file);

        let recovered = FeltDb::open(&path)?;
        assert_eq!(
            visible_count(&recovered),
            3,
            "the committed transaction survives"
        );
        assert!(
            !present(&recovered, "users:u2"),
            "a torn transaction must apply nothing"
        );
        Ok(())
    }

    /// Damage before the end of the log is corruption, not an interrupted
    /// write, and recovery must refuse rather than silently drop history.
    #[test]
    fn interior_corruption_is_still_refused() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("app.log");

        {
            let db = FeltDb::open(&path)?;
            db.apply_atomic_transaction("tx-1", None, &[], &user_order_audit(), None)?;
            db.apply_atomic_transaction(
                "tx-2",
                None,
                &[],
                &[mutation("users", "users:u2", json!({ "id": "u2" }))],
                None,
            )?;
        }

        let mut file = std::fs::OpenOptions::new().write(true).open(&path)?;
        file.seek(SeekFrom::Start(30))?;
        file.write_all(b"@@@@@@@@")?;
        file.sync_all()?;
        drop(file);

        assert!(
            FeltDb::open(&path).is_err(),
            "corruption before the end of the log must not be silently discarded"
        );
        Ok(())
    }

    #[test]
    fn single_operation_writes_remain_compatible() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("app.log");

        {
            let db = FeltDb::open(&path)?;
            // The existing single-operation API, unchanged.
            db.insert("users:solo", json!({ "id": "solo", "name": "Alan" }))?;
            // Mixed with a transaction.
            db.apply_atomic_transaction("tx-1", None, &[], &user_order_audit(), None)?;
            db.insert("users:solo2", json!({ "id": "solo2" }))?;
        }

        let reopened = FeltDb::open(&path)?;
        assert!(
            present(&reopened, "users:solo"),
            "single writes must still recover"
        );
        assert!(present(&reopened, "users:solo2"));
        assert_eq!(
            visible_count(&reopened),
            3,
            "transaction rows recover alongside them"
        );
        Ok(())
    }

    #[test]
    fn deletes_participate_in_a_transaction() -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("app.log");

        {
            let db = FeltDb::open(&path)?;
            db.apply_atomic_transaction("seed", None, &[], &user_order_audit(), None)?;
            // Delete one row and create another, atomically.
            db.apply_atomic_transaction(
                "move",
                None,
                &[],
                &[
                    AtomicMutation {
                        capability: "orders".to_string(),
                        key: "orders:o1".to_string(),
                        value: None,
                    },
                    mutation("orders", "orders:o2", json!({ "id": "o2", "total": 42 })),
                ],
                None,
            )?;
        }

        let reopened = FeltDb::open(&path)?;
        assert!(!present(&reopened, "orders:o1"), "the delete must persist");
        assert!(present(&reopened, "orders:o2"), "the insert must persist");
        assert!(present(&reopened, "users:u1"), "untouched rows are kept");
        Ok(())
    }
}
