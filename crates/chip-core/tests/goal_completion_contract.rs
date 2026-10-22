//! The contract between a satisfied goal and a completed work.
//!
//! A goal's required observations being met is not completion. Only an accepted `Complete` decision
//! (from Chip's local policy or from the model, passing the goal and answer rules) completes a work.
//! Every other ending of a work whose goal is met keeps the goal's status alongside the outcome, in
//! the events the loop already emits: `GoalEvaluated` for what it observed, and `ObservationRestored`
//! (carrying the same count) for what a resume joined. `WorkSpec::goal_level` reads both.
//!
//! The decision matrix, real loop, scripted model, no persistence. The same cases restored from
//! FeltDB are in `chip-agent-state/tests/stuck_guard.rs`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_core::{
    Agent, AnswerPredicate, Capability, CapabilityAvailability, CapabilityDescriptor,
    CapabilityError, CapabilityId, CapabilityInput, CapabilityProvider, CapabilityRequest,
    DecisionError, ExecutionError, ExecutionId, ExecutionObserver, ExecutionRequest,
    ExecutionResult, Executor, InputValue, LimitKind, LocalWorkPolicy, NoLocalPolicy, Observation,
    ObservationPredicate, RestoredObservation, STUCK_REASON_PREFIX, StopCause, StuckThresholds,
    WorkControl, WorkDecision, WorkDecisionBoundary, WorkEvent, WorkGoal, WorkId, WorkOutcome,
    WorkReport, WorkSpec, WorkView, audit_safety,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};

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

fn complete(summary: &str) -> WorkDecision {
    WorkDecision::Complete {
        summary: summary.into(),
    }
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

/// The n-th read answers `script[n]`, the last entry once exhausted.
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
        Ok(ExecutionResult::success(r.id, out).with_receipt_id(format!("sha256:{n}")))
    }
}

/// Met when the latest observation says `ready`.
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

/// Chip completes the work itself the moment the goal holds.
struct CompleteWhenReady;

impl LocalWorkPolicy for CompleteWhenReady {
    fn propose(&self, view: &WorkView<'_>) -> Option<WorkDecision> {
        Ready
            .satisfied_by_trajectory(view.observations)
            .then(|| complete("ready, grounded in the read"))
    }
}

/// An answer is accepted only if it says `grounded`.
#[derive(Debug)]
struct SaysGrounded;

impl AnswerPredicate for SaysGrounded {
    fn describe(&self) -> String {
        "an answer that says grounded".into()
    }
    fn accepts(&self, answer: &str, _observations: &[Observation]) -> bool {
        answer.contains("grounded")
    }
}

struct Stop(StopCause);

impl WorkControl for Stop {
    fn stop_requested(&self) -> Option<StopCause> {
        Some(self.0)
    }
}

/// Says "go on" for the first `quiet` questions, then stops.
struct StopAfter(usize, AtomicUsize);

impl WorkControl for StopAfter {
    fn stop_requested(&self) -> Option<StopCause> {
        (self.1.fetch_add(1, Ordering::SeqCst) >= self.0).then_some(StopCause::Cancelled)
    }
}

fn spec(guard: bool, turns: usize) -> WorkSpec {
    let mut s = WorkSpec::new(WorkId::new("w"), WorkGoal::new("get a ready reading"))
        .with_required_observation(Arc::new(Ready))
        .with_required_answer(Arc::new(SaysGrounded))
        .with_malformed_reply_limit(0);
    s.limits.max_turns = turns;
    s.limits.max_executions = 20;
    if guard {
        s = s.with_stuck_guard(StuckThresholds::default());
    }
    s
}

struct Ran {
    report: WorkReport,
    spec: WorkSpec,
    world: Arc<World>,
    model_calls: usize,
}

impl Ran {
    fn goal(&self) -> Option<bool> {
        self.spec.goal_level(&self.report.events)
    }
    fn reason(&self) -> String {
        match &self.report.outcome {
            WorkOutcome::Blocked { reason } => reason.clone(),
            other => format!("{other:?}"),
        }
    }
    fn count(&self, f: impl Fn(&WorkEvent) -> bool) -> usize {
        self.report.events.iter().filter(|e| f(e)).count()
    }
}

async fn run(
    spec: WorkSpec,
    world: &Arc<World>,
    policy: &dyn LocalWorkPolicy,
    decisions: Vec<WorkDecision>,
) -> Ran {
    let model = Arc::new(Model::default());
    let report = Agent::new(model.clone())
        .with_capabilities(Arc::new(Caps))
        .with_executor(world.clone())
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &spec,
            policy,
            &Script {
                decisions,
                next: Default::default(),
            },
        )
        .await;
    audit_safety(&report, &spec, &[CapabilityId::new("world.read").unwrap()]).assert_clean();
    Ran {
        report,
        spec,
        world: world.clone(),
        model_calls: model.0.load(Ordering::SeqCst),
    }
}

fn many(d: WorkDecision, n: usize) -> Vec<WorkDecision> {
    std::iter::repeat_n(d, n).collect()
}

fn is_completed(r: &Ran) -> bool {
    matches!(r.report.outcome, WorkOutcome::Completed { .. })
}

