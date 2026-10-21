//! `chip verify [--json] [--print-reply]`: the software-verification agent.
//!
//! The goal is fixed: "Verify that the project's tests pass." The project is the current working
//! directory; neither the model nor a flag can name another. The model may select `pax.test` (and
//! nothing else, with no inputs); Chip builds `pax --dir <cwd> --json test`; PAX interprets the
//! project and its native tooling; Chip validates PAX's `pax.execution-result.v1` and decides the
//! goal from its `status` alone. This reuses `Agent::run_work`, the PR43 policy, the existing safety
//! auditor and the existing utility measurement. Nothing here knows any ecosystem.
//!
//! Exit status (it is never a native exit code):
//!   0  verified: the goal was satisfied (Completed)
//!   1  not verified: the goal was not satisfied (Blocked, LimitReached, Escalated)
//!   2  usage error
//!   3  required infrastructure unavailable (no model provider, no usable PAX); nothing ran
//!   4  runtime failure: the work Failed, or a safety invariant was violated

use std::sync::{Arc, Mutex};

use chip_core::{
    Agent, CapabilityAvailability, CapabilityId, CapabilityProvider, ExecutionObserver,
    LocalWorkPolicy, ModelDecisionBoundary, ObservationKind, SafetyAudit, WorkDecision, WorkGoal,
    WorkId, WorkLimits, WorkOutcome, WorkReport, WorkSpec, WorkUtilityMeasurement, WorkView,
    audit_safety, measure_utility, verify_trajectory,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse};
use chip_pax::{
    PAX_TEST_CAPABILITY, PaxExecutionResult, PaxExecutor, PaxTestPassed, ResolvedPax,
    parse_execution_result,
};

/// Keeps what the model replied, for the report, and every failed call's typed error, for the
/// exit contract. Replies only: no prompt, no credentials.
pub(crate) struct Recording<P> {
    pub(crate) inner: P,
    pub(crate) replies: Arc<Mutex<Vec<String>>>,
    pub(crate) failures: Arc<Mutex<Vec<ModelFailure>>>,
}

#[async_trait::async_trait]
impl<P: ModelProvider> ModelProvider for Recording<P> {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, FxError> {
        match self.inner.complete(request).await {
            Ok(response) => {
                self.replies.lock().unwrap().push(response.output.clone());
                Ok(response)
            }
            Err(error) => {
                self.failures.lock().unwrap().push(ModelFailure::of(&error));
                Err(error)
            }
        }
    }
}

// ---- the exit contract ----------------------------------------------------------------------------

/// A failed model call, as typed values from the inference executor: never parsed from text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelFailure {
    /// FX's stable failure kind (`transport`, `rate_limited`, `protocol_violation`, ...), or for an
    /// error raised before the executor, the error's class (`configuration`, `provider`, ...).
    pub kind: String,
    pub status: Option<u16>,
    pub delivery: Option<chip_inference::Delivery>,
}

impl ModelFailure {
    pub fn of(error: &FxError) -> Self {
        let (kind, status) = match error {
            FxError::Inference(f) => (f.kind.clone(), f.status),
            FxError::InvalidRequest(_) => ("invalid_request".into(), None),
            FxError::Provider(_) => ("provider".into(), None),
            FxError::Configuration(_) => ("configuration".into(), None),
            FxError::Authentication(_) => ("unauthenticated".into(), None),
            FxError::Http(_) => ("http".into(), None),
            FxError::InvalidResponse(_) => ("invalid_response".into(), None),
            FxError::Timeout(_) => ("timeout".into(), None),
            FxError::Serialization(_) => ("serialization".into(), None),
        };
        Self {
            kind,
            status,
            delivery: match error {
                FxError::Inference(f) => Some(f.delivery),
                _ => None,
            },
        }
    }

    /// The model dependency is unavailable: it cannot be reached, refuses this caller, has no
    /// such model, or is over capacity. A dependency that answered but broke the protocol, timed
    /// out mid-answer, or was cancelled is not "unavailable": that is a runtime failure.
    pub fn dependency_unavailable(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "capability_unavailable"
                | "model_unavailable"
                | "provider_unavailable"
                | "unauthenticated"
                | "forbidden"
                | "rate_limited"
                | "configuration"
        ) || (self.kind == "transport" && self.status.is_none())
    }
}

