//! Durable admission: a work is a FeltDB task from the moment it is accepted.
//!
//! The database is a real file reopened across "restarts". A `ready` task is FeltDB's own status
//! for a task that exists and has not started; nothing here adds a state or a queue.

use std::sync::{Arc, Barrier};
use std::time::Duration;

use chip_agent_state::feltdb::{AtomicMutation, FeltDb};
use chip_agent_state::{
    ADMISSION_COLLECTION, Admission, AdmissionOrigin, AdmissionRecord, AgentStore, Begun,
    DurableConfig, Ensured, StateError, admission, admit, begin, cancel_admitted, ensure_admitted,
    lease_is_live, waiting,
};

fn config() -> DurableConfig {
    DurableConfig {
        lease_ms: 60_000,
        ..Default::default()
    }
}

fn path(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path().join("state.log")
}

fn open(dir: &tempfile::TempDir) -> AgentStore {
    AgentStore::new(FeltDb::open(path(dir)).unwrap())
}

/// One task as the database lists it.
fn task_of(store: &AgentStore, id: &str) -> chip_agent_state::TaskValue {
    store
        .tasks()
        .unwrap()
        .into_iter()
        .find(|t| t.task_id == id)
        .unwrap_or_else(|| panic!("no task {id}"))
}

/// The admission lease is 1 ms; a starter takes over once it has passed.
fn after_admission_lease() {
    std::thread::sleep(Duration::from_millis(5));
}

#[test]
fn an_admitted_work_is_a_ready_task_that_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = open(&dir);
        admit(
            &store,
            "work_a",
            "change",
            "Add `two`.",
            "Add `two`.",
            AdmissionOrigin::Service,
            &config(),
        )
        .unwrap();
        let task = task_of(&store, "work_a");
        assert_eq!(task.status, "ready");
        assert_eq!(task.objective, "Add `two`.");
    }
    // A new process over the same file.
    let store = open(&dir);
    let waiting = waiting(&store).unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].task.task_id, "work_a");
    match &waiting[0].admission {
        Admission::Valid(r) => {
            assert_eq!((r.kind.as_str(), r.goal.as_str()), ("change", "Add `two`."));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_goal_longer_than_the_objective_is_kept_whole_in_the_admission() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    let goal = "x".repeat(1900);
    admit(
        &store,
        "work_long",
        "inspect",
        &goal,
        "x…",
        AdmissionOrigin::Service,
        &config(),
    )
    .unwrap();
    let Admission::Valid(r) = admission(&store, "work_long") else {
        panic!()
    };
    assert_eq!(
        r.goal, goal,
        "the objective is cut at 1,024 bytes; the goal is not"
    );
}

#[test]
fn starting_an_admitted_work_is_the_ordinary_ready_to_running_transition() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    admit(
        &store,
        "work_s",
        "change",
        "g",
        "g",
        AdmissionOrigin::Service,
        &config(),
    )
    .unwrap();
    after_admission_lease();
    let Begun::Ready(work) = begin(&store, "work_s", "g", &config()).unwrap() else {
        panic!("the admitted task is taken over, not recreated")
    };
    assert_eq!(work.task_id(), "work_s", "no second identity");
    let task = task_of(&store, "work_s");
    assert_eq!(task.status, "running");
    // It is no longer waiting, and its admission record is unchanged.
    assert!(waiting(&store).unwrap().is_empty());
    assert!(matches!(admission(&store, "work_s"), Admission::Valid(_)));
}

#[test]
fn a_starter_must_name_the_objective_the_work_was_admitted_with() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    admit(
        &store,
        "work_o",
        "change",
        "the goal",
        "the goal",
        AdmissionOrigin::Service,
        &config(),
    )
    .unwrap();
    after_admission_lease();
    let err = begin(&store, "work_o", "another goal", &config())
        .err()
        .unwrap();
    assert!(matches!(err, StateError::ObjectiveMismatch { .. }), "{err}");
    assert_eq!(task_of(&store, "work_o").status, "ready");
}

