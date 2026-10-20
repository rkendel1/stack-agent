//! The deterministic stuck guard: a predicate over the work's own trajectory.
//!
//! A scripted model, deterministic fake capabilities, the real work loop. Nothing here asks a real
//! model anything. Each test establishes one of two things: a genuine loop ends, deterministically
//! and without another model call or execution, or superficially similar productive work goes on.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, CapabilityRequest, DecisionError, ExecutionError,
    ExecutionId, ExecutionObserver, ExecutionRequest, ExecutionResult, ExecutionStatus, Executor,
    InputValue, LimitKind, LocalWorkPolicy, NoLocalPolicy, Observation, ObservationKind,
    ObservationOrigin, ObservationPredicate, RestoredObservation, STUCK_REASON_PREFIX, StopCause,
    StuckPattern, StuckThresholds, WorkControl, WorkDecision, WorkDecisionBoundary, WorkEvent,
    WorkGoal, WorkId, WorkLimits, WorkOutcome, WorkReport, WorkSpec, WorkView, audit_safety,
    detect_stuck, verify_trajectory,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

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

fn read(path: &str) -> WorkDecision {
    WorkDecision::RequestCapability(
        CapabilityRequest::new(
            ExecutionId::new("x"),
            CapabilityId::new("world.read").unwrap(),
        )
        .with_input("path", InputValue::Text(path.into())),
    )
}

fn write(path: &str, content: &str) -> WorkDecision {
    WorkDecision::RequestCapability(
        CapabilityRequest::new(
            ExecutionId::new("x"),
            CapabilityId::new("world.write").unwrap(),
        )
        .with_input("path", InputValue::Text(path.into()))
        .with_input("content", InputValue::Text(content.into())),
    )
}

fn complete() -> WorkDecision {
    WorkDecision::Complete {
        summary: "done".into(),
    }
}

fn input(name: &str) -> CapabilityInput {
    CapabilityInput {
        name: name.into(),
        description: name.into(),
        required: true,
    }
}

struct Caps;

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        let mut r = CapabilityDescriptor::new(CapabilityId::new("world.read")?, "Read", "reads")
            .without_evidence_reuse();
        r.inputs.push(input("path"));
        let mut w = CapabilityDescriptor::new(CapabilityId::new("world.write")?, "Write", "writes")
            .without_evidence_reuse();
        w.inputs.push(input("path"));
        w.inputs.push(input("content"));
        Ok(vec![r, w])
    }
    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

/// How the world answers a read.
#[derive(Clone, Copy)]
enum Reads {
    /// What was last written to the path (or `empty`).
    Files,
    /// The same text for every path: different requests, one answer.
    Constant,
    /// A different answer every time: the world is changing under the model.
    Changing,
    /// The n-th read answers `script[n]`, the last entry once exhausted.
    Sequence(&'static [&'static str]),
}

struct World {
    reads: Reads,
    files: Mutex<BTreeMap<String, String>>,
    ran: Mutex<Vec<String>>,
    count: AtomicUsize,
}

impl World {
    fn new(reads: Reads) -> Arc<Self> {
        Arc::new(Self {
            reads,
            files: Mutex::default(),
            ran: Mutex::default(),
            count: AtomicUsize::new(0),
        })
    }

    fn executions(&self) -> usize {
        self.ran.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl Executor for World {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.ran.lock().unwrap().push(r.intent.clone());
        let text = |name: &str| match r.inputs.get(name) {
            Some(InputValue::Text(t)) => t.clone(),
            _ => String::new(),
        };
        let n = self.count.fetch_add(1, Ordering::SeqCst);
        let output = if r.intent == "world.write" {
            self.files
                .lock()
                .unwrap()
                .insert(text("path"), text("content"));
            format!("wrote {}", text("path"))
        } else {
            match self.reads {
                Reads::Files => self
                    .files
                    .lock()
                    .unwrap()
                    .get(&text("path"))
                    .cloned()
                    .unwrap_or_else(|| "empty".into()),
                Reads::Constant => "the same text".into(),
                Reads::Changing => format!("version {n}"),
                Reads::Sequence(script) => script[n.min(script.len() - 1)].to_string(),
            }
        };
        // A receipt is made per execution, so it differs between otherwise identical executions.
        Ok(ExecutionResult::success(r.id, output).with_receipt_id(format!("sha256:receipt-{n}")))
    }
}

struct Outcome {
    report: WorkReport,
    model_calls: usize,
}

fn limits(max_turns: usize, max_executions: usize) -> WorkLimits {
    WorkLimits {
        max_turns,
        max_executions,
    }
}

async fn run_spec(spec: WorkSpec, world: &Arc<World>, decisions: Vec<WorkDecision>) -> Outcome {
    let model = Arc::new(Model::default());
    let report = Agent::new(model.clone())
        .with_capabilities(Arc::new(Caps))
        .with_executor(world.clone())
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &spec,
            &NoLocalPolicy,
            &Script {
                decisions,
                next: Default::default(),
            },
        )
        .await;
    Outcome {
        report,
        model_calls: model.0.load(Ordering::SeqCst),
    }
}