/// The one mapping from how a model-driven command ended to its exit code, shared by `work`,
/// `verify`, the per-work `exit_status` that `serve` reports, and (through
/// [`exit_for_model_failure`]) the one-shot prompt.
///
/// - 0 [`EXIT_VERIFIED`]: completed, and an independent predicate verified the outcome.
/// - 1 [`EXIT_NOT_VERIFIED`]: the runtime ran correctly but nothing was verified: blocked, a limit,
///   escalated, cancelled (by request or by the whole-work deadline), or a grounded inspection.
/// - 2 [`EXIT_USAGE`]: usage (decided by each command's argument parser).
/// - 3 [`EXIT_UNAVAILABLE`]: a required dependency was unavailable and nothing was done: set by the
///   commands before the loop starts (no model selected, no PAX, no environment), and here when the
///   first model call failed with the dependency unavailable before anything executed.
/// - 4 [`EXIT_RUNTIME_FAILURE`]: anything else that failed, including a safety-invariant violation
///   and a model failure after work had begun or that was not an unavailable dependency.
pub fn exit_code(
    outcome: &WorkOutcome,
    verified: bool,
    grounded: bool,
    invariants_hold: bool,
    failed_before_any_execution: Option<&ModelFailure>,
) -> i32 {
    if !invariants_hold {
        return EXIT_RUNTIME_FAILURE;
    }
    match outcome {
        WorkOutcome::Completed { .. } if verified => EXIT_VERIFIED,
        WorkOutcome::Completed { .. } if grounded => EXIT_NOT_VERIFIED,
        WorkOutcome::Failed { .. } => match failed_before_any_execution {
            Some(f) if f.dependency_unavailable() => EXIT_UNAVAILABLE,
            _ => EXIT_RUNTIME_FAILURE,
        },
        WorkOutcome::Completed { .. } => EXIT_RUNTIME_FAILURE,
        WorkOutcome::Blocked { .. }
        | WorkOutcome::LimitReached { .. }
        | WorkOutcome::Escalated { .. }
        | WorkOutcome::Cancelled { .. } => EXIT_NOT_VERIFIED,
    }
}

/// The one-shot prompt's code for a failed model call: 3 when the dependency was unavailable, 4
/// otherwise. A turn that completed is 0: the prompt path makes no claim to verify.
pub fn exit_for_model_failure(failure: &ModelFailure) -> i32 {
    if failure.dependency_unavailable() {
        EXIT_UNAVAILABLE
    } else {
        EXIT_RUNTIME_FAILURE
    }
}

/// The first model failure, when it came from the first model call and nothing had executed: the
/// only case in which a model failure can mean "nothing was done because a dependency was
/// unavailable".
pub fn failure_before_any_execution<'a>(
    events: &[chip_core::WorkEvent],
    executions: usize,
    failures: &'a [ModelFailure],
) -> Option<&'a ModelFailure> {
    let first_call_failed = events
        .iter()
        .find_map(|e| match e {
            chip_core::WorkEvent::ModelCalled { succeeded, .. } => Some(!*succeeded),
            _ => None,
        })
        .unwrap_or(false);
    (first_call_failed && executions == 0)
        .then(|| failures.first())
        .flatten()
}

/// The local policy of the verification work: ask the model first; afterwards propose to complete
/// if the execution completed and to block if it did not. It never proposes a request. The loop,
/// not this policy, decides whether a completion is allowed: it is refused unless PAX established
/// `passed`.
pub(crate) struct ReactToObservation;

impl LocalWorkPolicy for ReactToObservation {
    fn propose(&self, view: &WorkView<'_>) -> Option<WorkDecision> {
        if view.turn == 0 {
            return None;
        }
        Some(match view.observations.last().map(|o| o.kind) {
            Some(ObservationKind::ExecutionCompleted) => WorkDecision::Complete {
                summary: "pax.test completed".into(),
            },
            _ => WorkDecision::Block {
                reason: "pax.test did not establish the goal".into(),
            },
        })
    }
}

pub const GOAL: &str = "Verify that the project's tests pass.";

