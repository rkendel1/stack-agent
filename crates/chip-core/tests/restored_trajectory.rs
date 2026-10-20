//! A work that starts from what earlier runs of it recorded (`WorkSpec::restored`): the restored
//! observations join the trajectory before the first turn, in their order, exactly as the loop
//! itself would have recorded them, and none is executed again. A restored trajectory that cannot
//! be joined whole ends the work instead of being used in part.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityProvider, CapabilityRequest, DecisionError, ExecutionError, ExecutionId,
    ExecutionObserver, ExecutionRequest, ExecutionResult, Executor, InputValue, LocalWorkPolicy,
    Observation, ObservationKind, ObservationPredicate, RestoredObservation, WorkDecision,
    WorkDecisionBoundary, WorkEvent, WorkGoal, WorkId, WorkOutcome, WorkReport, WorkSpec, WorkView,
    audit_safety, verify_trajectory,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};

/// Answers every call; what the answer means is the script's.
struct AnyModel;

#[async_trait::async_trait]
impl ModelProvider for AnyModel {
    async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
        Ok(ModelResponse::new("r", "reply", Usage::new(1, 1)))
    }
}

/// Plays back one decision per model call. Chip assigns each requested execution its identity.
struct Script(Mutex<VecDeque<WorkDecision>>);

impl WorkDecisionBoundary for Script {
    fn interpret(
        &self,
        _r: &ModelResponse,
        _c: &[Capability],
    ) -> Result<WorkDecision, DecisionError> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| DecisionError::InvalidDecision("the script ran out".into()))
    }
}

struct Caps;

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(["step.read", "step.test"]
            .into_iter()
            .map(|id| {
                CapabilityDescriptor::new(CapabilityId::new(id).unwrap(), id, id)
                    .without_evidence_reuse()
            })
            .collect())
    }
    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

/// Keeps what it really executed. `step.test` passes; anything else just ran.
#[derive(Default)]
struct Exec {
    ran: Mutex<Vec<String>>,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Executor for Exec {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.ran.lock().unwrap().push(r.id.0.clone());
        let output = if r.intent == "step.test" {
            "tests passed".to_string()
        } else {
            format!("{} ran", r.intent)
        };
        Ok(ExecutionResult::success(r.id, output))
    }
}

/// The goal is met once an observation says the tests passed.
#[derive(Debug)]
struct TestsPassed;

impl ObservationPredicate for TestsPassed {
    fn describe(&self) -> String {
        "the tests passed".into()
    }
    fn satisfied_by(&self, o: &Observation) -> bool {
        o.output.as_deref() == Some("tests passed")
    }
}

/// Completes locally once the goal is met; otherwise asks the model.
struct CompleteWhenPassed;

impl LocalWorkPolicy for CompleteWhenPassed {
    fn propose(&self, view: &WorkView<'_>) -> Option<WorkDecision> {
        view.observations
            .iter()
            .any(|o| TestsPassed.satisfied_by(o))
            .then(|| WorkDecision::Complete {
                summary: "the tests pass".into(),
            })
    }
}

fn request(capability: &str) -> WorkDecision {
    WorkDecision::RequestCapability(CapabilityRequest::new(
        ExecutionId::new("chosen-by-the-model"),
        CapabilityId::new(capability).unwrap(),
    ))
}

/// What an earlier run executed: `capability` as execution `n` of work `w`.
fn restored(n: usize, capability: &str, output: &str) -> RestoredObservation {
    let id = ExecutionId::new(format!("w-exec-{n}"));
    let mut request = CapabilityRequest::new(id.clone(), CapabilityId::new(capability).unwrap());
    request
        .inputs
        .insert("path".into(), InputValue::Text(format!("file-{n}")));
    request.inputs_present = true;
    RestoredObservation {
        request,
        result: ExecutionResult::success(id, output),
        reobserved: false,
    }
}

fn declared() -> Vec<CapabilityId> {
    ["step.read", "step.test"]
        .into_iter()
        .map(|c| CapabilityId::new(c).unwrap())
        .collect()
}

fn spec(restored: Vec<RestoredObservation>) -> WorkSpec {
    WorkSpec::new(WorkId::new("w"), WorkGoal::new("make the tests pass"))
        .with_required_observation(Arc::new(TestsPassed))
        .with_restored(restored)
}

async fn run(exec: &Arc<Exec>, spec: &WorkSpec, decisions: Vec<WorkDecision>) -> WorkReport {
    Agent::new(Arc::new(AnyModel))
        .with_capabilities(Arc::new(Caps))
        .with_executor(exec.clone())
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            spec,
            &CompleteWhenPassed,
            &Script(Mutex::new(decisions.into())),
        )
        .await
}