#[test]
fn two_starters_cannot_both_start_one_admission() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    admit(
        &store,
        "work_c",
        "change",
        "g",
        "g",
        AdmissionOrigin::Service,
        &config(),
    )
    .unwrap();
    after_admission_lease();
    let barrier = Arc::new(Barrier::new(2));
    let starters: Vec<_> = (0..2)
        .map(|_| {
            let (store, barrier) = (store.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                begin(&store, "work_c", "g", &config()).map(|b| matches!(b, Begun::Ready(_)))
            })
        })
        .collect();
    let results: Vec<_> = starters.into_iter().map(|s| s.join().unwrap()).collect();
    let started = results.iter().filter(|r| matches!(r, Ok(true))).count();
    assert_eq!(started, 1, "exactly one starter owns the work: {results:?}");
    assert!(
        results
            .iter()
            .any(|r| matches!(r, Err(StateError::LeaseHeld { .. }))),
        "the other is refused, not run twice: {results:?}"
    );
}

#[test]
fn admitting_the_same_work_twice_keeps_the_first_and_refuses_the_second() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    admit(
        &store,
        "work_d",
        "change",
        "first",
        "first",
        AdmissionOrigin::Service,
        &config(),
    )
    .unwrap();
    let err = admit(
        &store,
        "work_d",
        "inspect",
        "second",
        "second",
        AdmissionOrigin::Service,
        &config(),
    )
    .unwrap_err();
    let _ = err;
    let Admission::Valid(r) = admission(&store, "work_d") else {
        panic!()
    };
    assert_eq!((r.kind.as_str(), r.goal.as_str()), ("change", "first"));
    assert_eq!(task_of(&store, "work_d").objective, "first");
}

#[test]
fn a_bad_request_is_refused_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    for (id, kind, goal) in [
        ("bad id!", "change", "g"),
        ("work_e", "deploy", "g"),
        ("work_e", "change", "   "),
    ] {
        let err = admit(
            &store,
            id,
            kind,
            goal,
            "g",
            AdmissionOrigin::Service,
            &config(),
        )
        .unwrap_err();
        assert!(matches!(err, StateError::InvalidInput(_)), "{err}");
    }
    assert!(store.tasks().unwrap().is_empty());
    assert!(matches!(admission(&store, "work_e"), Admission::Missing));
}

#[test]
fn cancelling_an_admitted_work_is_durable_and_it_never_waits_again() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = open(&dir);
        admit(
            &store,
            "work_x",
            "change",
            "g",
            "g",
            AdmissionOrigin::Service,
            &config(),
        )
        .unwrap();
        after_admission_lease();
        cancel_admitted(&store, "work_x", &config()).unwrap();
        assert_eq!(task_of(&store, "work_x").status, "cancelled");
    }
    let store = open(&dir);
    assert_eq!(task_of(&store, "work_x").status, "cancelled");
    assert!(waiting(&store).unwrap().is_empty());
    // A starter that races the cancellation is told it is finished, and runs nothing.
    assert!(matches!(
        begin(&store, "work_x", "g", &config()).unwrap(),
        Begun::Finished(_)
    ));
}

#[test]
fn only_a_work_that_has_not_started_can_be_cancelled_this_way() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    admit(
        &store,
        "work_r",
        "change",
        "g",
        "g",
        AdmissionOrigin::Service,
        &config(),
    )
    .unwrap();
    after_admission_lease();
    let Begun::Ready(_work) = begin(&store, "work_r", "g", &config()).unwrap() else {
        panic!()
    };
    let err = cancel_admitted(&store, "work_r", &config()).unwrap_err();
    assert!(matches!(err, StateError::InvalidInput(_)), "{err}");
    assert_eq!(task_of(&store, "work_r").status, "running");
    assert!(cancel_admitted(&store, "work_missing", &config()).is_err());
}

#[test]
fn waiting_work_comes_oldest_first_and_in_the_same_order_every_time() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    for id in ["work_3", "work_1", "work_2"] {
        admit(
            &store,
            id,
            "inspect",
            "g",
            "g",
            AdmissionOrigin::Service,
            &config(),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(3));
    }
    let order = |s: &AgentStore| {
        waiting(s)
            .unwrap()
            .into_iter()
            .map(|w| w.task.task_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&store), ["work_3", "work_1", "work_2"]);
    assert_eq!(order(&store), order(&store), "reading changes nothing");
}