pub const EXIT_VERIFIED: i32 = 0;
pub const EXIT_NOT_VERIFIED: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_UNAVAILABLE: i32 = 3;
pub const EXIT_RUNTIME_FAILURE: i32 = 4;

/// Everything a verification run established, read from the report and the audit.
pub struct Verification {
    pub report: WorkReport,
    pub audit: SafetyAudit,
    pub trajectory_violations: usize,
    pub utility: WorkUtilityMeasurement,
    /// What PAX said, if a valid result was observed: line one of the recorded observation, read
    /// back through the same strict parser that admitted it.
    pub pax: Option<PaxExecutionResult>,
    pub goal_satisfied: Option<bool>,
    /// The receipt id on the recorded observation, if any. PAX issues none, so this is `None`.
    pub receipt: Option<String>,
    /// Every failed model call, typed, in order. For the exit contract and the report.
    pub model_failures: Vec<ModelFailure>,
}

impl Verification {
    pub fn invariants_hold(&self) -> bool {
        self.audit.is_clean() && self.trajectory_violations == 0
    }

    /// The product-level result. It depends on the work's terminal state and the audit, never on a
    /// native exit code or on PAX's process status.
    pub fn exit_status(&self) -> i32 {
        exit_code(
            &self.report.outcome,
            self.goal_satisfied == Some(true),
            false,
            self.invariants_hold(),
            failure_before_any_execution(
                &self.report.events,
                self.utility.executions,
                &self.model_failures,
            ),
        )
    }
}

/// The specification of the one supported piece of work.
pub fn spec() -> WorkSpec {
    WorkSpec::new(WorkId::new("verify"), WorkGoal::new(GOAL))
        .with_limits(WorkLimits {
            max_turns: 3,
            max_executions: 1,
        })
        .with_required_observation(Arc::new(PaxTestPassed))
}

/// Runs the verification through the existing work loop with the given model and PAX executor.
pub async fn run_verification(
    model: Arc<dyn ModelProvider>,
    model_name: String,
    pax: PaxExecutor,
    policy: &dyn LocalWorkPolicy,
) -> Verification {
    let agent = Agent::with_model(model, model_name)
        .with_capabilities(Arc::new(pax.clone()))
        .with_executor(Arc::new(pax))
        .with_observer(Arc::new(ExecutionObserver));
    let spec = spec();
    let report = agent.run_work(&spec, policy, &ModelDecisionBoundary).await;
    let declared = vec![CapabilityId::new(PAX_TEST_CAPABILITY).unwrap()];
    let audit = audit_safety(&report, &spec, &declared);
    let trajectory_violations = verify_trajectory(&report.events, &spec.limits).len();
    let utility = measure_utility(&report, &spec);
    let pax = report
        .observations
        .last()
        .and_then(|o| o.output.as_deref())
        .and_then(|text| text.lines().next())
        .and_then(|line| parse_execution_result(line.as_bytes()).ok());
    let goal_satisfied = spec.goal_level(&report.events);
    let receipt = report
        .observations
        .last()
        .and_then(|o| o.receipt_id.clone());
    Verification {
        receipt,
        report,
        audit,
        trajectory_violations,
        utility,
        pax,
        goal_satisfied,
        model_failures: Vec::new(),
    }
}

fn heading(outcome: &WorkOutcome) -> &'static str {
    match outcome {
        WorkOutcome::Completed { .. } => "Verification completed.",
        WorkOutcome::Blocked { .. } => "Verification blocked.",
        WorkOutcome::Failed { .. } => "Verification failed.",
        WorkOutcome::LimitReached { .. } => "Verification stopped at a limit.",
        WorkOutcome::Escalated { .. } => "Verification escalated.",
        WorkOutcome::Cancelled { .. } => "Verification cancelled.",
    }
}

fn outcome_reason(outcome: &WorkOutcome) -> Option<String> {
    match outcome {
        WorkOutcome::Completed { .. } => None,
        WorkOutcome::Blocked { reason }
        | WorkOutcome::Failed { reason, .. }
        | WorkOutcome::Escalated { reason }
        | WorkOutcome::Cancelled { reason, .. } => Some(reason.clone()),
        WorkOutcome::LimitReached { limit } => Some(format!("{} limit reached", limit.name())),
    }
}