fn spec(guard: Option<StuckThresholds>, max_turns: usize, max_executions: usize) -> WorkSpec {
    let mut s = WorkSpec::new(WorkId::new("w"), WorkGoal::new("look into the files"));
    s.limits = limits(max_turns, max_executions);
    s.stuck_guard = guard;
    s
}

fn stuck_reason(report: &WorkReport) -> Option<&str> {
    match &report.outcome {
        WorkOutcome::Blocked { reason } if reason.starts_with(STUCK_REASON_PREFIX) => {
            Some(reason.as_str())
        }
        _ => None,
    }
}

fn decisions_started(report: &WorkReport) -> usize {
    report
        .events
        .iter()
        .filter(|e| matches!(e, WorkEvent::DecisionStarted { .. }))
        .count()
}

fn many(d: WorkDecision, n: usize) -> Vec<WorkDecision> {
    std::iter::repeat_n(d, n).collect()
}

fn on() -> Option<StuckThresholds> {
    Some(StuckThresholds::default())
}

// ---------------------------------------------------------------------------------------------
// The predicate, directly
// ---------------------------------------------------------------------------------------------

fn pair(invocation: &str, output: &str, n: usize) -> (ObservationOrigin, Observation) {
    (
        ObservationOrigin {
            capability: CapabilityId::new(invocation.split('|').next().unwrap()).unwrap(),
            invocation: invocation.into(),
            reusable: false,
            provider_response_id: None,
        },
        Observation {
            // Ids and receipts differ in every pair, as they do in a real trajectory.
            execution_id: ExecutionId::new(format!("w-exec-{n}")),
            kind: ObservationKind::ExecutionCompleted,
            status: ExecutionStatus::Success,
            output: Some(output.into()),
            receipt_id: Some(format!("sha256:{n}")),
            evidence: None,
        },
    )
}

fn trajectory(pairs: &[(&str, &str)]) -> (Vec<ObservationOrigin>, Vec<Observation>) {
    pairs
        .iter()
        .enumerate()
        .map(|(n, (i, o))| pair(i, o, n))
        .unzip()
}

fn detect(pairs: &[(&str, &str)]) -> Option<StuckPattern> {
    let (origins, observations) = trajectory(pairs);
    detect_stuck(&origins, &observations, &StuckThresholds::default())
}

#[test]
fn the_predicate_fires_exactly_at_its_thresholds() {
    let x = ("a.read|p=1", "same");
    let y = ("b.read|p=2", "other");
    assert_eq!(detect(&[]), None);
    assert_eq!(detect(&[x, x]), None, "two identical pairs: below 3");
    assert_eq!(
        detect(&[x, x, x]),
        Some(StuckPattern::Repeated { pairs: 3 })
    );
    assert_eq!(detect(&[x, y, x]), None, "ABA is not yet a cycle");
    assert_eq!(
        detect(&[x, y, x, y]),
        Some(StuckPattern::Alternating { pairs: 4 })
    );
    // Only the end counts: an earlier different pair breaks nothing, a later one ends it.
    assert_eq!(
        detect(&[y, x, x, x]),
        Some(StuckPattern::Repeated { pairs: 3 })
    );
    assert_eq!(detect(&[x, x, x, y]), None);
    assert_eq!(
        detect(&[x, x, y, x]),
        None,
        "a different pair in the middle"
    );
    assert_eq!(
        detect(&[y, y, x, y, x]),
        Some(StuckPattern::Alternating { pairs: 4 })
    );
}

#[test]
fn a_pair_is_the_request_with_its_inputs_and_what_it_observed() {
    let x = ("world.read|path=a", "same");
    // Same capability, different inputs: a different request.
    assert_eq!(
        detect(&[
            x,
            ("world.read|path=b", "same"),
            x,
            ("world.read|path=b", "same")
        ]),
        Some(StuckPattern::Alternating { pairs: 4 }),
        "alternating between two requests is a cycle"
    );
    assert_eq!(
        detect(&[
            ("world.read|path=a", "same"),
            ("world.read|path=b", "same"),
            ("world.read|path=c", "same"),
        ]),
        None
    );
    // Same request, a materially different observation.
    assert_eq!(detect(&[x, ("world.read|path=a", "changed"), x]), None);
    assert_eq!(
        detect(&[
            ("world.read|path=a", "v1"),
            ("world.read|path=a", "v2"),
            ("world.read|path=a", "v3"),
        ]),
        None
    );
}

