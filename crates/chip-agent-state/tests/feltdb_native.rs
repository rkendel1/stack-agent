//! Chip work through FeltDB's real native agent state: a file-backed `FeltDb`, the production
//! `feltdb::agent_state::AgentState`, and Chip's unchanged work loop. Only the model's decisions
//! are scripted. Restarts close every handle and reopen the database file; leases are driven
//! through time with an injected decision clock, which FeltDB supports for exactly this.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use chip_agent_state::feltdb::agent_state::AuthorityClock;
use chip_agent_state::feltdb::agent_transition_contract::{
    AGENT_TRANSITION_CONTRACT_ID, AgentTransitionInput, ErrorCode, Operation, Records, evaluate,
};
use chip_agent_state::feltdb::{AtomicMutation, FeltDb, RecordPrecondition};
use chip_agent_state::{
    AgentStore, Begun, DurableConfig, DurableWork, OBSERVATION_COLLECTION, OBSERVATION_SCHEMA,
    OBSERVATION_SCHEMA_V1, RESULT_COLLECTION, Restoration, StateError, begin, check_contract,
    intent_key,
};
use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityProvider, CapabilityRequest, DecisionError, ExecutionError, ExecutionId,
    ExecutionObserver, ExecutionRequest, ExecutionResult, Executor, NoLocalPolicy, WorkDecision,
    WorkDecisionBoundary, WorkEvent, WorkGoal, WorkId, WorkOutcome, WorkReport, WorkSpec,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use serde_json::{Value, json};
use tempfile::TempDir;

const LEASE_MS: u64 = 60_000;
const GOAL: &str = "perform the operation and report whether it succeeded";

// ---------------------------------------------------------------------------------------------
// Fixtures: a scripted model, one capability, a counting executor
// ---------------------------------------------------------------------------------------------

struct Model;

#[async_trait::async_trait]
impl ModelProvider for Model {
    async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
        Ok(ModelResponse::new("r", "reply", Usage::new(1, 1)))
    }
}

/// Plays back one decision per model call.
struct Script {
    decisions: Vec<WorkDecision>,
    next: AtomicUsize,
}

impl Script {
    fn new(decisions: Vec<WorkDecision>) -> Self {
        Self {
            decisions,
            next: AtomicUsize::new(0),
        }
    }
}

impl WorkDecisionBoundary for Script {
    fn interpret(
        &self,
        _r: &ModelResponse,
        _c: &[Capability],
    ) -> Result<WorkDecision, DecisionError> {
        let i = self.next.fetch_add(1, Ordering::SeqCst);
        self.decisions
            .get(i)
            .cloned()
            .ok_or_else(|| DecisionError::InvalidDecision("the script ran out".into()))
    }
}

fn perform() -> WorkDecision {
    WorkDecision::RequestCapability(CapabilityRequest::new(
        ExecutionId::new("x"),
        CapabilityId::new("op.perform").unwrap(),
    ))
}

fn complete() -> WorkDecision {
    WorkDecision::Complete {
        summary: "done".into(),
    }
}

fn block() -> WorkDecision {
    WorkDecision::Block {
        reason: "a dependency is unavailable".into(),
    }
}

struct Caps;

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(vec![CapabilityDescriptor::new(
            CapabilityId::new("op.perform")?,
            "Perform",
            "Performs the operation",
        )])
    }
    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

/// Counts real executions. `during` runs inside the execution, after the intent was recorded.
#[derive(Default)]
struct Exec {
    calls: AtomicUsize,
    during: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
}

#[async_trait::async_trait]
impl Executor for Exec {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(during) = self.during.lock().unwrap().as_ref() {
            during();
        }
        Ok(ExecutionResult::success(r.id, "operation performed"))
    }
}

impl Exec {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// Never returns: the process "dies" while the action is in flight.
struct Hangs;

#[async_trait::async_trait]
impl Executor for Hangs {
    async fn execute(&self, _r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        std::future::pending().await
    }
}

/// The authority's real time plus an offset a test can advance. Never behind the real clock, so
/// the commit-time lease predicate (which uses real time) agrees with every decision made here.
#[derive(Default)]
struct Clock(AtomicU64);

impl Clock {
    fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl AuthorityClock for Clock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + self.0.load(Ordering::SeqCst)
    }
}

fn path(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join("agent-state.log")
}

fn store(dir: &TempDir, clock: &Arc<Clock>) -> AgentStore {
    let db = FeltDb::open(path(dir)).expect("open FeltDB");
    AgentStore::with_clock(db, clock.clone())
}

fn config(actor: &str) -> DurableConfig {
    DurableConfig {
        actor: actor.into(),
        lease_ms: LEASE_MS,
        ..Default::default()
    }
}

fn ready(begun: Result<Begun, StateError>) -> DurableWork {
    match begun {
        Ok(Begun::Ready(work)) => work,
        Ok(Begun::Finished(h)) => panic!("expected a runnable task, found {}", h.task.status),
        Err(e) => panic!("expected a runnable task: {e}"),
    }
}

