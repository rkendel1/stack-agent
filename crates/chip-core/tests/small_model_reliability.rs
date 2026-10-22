//! What a small local model needs from the loop to finish work it can do, and nothing that weakens
//! what the loop checks:
//!
//! * the request puts what never changes first (the goal and the reply contract), then the
//!   observations in the order they were made, then where the work stands, so a provider's prefix
//!   cache can reuse every earlier request in the next one;
//! * the model is told, from Chip's own check of the observations, when the goal's requirements
//!   are all met, and is not told that an observation "did not satisfy the goal" when nothing was
//!   outstanding for it to satisfy;
//! * a successful request that returns what the same request already returned is noted before the
//!   stuck guard would stop the work, and the guard still stops it.
//!
//! The world is two capabilities with real outputs; only the model is scripted.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_core::{
    Agent, AnswerPredicate, CapabilityAvailability, CapabilityDescriptor, CapabilityError,
    CapabilityId, CapabilityInput, CapabilityProvider, ExecutionError, ExecutionObserver,
    ExecutionRequest, ExecutionResult, Executor, InputValue, ModelDecisionBoundary, NEXT_STEP,
    NoLocalPolicy, Observation, StuckThresholds, TestLocalReasoner, WorkEvent, WorkGoal, WorkId,
    WorkLimits, WorkOutcome, WorkReport, WorkSpec, audit_safety,
};
use chip_inference::{FxError, MessageRole, ModelProvider, ModelRequest, ModelResponse, Usage};

struct World {
    executions: AtomicUsize,
}

fn id(s: &str) -> CapabilityId {
    CapabilityId::new(s).unwrap()
}

#[async_trait::async_trait]
impl CapabilityProvider for World {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        let mut read = CapabilityDescriptor::new(id("file.read"), "file.read", "Read a file.")
            .without_evidence_reuse();
        read.inputs.push(CapabilityInput {
            name: "path".into(),
            description: "path".into(),
            required: true,
        });
        Ok(vec![read])
    }
    async fn availability(&self, _: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

#[async_trait::async_trait]
impl Executor for World {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        let path = match r.inputs.get("path") {
            Some(InputValue::Text(t)) => t.clone(),
            _ => String::new(),
        };
        Ok(ExecutionResult::success(
            r.id,
            format!("contents of {path}"),
        ))
    }
}

struct Script {
    replies: Mutex<Vec<String>>,
    seen: Mutex<Vec<ModelRequest>>,
}

#[async_trait::async_trait]
impl ModelProvider for Script {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, FxError> {
        self.seen.lock().unwrap().push(request);
        let mut replies = self.replies.lock().unwrap();
        if replies.is_empty() {
            return Err(FxError::Provider("unscripted model call".into()));
        }
        Ok(ModelResponse::new("r", replies.remove(0), Usage::new(5, 2)))
    }
}

fn read(path: &str) -> String {
    format!(
        r#"{{"decision":"request_capability","capability":"file.read","inputs":{{"path":"{path}"}}}}"#
    )
}

fn done() -> String {
    r#"{"decision":"complete","summary":"a.txt holds what was asked"}"#.to_string()
}

struct Run {
    report: WorkReport,
    spec: WorkSpec,
    seen: Vec<ModelRequest>,
    executions: usize,
}

async fn run(replies: Vec<String>, spec: WorkSpec) -> Run {
    let world = Arc::new(World {
        executions: AtomicUsize::new(0),
    });
    let model = Arc::new(Script {
        replies: Mutex::new(replies),
        seen: Mutex::new(Vec::new()),
    });
    let report = Agent::new(model.clone())
        .with_capabilities(world.clone())
        .with_executor(world.clone())
        .with_observer(Arc::new(ExecutionObserver))
        .with_local_reasoner(Arc::new(TestLocalReasoner::default()))
        .run_work(&spec, &NoLocalPolicy, &ModelDecisionBoundary)
        .await;
    let seen = model.seen.lock().unwrap().clone();
    Run {
        report,
        spec,
        seen,
        executions: world.executions.load(Ordering::SeqCst),
    }
}

