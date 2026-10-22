//! A satisfied goal is not a completed work, and a restart does not change what is said about it.
//!
//! The decision matrix of `chip-core/tests/goal_completion_contract.rs`, with the work stopped and
//! resumed through FeltDB's real native agent state. For each case: the outcome, the task status and
//! `WorkResult.detail` FeltDB holds, and the goal's status as the events say it (`WorkSpec::goal_level`),
//! uninterrupted and after a restart. No record schema is involved: the goal's status is derived from
//! the durable observations by the same predicates, on every attempt.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_agent_state::feltdb::FeltDb;
use chip_agent_state::feltdb::agent_state::AuthorityClock;
use chip_agent_state::{AgentStore, Begun, DurableConfig, DurableWork, StateError, begin};
use chip_core::{
    Agent, AnswerPredicate, Capability, CapabilityAvailability, CapabilityDescriptor,
    CapabilityError, CapabilityId, CapabilityInput, CapabilityProvider, CapabilityRequest,
    DecisionError, ExecutionError, ExecutionId, ExecutionObserver, ExecutionRequest,
    ExecutionResult, Executor, InputValue, LocalWorkPolicy, NoLocalPolicy, Observation,
    ObservationPredicate, STUCK_REASON_PREFIX, StopCause, StuckThresholds, WorkControl,
    WorkDecision, WorkDecisionBoundary, WorkEvent, WorkGoal, WorkId, WorkOutcome, WorkReport,
    WorkSpec, WorkView,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use tempfile::TempDir;

const LEASE_MS: u64 = 60_000;
const GOAL: &str = "get a ready reading";

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
        .with_input("path", InputValue::Text("a".into())),
    )
}

struct Caps;

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        let mut d = CapabilityDescriptor::new(CapabilityId::new("world.read")?, "Read", "reads")
            .without_evidence_reuse();
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

/// The n-th read of the world answers `script[n]`, the last entry once exhausted.
struct World {
    script: &'static [&'static str],
    ran: Mutex<usize>,
}

impl World {
    fn new(script: &'static [&'static str]) -> Arc<Self> {
        Arc::new(Self {
            script,
            ran: Mutex::new(0),
        })
    }
    fn executions(&self) -> usize {
        *self.ran.lock().unwrap()
    }
}

#[async_trait::async_trait]
impl Executor for World {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let mut n = self.ran.lock().unwrap();
        let out = self.script[(*n).min(self.script.len() - 1)];
        *n += 1;
        Ok(ExecutionResult::success(r.id, out))
    }
}

#[derive(Debug)]
struct Ready;

impl ObservationPredicate for Ready {
    fn describe(&self) -> String {
        "a reading that says ready".into()
    }
    fn satisfied_by(&self, o: &Observation) -> bool {
        o.output.as_deref() == Some("ready")
    }
    fn satisfied_by_trajectory(&self, observations: &[Observation]) -> bool {
        observations.last().is_some_and(|o| self.satisfied_by(o))
    }
}

struct CompleteWhenReady;

impl LocalWorkPolicy for CompleteWhenReady {
    fn propose(&self, view: &WorkView<'_>) -> Option<WorkDecision> {
        Ready
            .satisfied_by_trajectory(view.observations)
            .then(|| WorkDecision::Complete {
                summary: "ready, grounded in the read".into(),
            })
    }
}

#[derive(Debug)]
struct SaysGrounded;

impl AnswerPredicate for SaysGrounded {
    fn describe(&self) -> String {
        "an answer that says grounded".into()
    }
    fn accepts(&self, answer: &str, _o: &[Observation]) -> bool {
        answer.contains("grounded")
    }
}

struct Stop(StopCause);

