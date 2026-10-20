//! The stuck guard over durable state: a work the guard stopped is recorded as blocked, and a fresh
//! process that resumes it restores the same trajectory and reaches the same terminal result, with
//! no model call and no execution. FeltDB is the real native agent state.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chip_agent_state::feltdb::FeltDb;
use chip_agent_state::feltdb::agent_state::AuthorityClock;
use chip_agent_state::{AgentStore, Begun, DurableConfig, DurableWork, StateError, begin};
use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, CapabilityRequest, DecisionError, Effect, ExecutionError,
    ExecutionId, ExecutionObserver, ExecutionRequest, ExecutionResult, Executor, InputValue,
    NoLocalPolicy, STUCK_REASON_PREFIX, StuckThresholds, WorkDecision, WorkDecisionBoundary,
    WorkGoal, WorkId, WorkOutcome, WorkReport, WorkSpec,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use tempfile::TempDir;

const LEASE_MS: u64 = 60_000;
const GOAL: &str = "look into the file";

#[derive(Default)]
struct Model(AtomicUsize);

#[async_trait::async_trait]
impl ModelProvider for Model {
    async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ModelResponse::new("r", "reply", Usage::new(1, 1)))
    }
}

struct Script {
    decisions: Vec<WorkDecision>,
    next: AtomicUsize,
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

fn read() -> WorkDecision {
    WorkDecision::RequestCapability(
        CapabilityRequest::new(
            ExecutionId::new("x"),
            CapabilityId::new("world.read").unwrap(),
        )
        .with_input("path", InputValue::Text("src/lib.rs".into())),
    )
}

struct Caps;

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        let mut d = CapabilityDescriptor::new(CapabilityId::new("world.read")?, "Read", "reads")
            .without_evidence_reuse()
            .with_effect(Effect::Observation);
        d.inputs.push(CapabilityInput {
            name: "path".into(),
            description: "path".into(),
            required: true,
        });
        Ok(vec![d])
    }
    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

/// Always reads the same text. `hang_after` makes the call of that number perform its read and
/// never return.
#[derive(Default)]
struct World {
    runs: AtomicUsize,
    hang_on: Mutex<Option<usize>>,
}

impl World {
    fn executions(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Executor for World {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let n = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
        if *self.hang_on.lock().unwrap() == Some(n) {
            std::future::pending::<()>().await;
        }
        Ok(ExecutionResult::success(r.id, "the same text"))
    }
}

#[derive(Default)]
struct Clock(AtomicU64);

impl AuthorityClock for Clock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + self.0.load(Ordering::SeqCst)
    }
}

fn store(dir: &TempDir, clock: &Arc<Clock>) -> AgentStore {
    let db = FeltDb::open(dir.path().join("agent-state.log")).expect("open FeltDB");
    AgentStore::with_clock(db, clock.clone())
}

fn config(actor: &str) -> DurableConfig {
    DurableConfig {
        actor: actor.into(),
        lease_ms: LEASE_MS,
        effects: BTreeMap::from([("world.read".to_string(), Effect::Observation)]),
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
    model: &Arc<Model>,
    world: &Arc<World>,
    script: usize,
) -> WorkReport {
    let mut spec = WorkSpec::new(WorkId::new(work.task_id()), WorkGoal::new(GOAL))
        .with_restored(work.restored())
        .with_stuck_guard(StuckThresholds::default());
    spec.limits.max_turns = 20;
    spec.limits.max_executions = 20;
    Agent::new(model.clone())
        .with_capabilities(Arc::new(Caps))
        .with_executor(work.executor(world.clone()))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &spec,
            &NoLocalPolicy,
            &Script {
                decisions: (0..script).map(|_| read()).collect(),
                next: Default::default(),
            },
        )
        .await
}

fn is_stuck(report: &WorkReport) -> bool {
    matches!(&report.outcome, WorkOutcome::Blocked { reason } if reason.starts_with(STUCK_REASON_PREFIX))
}

