//! What the model is told when the same request keeps failing, and that the stuck guard is
//! unchanged by it.
//!
//! A scripted model, a capability that always fails, the real work loop. The feedback is context
//! only: the recorded observations of identical requests stay identical (the guard compares
//! those), the guard fires exactly where it did, and it still stops the work without another
//! model call or execution.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, CapabilityRequest, DecisionError, ExecutionError,
    ExecutionId, ExecutionObserver, ExecutionRequest, ExecutionResult, ExecutionStatus, Executor,
    InputValue, NoLocalPolicy, STUCK_REASON_PREFIX, StuckThresholds, WorkDecision,
    WorkDecisionBoundary, WorkEvent, WorkGoal, WorkId, WorkLimits, WorkOutcome, WorkReport,
    WorkSpec,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};

/// Keeps everything the model was sent, per call.
#[derive(Default)]
struct Model {
    sent: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ModelProvider for Model {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, FxError> {
        self.sent.lock().unwrap().push(
            request
                .messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        );
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

struct Caps;

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        let mut r = CapabilityDescriptor::new(CapabilityId::new("world.read")?, "Read", "reads")
            .without_evidence_reuse();
        r.inputs.push(CapabilityInput {
            name: "path".into(),
            description: "path".into(),
            required: true,
        });
        Ok(vec![r])
    }
    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

/// Every read of `missing*` fails with the same text; anything else succeeds.
struct World {
    ran: AtomicUsize,
}

#[async_trait::async_trait]
impl Executor for World {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let n = self.ran.fetch_add(1, Ordering::SeqCst);
        let path = match r.inputs.get("path") {
            Some(InputValue::Text(t)) => t.clone(),
            _ => String::new(),
        };
        let receipt = format!("sha256:receipt-{n}");
        Ok(if path.starts_with("missing") {
            ExecutionResult::failure(
                r.id,
                format!("{{\"error\":\"not_found\",\"path\":\"{path}\",\"nearest\":\".\"}}"),
            )
        } else {
            ExecutionResult::success(r.id, "fine")
        }
        .with_receipt_id(receipt))
    }
}

struct Run {
    report: WorkReport,
    model: Arc<Model>,
    executions: usize,
}

async fn run(decisions: Vec<WorkDecision>, guard: Option<StuckThresholds>) -> Run {
    let model = Arc::new(Model::default());
    let world = Arc::new(World {
        ran: AtomicUsize::new(0),
    });
    let mut spec = WorkSpec::new(WorkId::new("w"), WorkGoal::new("look into the files"));
    spec.limits = WorkLimits {
        max_turns: 12,
        max_executions: 12,
    };
    spec.stuck_guard = guard;
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
    Run {
        report,
        model,
        executions: world.ran.load(Ordering::SeqCst),
    }
}

fn on() -> Option<StuckThresholds> {
    Some(StuckThresholds::default())
}

fn stuck_reason(report: &WorkReport) -> Option<&str> {
    match &report.outcome {
        WorkOutcome::Blocked { reason } if reason.starts_with(STUCK_REASON_PREFIX) => {
            Some(reason.as_str())
        }
        _ => None,
    }
}

fn sent(run: &Run, call: usize) -> String {
    run.model.sent.lock().unwrap()[call].clone()
}

const REPEAT_NOTE: &str = "times with the identical result";

#[tokio::test]
async fn the_first_failure_is_noted_as_before_and_the_second_explains_the_repeat() {
    let r = run(
        vec![read("missing.rs"), read("missing.rs"), read("fine.rs")],
        None,
    )
    .await;
    // Call 0 has no observations; call 1 follows the first failure; call 2 follows the second.
    assert!(!sent(&r, 1).contains(REPEAT_NOTE), "{}", sent(&r, 1));
    assert!(
        sent(&r, 1).contains("its execution failed"),
        "the first failure is noted: {}",
        sent(&r, 1)
    );
    let second = sent(&r, 2);
    assert!(
        second.contains("failed 2 times with the identical result"),
        "{second}"
    );
    assert!(
        second.contains("not_found"),
        "it says what the observation said: {second}"
    );
    assert!(second.contains("cannot change that result"), "{second}");
    assert!(
        second.contains("change the request"),
        "it suggests another action: {second}"
    );
    // One note per invocation, not one per failure.
    assert_eq!(
        second.matches("its execution failed").count(),
        1,
        "{second}"
    );
}

#[tokio::test]
async fn a_different_failing_request_is_a_different_note() {
    let r = run(
        vec![
            read("missing_a.rs"),
            read("missing_b.rs"),
            read("missing_a.rs"),
            read("fine.rs"),
        ],
        None,
    )
    .await;
    let last = sent(&r, 3);
    // a failed twice with the same result, b once: two notes, only a's explains a repeat.
    assert_eq!(last.matches("its execution failed").count(), 2, "{last}");
    assert_eq!(last.matches(REPEAT_NOTE).count(), 1, "{last}");
}

#[tokio::test]
async fn the_guard_fires_at_the_same_third_identical_failure_and_stops_the_work() {
    let r = run(vec![read("missing.rs"); 8], on()).await;
    let reason = stuck_reason(&r.report).unwrap_or_else(|| panic!("{:?}", r.report.outcome));
    // Unchanged: three identical requests, three executions, then it stops with no fourth.
    assert_eq!(r.executions, 3);
    assert_eq!(
        r.model.sent.lock().unwrap().len(),
        3,
        "the model is not asked after the stop"
    );
    assert!(reason.contains("3 times in a row"), "{reason}");
    assert!(
        reason.contains("no further capability was executed"),
        "{reason}"
    );
}

#[tokio::test]
async fn the_feedback_does_not_change_what_the_guard_compares() {
    let r = run(vec![read("missing.rs"); 8], on()).await;
    let failures: Vec<_> = r
        .report
        .observations
        .iter()
        .filter(|o| o.status == ExecutionStatus::Failure)
        .collect();
    assert_eq!(failures.len(), 3);
    // The recorded observations are byte-identical: no repeat count or note leaked into them.
    assert!(
        failures.windows(2).all(|w| w[0].output == w[1].output),
        "{failures:?}"
    );
    for o in &failures {
        assert!(!o.output.as_deref().unwrap().contains(REPEAT_NOTE));
    }
}

#[tokio::test]
async fn the_stop_says_how_to_recover_without_quoting_outputs_or_inputs() {
    let failing = run(vec![read("missing.rs"); 8], on()).await;
    let reason = stuck_reason(&failing.report).unwrap();
    assert!(
        reason.contains("Recovery: every one of those requests failed"),
        "{reason}"
    );
    assert!(reason.contains("a different request is needed"), "{reason}");
    assert!(
        reason.contains("did not retry, change a request or ask the model again"),
        "{reason}"
    );
    // The reason is recorded durably, where observations are kept only as sizes and hashes.
    assert!(
        !reason.contains("not_found") && !reason.contains("missing.rs"),
        "{reason}"
    );

    let succeeding = run(vec![read("fine.rs"); 8], on()).await;
    let reason = stuck_reason(&succeeding.report).unwrap();
    assert!(
        reason.contains("Recovery: every one of those requests succeeded"),
        "{reason}"
    );
}

#[tokio::test]
async fn the_loop_cannot_continue_past_the_guard_whatever_the_model_says() {
    // Even a script that would ask forever ends at the third identical failure.
    let r = run(vec![read("missing.rs"); 50], on()).await;
    assert!(stuck_reason(&r.report).is_some());
    assert_eq!(r.executions, 3);
    let starts = r
        .report
        .events
        .iter()
        .filter(|e| matches!(e, WorkEvent::DecisionStarted { .. }))
        .count();
    assert_eq!(starts, 3);
}