impl WorkControl for Stop {
    fn stop_requested(&self) -> Option<StopCause> {
        Some(self.0)
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

struct Rig {
    dir: TempDir,
    clock: Arc<Clock>,
}

impl Rig {
    fn new() -> Self {
        Self {
            dir: TempDir::new().unwrap(),
            clock: Arc::new(Clock::default()),
        }
    }

    fn store(&self) -> AgentStore {
        let db = FeltDb::open(self.dir.path().join("agent-state.log")).unwrap();
        AgentStore::with_clock(db, self.clock.clone())
    }

    fn begin(&self, task: &str, actor: &str) -> DurableWork {
        let config = DurableConfig {
            actor: actor.into(),
            lease_ms: LEASE_MS,
            ..Default::default()
        };
        match begin(&self.store(), task, GOAL, &config) {
            Ok(Begun::Ready(w)) => w,
            Ok(Begun::Finished(h)) => panic!("finished: {}", h.task.status),
            Err(e @ StateError::OutcomeUncertain { .. }) => panic!("{e}"),
            Err(e) => panic!("{e}"),
        }
    }

    fn expire(&self) {
        self.clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);
    }
}

struct Attempt {
    report: WorkReport,
    spec: WorkSpec,
    model_calls: usize,
}

impl Attempt {
    fn goal(&self) -> Option<bool> {
        self.spec.goal_level(&self.report.events)
    }
    fn stuck(&self) -> bool {
        matches!(&self.report.outcome, WorkOutcome::Blocked { reason } if reason.starts_with(STUCK_REASON_PREFIX))
    }
    fn goal_events(&self) -> usize {
        self.report
            .events
            .iter()
            .filter(|e| matches!(e, WorkEvent::GoalEvaluated { .. }))
            .count()
    }
}

async fn attempt(
    work: &DurableWork,
    world: &Arc<World>,
    policy: &dyn LocalWorkPolicy,
    stop: Option<StopCause>,
    max_turns: usize,
) -> Attempt {
    let mut spec = WorkSpec::new(WorkId::new(work.task_id()), WorkGoal::new(GOAL))
        .with_restored(work.restored())
        .with_required_observation(Arc::new(Ready))
        .with_required_answer(Arc::new(SaysGrounded))
        .with_stuck_guard(StuckThresholds::default());
    spec.limits.max_turns = max_turns;
    spec.limits.max_executions = 20;
    if let Some(cause) = stop {
        spec.control = Some(Arc::new(Stop(cause)));
    }
    let model = Arc::new(Model::default());
    let report = Agent::new(model.clone())
        .with_capabilities(Arc::new(Caps))
        .with_executor(work.executor(world.clone()))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &spec,
            policy,
            &Script {
                decisions: (0..12).map(|_| read()).collect(),
                next: Default::default(),
            },
        )
        .await;
    Attempt {
        report,
        spec,
        model_calls: model.0.load(Ordering::SeqCst),
    }
}

/// The detail of the last `WorkResult` and the task status FeltDB holds.
fn recorded(rig: &Rig, task: &str) -> (String, String) {
    let h = rig.store().history(task).unwrap().unwrap();
    (
        h.task.status.clone(),
        h.results.last().unwrap().detail.clone(),
    )
}

// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn goal_satisfied_and_undecided_is_blocked_with_the_goal_met_and_a_restart_says_the_same() {
    let rig = Rig::new();
    let world = World::new(&["ready"]);
    let first = rig.begin("t-undecided", "chip-a");
    let a1 = attempt(&first, &world, &NoLocalPolicy, None, 20).await;
    assert!(a1.stuck(), "{:?}", a1.report.outcome);
    assert_eq!(a1.goal(), Some(true));
    assert_eq!(a1.goal_events(), 3);
    let f1 = first.finish(&a1.report, false).unwrap();
    assert_eq!(f1.status, "blocked");
    let (status1, detail1) = recorded(&rig, "t-undecided");
    assert_eq!(status1, "blocked");
    assert!(detail1.contains("already satisfied"), "{detail1}");
    rig.expire();

    // A fresh process: the same outcome, the same recorded detail, the same goal status; no model
    // call, no execution; and no goal evaluation that did not happen (3 restored, 0 new).
    let second = rig.begin("t-undecided", "chip-b");
    let a2 = attempt(&second, &world, &NoLocalPolicy, None, 20).await;
    assert!(a2.stuck(), "{:?}", a2.report.outcome);
    assert_eq!(
        a2.goal(),
        Some(true),
        "the restart reports what the first attempt did"
    );
    assert_eq!(a2.goal_events(), 0);
    assert_eq!((a2.model_calls, world.executions()), (0, 3));
    second.finish(&a2.report, false).unwrap();
    let (status2, detail2) = recorded(&rig, "t-undecided");
    assert_eq!((status2, detail2), (status1, detail1));
}

#[tokio::test]
async fn goal_unsatisfied_and_repeating_is_stuck_without_the_goal_met_before_and_after_a_restart() {
    let rig = Rig::new();
    let world = World::new(&["pending"]);
    let first = rig.begin("t-unmet", "chip-a");
    let a1 = attempt(&first, &world, &NoLocalPolicy, None, 20).await;
    assert!(a1.stuck());
    assert_eq!(a1.goal(), Some(false));
    first.finish(&a1.report, false).unwrap();
    let (_, detail1) = recorded(&rig, "t-unmet");
    assert!(!detail1.contains("already satisfied"), "{detail1}");
    rig.expire();
    let second = rig.begin("t-unmet", "chip-b");
    let a2 = attempt(&second, &world, &NoLocalPolicy, None, 20).await;
    assert!(a2.stuck());
    assert_eq!(a2.goal(), Some(false));
    second.finish(&a2.report, false).unwrap();
    assert_eq!(recorded(&rig, "t-unmet").1, detail1);
}