#[tokio::test]
async fn a_stuck_work_is_recorded_blocked_and_a_restart_reaches_the_same_result_at_once() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let world = Arc::new(World::default());

    // Attempt 1 loops and is stopped by the guard after three identical pairs.
    let first = ready(begin(
        &store(&dir, &clock),
        "task-s",
        GOAL,
        &config("chip-a"),
    ));
    let model = Arc::new(Model::default());
    let report = run(&first, &model, &world, 12).await;
    assert!(is_stuck(&report), "{:?}", report.outcome);
    assert_eq!((world.executions(), model.0.load(Ordering::SeqCst)), (3, 3));
    let finished = first.finish(&report, false).unwrap();
    assert_eq!(finished.status, "blocked");
    let detail = finished.history.results.last().unwrap().detail.clone();
    assert!(detail.starts_with(STUCK_REASON_PREFIX), "{detail}");
    clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);

    // A fresh process resumes. The recorded trajectory already repeats, so it ends before a model
    // call or an execution, and says the same thing.
    let second = ready(begin(
        &store(&dir, &clock),
        "task-s",
        GOAL,
        &config("chip-b"),
    ));
    assert_eq!(second.restored().len(), 3);
    let model2 = Arc::new(Model::default());
    let report2 = run(&second, &model2, &world, 12).await;
    assert!(is_stuck(&report2), "{:?}", report2.outcome);
    assert_eq!(model2.0.load(Ordering::SeqCst), 0);
    assert_eq!(world.executions(), 3, "nothing ran again");
    assert_eq!(
        report2.observations.len(),
        3,
        "the recorded progress is kept"
    );
    let finished2 = second.finish(&report2, false).unwrap();
    assert_eq!(finished2.status, "blocked");
    assert_eq!(
        finished2.history.results.last().unwrap().detail,
        detail,
        "the same reason after the restart"
    );
    assert_eq!(finished2.history.results.len(), 2);
    assert_eq!(finished2.history.task.unresolved_intents, 0);
}

#[tokio::test]
async fn a_crash_inside_the_third_identical_read_is_re_observed_and_the_guard_then_ends_the_work() {
    // P4 and the guard together. The process dies inside the third identical read. The next process
    // performs it again (it declares an observation), which is the third identical pair, and the
    // guard ends the work before it asks the model anything.
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let world = Arc::new(World::default());
    *world.hang_on.lock().unwrap() = Some(3);
    let first = ready(begin(
        &store(&dir, &clock),
        "task-c",
        GOAL,
        &config("chip-a"),
    ));
    let died = tokio::time::timeout(
        Duration::from_millis(500),
        run(&first, &Arc::new(Model::default()), &world, 12),
    )
    .await;
    assert!(died.is_err(), "the process was meant to die");
    assert_eq!(world.executions(), 3);
    clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);

    let second = ready(begin(
        &store(&dir, &clock),
        "task-c",
        GOAL,
        &config("chip-b"),
    ));
    assert_eq!(second.restored().len(), 2);
    second
        .reobserve(Arc::new(Backend(world.clone())), None)
        .await
        .unwrap();
    assert_eq!(world.executions(), 4, "the lost read, once more");
    assert_eq!(second.restored().len(), 3);
    let model = Arc::new(Model::default());
    let report = run(&second, &model, &world, 12).await;
    assert!(is_stuck(&report), "{:?}", report.outcome);
    assert_eq!(model.0.load(Ordering::SeqCst), 0);
    assert_eq!(world.executions(), 4);
}

struct Backend(Arc<World>);

#[async_trait::async_trait]
impl CapabilityProvider for Backend {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Caps.capabilities().await
    }
    async fn availability(&self, id: &CapabilityId) -> CapabilityAvailability {
        Caps.availability(id).await
    }
}

#[async_trait::async_trait]
impl Executor for Backend {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.0.execute(r).await
    }
}