fn is_stuck(r: &Ran) -> bool {
    r.reason().starts_with(STUCK_REASON_PREFIX)
}

// ---------------------------------------------------------------------------------------------
// The decision matrix
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn goal_unsatisfied_and_repeating_is_the_existing_stuck_stop() {
    let r = run(
        spec(true, 20),
        &World::new(&["pending"]),
        &NoLocalPolicy,
        many(read(), 8),
    )
    .await;
    assert!(is_stuck(&r), "{:?}", r.report.outcome);
    assert!(!r.reason().contains("already satisfied"), "{}", r.reason());
    assert_eq!(r.goal(), Some(false));
}

#[tokio::test]
async fn goal_satisfied_and_the_local_policy_authorizes_completion_completes() {
    for guard in [false, true] {
        let r = run(
            spec(guard, 20),
            &World::new(&["ready"]),
            &CompleteWhenReady,
            many(read(), 8),
        )
        .await;
        assert!(is_completed(&r), "guard {guard}: {:?}", r.report.outcome);
        assert_eq!(r.goal(), Some(true));
        assert_eq!(r.world.executions(), 1);
    }
}

#[tokio::test]
async fn goal_satisfied_and_the_model_completes_with_an_accepted_answer_completes() {
    let script = vec![
        read(),
        complete("the reading is ready, grounded in the read"),
    ];
    let r = run(
        spec(true, 20),
        &World::new(&["ready"]),
        &NoLocalPolicy,
        script,
    )
    .await;
    assert!(is_completed(&r), "{:?}", r.report.outcome);
    assert_eq!(r.goal(), Some(true));
}

#[tokio::test]
async fn goal_satisfied_and_the_models_answer_is_refused_is_blocked_with_the_goal_still_met() {
    // The completion decision exists, and the answer rule refuses it: not completed, and not stuck.
    let script = vec![read(), complete("it is fine")];
    let r = run(
        spec(true, 20),
        &World::new(&["ready"]),
        &NoLocalPolicy,
        script,
    )
    .await;
    assert!(
        r.reason().contains("the proposed answer is not supported"),
        "{:?}",
        r.report.outcome
    );
    assert!(!is_stuck(&r));
    assert_eq!(r.goal(), Some(true));
}

#[tokio::test]
async fn goal_satisfied_and_no_completion_decision_while_repeating_is_blocked_and_says_so() {
    let r = run(
        spec(true, 20),
        &World::new(&["ready"]),
        &NoLocalPolicy,
        many(read(), 8),
    )
    .await;
    assert!(is_stuck(&r), "{:?}", r.report.outcome);
    assert!(r.reason().contains("already satisfied"), "{}", r.reason());
    assert!(!is_completed(&r));
    assert_eq!(r.goal(), Some(true));
    // The stop did not manufacture a decision: no model call after the third pair, no completion.
    assert_eq!((r.world.executions(), r.model_calls), (3, 3));
    assert_eq!(r.count(|e| matches!(e, WorkEvent::WorkCompleted { .. })), 0);
}

#[tokio::test]
async fn goal_satisfied_is_distinguishable_from_unsatisfied_in_every_other_ending() {
    // The model blocks itself, a limit is reached, or the owner stops the work: the outcome says
    // nothing of the goal, and the goal's level, from the same events, does.
    for (ready, expect) in [("ready", Some(true)), ("pending", Some(false))] {
        let world = World::new(if ready == "ready" {
            &["ready"]
        } else {
            &["pending"]
        });
        let blocks = run(
            spec(false, 20),
            &world,
            &NoLocalPolicy,
            vec![
                read(),
                WorkDecision::Block {
                    reason: "I will not".into(),
                },
            ],
        )
        .await;
        assert!(matches!(blocks.report.outcome, WorkOutcome::Blocked { .. }));
        assert_eq!(blocks.goal(), expect, "model blocks, {ready}");

        let world = World::new(if ready == "ready" {
            &["ready"]
        } else {
            &["pending"]
        });
        let limited = run(spec(false, 2), &world, &NoLocalPolicy, many(read(), 8)).await;
        assert!(matches!(
            limited.report.outcome,
            WorkOutcome::LimitReached {
                limit: LimitKind::Turns
            }
        ));
        assert_eq!(limited.goal(), expect, "limit, {ready}");

        let world = World::new(if ready == "ready" {
            &["ready"]
        } else {
            &["pending"]
        });
        let mut s = spec(true, 20);
        s.control = Some(Arc::new(StopAfter(3, AtomicUsize::new(0))));
        let stopped = run(s, &world, &NoLocalPolicy, many(read(), 8)).await;
        assert!(
            matches!(stopped.report.outcome, WorkOutcome::Cancelled { .. }),
            "{:?}",
            stopped.report.outcome
        );
        assert_eq!(stopped.goal(), expect, "stopped, {ready}");
    }
}