fn spec() -> WorkSpec {
    WorkSpec::new(WorkId::new("w"), WorkGoal::new("Find what a.txt holds.")).with_limits(
        WorkLimits {
            max_turns: 10,
            max_executions: 10,
        },
    )
}

/// The request as a provider that joins consecutive system messages (the OpenAI-compatible and
/// Anthropic wires both do) serializes it: the text its prefix cache is keyed on.
fn serialized(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .map(|m| format!("<{:?}>{}", m.role, m.content))
        .collect()
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

fn turn_text(request: &ModelRequest) -> &str {
    &request.messages.last().unwrap().content
}

#[tokio::test]
async fn what_never_changes_is_sent_first_and_each_request_extends_the_one_before() {
    let r = run(vec![read("a.txt"), read("b.txt"), done()], spec()).await;
    assert!(matches!(r.report.outcome, WorkOutcome::Completed { .. }));
    assert_eq!(r.seen.len(), 3);
    for (n, request) in r.seen.iter().enumerate() {
        // The instructions, one message per observation made so far, then the turn.
        assert_eq!(request.messages.len(), n + 2, "request {}", n + 1);
        let first = &request.messages[0];
        assert_eq!(first.role, MessageRole::System);
        assert!(
            first
                .content
                .starts_with("Goal: Find what a.txt holds.\nQuestion: ")
        );
        assert!(first.content.contains("Available capabilities: file.read"));
        // The same text on every turn: nothing about the turn is in it.
        assert_eq!(first.content, r.seen[0].messages[0].content);
        for observation in &request.messages[1..request.messages.len() - 1] {
            assert_eq!(observation.role, MessageRole::System);
            assert!(observation.content.starts_with("Observation:\n"));
        }
        let last = request.messages.last().unwrap();
        assert_eq!(last.role, MessageRole::User);
        assert!(
            last.content
                .starts_with(&format!("State: turn {} of 10", n + 1))
        );
        assert!(last.content.ends_with(NEXT_STEP));
    }
    // Everything before the turn is carried over unchanged into the next request.
    for pair in r.seen.windows(2) {
        let (before, after) = (serialized(&pair[0]), serialized(&pair[1]));
        let stable = before.len() - turn_text(&pair[0]).len() - "<User>".len();
        assert!(
            common_prefix(&before, &after) >= stable,
            "the next request starts with everything but the last turn of this one"
        );
    }
    // Measured as sent.
    let sent: usize = r
        .seen
        .iter()
        .flat_map(|q| q.messages.iter())
        .map(|m| m.content.len())
        .sum();
    assert_eq!(r.report.summary.context_bytes, sent);
    assert!(audit_safety(&r.report, &r.spec, &[id("file.read")]).is_clean());
}

#[tokio::test]
async fn the_model_is_told_when_every_requirement_is_met_and_not_told_otherwise() {
    let spec = spec().with_required_output("contents of a.txt");
    let r = run(
        vec![read("b.txt"), read("a.txt"), read("c.txt"), done()],
        spec,
    )
    .await;
    assert!(
        matches!(r.report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        r.report.outcome
    );
    let progress: Vec<Option<&str>> = r
        .seen
        .iter()
        .map(|q| turn_text(q).lines().find(|l| l.contains("goal progress: ")))
        .collect();
    // Nothing is said while a requirement is outstanding; from the turn after it is met, it is.
    assert_eq!(progress[0], None);
    assert_eq!(progress[1], None);
    for met in progress[2..]
        .iter()
        .map(|p| p.expect("told once every requirement is met"))
    {
        assert!(
            met.contains("every required output has been observed; an answer the observations support can complete the work"),
            "{met}"
        );
    }
    let last = turn_text(r.seen.last().unwrap());
    // b.txt ran while a.txt was still outstanding: it is ruled out for not producing it.
    assert!(last.contains(
        r#"capability file.read (path="b.txt"): executed, but its observation did not satisfy the goal"#
    ));
    // c.txt ran after the goal was met: there was nothing left for it to satisfy.
    assert!(!last.contains(r#"(path="c.txt"): executed, but"#), "{last}");

    // A work with no requirements has nothing to report progress on.
    let r = run(vec![read("a.txt"), done()], self::spec()).await;
    assert!(
        r.seen
            .iter()
            .all(|q| !turn_text(q).contains("goal progress"))
    );
}

/// The rejected decisions in a work: (turn, consecutive, recovering).
fn rejections(report: &WorkReport) -> Vec<(usize, usize, bool)> {
    report
        .events
        .iter()
        .filter_map(|e| match e {
            WorkEvent::DecisionRejected {
                turn,
                consecutive,
                recovering,
                ..
            } => Some((*turn, *consecutive, *recovering)),
            _ => None,
        })
        .collect()
}

fn completed(report: &WorkReport) -> bool {
    matches!(report.outcome, WorkOutcome::Completed { .. })
}

#[tokio::test]
async fn an_early_completion_claim_is_refused_and_the_work_goes_on_to_the_evidence() {
    let spec = spec().with_required_output("contents of a.txt");
    // Claims done before reading anything, then after reading the wrong file, then reads the
    // right one and claims done again.
    let r = run(
        vec![done(), read("b.txt"), done(), read("a.txt"), done()],
        spec,
    )
    .await;
    assert!(completed(&r.report), "{:?}", r.report.outcome);
    // Both early claims were refused; the request between them reset the count.
    assert_eq!(rejections(&r.report), vec![(0, 1, true), (2, 1, true)]);
    // The model was told what the goal still lacks, first, at the next escalation.
    let told = turn_text(&r.seen[1]);
    assert!(
        told.contains("turn 1: your complete decision was refused and nothing was completed: no authoritative observation satisfies the goal yet"),
        "{told}"
    );
    assert!(told.contains("rejected decision 1 in a row; after 2 the next one ends the work"));
    // A completion counts only when the observation it rests on exists.
    assert_eq!(r.executions, 2);
    assert!(audit_safety(&r.report, &r.spec, &[id("file.read")]).is_clean());
}

#[tokio::test]
async fn a_completion_claim_that_keeps_being_refused_ends_at_the_bound_and_never_completes() {
    let spec = spec().with_required_output("contents of a.txt");
    let r = run(vec![done(), done(), done(), read("a.txt")], spec).await;
    assert!(
        matches!(&r.report.outcome, WorkOutcome::Blocked { reason }
            if reason.starts_with("completion refused: no authoritative observation satisfies the goal")
                && reason.ends_with("(3 rejected decisions in a row; the limit is 2)")),
        "{:?}",
        r.report.outcome
    );
    assert_eq!(
        rejections(&r.report),
        vec![(0, 1, true), (1, 2, true), (2, 3, false)]
    );
    assert_eq!(r.seen.len(), 3, "not asked again past the bound");
    assert_eq!(r.executions, 0);
    // The turns spent recovering are spent: three of the ten.
    assert_eq!(r.report.summary.turns, 3);
    assert!(audit_safety(&r.report, &r.spec, &[id("file.read")]).is_clean());
}

#[derive(Debug)]
struct CitesAFile;

impl AnswerPredicate for CitesAFile {
    fn describe(&self) -> String {
        "the answer names a file that was read".into()
    }
    fn accepts(&self, answer: &str, _observations: &[Observation]) -> bool {
        answer.contains(".txt")
    }
}

#[tokio::test]
async fn an_answer_the_observations_do_not_support_is_refused_with_what_an_answer_must_do() {
    let spec = spec()
        .with_required_output("contents of a.txt")
        .with_required_answer(Arc::new(CitesAFile));
    let vague = r#"{"decision":"complete","summary":"It holds what was asked."}"#.to_string();
    let r = run(vec![read("a.txt"), vague, done()], spec).await;
    assert!(completed(&r.report), "{:?}", r.report.outcome);
    assert_eq!(rejections(&r.report), vec![(1, 1, true)]);
    let told = turn_text(&r.seen[2]);
    assert!(
        told.contains("your complete decision was refused and nothing was completed: its answer is not supported by the observations; an accepted answer must meet: the answer names a file that was read"),
        "{told}"
    );
    match &r.report.outcome {
        WorkOutcome::Completed { summary } => assert!(summary.contains("a.txt")),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_request_with_inputs_its_capability_refuses_is_not_executed_and_the_work_goes_on() {
    let undeclared = r#"{"decision":"request_capability","capability":"file.read","inputs":{"path":"a.txt","mode":"rw"}}"#;
    let missing = r#"{"decision":"request_capability","capability":"file.read","inputs":{}}"#;
    let r = run(
        vec![undeclared.into(), missing.into(), read("a.txt"), done()],
        spec(),
    )
    .await;
    assert!(completed(&r.report), "{:?}", r.report.outcome);
    // Two refused requests in a row, neither executed; the valid one ran once.
    assert_eq!(rejections(&r.report), vec![(0, 1, true), (1, 2, true)]);
    assert_eq!(r.executions, 1);
    let told = turn_text(&r.seen[1]);
    assert!(
        told.contains("your request for file.read was rejected and nothing was executed: invalid capability input"),
        "{told}"
    );
    assert!(audit_safety(&r.report, &r.spec, &[id("file.read")]).is_clean());

    // Past the bound, the work ends as a refused request always ended it: blocked, unexecuted.
    let r = run(vec![undeclared.into(); 4], spec()).await;
    assert!(
        matches!(&r.report.outcome, WorkOutcome::Blocked { reason } if reason.starts_with("invalid capability input")),
        "{:?}",
        r.report.outcome
    );
    assert_eq!(r.executions, 0);
    assert_eq!(r.seen.len(), 3);
}

#[tokio::test]
async fn an_unknown_capability_is_rejected_with_the_ones_the_work_offers_and_never_executed() {
    let unknown =
        r#"{"decision":"request_capability","capability":"shell.exec","inputs":{"cmd":"ls"}}"#;
    let r = run(vec![unknown.into(), read("a.txt"), done()], spec()).await;
    assert!(completed(&r.report), "{:?}", r.report.outcome);
    assert_eq!(rejections(&r.report), vec![(0, 1, true)]);
    assert_eq!(r.executions, 1);
    let told = turn_text(&r.seen[1]);
    assert!(told.contains("unknown capability: shell.exec"), "{told}");
    assert!(
        told.contains("the capabilities this work may request are file.read"),
        "{told}"
    );
    // Its identity never reached anything: no request of it was recorded.
    assert!(!r.report.events.iter().any(|e| matches!(
        e,
        WorkEvent::CapabilityRequested { capability, .. } if capability.as_str() == "shell.exec"
    )));
    assert!(audit_safety(&r.report, &r.spec, &[id("file.read")]).is_clean());
}

#[tokio::test]
async fn a_repeated_identical_result_is_noted_before_the_stuck_guard_and_the_guard_still_stops() {
    let spec = spec().with_stuck_guard(StuckThresholds::default());
    let r = run(
        vec![read("a.txt"), read("a.txt"), read("a.txt"), read("a.txt")],
        spec,
    )
    .await;
    // The guard is unchanged: three identical requests with identical results stop the work.
    assert!(
        matches!(&r.report.outcome, WorkOutcome::Blocked { reason } if reason.starts_with("stuck: ")),
        "{:?}",
        r.report.outcome
    );
    assert_eq!(r.executions, 3);
    assert_eq!(r.seen.len(), 3);
    // The first request carries no note; the third, made after the second identical result, does.
    assert!(!turn_text(&r.seen[1]).contains("returned the identical observation"));
    let third = turn_text(&r.seen[2]);
    assert!(
        third.contains(
            r#"capability file.read (path="a.txt"): returned the identical observation 2 times. Sending this exact request again is not expected to add anything"#
        ),
        "{third}"
    );
    // Said once, kept current.
    assert_eq!(
        third.matches("returned the identical observation").count(),
        1
    );
    assert!(audit_safety(&r.report, &r.spec, &[id("file.read")]).is_clean());
}

#[tokio::test]
async fn a_different_result_from_the_same_request_is_not_called_a_repeat() {
    let r = run(
        vec![read("a.txt"), read("b.txt"), read("a.txt"), done()],
        spec(),
    )
    .await;
    assert!(matches!(r.report.outcome, WorkOutcome::Completed { .. }));
    // a.txt twice, with the same result: noted. b.txt once: not.
    let last = turn_text(r.seen.last().unwrap());
    assert!(last.contains(r#"(path="a.txt"): returned the identical observation 2 times"#));
    assert!(!last.contains(r#"(path="b.txt"): returned"#));
}
