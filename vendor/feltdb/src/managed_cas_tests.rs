use std::{fs, path::PathBuf};

use serde_json::json;

use crate::{FeltDb, JsonCasResult};

fn database(name: &str) -> (FeltDb, PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "feltdb-managed-cas-{name}-{}-{}.log",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    (FeltDb::open(&path).unwrap(), path)
}

#[test]
fn version_and_epoch_are_one_atomic_predicate() {
    let (db, path) = database("predicate");
    let key = "_cells:cell-1";
    db.insert(
        key,
        json!({
            "id": "cell-1",
            "version": 42,
            "value": "before",
            "authority": { "owner": "A", "epoch": 7, "lifecycle": "active" },
            "__version": 1
        }),
    )
    .unwrap();

    let stale_epoch = db
        .compare_and_set_json(
            key,
            1,
            Some(6),
            None,
            false,
            json!({
                "id": "cell-1",
                "version": 42,
                "value": "stale",
                "authority": { "owner": "stale", "epoch": 7, "lifecycle": "active" }
            }),
        )
        .unwrap();
    assert_eq!(
        stale_epoch,
        JsonCasResult::AuthorityConflict {
            current_version: 1,
            current_epoch: 7,
        }
    );
    assert_eq!(db.get_value(key).unwrap().unwrap()["value"], "before");

    let updated = db
        .compare_and_set_json(
            key,
            1,
            Some(7),
            None,
            false,
            json!({
                "id": "cell-1",
                "version": 42,
                "value": "after",
                "authority": { "owner": "B", "epoch": 8, "lifecycle": "active" }
            }),
        )
        .unwrap();
    assert!(matches!(
        updated,
        JsonCasResult::Updated {
            current_version: 2,
            current_epoch: 8,
            ..
        }
    ));

    let stale_version = db
        .compare_and_set_json(key, 1, Some(8), None, false, json!({ "value": "lost" }))
        .unwrap();
    assert_eq!(
        stale_version,
        JsonCasResult::VersionConflict {
            current_version: 2,
            current_epoch: 8,
        }
    );
    let final_value = db.get_value(key).unwrap().unwrap();
    assert_eq!(final_value["value"], "after");
    assert_eq!(final_value["__version"], 2);
    assert_eq!(final_value["authority"]["epoch"], 8);

    drop(db);
    let _ = fs::remove_file(path);
}

#[test]
fn lease_is_checked_with_the_server_clock_inside_cas() {
    let (db, path) = database("lease");
    let key = "_cells:leased";
    let future = crate::now_ms() as u64 + 60_000;
    db.insert(
        key,
        json!({
            "value": "before",
            "authority": { "epoch": 3 },
            "lease": { "leaseId": "lease-good", "expiresAt": future },
            "__version": 1
        }),
    )
    .unwrap();

    for expected_lease_id in [None, Some("lease-wrong")] {
        assert_eq!(
            db.compare_and_set_json(
                key,
                1,
                Some(3),
                expected_lease_id,
                false,
                json!({ "value": "rejected", "authority": { "epoch": 3 } }),
            )
            .unwrap(),
            JsonCasResult::LeaseConflict {
                current_version: 1,
                current_epoch: 3,
            }
        );
        assert_eq!(db.get_value(key).unwrap().unwrap()["value"], "before");
    }

    assert!(matches!(
        db.compare_and_set_json(
            key,
            1,
            Some(3),
            Some("lease-good"),
            false,
            json!({ "value": "accepted", "authority": { "epoch": 3 }, "lease": { "leaseId": "lease-good", "expiresAt": future } }),
        )
        .unwrap(),
        JsonCasResult::Updated { .. }
    ));

    let expired_key = "_cells:expired";
    db.insert(
        expired_key,
        json!({
            "value": "before",
            "authority": { "epoch": 3 },
            "lease": { "leaseId": "lease-expired", "expiresAt": 1 },
            "__version": 1
        }),
    )
    .unwrap();
    assert_eq!(
        db.compare_and_set_json(
            expired_key,
            1,
            Some(3),
            Some("lease-expired"),
            false,
            json!({ "value": "rejected", "authority": { "epoch": 3 } }),
        )
        .unwrap(),
        JsonCasResult::LeaseConflict {
            current_version: 1,
            current_epoch: 3,
        }
    );
    assert_eq!(
        db.get_value(expired_key).unwrap().unwrap()["value"],
        "before"
    );

    drop(db);
    let _ = fs::remove_file(path);
}

#[test]
fn divergent_replica_blocks_promotion_inside_cas() {
    let (db, path) = database("divergence");
    let cell_key = "_cells:replica";
    let replica_key = "_replicas:replica";
    db.insert(
        cell_key,
        json!({
            "version": 42,
            "value": "A",
            "authority": { "owner": "A", "epoch": 7 },
            "lease": null,
            "__version": 42
        }),
    )
    .unwrap();
    db.insert(
        replica_key,
        json!({ "version": 42, "value": "B", "divergent": true }),
    )
    .unwrap();
    let sequence_before = db.sequence().unwrap();
    let value_before = db.get_value(cell_key).unwrap().unwrap();

    assert_eq!(
        db.compare_and_set_json(
            cell_key,
            42,
            Some(7),
            None,
            true,
            json!({
                "version": 43,
                "value": "A",
                "authority": { "owner": "B", "epoch": 8 },
                "lease": null
            }),
        )
        .unwrap(),
        JsonCasResult::ReplicationConflict {
            current_version: 42,
            current_epoch: 7,
        }
    );
    assert_eq!(db.sequence().unwrap(), sequence_before);
    assert_eq!(db.get_value(cell_key).unwrap().unwrap(), value_before);

    drop(db);
    let _ = fs::remove_file(path);
}