#[test]
fn status_and_kind_are_part_of_the_observation() {
    let (origins, mut observations) = trajectory(&[
        ("world.read|path=a", "same"),
        ("world.read|path=a", "same"),
        ("world.read|path=a", "same"),
    ]);
    assert!(detect_stuck(&origins, &observations, &StuckThresholds::default()).is_some());
    observations[1].status = ExecutionStatus::Failure;
    assert!(detect_stuck(&origins, &observations, &StuckThresholds::default()).is_none());
    observations[1].status = ExecutionStatus::Success;
    observations[1].kind = ObservationKind::ExecutionFailed;
    assert!(detect_stuck(&origins, &observations, &StuckThresholds::default()).is_none());
}

#[test]
fn the_predicate_reads_a_bounded_window_whatever_the_trajectory_length() {
    // The answer depends on the last `window` pairs only: a long prefix of anything changes nothing.
    let t = StuckThresholds::default();
    assert_eq!(t.window(), 4);
    let x = ("a.read|p=1", "same");
    let tail = [x, x, x];
    for prefix_len in [0usize, 1, 10, 1_000, 50_000] {
        let mut pairs: Vec<(String, String)> = (0..prefix_len)
            .map(|i| (format!("w.read|p={i}"), format!("o{i}")))
            .collect();
        pairs.extend(tail.iter().map(|(a, b)| (a.to_string(), b.to_string())));
        let refs: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        assert_eq!(
            detect(&refs),
            Some(StuckPattern::Repeated { pairs: 3 }),
            "{prefix_len}"
        );
    }
}

#[test]
fn thresholds_are_held_to_values_that_mean_something() {
    let x = ("a.read|p=1", "same");
    let y = ("b.read|p=2", "other");
    let (o, b) = trajectory(&[x]);
    // `repeated: 0` or `1` would fire on one request.
    for repeated in [0, 1] {
        let t = StuckThresholds {
            repeated,
            alternating: 100,
        };
        assert_eq!(detect_stuck(&o, &b, &t), None);
    }
    // `alternating: 1` or `2` would fire on two different requests; 5 is read as 6.
    let (o, b) = trajectory(&[x, y]);
    let t = StuckThresholds {
        repeated: 100,
        alternating: 2,
    };
    assert_eq!(detect_stuck(&o, &b, &t), None);
    let (o, b) = trajectory(&[x, y, x, y, x]);
    let t = StuckThresholds {
        repeated: 100,
        alternating: 5,
    };
    assert_eq!(detect_stuck(&o, &b, &t), None, "five is read as six");
    let (o, b) = trajectory(&[y, x, y, x, y, x]);
    assert_eq!(
        detect_stuck(&o, &b, &t),
        Some(StuckPattern::Alternating { pairs: 6 })
    );
}

// ---------------------------------------------------------------------------------------------
// In the work loop
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_identical_pair_below_the_threshold_goes_on() {
    let world = World::new(Reads::Files);
    let mut script = many(read("a"), 2);
    script.push(complete());
    let out = run_spec(spec(on(), 8, 4), &world, script).await;
    assert!(matches!(out.report.outcome, WorkOutcome::Completed { .. }));
    assert_eq!(world.executions(), 2);
}

#[tokio::test]
async fn the_same_pair_beyond_the_threshold_ends_the_work_without_another_call_or_execution() {
    let guarded = World::new(Reads::Files);
    let out = run_spec(spec(on(), 20, 12), &guarded, many(read("a"), 20)).await;
    let reason = stuck_reason(&out.report).unwrap_or_else(|| panic!("{:?}", out.report.outcome));
    assert!(
        reason.contains("world.read was requested 3 times in a row"),
        "{reason}"
    );
    // Three requests, three model calls, three executions, and nothing after.
    assert_eq!(guarded.executions(), 3);
    assert_eq!(out.model_calls, 3);
    assert_eq!(out.report.observations.len(), 3);
    assert_eq!(decisions_started(&out.report), 3);
    let last = out.report.events.last().unwrap();
    assert!(matches!(last, WorkEvent::WorkBlocked { .. }), "{last:?}");
    // The audit and the trajectory verifier find nothing wrong with a work that stopped itself.
    assert!(
        audit_safety(
            &out.report,
            &spec(on(), 20, 12),
            &[CapabilityId::new("world.read").unwrap()]
        )
        .is_clean()
    );
    assert!(verify_trajectory(&out.report.events, &limits(20, 12)).is_empty());

    // The same script without the guard runs to its execution limit, and asks once more.
    let free = World::new(Reads::Files);
    let base = run_spec(spec(None, 20, 12), &free, many(read("a"), 20)).await;
    assert_eq!(free.executions(), 12);
    assert_eq!(base.model_calls, 13);
    assert!(matches!(
        base.report.outcome,
        WorkOutcome::LimitReached {
            limit: LimitKind::Executions
        }
    ));
}

