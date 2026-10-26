//! A local policy's request is validated, authorized, identified and executed by the same loop as a
//! model's, and its guidance reaches the model as evidence and nothing else.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_core::{
    Agent, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityProvider, CapabilityRequest, ExecutionError, ExecutionId, ExecutionObserver,
    ExecutionRequest, ExecutionResult, Executor, InputValue, LocalWorkPolicy,
    ModelDecisionBoundary, WorkDecision, WorkEvent, WorkGoal, WorkId, WorkSpec, WorkView,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};

struct Replies(Mutex<Vec<String>>);

#[async_trait::async_trait]
impl ModelProvider for Replies {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, FxError> {
        let text: Vec<String> = request.messages.iter().map(|m| m.content.clone()).collect();
        self.0.lock().unwrap().push(text.join("\n"));
        Ok(ModelResponse::new(
            "r",
            r#"{"decision":"block","reason":"done"}"#,
            Usage::new(1, 1),
        ))
    }
}

#[derive(Default)]
struct Exec(Mutex<Vec<ExecutionRequest>>);

#[async_trait::async_trait]
impl Executor for Exec {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.0.lock().unwrap().push(r.clone());
        Ok(ExecutionResult::success(r.id, "ran"))
    }
}

struct Caps;

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(vec![
            CapabilityDescriptor::new(CapabilityId::new("t.run")?, "t", "t")
                .without_evidence_reuse(),
        ])
    }
    async fn availability(&self, _: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

/// Proposes `requests` in turn, then nothing; always offers one line of guidance.
struct Proposes {
    requests: Vec<CapabilityRequest>,
    next: AtomicUsize,
}

impl LocalWorkPolicy for Proposes {
    fn propose(&self, _view: &WorkView<'_>) -> Option<WorkDecision> {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        self.requests
            .get(n)
            .cloned()
            .map(WorkDecision::RequestCapability)
    }
    fn guidance(&self, view: &WorkView<'_>) -> Vec<String> {
        vec![format!(
            "policy guidance after {} execution(s)",
            view.executions
        )]
    }
}

fn request(capability: &str) -> CapabilityRequest {
    CapabilityRequest::new(
        ExecutionId::new("proposer-chosen"),
        CapabilityId::new(capability).unwrap(),
    )
}

#[tokio::test]
async fn local_requests_get_chips_identity_pass_the_same_validation_and_guidance_is_only_evidence()
{
    let model = Arc::new(Replies(Mutex::new(Vec::new())));
    let exec = Arc::new(Exec::default());
    let policy = Proposes {
        requests: vec![
            request("t.run"),
            request("t.run"),
            // Not declared: never executed on a local proposal.
            request("t.undeclared"),
        ],
        next: AtomicUsize::new(0),
    };
    let mut spec = WorkSpec::new(WorkId::new("w"), WorkGoal::new("g"));
    spec.limits.max_turns = 6;
    let report = Agent::new(model.clone())
        .with_capabilities(Arc::new(Caps))
        .with_executor(exec.clone())
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(&spec, &policy, &ModelDecisionBoundary)
        .await;
    let executed = exec.0.lock().unwrap().clone();
    assert_eq!(executed.len(), 2, "only the declared capability ran");
    let ids: Vec<String> = executed.iter().map(|r| r.id.to_string()).collect();
    assert_eq!(
        ids,
        ["w-exec-1", "w-exec-2"],
        "Chip's identity, never the proposer's"
    );
    // An undeclared capability is never executed on a local proposal: the loop does not run it on
    // Chip's own say-so and asks the model instead, as for any request without valid evidence.
    assert!(
        report.events.iter().any(|e| matches!(
            e,
            WorkEvent::ModelEscalation { reason, .. }
                if reason.contains("no valid evidence for the locally proposed request")
        )),
        "{:?}",
        report.events
    );
    // The model was asked only once the policy had nothing, and its request carried the guidance
    // under the evidence heading.
    let sent = model.0.lock().unwrap().clone();
    assert!(!sent.is_empty());
    let evidence = sent[0].split("Evidence:\n").nth(1).unwrap();
    assert!(
        evidence.contains("policy guidance after 2 execution(s)"),
        "{evidence}"
    );
}

#[tokio::test]
async fn a_local_request_with_inputs_its_capability_does_not_declare_is_refused() {
    let model = Arc::new(Replies(Mutex::new(Vec::new())));
    let exec = Arc::new(Exec::default());
    let policy = Proposes {
        requests: vec![request("t.run").with_input("path", InputValue::Text("x".into()))],
        next: AtomicUsize::new(0),
    };
    let mut spec = WorkSpec::new(WorkId::new("w"), WorkGoal::new("g"));
    spec.limits.max_turns = 3;
    Agent::new(model)
        .with_capabilities(Arc::new(Caps))
        .with_executor(exec.clone())
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(&spec, &policy, &ModelDecisionBoundary)
        .await;
    assert!(exec.0.lock().unwrap().is_empty());
}