async fn run(
    work: &DurableWork,
    exec: Arc<dyn Executor>,
    decisions: Vec<WorkDecision>,
) -> WorkReport {
    Agent::new(Arc::new(Model))
        .with_capabilities(Arc::new(Caps))
        .with_executor(work.executor(exec))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &WorkSpec::new(WorkId::new(work.task_id()), WorkGoal::new(GOAL)),
            &NoLocalPolicy,
            &Script::new(decisions),
        )
        .await
}

static NEXT_TX: AtomicU64 = AtomicU64::new(0);

fn write(db: &FeltDb, collection: &str, id: &str, value: Option<Value>, absent: bool) {
    let key = format!("{collection}:{id}");
    let current = db.get_value(&key).unwrap();
    let precondition = RecordPrecondition {
        capability: collection.into(),
        key: key.clone(),
        require_absent: absent,
        expected_version: (!absent).then(|| {
            current
                .as_ref()
                .and_then(|v| v.get("__version"))
                .and_then(Value::as_u64)
                .unwrap_or(1)
        }),
        ..Default::default()
    };
    db.apply_atomic_transaction_guarded(
        // FeltDB treats a repeated transaction id as a replay, so every write gets its own.
        &format!("test-{}", NEXT_TX.fetch_add(1, Ordering::SeqCst)),
        None,
        None,
        &[],
        &[precondition],
        &[AtomicMutation {
            capability: collection.into(),
            key,
            value,
        }],
        None,
    )
    .expect("test write");
}

// ---------------------------------------------------------------------------------------------
// Creating, persisting and reading back
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn work_creates_a_task_records_each_execution_and_persists_its_completion() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let work = ready(begin(&store, "task-1", GOAL, &config("chip-a")));
    assert_eq!(
        (work.fence(), work.attempt(), work.resumed_from()),
        (1, 2, None)
    );
    let exec = Arc::new(Exec::default());

    let report = run(&work, exec.clone(), vec![perform(), complete()]).await;

    // Execution behaviour is unchanged by the durable executor.
    assert_eq!(
        report.outcome,
        WorkOutcome::Completed {
            summary: "done".into()
        }
    );
    assert_eq!(exec.calls(), 1);
    assert_eq!(
        report.observations[0].output.as_deref(),
        Some("operation performed")
    );

    let finished = work.finish(&report, true).unwrap();
    assert_eq!(finished.status, "completed");
    assert!(!finished.completion_refused);
    let h = finished.history;
    assert_eq!(h.task.task_id, "task-1");
    assert_eq!(h.task.owner, "chip-a");
    assert!(h.task.lease.is_none(), "a terminal task holds no lease");
    let statuses: Vec<&str> = h.checkpoints.iter().map(|c| c.status.as_str()).collect();
    assert_eq!(statuses, ["ready", "running", "completed"]);
    // The completion is backed by evidence FeltDB observed: the result and the observation.
    let cited: Vec<(Option<&str>, bool)> = h.checkpoints[2]
        .evidence
        .iter()
        .map(|c| (c.collection.as_deref(), c.verified))
        .collect();
    assert_eq!(
        cited,
        [
            (Some(RESULT_COLLECTION), true),
            (Some(OBSERVATION_COLLECTION), true)
        ]
    );
    // One intent, resolved by a verified outcome.
    assert_eq!(h.intents.len(), 1);
    let intent = &h.intents[0].value;
    assert_eq!(intent.key, intent_key(2, "task-1-exec-1"));
    assert_eq!(
        (
            intent.state.as_str(),
            intent.result.as_deref(),
            intent.verified
        ),
        ("resolved", Some("succeeded"), true)
    );
    // Turn and execution identity are kept with the result.
    assert_eq!(h.results.len(), 1);
    let result = &h.results[0];
    assert_eq!((result.work_id.as_str(), result.attempt), ("task-1", 2));
    assert_eq!(result.outcome, "completed");
    assert_eq!(result.executions.len(), 1);
    assert_eq!(result.executions[0].execution_id, "task-1-exec-1");
    assert_eq!(result.executions[0].turn, 0);
    assert_eq!(
        result
            .decisions
            .iter()
            .map(|d| (d.turn, d.source.as_str(), d.decision.as_str()))
            .collect::<Vec<_>>(),
        [(0, "model", "request op.perform"), (1, "model", "complete")]
    );
    assert!(result.persistence_failures.is_empty());
}