/// Concise, human-readable. The result is the authoritative PAX result, never model prose.
pub fn render_human(v: &Verification, pax: &ResolvedPax) -> String {
    let m = v.report.measurement();
    let u = &v.utility;
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };
    line(heading(&v.report.outcome).to_string());
    line(format!("Goal: {GOAL}"));
    if u.executions > 0 {
        line(format!("Capability: {PAX_TEST_CAPABILITY}"));
    } else {
        line("Capability: none (nothing was executed)".to_string());
    }
    match &v.pax {
        Some(r) => {
            line(format!("Result: {}", r.status.as_str()));
            if r.status != chip_pax::PaxStatus::Passed {
                line(format!("Reason: {}", r.reason));
            }
            if let Some(t) = r.tests {
                line(format!(
                    "Tests: {} passed, {} failed, {} ignored",
                    t.passed, t.failed, t.ignored
                ));
            }
            line(format!(
                "Native exit code: {} (recorded in the observation; not Chip's result)",
                r.exit_code.map_or("none".to_string(), |c| c.to_string())
            ));
        }
        None => line("Result: none (PAX produced no valid result)".to_string()),
    }
    if let Some(reason) = outcome_reason(&v.report.outcome) {
        line(format!("Outcome: {reason}"));
    }
    line(
        "Evidence: PAX's verified result; no cryptographic execution receipt (PAX issues none)"
            .to_string(),
    );
    line(format!(
        "Work: {} model call(s), {} execution(s), {} observation(s), verified outputs {}/{}",
        m.model_calls, u.executions, m.observations, u.verified_outputs, u.required_outputs
    ));
    line(format!(
        "Latency: total {:.0} ms (model {:.0} ms, PAX {:.0} ms)",
        m.total_latency.as_secs_f64() * 1000.0,
        m.model_latency.as_secs_f64() * 1000.0,
        m.compute_latency.as_secs_f64() * 1000.0
    ));
    line(format!(
        "Tokens: {}",
        u.total_tokens
            .map_or("not reported".to_string(), |t| t.to_string())
    ));
    line(format!("PAX: {} ({})", pax.version, pax.path.display()));
    line(if v.invariants_hold() {
        "Audit: clean".to_string()
    } else {
        format!(
            "Audit: VIOLATION {:?} (trajectory violations: {})",
            v.audit, v.trajectory_violations
        )
    });
    out
}

pub(crate) fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

pub(crate) fn json_opt(s: Option<&str>) -> String {
    s.map_or("null".to_string(), json_string)
}

/// Extends the existing machine-readable work measurement (`measurement_json`) with the verdict.
/// PAX's fields are carried as PAX stated them; nothing is reinterpreted.
pub fn render_json(v: &Verification, pax: &ResolvedPax) -> String {
    let m = v.report.measurement();
    let u = &v.utility;
    let pax_json = match &v.pax {
        Some(r) => format!(
            "{{\"version\":{},\"status\":{},\"reason\":{},\"tool\":{},\"exit_code\":{},\"tests\":{}}}",
            json_string(&pax.version),
            json_string(r.status.as_str()),
            json_string(&r.reason),
            json_opt(r.tool.as_deref()),
            r.exit_code.map_or("null".to_string(), |c| c.to_string()),
            r.tests.map_or("null".to_string(), |t| format!(
                "{{\"passed\":{},\"failed\":{},\"ignored\":{},\"measured\":{}}}",
                t.passed, t.failed, t.ignored, t.measured
            )),
        ),
        None => format!(
            "{{\"version\":{},\"status\":null,\"reason\":null,\"tool\":null,\"exit_code\":null,\"tests\":null}}",
            json_string(&pax.version)
        ),
    };
    let a = &v.audit;
    format!(
        "{{\"command\":\"verify\",\"goal\":{},\"terminal_state\":{},\"outcome_reason\":{},\"delivery\":{},\"goal_satisfied\":{},\"capability\":{},\"pax\":{pax_json},\"receipt\":{},\"verified_outputs\":{},\"required_outputs\":{},\"audit\":{{\"clean\":{},\"unauthorized_executions\":{},\"unauthorized_completions\":{},\"false_completions\":{},\"evidence_without_observation\":{},\"observation_without_execution\":{},\"execution_without_valid_request\":{},\"limit_violations\":{}}},\"exit_status\":{},\"measurement\":{}}}",
        json_string(GOAL),
        json_string(m.terminal_state().name()),
        json_opt(outcome_reason(&v.report.outcome).as_deref()),
        json_opt(crate::software_work::outcome_delivery(&v.report.outcome)),
        v.goal_satisfied
            .map_or("null".to_string(), |s| s.to_string()),
        if u.executions > 0 {
            json_string(PAX_TEST_CAPABILITY)
        } else {
            "null".to_string()
        },
        json_opt(v.receipt.as_deref()),
        u.verified_outputs,
        u.required_outputs,
        v.invariants_hold(),
        a.unauthorized_executions,
        a.unauthorized_completions,
        a.false_completions,
        a.evidence_without_observation,
        a.observation_without_execution,
        a.execution_without_valid_request,
        a.limit_violations,
        v.exit_status(),
        crate::work_demo::measurement_json("verify", &m),
    )
}

