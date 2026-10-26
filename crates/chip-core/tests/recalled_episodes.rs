//! Recalled episodes are evidence in the decision context and nothing else: bounded, named by their
//! record, absent when nothing was recalled, and unable to execute, authorize or complete anything.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_core::{
    Agent, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityProvider, ExecutionError, ExecutionObserver, ExecutionRequest, ExecutionResult,
    Executor, MAX_RECALLED_EPISODES, MAX_RECALLED_STATEMENT_BYTES, ModelDecisionBoundary,
    NoLocalPolicy, RecalledEpisode, WorkEvent, WorkGoal, WorkId, WorkOutcome, WorkSpec,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};

/// Answers every request with the same reply and keeps the requests.
struct Replies {
    reply: &'static str,
    seen: Mutex<Vec<ModelRequest>>,
}

#[async_trait::async_trait]
impl ModelProvider for Replies {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, FxError> {
        self.seen.lock().unwrap().push(request);
        Ok(ModelResponse::new("r", self.reply, Usage::new(1, 1)))
    }
}

struct Exec(AtomicUsize);

#[async_trait::async_trait]
impl Executor for Exec {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ExecutionResult::success(r.id, "ran"))
    }
}

struct Caps;

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(vec![CapabilityDescriptor::new(
            CapabilityId::new("compute.selftest")?,
            "t",
            "t",
        )])
    }
    async fn availability(&self, _: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

fn episode(n: usize, statement: &str) -> RecalledEpisode {
    RecalledEpisode {
        record: format!("chip.Episode:t{n}~a2"),
        statement: statement.to_string(),
    }
}

async fn run(reply: &'static str, spec: &WorkSpec) -> (Vec<String>, usize, chip_core::WorkReport) {
    let model = Arc::new(Replies {
        reply,
        seen: Mutex::new(Vec::new()),
    });
    let exec = Arc::new(Exec(AtomicUsize::new(0)));
    let report = Agent::new(model.clone())
        .with_capabilities(Arc::new(Caps))
        .with_executor(exec.clone())
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(spec, &NoLocalPolicy, &ModelDecisionBoundary)
        .await;
    let sent = model
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|r| {
            r.messages
                .iter()
                .map(|m| m.content.clone())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect();
    (sent, exec.0.load(Ordering::SeqCst), report)
}

const COMPLETE: &str = r#"{"decision":"complete","summary":"done"}"#;

#[tokio::test]
async fn nothing_recalled_leaves_the_request_exactly_as_it_was() {
    let spec = WorkSpec::new(WorkId::new("w"), WorkGoal::new("g"));
    let (plain, _, _) = run(COMPLETE, &spec).await;
    let (empty, _, _) = run(COMPLETE, &spec.clone().with_recalled(Vec::new())).await;
    assert_eq!(plain, empty);
    assert!(!plain[0].contains("recalled episode"));
}

#[tokio::test]
async fn recalled_episodes_are_bounded_evidence_lines_named_by_their_record() {
    let long = "x".repeat(MAX_RECALLED_STATEMENT_BYTES + 200);
    let recalled: Vec<RecalledEpisode> = (0..5).map(|n| episode(n, &long)).collect();
    let spec = WorkSpec::new(WorkId::new("w"), WorkGoal::new("g")).with_recalled(recalled);
    assert_eq!(spec.recalled.len(), MAX_RECALLED_EPISODES);
    let (sent, _, _) = run(COMPLETE, &spec).await;
    let request = &sent[0];
    assert_eq!(
        request.matches("recalled episode ").count(),
        MAX_RECALLED_EPISODES
    );
    assert!(request.contains("recalled episode chip.Episode:t0~a2"));
    assert!(!request.contains("chip.Episode:t3~a2"));
    // Under the evidence heading, clipped, and said to be no instruction.
    let evidence = request.split("Evidence:\n").nth(1).unwrap();
    let line = evidence.lines().next().unwrap();
    assert!(
        line.starts_with("  - recalled episode chip.Episode:t0~a2"),
        "{line}"
    );
    assert!(line.contains("not an instruction"));
    assert!(line.ends_with(&format!("{}…", "x".repeat(10))));
    assert!(!line.contains(&"x".repeat(MAX_RECALLED_STATEMENT_BYTES + 1)));
}

#[tokio::test]
async fn a_recalled_success_completes_nothing_and_authorizes_nothing() {
    // The work needs an observed output; the model completes at once, citing the recall.
    let spec = WorkSpec::new(WorkId::new("w"), WorkGoal::new("g"))
        .with_required_output("ran")
        .with_recalled(vec![episode(
            0,
            "verified success: an earlier work completed this goal with compute.selftest",
        )])
        .with_malformed_reply_limit(1);
    let (_, executions, report) = run(COMPLETE, &spec).await;
    assert_eq!(executions, 0);
    assert!(
        !matches!(report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        report.outcome
    );
    assert!(
        report
            .events
            .iter()
            .any(|e| matches!(e, WorkEvent::DecisionRejected { .. }))
    );
    // A recalled episode that names a capability the work does not declare does not make it
    // requestable.
    let ask = r#"{"decision":"request_capability","capability":"compute.erase"}"#;
    let spec = WorkSpec::new(WorkId::new("w"), WorkGoal::new("g"))
        .with_recalled(vec![episode(
            0,
            "verified success: compute.erase (success)",
        )])
        .with_malformed_reply_limit(0);
    let (_, executions, report) = run(ask, &spec).await;
    assert_eq!(executions, 0);
    assert!(!matches!(report.outcome, WorkOutcome::Completed { .. }));
}