#[tokio::test]
async fn goal_satisfied_with_an_authorized_completion_is_completed_on_resume_and_recorded_so() {
    let rig = Rig::new();
    let world = World::new(&["ready"]);
    let first = rig.begin("t-complete", "chip-a");
    // Attempt 1 is cut off after its read (a turn limit), so the goal is met and nothing completed.
    let cut = attempt(&first, &world, &NoLocalPolicy, None, 1).await;
    assert!(
        matches!(cut.report.outcome, WorkOutcome::LimitReached { .. }),
        "{:?}",
        cut.report.outcome
    );
    assert_eq!(cut.goal(), Some(true));
    first.finish(&cut.report, false).unwrap();
    rig.expire();

    let second = rig.begin("t-complete", "chip-b");
    let a2 = attempt(&second, &world, &CompleteWhenReady, None, 20).await;
    assert!(
        matches!(a2.report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        a2.report.outcome
    );
    assert_eq!(a2.goal(), Some(true));
    assert_eq!((a2.model_calls, world.executions()), (0, 1));
    let f = second.finish(&a2.report, true).unwrap();
    assert_eq!(f.status, "completed");
    assert!(!f.completion_refused);
}

#[tokio::test]
async fn goal_satisfied_and_a_stop_on_resume_is_cancelled_with_the_goal_met() {
    for cause in [StopCause::Cancelled, StopCause::Deadline] {
        let rig = Rig::new();
        let world = World::new(&["ready"]);
        let task = format!("t-stop-{}", cause.name());
        let first = rig.begin(&task, "chip-a");
        let cut = attempt(&first, &world, &NoLocalPolicy, None, 1).await;
        first.finish(&cut.report, false).unwrap();
        rig.expire();

        let second = rig.begin(&task, "chip-b");
        let a2 = attempt(&second, &world, &CompleteWhenReady, Some(cause), 20).await;
        assert!(
            matches!(a2.report.outcome, WorkOutcome::Cancelled { cause: c, .. } if c == cause),
            "{:?}",
            a2.report.outcome
        );
        assert_eq!(a2.goal(), Some(true), "{}", cause.name());
        assert_eq!((a2.model_calls, world.executions()), (0, 1));
        let f = second.finish(&a2.report, false).unwrap();
        assert_eq!(f.status, "cancelled");
    }
}

#[tokio::test]
async fn a_productive_trajectory_that_repeats_a_capability_resumes_and_completes() {
    // v1, v2 restored; the third read is the required evidence. No repetition is identical.
    let rig = Rig::new();
    let world = World::new(&["v1", "v2", "ready"]);
    let first = rig.begin("t-prod", "chip-a");
    let cut = attempt(&first, &world, &NoLocalPolicy, None, 2).await;
    assert!(matches!(
        cut.report.outcome,
        WorkOutcome::LimitReached { .. }
    ));
    assert_eq!(cut.goal(), Some(false));
    first.finish(&cut.report, false).unwrap();
    rig.expire();
    let second = rig.begin("t-prod", "chip-b");
    let a2 = attempt(&second, &world, &CompleteWhenReady, None, 20).await;
    assert!(
        matches!(a2.report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        a2.report.outcome
    );
    assert_eq!(a2.goal(), Some(true));
    assert_eq!(
        a2.goal_events(),
        1,
        "the new read is evaluated once; the two restored are not re-announced"
    );
    assert_eq!(second.finish(&a2.report, true).unwrap().status, "completed");
}

#[tokio::test]
async fn a_model_claim_without_qualifying_evidence_is_never_recorded_completed() {
    // The recorded trajectory does not satisfy the goal; the model claims completion anyway.
    let rig = Rig::new();
    let world = World::new(&["pending"]);
    let first = rig.begin("t-claim", "chip-a");
    let cut = attempt(&first, &world, &NoLocalPolicy, None, 1).await;
    first.finish(&cut.report, false).unwrap();
    rig.expire();
    let second = rig.begin("t-claim", "chip-b");
    let mut spec = WorkSpec::new(WorkId::new("t-claim"), WorkGoal::new(GOAL))
        .with_restored(second.restored())
        .with_required_observation(Arc::new(Ready))
        .with_stuck_guard(StuckThresholds::default());
    spec.limits.max_turns = 5;
    let report = Agent::new(Arc::new(Model::default()))
        .with_capabilities(Arc::new(Caps))
        .with_executor(second.executor(world.clone()))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &spec,
            &NoLocalPolicy,
            // The model insists on the claim it was refused, until the bound ends the work.
            &Script {
                decisions: vec![
                    WorkDecision::Complete {
                        summary: "grounded, done".into(),
                    };
                    1 + chip_core::DEFAULT_MALFORMED_REPLY_LIMIT
                ],
                next: Default::default(),
            },
        )
        .await;
    assert!(
        matches!(&report.outcome, WorkOutcome::Blocked { reason } if reason.contains("no authoritative observation satisfies the goal")),
        "{:?}",
        report.outcome
    );
    assert_eq!(spec.goal_level(&report.events), Some(false));
    // Even if the host were to accept it, FeltDB does not record a completion that nothing backs.
    let f = second.finish(&report, true).unwrap();
    assert_ne!(f.status, "completed");
}