#[tokio::test]
async fn a_restart_recovers_the_finished_task_from_the_file_and_does_not_run_it_again() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    {
        let store = store(&dir, &clock);
        let work = ready(begin(&store, "task-r", GOAL, &config("chip-a")));
        let report = run(
            &work,
            Arc::new(Exec::default()),
            vec![perform(), complete()],
        )
        .await;
        work.finish(&report, true).unwrap();
    } // every handle closed: the database file is all that remains

    let store = store(&dir, &clock);
    match begin(&store, "task-r", GOAL, &config("chip-b")).unwrap() {
        Begun::Finished(h) => {
            assert_eq!(h.task.status, "completed");
            assert_eq!(h.checkpoints.len(), 3);
            assert_eq!(h.results.len(), 1);
            assert_eq!(h.intents.len(), 1);
        }
        Begun::Ready(_) => panic!("a completed task must not run again"),
    }
}

#[tokio::test]
async fn blocked_work_resumes_after_a_restart_with_task_identity_and_versions_continuous() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let first = {
        let store = store(&dir, &clock);
        let work = ready(begin(&store, "task-b", GOAL, &config("chip-a")));
        let report = run(&work, Arc::new(Exec::default()), vec![perform(), block()]).await;
        assert!(matches!(report.outcome, WorkOutcome::Blocked { .. }));
        let finished = work.finish(&report, true).unwrap();
        assert_eq!(finished.status, "blocked");
        finished.history
    };
    assert!(
        first.task.lease.is_some(),
        "blocked is not terminal; the lease remains until it expires"
    );

    // A new process: until the old lease expires, resuming is refused, not forced.
    let store = store(&dir, &clock);
    assert_eq!(
        begin(&store, "task-b", GOAL, &config("chip-b")).err(),
        Some(StateError::LeaseHeld {
            task_id: "task-b".into(),
            owner: "chip-a".into()
        })
    );
    clock.advance(LEASE_MS + 1);
    let work = ready(begin(&store, "task-b", GOAL, &config("chip-b")));
    assert_eq!(work.resumed_from(), Some("blocked"));
    assert_eq!(work.fence(), 2, "taking the task over advances the fence");
    assert_eq!(
        work.attempt(),
        4,
        "the attempt is its running checkpoint's version"
    );
    let exec = Arc::new(Exec::default());
    let report = run(&work, exec.clone(), vec![perform(), complete()]).await;
    let h = work.finish(&report, true).unwrap().history;

    assert_eq!(h.task.status, "completed");
    assert_eq!(h.task.owner, "chip-b");
    let versions: Vec<(u64, &str, u64)> = h
        .checkpoints
        .iter()
        .map(|c| (c.version, c.status.as_str(), c.fence))
        .collect();
    assert_eq!(
        versions,
        [
            (1, "ready", 1),
            (2, "running", 1),
            (3, "blocked", 1),
            (4, "running", 2),
            (5, "completed", 2),
        ]
    );
    // Both attempts' history is kept; Chip's execution numbering restarted, the intents did not collide.
    let keys: Vec<&str> = h.intents.iter().map(|i| i.value.key.as_str()).collect();
    assert_eq!(keys, ["a2.task-b-exec-1", "a4.task-b-exec-1"]);
    assert!(h.intents.iter().all(|i| i.value.state == "resolved"));
    let attempts: Vec<(u64, &str)> = h
        .results
        .iter()
        .map(|r| (r.attempt, r.outcome.as_str()))
        .collect();
    assert_eq!(attempts, [(2, "blocked"), (4, "completed")]);
}

#[tokio::test]
async fn cancelled_work_is_terminal() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let work = ready(begin(&store, "task-c", GOAL, &config("chip-a")));
    let report = WorkReport {
        outcome: WorkOutcome::Cancelled {
            cause: chip_core::StopCause::Cancelled,
            reason: "an operator stopped it".into(),
            delivery: None,
            remote: None,
        },
        ..run(
            &work,
            Arc::new(Exec::default()),
            vec![perform(), complete()],
        )
        .await
    };
    assert_eq!(work.finish(&report, true).unwrap().status, "cancelled");
    assert!(matches!(
        begin(&store, "task-c", GOAL, &config("chip-a")),
        Ok(Begun::Finished(_))
    ));
}

// ---------------------------------------------------------------------------------------------
// Guarded transitions, conflicts and races
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_task_held_by_one_owner_is_refused_to_every_other() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let _held = ready(begin(&store, "task-h", GOAL, &config("chip-a")));
    for actor in ["chip-b", "chip-a"] {
        // The same actor in another process has another lease and is refused too.
        assert_eq!(
            begin(&store, "task-h", GOAL, &config(actor)).err(),
            Some(StateError::LeaseHeld {
                task_id: "task-h".into(),
                owner: "chip-a".into()
            })
        );
    }
    assert_eq!(
        begin(&store, "task-h", "another objective", &config("chip-b")).err(),
        Some(StateError::ObjectiveMismatch {
            task_id: "task-h".into()
        })
    );
}

