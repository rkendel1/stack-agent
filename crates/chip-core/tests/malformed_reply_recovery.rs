//! A malformed model reply is rejected, recorded and answered with its diagnostic, within a bound;
//! nothing in it is ever acted on.
//!
//! Every reply below goes through the real strict boundary (`ModelDecisionBoundary`) and the real
//! work loop. The model is a script of reply texts; the executor counts what it was asked to do.
//! Only a reply that cannot be read as exactly one JSON object is recoverable
//! (`DecisionError::MalformedReply`): an object that is not an allowed decision, a refused
//! capability, and a provider failure still end the work at once, as they always did.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_core::{
    Agent, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityProvider, DEFAULT_MALFORMED_REPLY_LIMIT, ExecutionError, ExecutionObserver,
    ExecutionRequest, ExecutionResult, ExecutionStatus, Executor, LimitKind, ModelDecisionBoundary,
    NoLocalPolicy, StopCause, WorkControl, WorkEvent, WorkGoal, WorkId, WorkLimits, WorkOutcome,
    WorkReport, WorkSpec, audit_safety, verify_trajectory,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};

const SELFTEST: &str = r#"{"decision":"request_capability","capability":"compute.selftest"}"#;
/// A capability whose earlier observation never answers a later request: every request executes.
const PERFORM: &str = r#"{"decision":"request_capability","capability":"op.perform"}"#;
const COMPLETE: &str = r#"{"decision":"complete","summary":"the self test ran"}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"nothing more to do"}"#;
const PROSE: &str = "I have read the code; next I will run the self test.";

/// One scripted reply: its text, or a provider failure.
#[derive(Clone)]
enum Reply {
    Text(String),
    Fail(FxError),
}

fn text(t: &str) -> Reply {
    Reply::Text(t.to_string())
}

/// The model: plays `script` in order, one entry per call, and keeps every request it was sent.
struct Script {
    replies: Vec<Reply>,
    calls: AtomicUsize,
    seen: Mutex<Vec<ModelRequest>>,
}

