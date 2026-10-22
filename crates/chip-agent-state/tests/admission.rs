//! Durable admission: a work is a FeltDB task from the moment it is accepted.
//!
//! The database is a real file reopened across "restarts". A `ready` task is FeltDB's own status
//! for a task that exists and has not started; nothing here adds a state or a queue.

use std::sync::{Arc, Barrier};
use std::time::Duration;

use chip_agent_state::feltdb::{AtomicMutation, FeltDb};
use chip_agent_state::{
    ADMISSION_COLLECTION, Admission, AgentStore, Begun, DurableConfig, StateError, admission,
    admit, begin, cancel_admitted, waiting,
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
    admit(&store, "work_long", "inspect", &goal, "x…", &config()).unwrap();
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
    admit(&store, "work_s", "change", "g", "g", &config()).unwrap();
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
    admit(&store, "work_c", "change", "g", "g", &config()).unwrap();
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
    admit(&store, "work_d", "change", "first", "first", &config()).unwrap();
    let err = admit(&store, "work_d", "inspect", "second", "second", &config()).unwrap_err();
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
        let err = admit(&store, id, kind, goal, "g", &config()).unwrap_err();
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
        admit(&store, "work_x", "change", "g", "g", &config()).unwrap();
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
    admit(&store, "work_r", "change", "g", "g", &config()).unwrap();
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
        admit(&store, id, "inspect", "g", "g", &config()).unwrap();
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
            admit(&store, id, "inspect", "g", "g", &config()).unwrap();
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