#[test]
fn racing_owners_of_one_new_task_produce_exactly_one_runnable_attempt() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    for round in 0..8 {
        let task = format!("race-{round}");
        let barrier = Arc::new(Barrier::new(4));
        let results: Vec<Result<Begun, StateError>> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..4)
                .map(|n| {
                    let (store, barrier, task) = (store.clone(), barrier.clone(), task.clone());
                    s.spawn(move || {
                        barrier.wait();
                        begin(&store, &task, GOAL, &config(&format!("chip-{n}")))
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let winners = results
            .iter()
            .filter(|r| matches!(r, Ok(Begun::Ready(_))))
            .count();
        assert_eq!(winners, 1, "round {round}: exactly one owner may run");
        for r in &results {
            assert!(
                matches!(r, Ok(Begun::Ready(_)) | Err(StateError::LeaseHeld { .. })),
                "round {round}: a loser is refused as LeaseHeld"
            );
        }
        let h = store.history(&task).unwrap().unwrap();
        assert_eq!(
            h.checkpoints
                .iter()
                .map(|c| c.status.as_str())
                .collect::<Vec<_>>(),
            ["ready", "running"],
            "round {round}: the losers wrote nothing"
        );
    }
}

#[tokio::test]
async fn a_takeover_while_an_action_runs_fences_out_the_old_owner_and_its_success_is_not_reported()
{
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let work = ready(begin(&store, "task-t", GOAL, &config("chip-a")));
    let exec = Arc::new(Exec::default());
    let thief: Arc<Mutex<Option<Result<Begun, StateError>>>> = Arc::default();
    {
        let (store, clock, thief) = (store.clone(), clock.clone(), thief.clone());
        *exec.during.lock().unwrap() = Some(Box::new(move || {
            // The lease runs out mid-action and another owner takes the task.
            clock.advance(LEASE_MS + 1);
            *thief.lock().unwrap() = Some(begin(&store, "task-t", GOAL, &config("chip-b")));
        }));
    }
    let report = run(&work, exec.clone(), vec![perform(), complete()]).await;

    assert_eq!(exec.calls(), 1, "the action ran");
    // The new owner advanced the fence, but the intent's outcome was not yet known to it.
    assert!(matches!(
        thief.lock().unwrap().take(),
        Some(Err(StateError::OutcomeUncertain { .. }))
    ));
    // The old owner could not record the outcome, so the work is a failure, not a success.
    match &report.outcome {
        WorkOutcome::Failed { reason, .. } => {
            assert!(reason.contains("could not be made durable"), "{reason}")
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    assert!(matches!(
        work.failures().as_slice(),
        [StateError::Refused {
            operation: "record_outcome",
            code: ErrorCode::FenceStale
        }]
    ));
    assert!(matches!(
        work.finish(&report, true),
        Err(StateError::Refused {
            operation: "checkpoint",
            code: ErrorCode::FenceStale
        })
    ));
    let h = store.history("task-t").unwrap().unwrap();
    assert_eq!((h.task.status.as_str(), h.task.fence), ("running", 2));
    assert_eq!(h.unresolved_intents().len(), 1);

    // The observation the old owner wrote is evidence: the next owner reconciles from it.
    clock.advance(LEASE_MS + 1);
    let resumed = ready(begin(&store, "task-t", GOAL, &config("chip-c")));
    assert_eq!(resumed.fence(), 3);
    let h = store.history("task-t").unwrap().unwrap();
    let intent = &h.intents[0].value;
    assert_eq!(
        (
            intent.state.as_str(),
            intent.conclusion.as_deref(),
            intent.verified
        ),
        ("resolved", Some("executed"), true)
    );
}

// ---------------------------------------------------------------------------------------------
// Crashes, uncertain outcomes and persistence failures
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_crash_after_the_intent_blocks_resumption_until_evidence_settles_it() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let key = {
        let store = store(&dir, &clock);
        let work = ready(begin(&store, "task-x", GOAL, &config("chip-a")));
        // The process dies while the action is in flight.
        let crashed = tokio::time::timeout(
            Duration::from_millis(200),
            run(&work, Arc::new(Hangs), vec![perform(), complete()]),
        )
        .await;
        assert!(crashed.is_err());
        intent_key(work.attempt(), "task-x-exec-1")
    };

    let store = store(&dir, &clock);
    let h = store.history("task-x").unwrap().unwrap();
    assert_eq!(h.task.status, "running");
    assert_eq!(h.task.unresolved_intents, 1);
    clock.advance(LEASE_MS + 1);
    // No observation: the action may or may not have run. Resuming could repeat it.
    assert_eq!(
        begin(&store, "task-x", GOAL, &config("chip-b")).err(),
        Some(StateError::OutcomeUncertain {
            task_id: "task-x".into(),
            intents: vec![key.clone()]
        })
    );

    // Evidence arrives: an observation of that execution, written the way Chip writes one.
    write(
        store.database(),
        OBSERVATION_COLLECTION,
        &format!("task-x~{key}"),
        Some(json!({
            "schema": OBSERVATION_SCHEMA, "task_id": "task-x", "intent_key": key,
            "execution_id": "task-x-exec-1", "attempt": 2, "status": "success",
            "executed": true, "output": "operation performed", "receipt_id": null,
        })),
        true,
    );
    // The previous attempt's lease (chip-b's) must expire before chip-c takes over.
    clock.advance(LEASE_MS + 1);
    let work = ready(begin(&store, "task-x", GOAL, &config("chip-c")));
    let report = run(
        &work,
        Arc::new(Exec::default()),
        vec![perform(), complete()],
    )
    .await;
    let h = work.finish(&report, true).unwrap().history;
    assert_eq!(h.task.status, "completed");
    assert_eq!(h.intents.len(), 2);
    assert_eq!(h.intents[0].value.conclusion.as_deref(), Some("executed"));
}

#[tokio::test]
async fn an_expired_lease_stops_execution_before_it_starts_and_nothing_is_reported_done() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let work = ready(begin(&store, "task-e", GOAL, &config("chip-a")));
    clock.advance(LEASE_MS + 1);
    let exec = Arc::new(Exec::default());

    let report = run(&work, exec.clone(), vec![perform(), complete()]).await;

    assert_eq!(exec.calls(), 0, "no intent, no action");
    assert!(matches!(report.outcome, WorkOutcome::Failed { .. }));
    assert!(matches!(
        work.failures().as_slice(),
        [StateError::Refused {
            operation: "renew",
            code: ErrorCode::LeaseExpired
        }]
    ));
    assert!(work.finish(&report, true).is_err());
    let h = store.history("task-e").unwrap().unwrap();
    assert_eq!(h.task.status, "running");
    assert!(h.intents.is_empty());
}

#[tokio::test]
async fn a_completion_no_observation_backs_is_not_persisted_as_complete() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let work = ready(begin(&store, "task-u", GOAL, &config("chip-a")));
    let report = run(&work, Arc::new(Exec::default()), vec![complete()]).await;
    assert!(matches!(report.outcome, WorkOutcome::Completed { .. }));

    let finished = work.finish(&report, true).unwrap();
    assert!(finished.completion_refused);
    assert_eq!(finished.status, "blocked");
}

#[tokio::test]
async fn a_completion_the_host_did_not_verify_is_not_persisted_as_complete() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let work = ready(begin(&store, "task-v", GOAL, &config("chip-a")));
    let report = run(
        &work,
        Arc::new(Exec::default()),
        vec![perform(), complete()],
    )
    .await;
    assert!(matches!(report.outcome, WorkOutcome::Completed { .. }));

    let finished = work.finish(&report, false).unwrap();
    assert!(finished.completion_refused);
    assert_eq!(finished.status, "blocked");
    assert!(
        finished.history.checkpoints[2]
            .summary
            .contains("did not verify")
    );
    // Blocked is resumable, not terminal.
    assert!(finished.history.task.lease.is_some());
}