impl Script {
    fn new(replies: Vec<Reply>) -> Arc<Script> {
        Arc::new(Script {
            replies,
            calls: AtomicUsize::new(0),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// The text of request `n` (0-based), all messages joined.
    fn sent(&self, n: usize) -> String {
        self.seen.lock().unwrap()[n]
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[async_trait::async_trait]
impl ModelProvider for Script {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, FxError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(request);
        match self.replies.get(n).cloned() {
            Some(Reply::Text(t)) => Ok(ModelResponse::new(format!("r{n}"), t, Usage::new(5, 5))),
            Some(Reply::Fail(e)) => Err(e),
            None => panic!(
                "the model was called {} times; the script has {}",
                n + 1,
                self.replies.len()
            ),
        }
    }
}

/// Counts executions; every one succeeds.
#[derive(Default)]
struct Exec {
    calls: AtomicUsize,
}

impl Exec {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Executor for Exec {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(ExecutionResult {
            id: r.id,
            status: ExecutionStatus::Success,
            output: "self test passed".to_string(),
            receipt_id: Some(format!("sha256:receipt-{n}")),
            evidence: None,
        })
    }
}

struct Caps;

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        let mut perform =
            CapabilityDescriptor::new(CapabilityId::new("op.perform")?, "Perform", "Not reusable");
        perform.reuse_evidence = false;
        Ok(vec![
            CapabilityDescriptor::new(
                CapabilityId::new("compute.selftest")?,
                "Self test",
                "Deterministic",
            ),
            perform,
        ])
    }

    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

struct Ran {
    report: WorkReport,
    spec: WorkSpec,
    model: Arc<Script>,
    exec: Arc<Exec>,
}

impl Ran {
    fn rejections(&self) -> Vec<(usize, usize, bool, String)> {
        self.report
            .events
            .iter()
            .filter_map(|e| match e {
                WorkEvent::DecisionRejected {
                    turn,
                    consecutive,
                    recovering,
                    reason,
                    ..
                } => Some((*turn, *consecutive, *recovering, reason.clone())),
                _ => None,
            })
            .collect()
    }

    fn reason(&self) -> String {
        match &self.report.outcome {
            WorkOutcome::Failed { reason, .. } | WorkOutcome::Blocked { reason } => reason.clone(),
            other => format!("{other:?}"),
        }
    }

    /// The trajectory is well formed and the safety audit finds nothing, whatever the outcome.
    fn assert_sound(&self) {
        assert_eq!(
            verify_trajectory(&self.report.events, &self.spec.limits),
            Vec::<String>::new()
        );
        let declared = [
            CapabilityId::new("compute.selftest").unwrap(),
            CapabilityId::new("op.perform").unwrap(),
        ];
        audit_safety(&self.report, &self.spec, &declared).assert_clean();
    }
}

fn spec() -> WorkSpec {
    WorkSpec::new(WorkId::new("w"), WorkGoal::new("run the self test")).with_limits(WorkLimits {
        max_turns: 8,
        max_executions: 4,
    })
}

async fn run_with(spec: WorkSpec, replies: Vec<Reply>) -> Ran {
    let model = Script::new(replies);
    let exec = Arc::new(Exec::default());
    let agent = Agent::new(model.clone())
        .with_capabilities(Arc::new(Caps))
        .with_executor(exec.clone())
        .with_observer(Arc::new(ExecutionObserver));
    let report = agent
        .run_work(&spec, &NoLocalPolicy, &ModelDecisionBoundary)
        .await;
    Ran {
        report,
        spec,
        model,
        exec,
    }
}

async fn run(replies: Vec<Reply>) -> Ran {
    run_with(spec(), replies).await
}

#[tokio::test]
async fn narration_around_one_valid_decision_is_rejected_and_the_next_turn_recovers() {
    // The benchmark's failure: a sentence of narration, then a decision that would be valid alone.
    let wrapped = format!("{PROSE}\n{SELFTEST}");
    let r = run(vec![text(&wrapped), text(SELFTEST), text(COMPLETE)]).await;

    // The wrapped request was not taken out of the narration: the one execution is the recovery's.
    assert_eq!(r.exec.calls(), 1);
    assert_eq!(r.report.summary.executions, 1);
    assert!(
        matches!(&r.report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        r.report.outcome
    );
    assert_eq!(r.model.calls(), 3);
    assert_eq!(r.report.summary.turns, 3, "the rejected reply used a turn");

    let rejected = r.rejections();
    assert_eq!(rejected.len(), 1);
    let (turn, consecutive, recovering, reason) = &rejected[0];
    assert_eq!((*turn, *consecutive, *recovering), (0, 1, true));
    assert!(reason.contains("malformed reply"), "{reason}");
    assert!(
        reason.contains("bytes of text before the first '{'"),
        "{reason}"
    );
    // No decision was recorded for the rejected turn.
    assert!(r.report.decisions.iter().all(|d| d.turn != 0));

    // The next request tells the model what was wrong, as evidence; the first one did not.
    assert!(!r.model.sent(0).contains("your reply was rejected"));
    let recovery = r.model.sent(1);
    assert!(
        recovery.contains("turn 1: your reply was rejected and nothing in it was done"),
        "{recovery}"
    );
    assert!(recovery.contains("reply with exactly one JSON decision object and no other text"));
    // Said once: the request after a valid decision carries no stale rejection.
    assert!(!r.model.sent(2).contains("your reply was rejected"));
    r.assert_sound();
}

#[tokio::test]
async fn plain_malformed_json_gets_an_actionable_diagnostic_and_a_bounded_retry() {
    let truncated = r#"{"decision":"request_capability","capability":"compute.selftest""#;
    let r = run(vec![text(truncated), text(SELFTEST), text(COMPLETE)]).await;
    assert!(matches!(r.report.outcome, WorkOutcome::Completed { .. }));
    assert_eq!(r.exec.calls(), 1);
    let rejected = r.rejections();
    assert_eq!(rejected.len(), 1);
    assert!(
        rejected[0].3.contains("not valid JSON"),
        "{}",
        rejected[0].3
    );
    assert!(r.model.sent(1).contains("not valid JSON"));
    r.assert_sound();
}

#[tokio::test]
async fn several_candidate_decisions_are_rejected_without_executing_any() {
    for several in [
        format!("{SELFTEST}{SELFTEST}"),
        format!("{SELFTEST}\n{COMPLETE}"),
        format!("{COMPLETE}\n{SELFTEST}"),
    ] {
        let r = run(vec![text(&several), text(BLOCK)]).await;
        assert_eq!(r.exec.calls(), 0, "{several}");
        assert!(
            matches!(&r.report.outcome, WorkOutcome::Blocked { reason } if reason == "nothing more to do"),
            "{several}: {:?}",
            r.report.outcome
        );
        let rejected = r.rejections();
        assert_eq!(rejected.len(), 1, "{several}");
        assert!(
            rejected[0].3.contains("no decision is chosen from several"),
            "{}",
            rejected[0].3
        );
        r.assert_sound();
    }
}

#[tokio::test]
async fn a_schema_invalid_or_refused_decision_is_not_executed_and_is_answered_like_a_malformed_one()
{
    for (reply, says) in [
        (
            r#"{"decision":"request_capability","capability":"compute.selftest","shell":"ls"}"#,
            "unexpected field \"shell\"",
        ),
        (
            r#"{"decision":"complete"}"#,
            "missing required field \"summary\"",
        ),
        (
            r#"{"decision":"run_shell","summary":"x"}"#,
            "unknown decision",
        ),
        (
            r#"{"decision":"request_capability","capability":"project.delete"}"#,
            "project.delete",
        ),
    ] {
        let r = run(vec![text(reply), text(SELFTEST), text(COMPLETE)]).await;
        // Nothing in the rejected reply ran; the next turn's request did, once.
        assert_eq!(r.exec.calls(), 1, "{reply}");
        assert_eq!(r.model.calls(), 3, "{reply}");
        let rejections = r.rejections();
        assert_eq!(rejections.len(), 1, "{reply}");
        let (turn, consecutive, recovering, reason) = &rejections[0];
        assert_eq!((*turn, *consecutive, *recovering), (0, 1, true), "{reply}");
        assert!(reason.contains("not a valid decision"), "{reason}");
        assert!(reason.contains(says), "{reply}: {reason}");
        // The model is told exactly why, first, at its next escalation.
        let told = r.model.sent(1);
        assert!(
            told.contains("your reply was rejected and nothing in it was done"),
            "{told}"
        );
        assert!(told.contains(says), "{reply}: {told}");
        if reply.contains("project.delete") {
            // Asked for a capability the work does not offer: told which ones it does.
            assert!(
                told.contains(
                    "the capabilities this work may request are compute.selftest, op.perform"
                ),
                "{told}"
            );
        }
        assert!(
            matches!(r.report.outcome, WorkOutcome::Completed { .. }),
            "{reply}: {:?}",
            r.report.outcome
        );
        r.assert_sound();
    }
}

#[tokio::test]
async fn repeated_malformed_replies_end_the_work_at_the_limit_with_nothing_done() {
    assert_eq!(DEFAULT_MALFORMED_REPLY_LIMIT, 2);
    let r = run(vec![text(PROSE), text(PROSE), text(PROSE), text(SELFTEST)]).await;
    assert_eq!(
        r.model.calls(),
        3,
        "the limit's worth of retries, then no more"
    );
    assert_eq!(r.exec.calls(), 0);
    let reason = r.reason();
    assert!(
        matches!(r.report.outcome, WorkOutcome::Failed { .. }),
        "{reason}"
    );
    assert!(reason.contains("not a valid decision"), "{reason}");
    assert!(
        reason.contains("3 rejected decisions in a row; the limit is 2"),
        "{reason}"
    );
    let recovering: Vec<(usize, bool)> = r.rejections().iter().map(|x| (x.1, x.2)).collect();
    assert_eq!(recovering, vec![(1, true), (2, true), (3, false)]);
    // The second recovery request says this is the last chance.
    assert!(
        r.model
            .sent(2)
            .contains("rejected decision 2 in a row; after 2 the next one ends the work")
    );
    r.assert_sound();

    // A limit of 0 is the strict behaviour: the first malformed reply ends the work.
    let r = run_with(
        spec().with_malformed_reply_limit(0),
        vec![text(PROSE), text(SELFTEST)],
    )
    .await;
    assert_eq!(r.model.calls(), 1);
    assert!(
        matches!(r.report.outcome, WorkOutcome::Failed { .. }),
        "{}",
        r.reason()
    );
    // The decision's own reason, as before recovery existed.
    assert!(!r.reason().contains("in a row"), "{}", r.reason());
    r.assert_sound();
}

#[tokio::test]
async fn a_valid_decision_resets_the_count_of_malformed_replies() {
    // Two malformed replies, a valid request, two more malformed replies: never three in a row.
    let r = run(vec![
        text(PROSE),
        text(PROSE),
        text(SELFTEST),
        text(PROSE),
        text(PROSE),
        text(COMPLETE),
    ])
    .await;
    assert!(
        matches!(r.report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        r.report.outcome
    );
    assert_eq!(r.exec.calls(), 1);
    let counts: Vec<usize> = r.rejections().iter().map(|x| x.1).collect();
    assert_eq!(counts, vec![1, 2, 1, 2]);
    r.assert_sound();
}

#[tokio::test]
async fn recovery_spends_the_turn_budget_and_never_resets_it() {
    // Two turns: both spent on malformed replies; the turn limit, not the malformed limit, ends it.
    let r = run_with(
        spec().with_limits(WorkLimits {
            max_turns: 2,
            max_executions: 4,
        }),
        vec![text(PROSE), text(PROSE), text(SELFTEST)],
    )
    .await;
    assert_eq!(
        r.report.outcome,
        WorkOutcome::LimitReached {
            limit: LimitKind::Turns
        }
    );
    assert_eq!(r.model.calls(), 2);
    assert_eq!(r.report.summary.turns, 2);
    r.assert_sound();

    // The execution limit counts across a rejection: one execution allowed, one used, then a
    // malformed reply, then another request: refused at the limit, not executed.
    let r = run_with(
        spec().with_limits(WorkLimits {
            max_turns: 8,
            max_executions: 1,
        }),
        vec![text(PERFORM), text(PROSE), text(PERFORM), text(COMPLETE)],
    )
    .await;
    assert_eq!(r.exec.calls(), 1);
    assert_eq!(
        r.report.outcome,
        WorkOutcome::LimitReached {
            limit: LimitKind::Executions
        }
    );
    r.assert_sound();
}

/// Asks the work to stop once the model has answered `after` times: the deadline passing while a
/// recovery is pending.
struct StopAfter {
    after: usize,
    responses: AtomicUsize,
}

impl WorkControl for StopAfter {
    fn stop_requested(&self) -> Option<StopCause> {
        (self.responses.load(Ordering::SeqCst) >= self.after).then_some(StopCause::Deadline)
    }

    fn on_model_response(
        &self,
        _turn: usize,
        _model: &str,
        _response: &ModelResponse,
        _observed: &[chip_core::ExecutionId],
    ) {
        self.responses.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn the_deadline_ends_a_work_that_is_recovering() {
    let control = Arc::new(StopAfter {
        after: 1,
        responses: AtomicUsize::new(0),
    });
    let r = run_with(
        spec().with_control(control),
        vec![text(PROSE), text(SELFTEST), text(COMPLETE)],
    )
    .await;
    assert_eq!(r.model.calls(), 1, "no recovery call after the deadline");
    assert!(
        matches!(
            r.report.outcome,
            WorkOutcome::Cancelled {
                cause: StopCause::Deadline,
                ..
            }
        ),
        "{:?}",
        r.report.outcome
    );
    assert_eq!(r.exec.calls(), 0);
}

#[tokio::test]
async fn a_malformed_completion_claim_does_not_satisfy_the_goal() {
    // The goal needs an observation of "self test passed". A narrated completion is rejected; a
    // bare claim after it is refused because nothing was observed, and so is the next one: three
    // rejected decisions in a row, of two kinds, end the work at the one bound. Nothing executed.
    let spec = spec().with_required_output("self test passed");
    let narrated = format!("All done! {COMPLETE}");
    let r = run_with(spec, vec![text(&narrated), text(COMPLETE), text(COMPLETE)]).await;
    assert_eq!(r.exec.calls(), 0);
    assert!(
        matches!(&r.report.outcome, WorkOutcome::Blocked { reason } if reason.starts_with("completion refused")),
        "{:?}",
        r.report.outcome
    );
    let consecutive: Vec<(usize, bool)> = r.rejections().iter().map(|x| (x.1, x.2)).collect();
    assert_eq!(consecutive, vec![(1, true), (2, true), (3, false)]);
    // The refused claim was answered with what the goal still lacks.
    assert!(
        r.model.sent(2).contains(
            "your complete decision was refused and nothing was completed: no authoritative observation satisfies the goal yet"
        ),
        "{}",
        r.model.sent(2)
    );
    r.assert_sound();
}

#[tokio::test]
async fn provider_failures_and_timeouts_keep_their_class_and_are_not_retried() {
    for failure in [
        FxError::Timeout("the call took longer than 30 s".into()),
        FxError::Provider("provider unavailable".into()),
        FxError::Http("connection reset".into()),
    ] {
        // Also after a malformed reply: the recovery's call failing is a provider failure.
        for prefix in [vec![], vec![text(PROSE)]] {
            let mut replies = prefix.clone();
            replies.push(Reply::Fail(failure.clone()));
            replies.push(text(SELFTEST));
            let r = run(replies).await;
            let reason = r.reason();
            assert!(
                reason.starts_with("model escalation failed"),
                "{failure:?}: {reason}"
            );
            assert!(!reason.contains("not a valid decision"), "{reason}");
            assert_eq!(
                r.model.calls(),
                prefix.len() + 1,
                "{failure:?}: not retried"
            );
            assert_eq!(r.rejections().len(), prefix.len());
            assert_eq!(r.exec.calls(), 0);
        }
    }
}
