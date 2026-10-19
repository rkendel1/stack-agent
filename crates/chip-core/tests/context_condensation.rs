//! A context policy may leave a recorded observation out of a request only by condensing it, and
//! only when Chip can account for it: the observation is one recorded, successful execution, is not
//! also sent, and keeps its place in the report. The loop checks this before anything is sent, the
//! audit re-reads it afterwards, and the budget check stays the one authority over what is sent.
//!
//! Only the model is scripted; the world executes for real.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_core::{
    Agent, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, CondensedObservation, DeduplicatedEscalationContext,
    EscalationContext, EscalationContextPolicy, ExecutionError, ExecutionObserver,
    ExecutionRequest, ExecutionResult, Executor, InputValue, LimitKind, ModelDecisionBoundary,
    NoLocalPolicy, TestLocalReasoner, WorkEvent, WorkGoal, WorkId, WorkLimits, WorkOutcome,
    WorkReport, WorkSpec, WorkState, WorkTrajectory, audit_safety, context_report,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};

fn id(s: &str) -> CapabilityId {
    CapabilityId::new(s).unwrap()
}

struct World {
    executions: AtomicUsize,
}

#[async_trait::async_trait]
impl CapabilityProvider for World {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        let mut echo = CapabilityDescriptor::new(id("echo.fixed"), "echo", "echo");
        echo.inputs.push(CapabilityInput {
            name: "tag".into(),
            description: "tag".into(),
            required: true,
        });
        let echo = echo.without_evidence_reuse();
        let bad = CapabilityDescriptor::new(id("always.fails"), "fails", "fails")
            .without_evidence_reuse();
        Ok(vec![echo, bad])
    }
    async fn availability(&self, _: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

#[async_trait::async_trait]
impl Executor for World {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let n = self.executions.fetch_add(1, Ordering::SeqCst) + 1;
        match r.intent.as_str() {
            "echo.fixed" => {
                let tag = match r.inputs.get("tag") {
                    Some(InputValue::Text(t)) => t.clone(),
                    _ => String::new(),
                };
                Ok(ExecutionResult::success(r.id, format!("echo:{tag}"))
                    .with_receipt_id(format!("sha256:real-{n}")))
            }
            "always.fails" => Ok(ExecutionResult::failure(r.id, "it failed")),
            other => Err(ExecutionError::InvalidRequest(other.into())),
        }
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
            return Err(FxError::Provider("the provider timed out".into()));
        }
        Ok(ModelResponse::new(
            "r",
            replies.remove(0),
            Usage::new(11, 3),
        ))
    }
}

fn echo(tag: &str) -> String {
    format!(
        r#"{{"decision":"request_capability","capability":"echo.fixed","inputs":{{"tag":"{tag}"}}}}"#
    )
}
fn fails() -> String {
    r#"{"decision":"request_capability","capability":"always.fails"}"#.to_string()
}
fn done() -> String {
    r#"{"decision":"complete","summary":"done"}"#.to_string()
}

/// What a condensing policy gets wrong, if anything.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Defect {
    None,
    UnknownExecution,
    AlsoSentInFull,
    WrongWithheldSize,
    NoFacts,
    Twice,
    CondensesTheFailure,
}

/// Condenses the observation at `index`, once it is not the latest, with the given defect.
struct Condense {
    index: usize,
    defect: Defect,
}

impl EscalationContextPolicy for Condense {
    fn id(&self) -> &'static str {
        "condense-test"
    }
    fn build(&self, state: &WorkState<'_>, trajectory: &WorkTrajectory<'_>) -> EscalationContext {
        let mut context = DeduplicatedEscalationContext.build(state, trajectory);
        if trajectory.observations.len() <= self.index + 1 {
            return context;
        }
        let target = &trajectory.observations[self.index];
        let mut entry = CondensedObservation {
            execution_id: target.execution_id.clone(),
            withheld_bytes: target.render().len(),
            facts: vec![format!("observed (observation {}): FACT", self.index + 1)],
        };
        match self.defect {
            Defect::None | Defect::CondensesTheFailure => {}
            Defect::UnknownExecution => {
                entry.execution_id = chip_core::ExecutionId::new("w-exec-404");
            }
            Defect::WrongWithheldSize => entry.withheld_bytes += 1,
            Defect::NoFacts => entry.facts.clear(),
            Defect::AlsoSentInFull | Defect::Twice => {}
        }
        if self.defect != Defect::AlsoSentInFull {
            context
                .relevant_observations
                .retain(|o| o.execution_id != target.execution_id);
        }
        if self.defect == Defect::Twice {
            context.condensed.push(entry.clone());
        }
        context.condensed.push(entry);
        context
    }
}