#[test]
fn a_database_that_cannot_be_opened_is_a_storage_error() {
    let dir = TempDir::new().unwrap();
    // A directory where the log file should be.
    std::fs::create_dir(path(&dir)).unwrap();
    assert!(matches!(
        AgentStore::open(path(&dir)),
        Err(StateError::Storage {
            operation: "open",
            ..
        })
    ));
}

// ---------------------------------------------------------------------------------------------
// Invalid, incomplete and unsupported state
// ---------------------------------------------------------------------------------------------

#[test]
fn only_the_supported_contract_is_accepted_and_feltdb_itself_refuses_other_versions() {
    assert!(check_contract(AGENT_TRANSITION_CONTRACT_ID, 1).is_ok());
    assert_eq!(
        check_contract(AGENT_TRANSITION_CONTRACT_ID, 2),
        Err(StateError::UnsupportedContract {
            contract: AGENT_TRANSITION_CONTRACT_ID.into(),
            version: 2
        })
    );
    let input = AgentTransitionInput {
        contract: AGENT_TRANSITION_CONTRACT_ID.into(),
        now_ms: 1,
        operation: serde_json::from_value::<Operation>(json!({
            "op": "create_task", "task_id": "t", "actor": "a", "objective": "o",
            "lease_id": "l", "lease_ms": 1000
        }))
        .unwrap(),
        records: Records::default(),
        version: 2,
    };
    assert_eq!(evaluate(&input), Err(ErrorCode::ContractVersionUnsupported));
}

#[test]
fn identities_the_contract_cannot_carry_are_refused_before_anything_is_written() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    for (task, objective) in [("has space", GOAL), ("a~b", GOAL), ("ok", "")] {
        assert!(matches!(
            begin(&store, task, objective, &config("chip-a")),
            Err(StateError::InvalidInput(_))
        ));
    }
    assert!(store.history("ok").unwrap().is_none());
}

