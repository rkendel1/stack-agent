//! The run's owner can stop a work (a cancellation or the whole-work deadline) and see its events
//! as they are recorded, through `WorkControl`. The loop checks before every model call and before
//! every execution, so neither can be extended by starting another one. Cooperative: an execution
//! already in flight finishes, is observed, and nothing it did is undone.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityProvider, CapabilityRequest, DecisionError, ExecutionError, ExecutionEvent,
    ExecutionId, ExecutionObserver, ExecutionRequest, ExecutionResult, ExecutionStatus, Executor,
    NoLocalPolicy, StopCause, TerminalState, WorkControl, WorkDecision, WorkDecisionBoundary,
    WorkEvent, WorkGoal, WorkId, WorkLimits, WorkOutcome, WorkReport, WorkSpec,
};
use chip_inference::{
    Delivery, FxError, InferenceFailure, ModelProvider, ModelRequest, ModelResponse, Usage,
};

/// The owner: a stop flag with a cause, and every event it was told about.
#[derive(Default)]
struct Owner {
    stop: Mutex<Option<StopCause>>,
    seen: Mutex<Vec<WorkEvent>>,
}

impl Owner {
    fn stop(&self, cause: StopCause) {
        *self.stop.lock().unwrap() = Some(cause);
    }
}

impl WorkControl for Owner {
    fn stop_requested(&self) -> Option<StopCause> {
        *self.stop.lock().unwrap()
    }
    fn on_event(&self, event: &WorkEvent) {
        self.seen.lock().unwrap().push(event.clone());
    }
}

/// A model that, when `stop_on_call` is set, asks its owner to stop while it is answering, and
/// can fail the call it is answering (as FX does for an abandoned attempt).
struct Model {
    calls: AtomicUsize,
    owner: Arc<Owner>,
    stop_on_call: Option<StopCause>,
    fail_after_stop: bool,
}

#[async_trait::async_trait]
impl ModelProvider for Model {
    async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(cause) = self.stop_on_call {
            self.owner.stop(cause);
            if self.fail_after_stop {
                return Err(FxError::Inference(Box::new(InferenceFailure {
                    kind: "cancelled".into(),
                    delivery: Delivery::PossiblySent,
                    status: None,
                    retry_after_ms: None,
                    detail: None,
                    partial_output: None,
                    remote_cancellation: Some(chip_inference::RemoteCancellation::Unconfirmed),
                })));
            }
        }
        Ok(ModelResponse::new("r", "reply", Usage::new(1, 1)))
    }
}

/// An executor that can ask the owner to stop while it runs, and still completes.
struct Exec {
    calls: AtomicUsize,
    owner: Arc<Owner>,
    stop_while_running: AtomicBool,
}

#[async_trait::async_trait]
impl Executor for Exec {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.stop_while_running.load(Ordering::SeqCst) {
            self.owner.stop(StopCause::Cancelled);
        }
        Ok(ExecutionResult::success(r.id, "operation performed"))
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

/// Every model reply asks to run `op.perform`.
struct AlwaysRun;

impl WorkDecisionBoundary for AlwaysRun {
    fn interpret(
        &self,
        _r: &ModelResponse,
        _c: &[Capability],
    ) -> Result<WorkDecision, DecisionError> {
        Ok(WorkDecision::RequestCapability(CapabilityRequest::new(
            ExecutionId::new("x"),
            CapabilityId::new("op.perform").unwrap(),
        )))
    }
}

struct Fixture {
    owner: Arc<Owner>,
    model: Arc<Model>,
    exec: Arc<Exec>,
}

fn fixture(stop_on_call: Option<StopCause>, fail_after_stop: bool) -> Fixture {
    let owner = Arc::new(Owner::default());
    Fixture {
        model: Arc::new(Model {
            calls: AtomicUsize::new(0),
            owner: owner.clone(),
            stop_on_call,
            fail_after_stop,
        }),
        exec: Arc::new(Exec {
            calls: AtomicUsize::new(0),
            owner: owner.clone(),
            stop_while_running: AtomicBool::new(false),
        }),
        owner,
    }
}

async fn run(f: &Fixture, controlled: bool) -> WorkReport {
    let agent = Agent::new(f.model.clone())
        .with_capabilities(Arc::new(Caps))
        .with_executor(f.exec.clone())
        .with_observer(Arc::new(ExecutionObserver));
    let mut spec =
        WorkSpec::new(WorkId::new("w"), WorkGoal::new("do the thing")).with_limits(WorkLimits {
            max_turns: 4,
            max_executions: 4,
        });
    if controlled {
        spec = spec.with_control(f.owner.clone());
    }
    agent.run_work(&spec, &NoLocalPolicy, &AlwaysRun).await
}

fn executions_started(report: &WorkReport) -> usize {
    report
        .events
        .iter()
        .filter(|e| {
            matches!(
                e,
                WorkEvent::Execution(ExecutionEvent::ExecutionStarted { .. })
            )
        })
        .count()
}

#[tokio::test]
async fn a_stop_requested_before_the_work_starts_calls_nothing_and_runs_nothing() {
    let f = fixture(None, false);
    f.owner.stop(StopCause::Cancelled);
    let report = run(&f, true).await;
    assert!(matches!(
        report.outcome,
        WorkOutcome::Cancelled {
            cause: StopCause::Cancelled,
            delivery: None,
            ..
        }
    ));
    assert_eq!(report.summary.terminal_state, TerminalState::Cancelled);
    assert_eq!(report.summary.terminal_state.name(), "cancelled");
    assert_eq!(f.model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.exec.calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        report.events.last(),
        Some(WorkEvent::WorkCancelled { .. })
    ));
}

