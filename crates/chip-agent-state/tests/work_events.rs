//! `chip.WorkEvent`: every event of a work, durable as it happens, with where its fact came from.
//! The database is a real file reopened across "restarts"; the events are the loop's own types.

use chip_agent_state::feltdb::{AtomicMutation, FeltDb};
use chip_agent_state::{
    AgentStore, Begun, DurableConfig, Provenance, StateError, WORK_EVENT_COLLECTION, begin,
    intent_key,
};
use chip_core::{CapabilityId, ExecutionEvent, ExecutionId, WorkEvent, WorkId, WorkLimits};

fn store(dir: &tempfile::TempDir) -> AgentStore {
    AgentStore::new(FeltDb::open(dir.path().join("state.log")).unwrap())
}

fn config() -> DurableConfig {
    DurableConfig {
        lease_ms: 60_000,
        ..Default::default()
    }
}

fn trajectory(work: &WorkId) -> Vec<WorkEvent> {
    let id = ExecutionId::new(format!("{work}-exec-1"));
    vec![
        WorkEvent::WorkStarted {
            work_id: work.clone(),
            goal: "add two; token=never-recorded".into(),
            limits: WorkLimits {
                max_turns: 3,
                max_executions: 2,
            },
        },
        WorkEvent::DecisionStarted {
            work_id: work.clone(),
            turn: 0,
        },
        WorkEvent::DecisionMade {
            work_id: work.clone(),
            turn: 0,
            decision: "request project.read".into(),
        },
        WorkEvent::CapabilityRequested {
            work_id: work.clone(),
            turn: 0,
            capability: CapabilityId::new("project.read").unwrap(),
        },
        WorkEvent::Execution(ExecutionEvent::ExecutionCompleted {
            id,
            output: "file content that stays in the observation record".into(),
        }),
        WorkEvent::GoalEvaluated {
            work_id: work.clone(),
            turn: 0,
            satisfied: true,
            remaining: 0,
        },
        WorkEvent::WorkCompleted {
            work_id: work.clone(),
        },
    ]
}

#[test]
fn every_event_is_durable_in_order_with_its_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let work_id = WorkId::new("work_0123456789abcdef");
    let attempt;
    {
        let Begun::Ready(work) =
            begin(&store(&dir), work_id.as_str(), "add two", &config()).unwrap()
        else {
            panic!("a new task is ready");
        };
        for event in trajectory(&work_id) {
            work.record_event(work_id.as_str(), &event);
        }
        attempt = work.attempt();
        assert_eq!(work.events_recorded(), 7);
        assert!(work.failures().is_empty());
        // The process "dies" here: nothing is finished, every handle is dropped.
    }
    let history = store(&dir).history(work_id.as_str()).unwrap().unwrap();
    let kinds: Vec<&str> = history
        .events
        .iter()
        .map(|e| e.event["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "WorkStarted",
            "DecisionStarted",
            "DecisionMade",
            "CapabilityRequested",
            "ExecutionCompleted",
            "GoalEvaluated",
            "WorkCompleted"
        ]
    );
    let provenance: Vec<Provenance> = history.events.iter().map(|e| e.provenance).collect();
    use Provenance::{HostObserved as H, ModelInferred as M, RuntimeDecided as R};
    assert_eq!(provenance, [R, R, M, M, H, H, R]);
    assert!(history.events.iter().all(|e| e.work_id == work_id.as_str()));
    assert_eq!(
        history.events.iter().map(|e| e.index).collect::<Vec<_>>(),
        (0..7).collect::<Vec<_>>()
    );
    // Neither the goal's text nor an execution's output is copied into an event: the goal is a
    // hash, and the output is named by the observation record that holds it.
    let all = serde_json::to_string(&history.events).unwrap();
    assert!(!all.contains("never-recorded") && !all.contains("stays in the observation"));
    let completed = &history.events[4].event;
    assert_eq!(
        completed["observation_record"],
        format!(
            "{}~{}",
            work_id.as_str(),
            intent_key(attempt, &format!("{work_id}-exec-1"))
        )
    );
    assert_eq!(
        completed["output_bytes"],
        "file content that stays in the observation record".len()
    );
}

#[test]
fn a_gap_in_a_recorded_trajectory_is_invalid_history() {
    let dir = tempfile::tempdir().unwrap();
    let work_id = WorkId::new("work_gap");
    let attempt;
    {
        let Begun::Ready(work) = begin(&store(&dir), work_id.as_str(), "g", &config()).unwrap()
        else {
            panic!()
        };
        attempt = work.attempt();
        for event in trajectory(&work_id) {
            work.record_event(work_id.as_str(), &event);
        }
    }
    // Remove a middle record, as a damaged store would lose it.
    let db = FeltDb::open(dir.path().join("state.log")).unwrap();
    db.apply_atomic_transaction_guarded(
        "remove-one",
        None,
        None,
        &[],
        &[],
        &[AtomicMutation {
            capability: WORK_EVENT_COLLECTION.into(),
            key: format!("{WORK_EVENT_COLLECTION}:work_gap~a{attempt}~e000003"),
            value: None,
        }],
        None,
    )
    .unwrap();
    let err = AgentStore::new(db).history("work_gap").unwrap_err();
    assert!(
        matches!(&err, StateError::InvalidHistory { detail, .. } if detail.contains("position 4 where 3")),
        "{err}"
    );
}

#[test]
fn every_task_in_the_database_can_be_listed() {
    let dir = tempfile::tempdir().unwrap();
    let s = store(&dir);
    for id in ["work_a", "work_b"] {
        assert!(matches!(
            begin(&s, id, "x", &config()).unwrap(),
            Begun::Ready(_)
        ));
    }
    let mut ids: Vec<String> = s.tasks().unwrap().into_iter().map(|t| t.task_id).collect();
    ids.sort();
    assert_eq!(ids, ["work_a", "work_b"]);
    assert!(s.tasks().unwrap().iter().all(|t| t.status == "running"));
}