#[tokio::test]
async fn incomplete_history_and_unknown_record_schemas_are_refused_not_read_around() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let work = ready(begin(&store, "task-i", GOAL, &config("chip-a")));
    let report = run(
        &work,
        Arc::new(Exec::default()),
        vec![perform(), complete()],
    )
    .await;
    work.finish(&report, true).unwrap();
    let db = store.database();

    // A result written by a future build.
    write(
        db,
        RESULT_COLLECTION,
        "task-i~a99",
        Some(json!({"schema": "chip.work_result.v9"})),
        true,
    );
    assert_eq!(
        store.history("task-i").err(),
        Some(StateError::UnsupportedRecord {
            collection: RESULT_COLLECTION.into(),
            id: "task-i~a99".into(),
            schema: "chip.work_result.v9".into()
        })
    );
    write(db, RESULT_COLLECTION, "task-i~a99", None, false);
    assert_eq!(store.history("task-i").err(), None);

    // A checkpoint missing from the middle of the history.
    write(db, "_feltdb.AgentCheckpoint", "task-i~v2", None, false);
    assert!(matches!(
        store.history("task-i"),
        Err(StateError::InvalidHistory { .. })
    ));
    // And begin does not resume from it either.
    assert!(matches!(
        begin(&store, "task-i", GOAL, &config("chip-a")),
        Err(StateError::InvalidHistory { .. })
    ));
}

// ---------------------------------------------------------------------------------------------
// Resuming from the recorded trajectory
// ---------------------------------------------------------------------------------------------

/// Names each result by how many times it has really run: `operation 1`, `operation 2`, ...
#[derive(Default)]
struct Numbered(AtomicUsize);

#[async_trait::async_trait]
impl Executor for Numbered {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let n = self.0.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(ExecutionResult::success(r.id, format!("operation {n}")))
    }
}

/// `Caps`, performed again on every request (as Chip's project capabilities are), so two
/// identical requests are two executions.
struct FreshCaps;

#[async_trait::async_trait]
impl CapabilityProvider for FreshCaps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(Caps
            .capabilities()
            .await?
            .into_iter()
            .map(CapabilityDescriptor::without_evidence_reuse)
            .collect())
    }
    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

/// `run`, with the spec seeded as `stack-agent work --state-db` seeds it.
async fn run_resumed(
    work: &DurableWork,
    exec: Arc<dyn Executor>,
    decisions: Vec<WorkDecision>,
) -> WorkReport {
    Agent::new(Arc::new(Model))
        .with_capabilities(Arc::new(FreshCaps))
        .with_executor(work.executor(exec))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &WorkSpec::new(WorkId::new(work.task_id()), WorkGoal::new(GOAL))
                .with_restored(work.restored()),
            &NoLocalPolicy,
            &Script::new(decisions),
        )
        .await
}

fn outputs(report: &WorkReport) -> Vec<String> {
    report
        .observations
        .iter()
        .map(|o| o.output.clone().unwrap_or_default())
        .collect()
}

#[tokio::test]
async fn a_resumed_attempt_starts_from_every_recorded_observation_in_order_and_runs_none_again() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let exec = Arc::new(Numbered::default());
    {
        let store = store(&dir, &clock);
        let work = ready(begin(&store, "task-r", GOAL, &config("chip-a")));
        assert_eq!(
            work.restoration(),
            &Restoration::Restored { observations: 0 }
        );
        let report = run_resumed(&work, exec.clone(), vec![perform(), perform(), block()]).await;
        assert_eq!(work.finish(&report, true).unwrap().status, "blocked");
    }
    clock.advance(LEASE_MS + 1);
    let store = store(&dir, &clock);
    let work = ready(begin(&store, "task-r", GOAL, &config("chip-b")));
    assert_eq!(
        work.restoration(),
        &Restoration::Restored { observations: 2 }
    );
    let restored: Vec<(String, String)> = work
        .restored()
        .iter()
        .map(|r| (r.result.id.0.clone(), r.result.output.clone()))
        .collect();
    assert_eq!(
        restored,
        [
            ("task-r-exec-1".to_string(), "operation 1".to_string()),
            ("task-r-exec-2".to_string(), "operation 2".to_string()),
        ]
    );
    let report = run_resumed(&work, exec.clone(), vec![perform(), complete()]).await;
    assert_eq!(exec.0.load(Ordering::SeqCst), 3, "only the new step ran");
    assert_eq!(
        outputs(&report),
        ["operation 1", "operation 2", "operation 3"]
    );
    assert!(matches!(
        report.events.iter().position(|e| matches!(e, WorkEvent::DecisionStarted { .. })),
        Some(i) if report.events[..i]
            .iter()
            .filter(|e| matches!(e, WorkEvent::ObservationRestored { .. }))
            .count() == 2
    ));
    let h = work.finish(&report, true).unwrap().history;
    assert_eq!(h.task.status, "completed");
    // Numbering continued after the restored executions: no second intent for either of them.
    let keys: Vec<&str> = h.intents.iter().map(|i| i.value.key.as_str()).collect();
    assert_eq!(
        keys,
        ["a2.task-r-exec-1", "a2.task-r-exec-2", "a4.task-r-exec-3"]
    );
}