#[tokio::test]
async fn a_stop_beats_a_satisfied_goal_and_the_guard_and_keeps_the_goals_status() {
    // The goal is met by restored observations, the trajectory repeats, and the owner says stop.
    for cause in [StopCause::Cancelled, StopCause::Deadline] {
        let mut s = spec(true, 20).with_restored((1..=3).map(|n| restored(n, "ready")).collect());
        s.control = Some(Arc::new(Stop(cause)));
        let r = run(s, &World::new(&["ready"]), &CompleteWhenReady, vec![read()]).await;
        assert!(
            matches!(r.report.outcome, WorkOutcome::Cancelled { cause: c, .. } if c == cause),
            "{:?}",
            r.report.outcome
        );
        assert_eq!(
            r.goal(),
            Some(true),
            "the restored evaluation survives the stop"
        );
        assert_eq!((r.world.executions(), r.model_calls), (0, 0));
    }
}

#[tokio::test]
async fn a_model_claim_without_qualifying_evidence_is_never_completed() {
    for guard in [false, true] {
        // Claims before anything was observed.
        let r = run(
            spec(guard, 20),
            &World::new(&["pending"]),
            &NoLocalPolicy,
            vec![complete("grounded, done")],
        )
        .await;
        assert!(
            r.reason()
                .contains("no authoritative observation satisfies the goal"),
            "{}",
            r.reason()
        );
        assert_eq!(r.goal(), None, "nothing was evaluated");
        // Claims after an observation that does not satisfy the goal.
        let r = run(
            spec(guard, 20),
            &World::new(&["pending"]),
            &NoLocalPolicy,
            vec![read(), complete("grounded, done")],
        )
        .await;
        assert!(
            r.reason()
                .contains("no authoritative observation satisfies the goal"),
            "{}",
            r.reason()
        );
        assert_eq!(r.goal(), Some(false));
        assert!(!is_completed(&r));
    }
}

#[tokio::test]
async fn a_repeated_request_that_produces_the_required_evidence_completes() {
    let script: &'static [&'static str] = &["v1", "v2", "v3", "ready"];
    let r = run(
        spec(true, 20),
        &World::new(script),
        &CompleteWhenReady,
        many(read(), 8),
    )
    .await;
    assert!(is_completed(&r), "{:?}", r.report.outcome);
    assert_eq!(r.goal(), Some(true));
    assert_eq!(r.world.executions(), 4);
}

// ---------------------------------------------------------------------------------------------
// The goal's evaluation in the events
// ---------------------------------------------------------------------------------------------

fn restored(n: usize, output: &str) -> RestoredObservation {
    let id = ExecutionId::new(format!("w-exec-{n}"));
    let mut request = CapabilityRequest::new(id.clone(), CapabilityId::new("world.read").unwrap());
    request
        .inputs
        .insert("path".into(), InputValue::Text("a".into()));
    request.inputs_present = true;
    RestoredObservation {
        request,
        result: ExecutionResult::success(id, output).with_receipt_id(format!("sha256:r{n}")),
        reobserved: false,
    }
}

#[tokio::test]
async fn each_observation_is_evaluated_once_and_the_stop_discards_none() {
    let r = run(
        spec(true, 20),
        &World::new(&["ready"]),
        &NoLocalPolicy,
        many(read(), 8),
    )
    .await;
    assert!(is_stuck(&r));
    let evaluated = r.count(|e| matches!(e, WorkEvent::GoalEvaluated { .. }));
    assert_eq!(
        evaluated,
        r.world.executions(),
        "one evaluation per observation, none dropped, none added"
    );
    assert_eq!(r.report.observations.len(), 3);
}

#[tokio::test]
async fn a_resume_evaluates_what_it_restores_without_adding_goal_events() {
    // Three restored observations, the goal met by them, no local policy: the work is stuck at once.
    let s = spec(true, 20).with_restored((1..=3).map(|n| restored(n, "ready")).collect());
    let r = run(s, &World::new(&["ready"]), &NoLocalPolicy, vec![read()]).await;
    assert!(is_stuck(&r), "{:?}", r.report.outcome);
    assert!(r.reason().contains("already satisfied"), "{}", r.reason());
    // The restored observations carry their evaluation in `ObservationRestored`; the loop emitted no
    // `GoalEvaluated` for what it did not observe.
    assert_eq!(r.count(|e| matches!(e, WorkEvent::GoalEvaluated { .. })), 0);
    let remaining: Vec<usize> = r
        .report
        .events
        .iter()
        .filter_map(|e| match e {
            WorkEvent::ObservationRestored { remaining, .. } => Some(*remaining),
            _ => None,
        })
        .collect();
    assert_eq!(remaining, [0, 0, 0]);
    // And `goal_level` reads it, which is what makes a resume report what an uninterrupted run does.
    assert_eq!(r.goal(), Some(true));
}

#[tokio::test]
async fn a_restored_observation_claims_a_goal_only_if_the_work_has_one() {
    // Without a requirement `ObservationRestored.remaining` is vacuously 0. It must not read as "met".
    let mut s = WorkSpec::new(WorkId::new("w"), WorkGoal::new("look"))
        .with_restored((1..=2).map(|n| restored(n, "ready")).collect());
    s.limits.max_turns = 2;
    let r = run(s, &World::new(&["ready"]), &NoLocalPolicy, many(read(), 4)).await;
    assert_eq!(r.goal(), None);
}