pub async fn verify(args: &[String]) -> i32 {
    let mut json = false;
    let mut print_reply = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--print-reply" => print_reply = true,
            other => {
                eprintln!("error: unexpected argument `{other}`");
                eprintln!("usage: stack-agent verify [--json] [--print-reply]");
                eprintln!("       verifies the project in the current directory");
                return EXIT_USAGE;
            }
        }
    }
    let workdir = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("error: no current directory ({e})");
            return EXIT_UNAVAILABLE;
        }
    };
    let config = match crate::config_from_env(|name| std::env::var(name).ok()) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("error: no model provider is configured ({e}); nothing was run");
            eprint!("{}", crate::software_work::MODEL_SETUP);
            return EXIT_UNAVAILABLE;
        }
    };
    let model_name = config.model.to_string();
    let provider = match crate::provider_selection::build_model_from_process(config) {
        Ok(provider) => provider,
        Err(e) => {
            eprintln!("error: the model provider is unusable ({e}); nothing was run");
            return EXIT_UNAVAILABLE;
        }
    };
    // PAX is verified before any model is asked: with no usable PAX nothing could be run.
    let pax = PaxExecutor::new(&workdir);
    let resolved = match pax.resolve().await {
        Ok(resolved) => resolved,
        Err(why) => {
            eprintln!("error: PAX unavailable ({why}); nothing was run");
            return EXIT_UNAVAILABLE;
        }
    };
    let available = pax
        .availability(&CapabilityId::new(PAX_TEST_CAPABILITY).unwrap())
        .await;
    if !matches!(available, CapabilityAvailability::Available) {
        eprintln!("error: {PAX_TEST_CAPABILITY} is unavailable; nothing was run");
        return EXIT_UNAVAILABLE;
    }

    let replies = Arc::new(Mutex::new(Vec::new()));
    let failures = Arc::new(Mutex::new(Vec::new()));
    let model = Arc::new(Recording {
        inner: crate::software_work::Shared(provider),
        replies: replies.clone(),
        failures: failures.clone(),
    });
    let mut result = run_verification(model, model_name, pax, &ReactToObservation).await;
    result.model_failures = failures.lock().unwrap().clone();
    if json {
        println!("{}", render_json(&result, &resolved));
    } else {
        print!("{}", render_human(&result, &resolved));
    }
    if print_reply {
        for reply in replies.lock().unwrap().iter() {
            eprintln!("Model reply:\n{reply}");
        }
    }
    if !result.invariants_hold() {
        eprintln!(
            "SAFETY INVARIANT VIOLATED: {:?} (trajectory violations: {})",
            result.audit, result.trajectory_violations
        );
    }
    result.exit_status()
}

#[cfg(test)]
mod tests {
    //! The release policy blocks locally after an unsuccessful observation, so the model is never
    //! asked whether to complete. These tests use a policy that does ask it (`None` after the
    //! observation) to prove that a completion claim is refused by Chip, not merely not requested.
    //! Only the model is scripted; PAX and Cargo are real. Skipped if PAX is not installed.