#[test]
fn a_waiting_task_without_a_usable_admission_is_reported_not_repaired() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = open(&dir);
        for id in ["work_ok", "work_lost", "work_bad"] {
            admit(
                &store,
                id,
                "inspect",
                "g",
                "g",
                AdmissionOrigin::Service,
                &config(),
            )
            .unwrap();
        }
    }
    let db = FeltDb::open(path(&dir)).unwrap();
    let key = |id: &str| format!("{ADMISSION_COLLECTION}:{id}");
    let mutate = |id: &str, value: Option<serde_json::Value>| AtomicMutation {
        capability: ADMISSION_COLLECTION.into(),
        key: key(id),
        value,
    };
    // One record is lost, one is garbage.
    db.apply_atomic_transaction_guarded(
        "damage",
        None,
        None,
        &[],
        &[],
        &[mutate("work_lost", None)],
        None,
    )
    .unwrap();
    db.apply_atomic_transaction_guarded(
        "damage-two",
        None,
        None,
        &[],
        &[],
        &[mutate(
            "work_bad",
            Some(serde_json::json!({"nonsense": true})),
        )],
        None,
    )
    .unwrap();
    let store = AgentStore::new(db);
    let found = waiting(&store).unwrap();
    let state = |id: &str| {
        found
            .iter()
            .find(|w| w.task.task_id == id)
            .map(|w| w.admission.clone())
            .unwrap()
    };
    assert!(matches!(state("work_ok"), Admission::Valid(_)));
    assert_eq!(state("work_lost"), Admission::Missing);
    assert!(matches!(state("work_bad"), Admission::Unreadable(_)));
    // Reporting is all that happened: all three tasks are still there, still ready.
    assert_eq!(found.len(), 3);
    assert!(found.iter().all(|w| w.task.status == "ready"));
}

// ---- who admitted it, and what a crash between the steps leaves ---------------------------------

fn record_json(id: &str, kind: &str, goal: &str, extra: Option<(&str, &str)>) -> serde_json::Value {
    let mut v = serde_json::json!({
        "schema": "chip.admission.v1", "task_id": id, "kind": kind, "goal": goal,
        "admitted_at_ms": 1,
    });
    if let Some((k, val)) = extra {
        v[k] = val.into();
    }
    v
}

fn write_record(dir: &tempfile::TempDir, id: &str, value: Option<serde_json::Value>) {
    let db = FeltDb::open(path(dir)).unwrap();
    db.apply_atomic_transaction_guarded(
        "write-record",
        None,
        None,
        &[],
        &[],
        &[AtomicMutation {
            capability: ADMISSION_COLLECTION.into(),
            key: format!("{ADMISSION_COLLECTION}:{id}"),
            value,
        }],
        None,
    )
    .unwrap();
}

#[test]
fn an_admission_says_who_made_it_and_an_older_record_reads_as_a_service_admission() {
    // A record written before the field existed has no `origin`; it was a service admission.
    let old: AdmissionRecord =
        serde_json::from_value(record_json("work_old", "change", "g", None)).unwrap();
    assert_eq!(old.origin, AdmissionOrigin::Service);
    // A service admission is written without the field, so older readers still accept it...
    let service = serde_json::to_value(&old).unwrap();
    assert!(service.get("origin").is_none(), "{service}");
    // ...and a CLI admission says so.
    let cli = AdmissionRecord {
        origin: AdmissionOrigin::Cli,
        ..old
    };
    assert_eq!(serde_json::to_value(&cli).unwrap()["origin"], "cli");

    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    admit(
        &store,
        "work_cli",
        "inspect",
        "g",
        "g",
        AdmissionOrigin::Cli,
        &config(),
    )
    .unwrap();
    let Admission::Valid(r) = admission(&store, "work_cli") else {
        panic!()
    };
    assert_eq!(r.origin, AdmissionOrigin::Cli);
}

#[test]
fn a_crash_that_left_only_the_record_is_completed_by_the_same_request_and_refused_for_another() {
    let dir = tempfile::tempdir().unwrap();
    write_record(
        &dir,
        "work_half",
        Some(record_json(
            "work_half",
            "change",
            "g",
            Some(("origin", "cli")),
        )),
    );
    let store = open(&dir);
    assert!(store.tasks().unwrap().is_empty(), "a record with no task");
    // A different kind, goal or origin is a different request.
    for (kind, goal, origin) in [
        ("inspect", "g", AdmissionOrigin::Cli),
        ("change", "other", AdmissionOrigin::Cli),
        ("change", "g", AdmissionOrigin::Service),
    ] {
        assert!(admit(&store, "work_half", kind, goal, "g", origin, &config()).is_err());
        assert!(
            store.tasks().unwrap().is_empty(),
            "the refusal created no task"
        );
    }
    // The same request finishes what the crash interrupted.
    admit(
        &store,
        "work_half",
        "change",
        "g",
        "g",
        AdmissionOrigin::Cli,
        &config(),
    )
    .unwrap();
    assert_eq!(task_of(&store, "work_half").status, "ready");
}