struct Outcome {
    report: WorkReport,
    spec: WorkSpec,
    seen: Vec<ModelRequest>,
}

async fn run(
    replies: Vec<String>,
    policy: &dyn EscalationContextPolicy,
    budget: Option<usize>,
) -> Outcome {
    let world = Arc::new(World {
        executions: AtomicUsize::new(0),
    });
    let model = Arc::new(Script {
        replies: Mutex::new(replies),
        seen: Mutex::new(Vec::new()),
    });
    let agent = Agent::new(model.clone())
        .with_capabilities(world.clone())
        .with_executor(world)
        .with_observer(Arc::new(ExecutionObserver))
        .with_local_reasoner(Arc::new(TestLocalReasoner::default()));
    let mut spec =
        WorkSpec::new(WorkId::new("w"), WorkGoal::new("Look.")).with_limits(WorkLimits {
            max_turns: 12,
            max_executions: 10,
        });
    if let Some(b) = budget {
        spec = spec.with_context_budget_bytes(b);
    }
    let report = agent
        .run_work_with_context_policy(&spec, &NoLocalPolicy, &ModelDecisionBoundary, policy)
        .await;
    let seen = model.seen.lock().unwrap().clone();
    Outcome { report, spec, seen }
}

fn declared() -> Vec<CapabilityId> {
    vec![id("echo.fixed"), id("always.fails")]
}