#[tokio::test]
async fn an_alternating_loop_ends_the_work() {
    let world = World::new(Reads::Files);
    let script: Vec<WorkDecision> = (0..12)
        .map(|i| read(if i % 2 == 0 { "a" } else { "b" }))
        .collect();
    let out = run_spec(spec(on(), 20, 20), &world, script).await;
    let reason = stuck_reason(&out.report).unwrap();
    assert!(
        reason.contains("world.read and world.read alternated for 4 requests"),
        "{reason}"
    );
    assert_eq!(world.executions(), 4);
    assert_eq!(out.model_calls, 4);
}

#[tokio::test]
async fn an_alternation_that_makes_progress_is_not_a_loop() {
    // write, read, write, read: the same two capabilities alternate, but each read sees what the
    // write before it changed.
    let world = World::new(Reads::Files);
    let script = vec![
        write("a", "v1"),
        read("a"),
        write("a", "v2"),
        read("a"),
        write("a", "v3"),
        read("a"),
        complete(),
    ];
    let out = run_spec(spec(on(), 20, 20), &world, script).await;
    assert!(
        matches!(out.report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        out.report.outcome
    );
    assert_eq!(world.executions(), 6);
}

#[tokio::test]
async fn one_context_limit_is_terminal_and_is_not_a_loop() {
    // Pattern C. A request over the context budget is not sent, trimmed or retried: the work ends at
    // the first one, with no model call. There is no repetition for a guard to see.
    let world = World::new(Reads::Files);
    let s = spec(on(), 20, 20).with_context_budget_bytes(10);
    let out = run_spec(s, &world, many(read("a"), 5)).await;
    assert!(
        matches!(
            out.report.outcome,
            WorkOutcome::LimitReached {
                limit: LimitKind::Context
            }
        ),
        "{:?}",
        out.report.outcome
    );
    assert_eq!(out.model_calls, 0);
    assert_eq!(world.executions(), 0);
    assert!(stuck_reason(&out.report).is_none());
    let rejections = out
        .report
        .events
        .iter()
        .filter(|e| matches!(e, WorkEvent::ContextLimit { .. }))
        .count();
    assert_eq!(rejections, 1);
}

#[tokio::test]
async fn a_failed_model_call_is_not_a_stuck_work() {
    struct Down;
    #[async_trait::async_trait]
    impl ModelProvider for Down {
        async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
            Err(FxError::Provider("down".into()))
        }
    }
    let report = Agent::new(Arc::new(Down))
        .with_capabilities(Arc::new(Caps))
        .with_executor(World::new(Reads::Files))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &spec(on(), 8, 4),
            &NoLocalPolicy,
            &Script {
                decisions: vec![],
                next: Default::default(),
            },
        )
        .await;
    assert!(
        matches!(report.outcome, WorkOutcome::Failed { .. }),
        "{:?}",
        report.outcome
    );
}