#[tokio::test]
async fn a_stop_requested_while_the_model_answers_starts_no_execution() {
    // The model answers with a capability request, but the stop arrived while it was answering.
    let f = fixture(Some(StopCause::Cancelled), false);
    let report = run(&f, true).await;
    assert!(
        matches!(report.outcome, WorkOutcome::Cancelled { .. }),
        "{:?}",
        report.outcome
    );
    assert_eq!(f.model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        f.exec.calls.load(Ordering::SeqCst),
        0,
        "the requested execution was not started after the stop"
    );
    assert_eq!(executions_started(&report), 0);
}

#[tokio::test]
async fn the_deadline_cannot_be_extended_by_starting_another_call() {
    let f = fixture(Some(StopCause::Deadline), false);
    let report = run(&f, true).await;
    assert!(matches!(
        report.outcome,
        WorkOutcome::Cancelled {
            cause: StopCause::Deadline,
            ..
        }
    ));
    assert!(matches!(
        report.events.last(),
        Some(WorkEvent::WorkCancelled {
            cause: StopCause::Deadline,
            ..
        })
    ));
    assert_eq!(
        f.model.calls.load(Ordering::SeqCst),
        1,
        "no second model call"
    );
    assert_eq!(f.exec.calls.load(Ordering::SeqCst), 0, "no execution");
}

#[tokio::test]
async fn an_execution_in_flight_finishes_and_is_observed_and_nothing_more_starts() {
    let f = fixture(None, false);
    f.exec.stop_while_running.store(true, Ordering::SeqCst);
    let report = run(&f, true).await;
    assert!(matches!(report.outcome, WorkOutcome::Cancelled { .. }));
    // The execution the stop arrived during is not interrupted, and its effect is reported, not
    // undone: it was started, completed and observed.
    assert_eq!(f.exec.calls.load(Ordering::SeqCst), 1);
    assert_eq!(executions_started(&report), 1);
    assert_eq!(report.summary.observations, 1);
    assert!(report.events.iter().any(|e| matches!(
        e,
        WorkEvent::Execution(ExecutionEvent::ExecutionCompleted { .. })
    )));
    assert_eq!(
        f.model.calls.load(Ordering::SeqCst),
        1,
        "no model call after the stop"
    );
}

#[tokio::test]
async fn a_model_call_abandoned_by_the_stop_keeps_its_delivery_certainty() {
    let f = fixture(Some(StopCause::Cancelled), true);
    let report = run(&f, true).await;
    match &report.outcome {
        WorkOutcome::Cancelled {
            cause,
            delivery,
            remote,
            ..
        } => {
            assert_eq!(*cause, StopCause::Cancelled);
            assert_eq!(
                *remote,
                Some(chip_inference::RemoteCancellation::Unconfirmed),
                "the remote side's stop is reported as the executor knew it, not assumed"
            );
            assert_eq!(
                *delivery,
                Some(Delivery::PossiblySent),
                "never reported as unsent once the call was made"
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(f.model.calls.load(Ordering::SeqCst), 1, "not retried");
}

#[tokio::test]
async fn without_a_stop_a_model_failure_is_still_a_failure() {
    // The same failing call with no owner: nothing reinterprets it as a cancellation.
    let f = fixture(Some(StopCause::Cancelled), true);
    let report = run(&f, false).await;
    assert!(
        matches!(report.outcome, WorkOutcome::Failed { .. }),
        "{:?}",
        report.outcome
    );
}

#[tokio::test]
async fn the_owner_sees_every_event_in_order_as_it_is_recorded() {
    let f = fixture(None, false);
    let report = run(&f, true).await;
    // Uncontrolled stop never came, so the work ran to its turn limit.
    assert_eq!(report.summary.terminal_state, TerminalState::LimitReached);
    assert_eq!(*f.owner.seen.lock().unwrap(), report.events);
    assert!(matches!(
        f.owner.seen.lock().unwrap().first(),
        Some(WorkEvent::WorkStarted { .. })
    ));
}