fn text_of(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn a_condensed_observation_is_named_in_the_request_and_stays_recorded_in_full() {
    let o = run(
        vec![echo("alpha"), echo("beta"), done()],
        &Condense {
            index: 0,
            defect: Defect::None,
        },
        None,
    )
    .await;
    assert!(
        matches!(o.report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        o.report.outcome
    );
    assert_eq!(o.seen.len(), 3);

    // The second request still carries the first observation; the third condenses it.
    assert!(text_of(&o.seen[1]).contains("echo:alpha"));
    let third = text_of(&o.seen[2]);
    assert!(!third.contains("echo:alpha"), "{third}");
    assert!(
        third.contains("echo:beta"),
        "the latest observation is sent in full"
    );
    assert!(third.contains("Condensed (recorded in full; not retransmitted in this request)"));
    assert!(third.contains("execution w-exec-1"), "{third}");
    assert!(third.contains("FACT"), "{third}");

    // The record is untouched: both observations, with their receipts, and a clean audit.
    assert_eq!(o.report.observations.len(), 2);
    assert!(
        o.report.observations[0]
            .output
            .as_deref()
            .unwrap()
            .contains("echo:alpha")
    );
    assert_eq!(
        o.report.observations[0].receipt_id.as_deref(),
        Some("sha256:real-1")
    );
    let audit = audit_safety(&o.report, &o.spec, &declared());
    assert!(audit.is_clean(), "{audit:?}");
    let c = context_report(&o.report, &o.spec);
    assert_eq!(c.condensed_observations(), 1);
    assert_eq!(
        c.calls
            .iter()
            .map(|k| k.condensed_observations)
            .collect::<Vec<_>>(),
        [0, 0, 1]
    );
}

#[tokio::test]
async fn a_condensation_chip_cannot_account_for_is_not_sent() {
    for defect in [
        Defect::UnknownExecution,
        Defect::AlsoSentInFull,
        Defect::WrongWithheldSize,
        Defect::NoFacts,
        Defect::Twice,
    ] {
        let o = run(
            vec![echo("alpha"), echo("beta"), done()],
            &Condense { index: 0, defect },
            None,
        )
        .await;
        assert!(
            matches!(&o.report.outcome, WorkOutcome::Failed { reason, .. }
                if reason.contains("condensation is not allowed")),
            "{defect:?}: {:?}",
            o.report.outcome
        );
        assert_eq!(
            o.seen.len(),
            2,
            "{defect:?}: the refused request was never sent"
        );
        assert!(
            audit_safety(&o.report, &o.spec, &declared()).is_clean(),
            "{defect:?}"
        );
    }
}

#[tokio::test]
async fn a_failed_observation_is_always_kept() {
    let o = run(
        vec![fails(), echo("beta"), done()],
        &Condense {
            index: 0,
            defect: Defect::CondensesTheFailure,
        },
        None,
    )
    .await;
    assert!(
        matches!(&o.report.outcome, WorkOutcome::Failed { reason, .. }
            if reason.contains("a failure is always kept")),
        "{:?}",
        o.report.outcome
    );
    assert_eq!(o.seen.len(), 2);
}

#[tokio::test]
async fn the_audit_catches_a_forged_condensation_count() {
    let mut o = run(
        vec![echo("alpha"), done()],
        &Condense {
            index: 0,
            defect: Defect::None,
        },
        None,
    )
    .await;
    assert!(audit_safety(&o.report, &o.spec, &declared()).is_clean());
    for e in &mut o.report.events {
        if let WorkEvent::ModelEscalation { context, .. } = e {
            context.condensed_observations = 5;
            break;
        }
    }
    let audit = audit_safety(&o.report, &o.spec, &declared());
    assert!(
        audit.unjustified_omissions >= 1 && !audit.is_clean(),
        "{audit:?}"
    );
}

#[tokio::test]
async fn every_rejection_reports_its_turn_size_and_budget_and_the_count_agrees() {
    // Measure what the second request costs, then allow one byte less.
    let probe = run(
        vec![echo("alpha"), done()],
        &DeduplicatedEscalationContext,
        None,
    )
    .await;
    let second = context_report(&probe.report, &probe.spec).calls[1].request_bytes;

    let o = run(
        vec![echo("alpha"), done()],
        &DeduplicatedEscalationContext,
        Some(second - 1),
    )
    .await;
    assert_eq!(
        o.report.outcome,
        WorkOutcome::LimitReached {
            limit: LimitKind::Context
        }
    );
    assert_eq!(
        o.seen.len(),
        1,
        "the oversized request was not sent, trimmed or retried"
    );

    let c = context_report(&o.report, &o.spec);
    assert_eq!(c.context_limit_rejections, c.rejections.len());
    assert_eq!(c.rejections.len(), 1);
    let from_events: Vec<(usize, usize, usize)> = o
        .report
        .events
        .iter()
        .filter_map(|e| match e {
            WorkEvent::ContextLimit {
                turn,
                request_bytes,
                budget_bytes,
                ..
            } => Some((*turn, *request_bytes, *budget_bytes)),
            _ => None,
        })
        .collect();
    let r = &c.rejections[0];
    assert_eq!(from_events, [(r.turn, r.request_bytes, r.budget_bytes)]);
    assert_eq!((r.request_bytes, r.budget_bytes), (second, second - 1));
    assert_eq!((c.requests_sent(), c.provider_failures()), (1, 0));
}

#[tokio::test]
async fn a_provider_timeout_is_a_provider_failure_and_not_a_context_rejection() {
    // The script has no second reply, so its second call fails the way a timeout does.
    let o = run(
        vec![echo("alpha")],
        &DeduplicatedEscalationContext,
        Some(1_000_000),
    )
    .await;
    assert!(
        matches!(o.report.outcome, WorkOutcome::Failed { .. }),
        "{:?}",
        o.report.outcome
    );
    let c = context_report(&o.report, &o.spec);
    assert_eq!(c.context_limit_rejections, 0);
    assert!(c.rejections.is_empty());
    assert_eq!(
        c.requests_sent(),
        2,
        "both requests were handed to the provider"
    );
    assert_eq!(c.provider_failures(), 1);
    assert!(audit_safety(&o.report, &o.spec, &declared()).is_clean());
}