#[tokio::test]
async fn the_same_capability_with_different_inputs_goes_on_even_when_the_answer_is_the_same() {
    let world = World::new(Reads::Constant);
    let mut script: Vec<WorkDecision> = (0..8).map(|i| read(&format!("file{i}"))).collect();
    script.push(complete());
    let out = run_spec(spec(on(), 20, 20), &world, script).await;
    assert!(
        matches!(out.report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        out.report.outcome
    );
    assert_eq!(world.executions(), 8);
}

#[tokio::test]
async fn an_identical_request_with_materially_different_observations_goes_on() {
    let world = World::new(Reads::Changing);
    let mut script = many(read("a"), 8);
    script.push(complete());
    let out = run_spec(spec(on(), 20, 20), &world, script).await;
    assert!(
        matches!(out.report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        out.report.outcome
    );
    assert_eq!(world.executions(), 8);
}

#[tokio::test]
async fn a_productive_trajectory_followed_by_a_loop_keeps_its_progress() {
    let world = World::new(Reads::Files);
    let mut script = vec![write("a", "good"), read("a"), read("b")];
    script.extend(many(read("b"), 6));
    let out = run_spec(spec(on(), 20, 20), &world, script).await;
    assert!(
        stuck_reason(&out.report).is_some(),
        "{:?}",
        out.report.outcome
    );
    // write, read a, then `read b` three times: three of the same pair end it.
    assert_eq!(world.executions(), 5);
    let outputs: Vec<&str> = out
        .report
        .observations
        .iter()
        .map(|o| o.output.as_deref().unwrap())
        .collect();
    assert_eq!(&outputs[..2], ["wrote a", "good"]);
    assert_eq!(
        world.files.lock().unwrap().get("a").map(String::as_str),
        Some("good")
    );
}

#[tokio::test]
async fn reaching_the_turn_limit_without_a_loop_is_not_stuck() {
    let world = World::new(Reads::Files);
    let script: Vec<WorkDecision> = (0..10).map(|i| read(&format!("f{i}"))).collect();
    let out = run_spec(spec(on(), 4, 20), &world, script).await;
    assert!(
        matches!(
            out.report.outcome,
            WorkOutcome::LimitReached {
                limit: LimitKind::Turns
            }
        ),
        "{:?}",
        out.report.outcome
    );
    assert_eq!(world.executions(), 4);
}

#[tokio::test]
async fn the_guard_names_a_loop_before_the_limit_does() {
    // At the production defaults (4 executions) the fourth identical request is the last one
    // allowed. The guard ends the work after the third, and says why.
    let world = World::new(Reads::Files);
    let out = run_spec(spec(on(), 8, 4), &world, many(read("a"), 8)).await;
    assert!(stuck_reason(&out.report).is_some());
    assert_eq!((world.executions(), out.model_calls), (3, 3));
    let base_world = World::new(Reads::Files);
    let base = run_spec(spec(None, 8, 4), &base_world, many(read("a"), 8)).await;
    assert!(matches!(
        base.report.outcome,
        WorkOutcome::LimitReached {
            limit: LimitKind::Executions
        }
    ));
    assert_eq!((base_world.executions(), base.model_calls), (4, 5));
}

// ---------------------------------------------------------------------------------------------
// A trajectory restored from durable records
// ---------------------------------------------------------------------------------------------

fn restored(n: usize, path: &str, output: &str) -> RestoredObservation {
    let id = ExecutionId::new(format!("w-exec-{n}"));
    let mut request = CapabilityRequest::new(id.clone(), CapabilityId::new("world.read").unwrap());
    request
        .inputs
        .insert("path".into(), InputValue::Text(path.into()));
    request.inputs_present = true;
    RestoredObservation {
        request,
        result: ExecutionResult::success(id, output).with_receipt_id(format!("sha256:r{n}")),
        reobserved: false,
    }
}

#[tokio::test]
async fn a_restored_trajectory_that_already_repeats_ends_before_anything_runs() {
    let world = World::new(Reads::Files);
    let s = spec(on(), 8, 4).with_restored(vec![
        restored(1, "a", "same"),
        restored(2, "a", "same"),
        restored(3, "a", "same"),
    ]);
    let out = run_spec(s, &world, vec![read("b"), complete()]).await;
    assert!(
        stuck_reason(&out.report).is_some(),
        "{:?}",
        out.report.outcome
    );
    assert_eq!(out.model_calls, 0);
    assert_eq!(world.executions(), 0);
    assert_eq!(decisions_started(&out.report), 0);
    // What was restored is kept.
    assert_eq!(out.report.observations.len(), 3);
}

#[tokio::test]
async fn a_restored_trajectory_that_does_not_repeat_continues() {
    let world = World::new(Reads::Files);
    let s = spec(on(), 8, 4).with_restored(vec![
        restored(1, "a", "same"),
        restored(2, "b", "same"),
        restored(3, "c", "same"),
    ]);
    let out = run_spec(s, &world, vec![complete()]).await;
    assert!(
        matches!(out.report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        out.report.outcome
    );
}

// ---------------------------------------------------------------------------------------------
// Cancellation and deadlines come first
// ---------------------------------------------------------------------------------------------

struct Stop(StopCause);

impl WorkControl for Stop {
    fn stop_requested(&self) -> Option<StopCause> {
        Some(self.0)
    }
}

#[tokio::test]
async fn a_stop_is_reported_as_a_stop_even_when_the_trajectory_is_stuck() {
    for cause in [StopCause::Cancelled, StopCause::Deadline] {
        let world = World::new(Reads::Files);
        let mut s = spec(on(), 8, 4).with_restored(vec![
            restored(1, "a", "same"),
            restored(2, "a", "same"),
            restored(3, "a", "same"),
        ]);
        s.control = Some(Arc::new(Stop(cause)));
        let out = run_spec(s, &world, vec![read("a")]).await;
        assert!(
            matches!(out.report.outcome, WorkOutcome::Cancelled { cause: c, .. } if c == cause),
            "{:?}",
            out.report.outcome
        );
        assert_eq!((world.executions(), out.model_calls), (0, 0));
    }
}

#[tokio::test]
async fn a_stop_requested_during_a_loop_ends_it_as_a_stop_and_runs_nothing_more() {
    // The owner says "go on" twice and then "stop": before the guard could fire, and before the
    // execution the model asked for.
    struct After(AtomicUsize);
    impl WorkControl for After {
        fn stop_requested(&self) -> Option<StopCause> {
            (self.0.fetch_add(1, Ordering::SeqCst) >= 4).then_some(StopCause::Cancelled)
        }
    }
    let world = World::new(Reads::Files);
    let mut s = spec(on(), 20, 20);
    s.control = Some(Arc::new(After(AtomicUsize::new(0))));
    let out = run_spec(s, &world, many(read("a"), 12)).await;
    assert!(
        matches!(out.report.outcome, WorkOutcome::Cancelled { .. }),
        "{:?}",
        out.report.outcome
    );
    assert!(
        world.executions() < 3,
        "stopped before the guard's third pair"
    );
}

// ---------------------------------------------------------------------------------------------
// A satisfied goal is not downgraded to `stuck`
// ---------------------------------------------------------------------------------------------

/// Met once the last three observations all say the same thing: "the reading is stable". A goal
/// that is itself a repetition, and one that depends on the trajectory's order.
#[derive(Debug)]
struct StableThrice;

impl ObservationPredicate for StableThrice {
    fn describe(&self) -> String {
        "three identical readings in a row".into()
    }
    fn satisfied_by(&self, _o: &Observation) -> bool {
        false
    }
    fn satisfied_by_trajectory(&self, observations: &[Observation]) -> bool {
        observations.len() >= 3
            && observations[observations.len() - 3..]
                .iter()
                .all(|o| o.output == observations[observations.len() - 1].output)
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

/// Chip's own completion, as `stack-agent` has it for change and verify work: the moment the
/// goal's predicate holds, the work completes, whatever the model would have said.
#[derive(Debug)]
struct CompleteWhen(Arc<dyn ObservationPredicate>);

impl LocalWorkPolicy for CompleteWhen {
    fn propose(&self, view: &WorkView<'_>) -> Option<WorkDecision> {
        self.0
            .satisfied_by_trajectory(view.observations)
            .then(|| WorkDecision::Complete {
                summary: "the goal's observations hold".into(),
            })
    }
}

async fn run_policy(
    spec: WorkSpec,
    world: &Arc<World>,
    policy: &dyn LocalWorkPolicy,
    decisions: Vec<WorkDecision>,
) -> Outcome {
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
    Outcome {
        report,
        model_calls: model.0.load(Ordering::SeqCst),
    }
}

fn goal_spec(guard: Option<StuckThresholds>, predicate: Arc<dyn ObservationPredicate>) -> WorkSpec {
    let mut s = spec(guard, 20, 20);
    s.required_observations.push(predicate);
    s
}

fn completed(report: &WorkReport) -> bool {
    matches!(report.outcome, WorkOutcome::Completed { .. })
}

#[tokio::test]
async fn a_goal_that_is_itself_a_repetition_completes_on_the_pair_the_guard_would_stop_at() {
    // "Confirm the reading is stable": three identical reads satisfy the goal, and three identical
    // pairs are the guard's threshold. Chip's own completion is available the moment the goal holds,
    // so the guard must not take the work away from it.
    for guard in [None, on()] {
        let world = World::new(Reads::Constant);
        let s = goal_spec(guard, Arc::new(StableThrice));
        let out = run_policy(
            s,
            &world,
            &CompleteWhen(Arc::new(StableThrice)),
            many(read("a"), 8),
        )
        .await;
        assert!(
            completed(&out.report),
            "guard {guard:?}: {:?}",
            out.report.outcome
        );
        assert_eq!(world.executions(), 3);
        assert_eq!(
            out.model_calls, 3,
            "the completion was Chip's: no fourth question"
        );
    }
}

#[tokio::test]
async fn a_goal_satisfied_before_the_threshold_completes() {
    let world = World::new(Reads::Sequence(&["pending", "pending", "ready"]));
    let s = goal_spec(on(), Arc::new(Ready));
    let out = run_policy(
        s,
        &world,
        &CompleteWhen(Arc::new(Ready)),
        many(read("a"), 8),
    )
    .await;
    assert!(completed(&out.report), "{:?}", out.report.outcome);
    assert_eq!(world.executions(), 3);
}

#[tokio::test]
async fn a_goal_that_was_never_satisfied_when_the_guard_fires_is_stuck() {
    let world = World::new(Reads::Sequence(&["pending"]));
    let s = goal_spec(on(), Arc::new(Ready));
    let out = run_policy(
        s,
        &world,
        &CompleteWhen(Arc::new(Ready)),
        many(read("a"), 8),
    )
    .await;
    let reason = stuck_reason(&out.report).unwrap_or_else(|| panic!("{:?}", out.report.outcome));
    assert!(
        !reason.contains("already"),
        "nothing was satisfied: {reason}"
    );
    assert_eq!(world.executions(), 3);
}

#[tokio::test]
async fn a_repeated_request_that_yields_new_required_evidence_is_not_a_loop() {
    // The same read, a different answer each time, the last one the one the goal needs.
    let world = World::new(Reads::Sequence(&["v1", "v2", "v3", "v4", "ready"]));
    let s = goal_spec(on(), Arc::new(Ready));
    let out = run_policy(
        s,
        &world,
        &CompleteWhen(Arc::new(Ready)),
        many(read("a"), 8),
    )
    .await;
    assert!(completed(&out.report), "{:?}", out.report.outcome);
    assert_eq!(world.executions(), 5);
}

#[tokio::test]
async fn a_satisfied_goal_the_runtime_cannot_complete_itself_is_still_stopped_and_says_so() {
    // The model, not Chip, completes this work (no local policy), and it keeps repeating the read
    // after the goal's observation was made. Completion needs a decision and none was made, so the
    // guard ends the work `Blocked`; it never completes it, and it says the goal's observation was
    // already there. The evaluation of the goal is kept in the events.
    let world = World::new(Reads::Sequence(&["ready"]));
    let s = goal_spec(on(), Arc::new(Ready));
    let out = run_policy(s, &world, &NoLocalPolicy, many(read("a"), 8)).await;
    let reason = stuck_reason(&out.report).unwrap_or_else(|| panic!("{:?}", out.report.outcome));
    assert!(reason.contains("already satisfied"), "{reason}");
    assert!(!completed(&out.report));
    let satisfied = out.report.events.iter().any(|e| {
        matches!(
            e,
            WorkEvent::GoalEvaluated {
                satisfied: true,
                remaining: 0,
                ..
            }
        )
    });
    assert!(satisfied, "the goal's evaluation is kept");
    // Without the guard the same work runs to its limit and is not completed either.
    let free = World::new(Reads::Sequence(&["ready"]));
    let base = run_policy(
        goal_spec(None, Arc::new(Ready)),
        &free,
        &NoLocalPolicy,
        many(read("a"), 30),
    )
    .await;
    assert!(
        matches!(base.report.outcome, WorkOutcome::LimitReached { .. }),
        "{:?}",
        base.report.outcome
    );
}

#[tokio::test]
async fn a_stop_still_comes_before_a_satisfied_goal_and_before_the_guard() {
    for cause in [StopCause::Cancelled, StopCause::Deadline] {
        let world = World::new(Reads::Constant);
        let mut s = goal_spec(on(), Arc::new(StableThrice)).with_restored(vec![
            restored(1, "a", "the same text"),
            restored(2, "a", "the same text"),
            restored(3, "a", "the same text"),
        ]);
        s.control = Some(Arc::new(Stop(cause)));
        let out = run_policy(
            s,
            &world,
            &CompleteWhen(Arc::new(StableThrice)),
            vec![read("a")],
        )
        .await;
        assert!(
            matches!(out.report.outcome, WorkOutcome::Cancelled { cause: c, .. } if c == cause),
            "{:?}",
            out.report.outcome
        );
        assert_eq!((world.executions(), out.model_calls), (0, 0));
    }
}

#[tokio::test]
async fn a_restored_trajectory_that_already_satisfies_the_goal_completes_with_or_without_the_guard()
{
    for guard in [None, on()] {
        let world = World::new(Reads::Constant);
        let s = goal_spec(guard, Arc::new(StableThrice)).with_restored(vec![
            restored(1, "a", "the same text"),
            restored(2, "a", "the same text"),
            restored(3, "a", "the same text"),
        ]);
        let out = run_policy(
            s,
            &world,
            &CompleteWhen(Arc::new(StableThrice)),
            vec![read("a")],
        )
        .await;
        assert!(
            completed(&out.report),
            "guard {guard:?}: {:?}",
            out.report.outcome
        );
        assert_eq!((world.executions(), out.model_calls), (0, 0));
        assert_eq!(out.report.observations.len(), 3);
    }
}

// ---------------------------------------------------------------------------------------------
// Boundaries, with the guard on and off
// ---------------------------------------------------------------------------------------------

fn read_n(n: i64) -> WorkDecision {
    WorkDecision::RequestCapability(
        CapabilityRequest::new(
            ExecutionId::new("x"),
            CapabilityId::new("world.read").unwrap(),
        )
        .with_input("path", InputValue::Integer(n)),
    )
}

/// What a script does under both settings: `(guard on, guard off)`, as (outcome label, executions).
async fn both(reads: Reads, script: Vec<WorkDecision>) -> [(String, usize); 2] {
    let mut out = Vec::new();
    for guard in [on(), None] {
        let world = World::new(reads);
        let r = run_spec(spec(guard, 20, 20), &world, script.clone()).await;
        let label = match &r.report.outcome {
            WorkOutcome::Completed { .. } => "completed".to_string(),
            WorkOutcome::Blocked { reason } if reason.starts_with(STUCK_REASON_PREFIX) => {
                "stuck".to_string()
            }
            WorkOutcome::LimitReached { .. } => "limit".to_string(),
            other => format!("{other:?}"),
        };
        out.push((label, world.executions()));
    }
    [out.remove(0), out.remove(0)]
}

fn ends(script: Vec<WorkDecision>, last: WorkDecision) -> Vec<WorkDecision> {
    let mut s = script;
    s.push(last);
    s
}

#[tokio::test]
async fn the_boundaries_with_the_guard_on_and_off() {
    let done = complete();
    fn c(s: &(String, usize)) -> (&str, usize) {
        (s.0.as_str(), s.1)
    }
    // Two identical pairs, then done: no trigger, either way.
    let r = both(Reads::Files, ends(many(read("a"), 2), done.clone())).await;
    assert_eq!((c(&r[0]), c(&r[1])), (("completed", 2), ("completed", 2)));
    // Three alternating pairs (A B A), then done: no trigger.
    let r = both(
        Reads::Files,
        ends(vec![read("a"), read("b"), read("a")], done.clone()),
    )
    .await;
    assert_eq!((c(&r[0]), c(&r[1])), (("completed", 3), ("completed", 3)));
    // Three identical pairs: the repeated threshold. On, it stops; off, it goes on to its end.
    let r = both(Reads::Files, ends(many(read("a"), 3), done.clone())).await;
    assert_eq!((c(&r[0]), c(&r[1])), (("stuck", 3), ("completed", 3)));
    let r = both(Reads::Files, ends(many(read("a"), 5), done.clone())).await;
    assert_eq!((c(&r[0]), c(&r[1])), (("stuck", 3), ("completed", 5)));
    // Four alternating pairs: the alternating threshold.
    let r = both(
        Reads::Files,
        ends(
            vec![read("a"), read("b"), read("a"), read("b")],
            done.clone(),
        ),
    )
    .await;
    assert_eq!((c(&r[0]), c(&r[1])), (("stuck", 4), ("completed", 4)));
    // The same capability, different typed inputs: text `1` and integer `1` are different requests.
    let r = both(
        Reads::Constant,
        ends(
            vec![read("1"), read_n(1), read("1"), read_n(1), read("2")],
            done.clone(),
        ),
    )
    .await;
    assert_eq!(
        (c(&r[0]), c(&r[1])),
        (("stuck", 4), ("completed", 5)),
        "alternating text and integer inputs ARE a cycle"
    );
    let r = both(
        Reads::Constant,
        ends(
            vec![read("1"), read_n(1), read("2"), read_n(2), read("3")],
            done.clone(),
        ),
    )
    .await;
    assert_eq!((c(&r[0]), c(&r[1])), (("completed", 5), ("completed", 5)));
    // The same request, a materially different observation each time.
    let r = both(Reads::Changing, ends(many(read("a"), 6), done.clone())).await;
    assert_eq!((c(&r[0]), c(&r[1])), (("completed", 6), ("completed", 6)));
}

#[tokio::test]
async fn polling_that_changes_goes_on_and_polling_that_does_not_is_stopped() {
    let done = complete();
    // Each poll shows something new, the last one the thing waited for.
    let r = both(
        Reads::Sequence(&["queued", "running 10%", "running 60%", "finished"]),
        ends(many(read("job"), 4), done.clone()),
    )
    .await;
    assert_eq!(r[0], ("completed".to_string(), 4));
    // Polling with an unchanged result: stopped on the third identical answer.
    let r = both(
        Reads::Sequence(&["queued"]),
        ends(many(read("job"), 8), done.clone()),
    )
    .await;
    assert_eq!(r[0], ("stuck".to_string(), 3));
    assert_eq!(
        r[1],
        ("completed".to_string(), 8),
        "off: runs to the end of the script"
    );
}

#[tokio::test]
async fn unchanged_polling_that_would_change_on_the_next_poll_is_a_known_false_positive() {
    // A characterization, not an endorsement. Three identical `queued` answers, then `finished`: the
    // guard compares what was observed, and cannot know the fourth poll would have differed. It stops
    // the work on the third. Without the guard the fourth poll runs and shows the change.
    let r = both(
        Reads::Sequence(&["queued", "queued", "queued", "finished"]),
        ends(many(read("job"), 4), complete()),
    )
    .await;
    assert_eq!(r[0], ("stuck".to_string(), 3));
    assert_eq!(r[1], ("completed".to_string(), 4));
    // Two `queued` answers are below the threshold, and are fine.
    let r = both(
        Reads::Sequence(&["queued", "queued", "finished"]),
        ends(many(read("job"), 3), complete()),
    )
    .await;
    assert_eq!(r[0], ("completed".to_string(), 3));
}

#[tokio::test]
async fn a_useful_write_followed_by_repeated_reads_is_stopped_after_the_third_identical_read() {
    let done = complete();
    // The first read after a write sees the write; the next two see nothing new.
    let script = ends(
        vec![write("a", "v1"), read("a"), read("a"), read("a"), read("a")],
        done.clone(),
    );
    let r = both(Reads::Files, script).await;
    assert_eq!(
        r[0],
        ("stuck".to_string(), 4),
        "write, then three identical reads"
    );
    assert_eq!(r[1], ("completed".to_string(), 5));
    // A write between the reads changes what the next read sees, so the reads are not a repeat.
    let script = ends(
        vec![
            write("a", "v1"),
            read("a"),
            write("a", "v2"),
            read("a"),
            write("a", "v3"),
            read("a"),
        ],
        done,
    );
    let r = both(Reads::Files, script).await;
    assert_eq!(r[0], ("completed".to_string(), 6));
}