fn timeline(report: &WorkReport) -> Vec<String> {
    report
        .events
        .iter()
        .filter_map(|e| match e {
            WorkEvent::ObservationRestored { execution_id, .. } => {
                Some(format!("restored {}", execution_id.0))
            }
            WorkEvent::DecisionStarted { turn, .. } => Some(format!("turn {turn}")),
            WorkEvent::ObservationRecorded { execution_id, .. } => {
                Some(format!("observed {}", execution_id.0))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn restored_observations_come_first_in_order_and_are_never_executed_again() {
    let exec = Arc::new(Exec::default());
    let spec = spec(vec![
        restored(1, "step.read", "first read"),
        restored(2, "step.read", "second read"),
    ]);
    let report = run(&exec, &spec, vec![request("step.test")]).await;

    assert!(
        matches!(report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        report.outcome
    );
    // Only the new step ran, under the next identity after the restored ones.
    assert_eq!(*exec.ran.lock().unwrap(), ["w-exec-3"]);
    let outputs: Vec<&str> = report
        .observations
        .iter()
        .map(|o| o.output.as_deref().unwrap())
        .collect();
    assert_eq!(outputs, ["first read", "second read", "tests passed"]);
    assert_eq!(
        timeline(&report),
        [
            "restored w-exec-1",
            "restored w-exec-2",
            "turn 0",
            "observed w-exec-3",
            "turn 1",
        ]
    );
    // Each restored observation keeps its origin: the capability and its complete inputs.
    assert_eq!(report.origins.len(), 3);
    assert_eq!(report.origins[0].capability.as_str(), "step.read");
    assert!(report.origins[1].invocation.contains("file-2"));
    // This run executed one thing; the restored ones are not counted as its executions.
    assert_eq!(report.summary.executions, 1);
    assert_eq!(report.summary.observations, 1);
    assert!(verify_trajectory(&report.events, &spec.limits).is_empty());
    let audit = audit_safety(&report, &spec, &declared());
    assert!(audit.is_clean(), "{:?}", audit.details);
}

#[tokio::test]
async fn a_requirement_a_restored_observation_meets_completes_without_executing_anything() {
    let exec = Arc::new(Exec::default());
    let spec = spec(vec![
        restored(1, "step.read", "read"),
        restored(2, "step.test", "tests passed"),
    ]);
    let report = run(&exec, &spec, Vec::new()).await;

    assert!(
        matches!(report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        report.outcome
    );
    assert_eq!(
        exec.calls.load(Ordering::SeqCst),
        0,
        "nothing is executed again"
    );
    let remaining: Vec<usize> = report
        .events
        .iter()
        .filter_map(|e| match e {
            WorkEvent::ObservationRestored { remaining, .. } => Some(*remaining),
            _ => None,
        })
        .collect();
    assert_eq!(
        remaining,
        [1, 0],
        "the second restored observation meets the requirement"
    );
    // The completion is backed by the restored evidence, and the audit agrees.
    let audit = audit_safety(&report, &spec, &declared());
    assert!(audit.is_clean(), "{:?}", audit.details);
    assert!(verify_trajectory(&report.events, &spec.limits).is_empty());
}

#[tokio::test]
async fn numbering_continues_after_the_highest_restored_identity() {
    // An execution whose executor erred was never an observation, so the restored identities can
    // have a gap: the next one is still after the highest.
    let exec = Arc::new(Exec::default());
    let spec = spec(vec![
        restored(1, "step.read", "read"),
        restored(3, "step.read", "read again"),
    ]);
    run(&exec, &spec, vec![request("step.test")]).await;
    assert_eq!(*exec.ran.lock().unwrap(), ["w-exec-4"]);
}

#[tokio::test]
async fn a_restored_trajectory_that_cannot_be_joined_whole_ends_the_work() {
    let cases = [
        (
            "an identity recorded twice",
            vec![restored(1, "step.read", "a"), restored(1, "step.read", "b")],
        ),
        ("a request for another execution", {
            let mut r = restored(1, "step.read", "a");
            r.request.execution_id = ExecutionId::new("w-exec-9");
            vec![r]
        }),
    ];
    for (why, entries) in cases {
        let exec = Arc::new(Exec::default());
        let report = run(&exec, &spec(entries), vec![request("step.read")]).await;
        match &report.outcome {
            WorkOutcome::Failed { reason, .. } => {
                assert!(reason.contains("cannot be restored"), "{why}: {reason}")
            }
            other => panic!("{why}: {other:?}"),
        }
        assert_eq!(exec.calls.load(Ordering::SeqCst), 0, "{why}: nothing ran");
        assert!(
            report.observations.is_empty(),
            "{why}: nothing is used in part"
        );
    }
}

#[test]
fn a_restored_observation_after_the_first_turn_is_a_trajectory_violation() {
    let events = vec![
        WorkEvent::WorkStarted {
            work_id: WorkId::new("w"),
            goal: "g".into(),
            limits: Default::default(),
        },
        WorkEvent::DecisionStarted {
            work_id: WorkId::new("w"),
            turn: 0,
        },
        WorkEvent::ObservationRestored {
            work_id: WorkId::new("w"),
            execution_id: ExecutionId::new("w-exec-1"),
            capability: CapabilityId::new("step.read").unwrap(),
            kind: ObservationKind::ExecutionCompleted,
            receipt_id: None,
            remaining: 0,
            reobserved: false,
        },
        WorkEvent::WorkBlocked {
            work_id: WorkId::new("w"),
            reason: "r".into(),
        },
    ];
    let violations = verify_trajectory(&events, &Default::default());
    assert!(
        violations
            .iter()
            .any(|v| v.contains("after the first turn")),
        "{violations:?}"
    );
}