    use chip_core::WorkEvent;
    use std::collections::VecDeque;
    use std::path::{Path, PathBuf};

    use chip_core::ObservationKind;
    use chip_inference::{FxError, ModelRequest, ModelResponse, Usage};

    use super::*;

    struct Script(Mutex<VecDeque<String>>);

    #[async_trait::async_trait]
    impl ModelProvider for Script {
        async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
            let reply = self
                .0
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| FxError::Provider("unscripted model call".into()))?;
            Ok(ModelResponse::new("m", reply, Usage::new(1, 1)))
        }
    }

    /// Consult the model at every turn.
    struct AskModel;

    impl LocalWorkPolicy for AskModel {
        fn propose(&self, _v: &chip_core::WorkView<'_>) -> Option<chip_core::WorkDecision> {
            None
        }
    }

    fn project(tag: &str, lib: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("chip-verify-unit-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"unit_{tag}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        std::fs::write(dir.join("src/lib.rs"), lib).unwrap();
        dir
    }

    const REQUEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
    const CLAIM: &str = r#"{"decision":"complete","summary":"All tests passed."}"#;

    async fn claim_after(dir: &Path, policy: &dyn LocalWorkPolicy) -> Option<Verification> {
        let pax = PaxExecutor::new(dir);
        if pax.resolve().await.is_err() {
            eprintln!("SKIPPED: no usable PAX");
            return None;
        }
        let model = Arc::new(Script(Mutex::new(
            [REQUEST, CLAIM].iter().map(|s| s.to_string()).collect(),
        )));
        Some(run_verification(model, "scripted".into(), pax, policy).await)
    }

    fn assert_refused(v: &Verification, status: chip_pax::PaxStatus) {
        assert_eq!(v.pax.as_ref().map(|r| r.status), Some(status));
        assert!(
            matches!(&v.report.outcome, WorkOutcome::Blocked { reason } if reason.starts_with("completion refused")),
            "{:?}",
            v.report.outcome
        );
        assert!(
            !v.report
                .events
                .iter()
                .any(|e| matches!(e, WorkEvent::WorkCompleted { .. }))
        );
        assert_eq!(v.goal_satisfied, Some(false));
        assert_eq!(v.utility.verified_outputs, 0);
        assert_eq!(v.exit_status(), EXIT_NOT_VERIFIED);
        // The claim was made after an unsuccessful observation, and nothing was recorded for it.
        assert_eq!(v.report.observations.len(), 1);
        assert_eq!(
            v.report.observations[0].kind,
            ObservationKind::ExecutionFailed
        );
        v.audit.assert_clean();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_completion_claim_after_a_failed_result_is_refused() {
        let dir = project(
            "claim-failed",
            "#[cfg(test)]\nmod t { #[test] fn a() { assert_eq!(1, 2); } }\n",
        );
        let Some(v) = claim_after(&dir, &AskModel).await else {
            return;
        };
        assert_refused(&v, chip_pax::PaxStatus::Failed);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_completion_claim_after_a_not_run_result_is_refused() {
        let dir = project("claim-not-run", "pub fn f() {}\n");
        let Some(v) = claim_after(&dir, &AskModel).await else {
            return;
        };
        assert_refused(&v, chip_pax::PaxStatus::NotRun);
        assert_eq!(
            v.pax.unwrap().exit_code,
            Some(0),
            "the native exit code was 0"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_release_policy_never_completes_on_an_unsuccessful_observation() {
        // The policy itself, over the same real runs: no model claim is even requested.
        let dir = project("policy", "pub fn f() {}\n");
        let pax = PaxExecutor::new(&dir);
        if pax.resolve().await.is_err() {
            return;
        }
        let model = Arc::new(Script(Mutex::new(
            [REQUEST].iter().map(|s| s.to_string()).collect(),
        )));
        let v = run_verification(model, "scripted".into(), pax, &ReactToObservation).await;
        assert!(
            matches!(v.report.outcome, WorkOutcome::Blocked { .. }),
            "{:?}",
            v.report.outcome
        );
        assert_eq!(v.report.measurement().model_calls, 1);
    }
}