#[tokio::test]
async fn a_crash_after_the_outcome_was_recorded_resumes_without_executing_it_again() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let exec = Arc::new(Exec::default());
    let store = store(&dir, &clock);
    let first = ready(begin(&store, "task-w3", GOAL, &config("chip-a")));
    // The action ran and its outcome is durable; the process dies before deciding anything else.
    let crashed = tokio::time::timeout(
        Duration::from_millis(300),
        Agent::new(Arc::new(ModelHangsAfter(AtomicUsize::new(1))))
            .with_capabilities(Arc::new(Caps))
            .with_executor(first.executor(exec.clone()))
            .with_observer(Arc::new(ExecutionObserver))
            .run_work(
                &WorkSpec::new(WorkId::new("task-w3"), WorkGoal::new(GOAL)),
                &NoLocalPolicy,
                &Script::new(vec![perform(), complete()]),
            ),
    )
    .await;
    assert!(crashed.is_err());
    assert_eq!(exec.calls(), 1);
    assert_eq!(
        store
            .history("task-w3")
            .unwrap()
            .unwrap()
            .task
            .unresolved_intents,
        0
    );

    clock.advance(LEASE_MS + 1);
    let work = ready(begin(&store, "task-w3", GOAL, &config("chip-b")));
    assert_eq!(
        work.restoration(),
        &Restoration::Restored { observations: 1 }
    );
    // The first owner was fenced out by the takeover: it can start nothing more.
    let stale = first
        .executor(exec.clone())
        .execute(ExecutionRequest::new(
            ExecutionId::new("task-w3-exec-2"),
            "op.perform",
        ))
        .await;
    assert!(stale.is_err());
    assert!(matches!(
        first.failures().last(),
        Some(StateError::Refused {
            code: ErrorCode::FenceStale,
            ..
        })
    ));
    assert_eq!(exec.calls(), 1, "the fenced-out owner executed nothing");
    let report = run_resumed(&work, exec.clone(), vec![complete()]).await;
    assert!(matches!(report.outcome, WorkOutcome::Completed { .. }));
    assert_eq!(exec.calls(), 1, "the recorded action is not executed again");
    // The restored observation backs the completion: it is persisted as complete.
    let finished = work.finish(&report, true).unwrap();
    assert!(!finished.completion_refused);
    assert_eq!(finished.status, "completed");
    let cited: Vec<String> = finished
        .history
        .checkpoints
        .last()
        .unwrap()
        .evidence
        .iter()
        .filter_map(|c| c.id.clone())
        .collect();
    assert!(
        cited.contains(&"task-w3~a2.task-w3-exec-1".to_string()),
        "{cited:?}"
    );

    // Nor can the first owner finish over it afterwards.
    assert!(matches!(
        first.finish(&report, true),
        Err(StateError::Refused {
            code: ErrorCode::Terminal,
            ..
        })
    ));
    assert_eq!(
        store.history("task-w3").unwrap().unwrap().task.status,
        "completed"
    );
}

/// Answers `answers` model calls, then never answers again: the process "dies" waiting for it.
struct ModelHangsAfter(AtomicUsize);

#[async_trait::async_trait]
impl ModelProvider for ModelHangsAfter {
    async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
        if self.0.fetch_sub(1, Ordering::SeqCst) == 0 {
            std::future::pending::<()>().await;
        }
        Ok(ModelResponse::new("r", "reply", Usage::new(1, 1)))
    }
}

#[tokio::test]
async fn an_execution_record_without_its_request_settles_its_intent_but_nothing_is_restored() {
    for (schema, why) in [
        (
            OBSERVATION_SCHEMA_V1,
            "a record from before requests were kept",
        ),
        (OBSERVATION_SCHEMA, "a v2 record that lost its request"),
    ] {
        let dir = TempDir::new().unwrap();
        let clock = Arc::new(Clock::default());
        let store = store(&dir, &clock);
        let (key, attempt) = {
            let work = ready(begin(&store, "task-s", GOAL, &config("chip-a")));
            let crashed = tokio::time::timeout(
                Duration::from_millis(200),
                run_resumed(&work, Arc::new(Hangs), vec![perform(), complete()]),
            )
            .await;
            assert!(crashed.is_err());
            (intent_key(work.attempt(), "task-s-exec-1"), work.attempt())
        };
        // A record of the execution itself (not an operator's), without the request.
        write(
            store.database(),
            OBSERVATION_COLLECTION,
            &format!("task-s~{key}"),
            Some(json!({
                "schema": schema, "task_id": "task-s", "intent_key": key,
                "execution_id": "task-s-exec-1", "attempt": attempt, "status": "success",
                "executed": true, "output": "operation performed", "receipt_id": null,
            })),
            true,
        );
        clock.advance(LEASE_MS + 1);
        let work = ready(begin(&store, "task-s", GOAL, &config("chip-b")));
        match work.restoration() {
            Restoration::NotRestored { reason } => {
                assert!(reason.contains("without its request"), "{why}: {reason}")
            }
            other => panic!("{why}: {other:?}"),
        }
        assert!(work.restored().is_empty(), "{why}");
        assert!(work.settled().is_empty(), "{why}");
        let h = store.history("task-s").unwrap().unwrap();
        assert_eq!(
            h.intents[0].value.conclusion.as_deref(),
            Some("executed"),
            "{why}"
        );
    }
}