#[test]
fn an_existing_task_is_never_admitted_again() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    admit(
        &store,
        "work_x1",
        "change",
        "g",
        "g",
        AdmissionOrigin::Service,
        &config(),
    )
    .unwrap();
    // Even the identical request: the task exists, so it is not admitted a second time.
    assert!(
        admit(
            &store,
            "work_x1",
            "change",
            "g",
            "g",
            AdmissionOrigin::Service,
            &config()
        )
        .is_err()
    );
    assert_eq!(store.tasks().unwrap().len(), 1);
}

#[test]
fn ensure_admitted_admits_a_new_task_and_leaves_an_existing_one_alone() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    let new = ensure_admitted(
        &store,
        "work_new",
        "change",
        "g",
        "g",
        AdmissionOrigin::Cli,
        &config(),
    );
    assert_eq!(new.unwrap(), Ensured::Admitted);
    assert_eq!(task_of(&store, "work_new").status, "ready");

    // A continuation: the task exists, nothing is written, and `begin` resumes it.
    after_admission_lease();
    let version = task_of(&store, "work_new").checkpoint_version;
    let again = ensure_admitted(
        &store,
        "work_new",
        "change",
        "g",
        "g",
        AdmissionOrigin::Cli,
        &config(),
    );
    assert_eq!(again.unwrap(), Ensured::Existing);
    assert_eq!(task_of(&store, "work_new").checkpoint_version, version);
    assert_eq!(store.tasks().unwrap().len(), 1, "no second identity");

    // A task made below this layer (a direct `begin`) has no admission, and gets none made up.
    let Begun::Ready(_w) = begin(&store, "work_low", "g", &config()).unwrap() else {
        panic!()
    };
    let low = ensure_admitted(
        &store,
        "work_low",
        "change",
        "g",
        "g",
        AdmissionOrigin::Cli,
        &config(),
    );
    assert_eq!(low.unwrap(), Ensured::Existing);
    assert!(matches!(admission(&store, "work_low"), Admission::Missing));
}

#[test]
fn a_continuation_cannot_widen_the_kind_it_was_admitted_as() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    admit(
        &store,
        "work_k",
        "inspect",
        "g",
        "g",
        AdmissionOrigin::Service,
        &config(),
    )
    .unwrap();
    after_admission_lease();
    for wider in ["change", "verify"] {
        let err = ensure_admitted(
            &store,
            "work_k",
            wider,
            "g",
            "g",
            AdmissionOrigin::Cli,
            &config(),
        )
        .unwrap_err();
        assert!(
            matches!(&err, StateError::InvalidInput(m) if m.contains("cannot be continued")),
            "{err}"
        );
    }
    assert_eq!(
        task_of(&store, "work_k").status,
        "ready",
        "refusing changed nothing"
    );
    // As what it was admitted as, it continues.
    assert_eq!(
        ensure_admitted(
            &store,
            "work_k",
            "inspect",
            "g",
            "g",
            AdmissionOrigin::Cli,
            &config()
        )
        .unwrap(),
        Ensured::Existing
    );
}

#[test]
fn a_lease_someone_holds_makes_a_ready_task_not_a_candidate_to_start() {
    use chip_agent_state::TaskValue;
    use chip_agent_state::feltdb::agent_transition_contract::Lease;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let task = |lease: Option<u64>| TaskValue {
        checkpoint_version: 1,
        created_at_ms: now,
        fence: 1,
        lease: lease.map(|expires_at| Lease {
            expires_at,
            lease_id: "l".into(),
        }),
        objective: "g".into(),
        owner: "someone".into(),
        status: "ready".into(),
        task_id: "work_l".into(),
        unresolved_intents: 0,
        updated_at_ms: now,
    };
    assert!(
        lease_is_live(&task(Some(now + 60_000))),
        "held for another minute"
    );
    assert!(!lease_is_live(&task(Some(now - 1))), "run out");
    assert!(!lease_is_live(&task(None)), "released");
}