#[tokio::test]
async fn an_operator_record_is_read_as_a_settlement_not_an_observation() {
    // The record the durability bench and `sa-state reconcile` wrote before `settle` existed:
    // execution `operator`, attempt 0, no request, no receipt. It is the operator's conclusion;
    // it reconciles its intent, is never restored as an observation, and no longer stops the rest
    // of the trajectory from being restored.
    for schema in [OBSERVATION_SCHEMA, OBSERVATION_SCHEMA_V1] {
        let dir = TempDir::new().unwrap();
        let clock = Arc::new(Clock::default());
        let store = store(&dir, &clock);
        let key = {
            let work = ready(begin(&store, "task-s", GOAL, &config("chip-a")));
            let crashed = tokio::time::timeout(
                Duration::from_millis(200),
                run_resumed(&work, Arc::new(Hangs), vec![perform(), complete()]),
            )
            .await;
            assert!(crashed.is_err());
            intent_key(work.attempt(), "task-s-exec-1")
        };
        write(
            store.database(),
            OBSERVATION_COLLECTION,
            &format!("task-s~{key}"),
            Some(json!({
                "schema": schema, "task_id": "task-s", "intent_key": key,
                "execution_id": "operator", "attempt": 0, "status": "success",
                "executed": true, "output": "verified by the operator", "receipt_id": null,
            })),
            true,
        );
        clock.advance(LEASE_MS + 1);
        let work = ready(begin(&store, "task-s", GOAL, &config("chip-b")));
        assert_eq!(
            work.restoration(),
            &Restoration::Restored { observations: 0 },
            "{schema}"
        );
        let settled = work.settled();
        assert_eq!(settled.len(), 1, "{schema}");
        assert_eq!(settled[0].intent_key, key);
        assert!(settled[0].executed);
        // Written before settlements named who decided: it says so rather than guess.
        assert_eq!(settled[0].decided_by, None);
        assert_eq!(settled[0].evidence, "verified by the operator");
        let h = store.history("task-s").unwrap().unwrap();
        assert_eq!(h.intents[0].value.conclusion.as_deref(), Some("executed"));
    }
}

#[tokio::test]
async fn a_recorded_execution_that_contradicts_its_intent_is_refused_not_restored() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let key = {
        let work = ready(begin(&store, "task-c", GOAL, &config("chip-a")));
        let crashed = tokio::time::timeout(
            Duration::from_millis(200),
            run_resumed(&work, Arc::new(Hangs), vec![perform(), complete()]),
        )
        .await;
        assert!(crashed.is_err());
        intent_key(work.attempt(), "task-c-exec-1")
    };
    // A record with a request, for an execution its intent does not name.
    write(
        store.database(),
        OBSERVATION_COLLECTION,
        &format!("task-c~{key}"),
        Some(json!({
            "schema": OBSERVATION_SCHEMA, "task_id": "task-c", "intent_key": key,
            "execution_id": "task-c-exec-7", "attempt": 2, "status": "success",
            "executed": true, "output": "operation performed", "receipt_id": null,
            "request": {"capability": "op.perform", "inputs": {}},
        })),
        true,
    );
    clock.advance(LEASE_MS + 1);
    assert!(matches!(
        begin(&store, "task-c", GOAL, &config("chip-b")),
        Err(StateError::InvalidHistory { .. })
    ));
}

#[tokio::test]
async fn an_unresolved_intent_still_blocks_resumption_with_a_recorded_trajectory_before_it() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let exec = Arc::new(Exec::default());
    let key = {
        let work = ready(begin(&store, "task-u", GOAL, &config("chip-a")));
        let report = run_resumed(&work, exec.clone(), vec![perform(), block()]).await;
        work.finish(&report, true).unwrap();
        clock.advance(LEASE_MS + 1);
        let work = ready(begin(&store, "task-u", GOAL, &config("chip-b")));
        assert_eq!(
            work.restoration(),
            &Restoration::Restored { observations: 1 }
        );
        // The second action is in flight when the process dies.
        let crashed = tokio::time::timeout(
            Duration::from_millis(200),
            run_resumed(&work, Arc::new(Hangs), vec![perform(), complete()]),
        )
        .await;
        assert!(crashed.is_err());
        intent_key(work.attempt(), "task-u-exec-2")
    };
    clock.advance(LEASE_MS + 1);
    assert_eq!(
        begin(&store, "task-u", GOAL, &config("chip-c")).err(),
        Some(StateError::OutcomeUncertain {
            task_id: "task-u".into(),
            intents: vec![key]
        })
    );
    assert_eq!(exec.calls(), 1);
}
