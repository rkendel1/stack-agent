//! The baseline-versus-candidate runner.
//!
//! Each run is one ordinary Chip work: the strategy's policies go into
//! [`Agent::run_work_with_context_policy`] with Chip's own model boundary, the work's capabilities
//! come from a [`CapabilitySet`] built for the run, the deadline and cancellation go in as the
//! work's [`WorkControl`], and the criteria that can be decided from observations go in as required
//! observations. The runner adds nothing to the loop: it builds the context, runs the work, reads
//! back what the host observed, and cleans up.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chip_core::{
    Agent, CapabilityId, CapabilitySet, EnvironmentDescription, EnvironmentId, ExecutionObserver,
    ExecutionStatus, ModelDecisionBoundary, ObservationInvariant, StopCause, WorkControl,
    WorkEnvironment, WorkGoal, WorkId, WorkLimits, WorkOutcome, WorkReport, WorkSpec, audit_safety,
    measure_utility, verify_trajectory,
};
use chip_inference::{
    FxError, ModelProvider, ModelRequest, ModelResponse, RemoteCancellation, Usage,
};

use crate::criteria::{CriterionResult, HostObservation};
use crate::definition::{EvidenceField, Mode, ModelSpec, digest};
use crate::plugin::{Replayed, TaskFixture};
use crate::registry::{ResolvedExperiment, ResolvedStrategy, descriptors};

/// A root replay hands fixtures for their declarations. Nothing is ever created or read there.
const REPLAY_ROOT: &str = "/nonexistent/chip-experiment-replay";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    Baseline,
    Candidate,
}

impl Side {
    pub fn name(self) -> &'static str {
        match self {
            Side::Baseline => "baseline",
            Side::Candidate => "candidate",
        }
    }
}

/// Whether the work is known to have ended, as the runtime established it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Termination {
    /// The loop stopped at a boundary with nothing in flight.
    Known,
    /// The loop stopped, but something it had started may still be running elsewhere (a model call
    /// abandoned with an unconfirmed remote stop).
    Uncertain(String),
}

/// How one run ended, as the host establishes it. A missing result is `NotRun`, never success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Completed, every criterion met, the audit clean, and every required measurement present.
    Verified,
    /// The work completed (its strategy claimed success and Chip accepted the completion), but a
    /// host criterion is not met: the result is grounded in the work's own terms and not verified.
    CompletedUnverified {
        failed: Vec<String>,
    },
    /// Blocked, a limit, or escalated: the work ended without completing.
    Incomplete {
        state: &'static str,
        reason: String,
    },
    RuntimeFailure {
        reason: String,
        delivery: Option<&'static str>,
    },
    Cancelled {
        cause: &'static str,
        reason: String,
        termination: Termination,
    },
    /// The host cannot judge the run: a criterion could not be decided, the safety audit is not
    /// clean, or a required measurement is missing.
    EvidenceInvalid {
        reason: String,
    },
    NotRun {
        reason: String,
    },
}

impl Verdict {
    pub fn class(&self) -> &'static str {
        match self {
            Verdict::Verified => "verified",
            Verdict::CompletedUnverified { .. } => "completed_unverified",
            Verdict::Incomplete { .. } => "incomplete",
            Verdict::RuntimeFailure { .. } => "runtime_failure",
            Verdict::Cancelled { .. } => "cancelled",
            Verdict::EvidenceInvalid { .. } => "evidence_invalid",
            Verdict::NotRun { .. } => "not_run",
        }
    }

    /// Whether the run reached an outcome the host could judge. Anything else makes a comparison
    /// incomplete.
    pub fn judged(&self) -> bool {
        matches!(
            self,
            Verdict::Verified
                | Verdict::CompletedUnverified { .. }
                | Verdict::Incomplete { .. }
                | Verdict::RuntimeFailure { .. }
        )
    }
}

/// A reference to one piece of host-observed evidence: which execution, of what, how it ended, and
/// a digest of exactly what it observed. The output itself is not copied into the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRef {
    pub execution_id: String,
    pub capability: String,
    pub status: &'static str,
    pub output_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cleanup {
    /// Nothing to clean (replay creates nothing).
    NotNeeded,
    Removed,
    Failed(String),
}

/// Everything recorded about one run.
#[derive(Debug, Clone)]
pub struct RunRecord {
    pub side: Side,
    pub strategy: String,
    pub strategy_version: u32,
    pub task: String,
    pub task_version: u32,
    pub task_digest: String,
    pub work_id: String,
    pub verdict: Verdict,
    pub terminal_state: Option<&'static str>,
    pub outcome_reason: Option<String>,
    pub delivery: Option<&'static str>,
    pub criteria: Vec<(String, CriterionResult)>,
    pub decisions: Vec<String>,
    pub executions: usize,
    pub executions_by_capability: BTreeMap<String, usize>,
    pub failed_observations: usize,
    pub model_calls: usize,
    /// `(prompt, completion)` as the provider reported them; `None` when none was reported.
    pub tokens: Option<(u64, u64)>,
    /// Observed wall clock. Never compared: it is not deterministic.
    pub duration: Option<Duration>,
    pub audit_clean: bool,
    pub audit_details: Vec<String>,
    pub trajectory_violations: usize,
    pub evidence: Vec<EvidenceRef>,
    pub isolation_root_named: bool,
    pub cleanup: Cleanup,
}

impl RunRecord {
    fn not_run(
        side: Side,
        strategy: &ResolvedStrategy,
        task: &TaskFixture,
        work_id: String,
        reason: String,
    ) -> Self {
        Self {
            side,
            strategy: strategy.strategy.id().into(),
            strategy_version: strategy.strategy.version(),
            task: task.id.clone(),
            task_version: task.version,
            task_digest: task.digest(),
            work_id,
            verdict: Verdict::NotRun { reason },
            terminal_state: None,
            outcome_reason: None,
            delivery: None,
            criteria: Vec::new(),
            decisions: Vec::new(),
            executions: 0,
            executions_by_capability: BTreeMap::new(),
            failed_observations: 0,
            model_calls: 0,
            tokens: None,
            duration: None,
            audit_clean: true,
            audit_details: Vec::new(),
            trajectory_violations: 0,
            evidence: Vec::new(),
            isolation_root_named: false,
            cleanup: Cleanup::NotNeeded,
        }
    }
}

/// What the comparison concludes. A winner is named only when every run was judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conclusion {
    /// Observe mode: only the baseline ran.
    NotApplicable(String),
    /// Some run was not judged (cancelled, not run, invalid evidence): no winner is declared.
    Incomplete(Vec<String>),
    CandidateVerifiesMore,
    BaselineVerifiesMore,
    /// The same tasks were verified on both sides. Other differences are listed, not ranked.
    SameVerifiedOutcomes,
    /// As many tasks verified on each side, but not the same ones: no side is ahead.
    DifferentTasksVerified,
}

impl Conclusion {
    pub fn name(&self) -> &'static str {
        match self {
            Conclusion::NotApplicable(_) => "not_applicable",
            Conclusion::Incomplete(_) => "incomplete",
            Conclusion::CandidateVerifiesMore => "candidate_verifies_more",
            Conclusion::BaselineVerifiesMore => "baseline_verifies_more",
            Conclusion::SameVerifiedOutcomes => "same_verified_outcomes",
            Conclusion::DifferentTasksVerified => "different_tasks_verified",
        }
    }

    /// The side the verified outcomes favour, when there is one and every run was judged.
    pub fn winner(&self) -> Option<Side> {
        match self {
            Conclusion::CandidateVerifiesMore => Some(Side::Candidate),
            Conclusion::BaselineVerifiesMore => Some(Side::Baseline),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Comparison {
    pub conclusion: Conclusion,
    pub verified_baseline: usize,
    pub verified_candidate: usize,
    pub tasks: usize,
    /// Observed differences between the sides, task by task (verdicts, decisions, executions).
    pub differences: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ExperimentResult {
    pub runs: Vec<RunRecord>,
    pub comparison: Comparison,
    pub cleanup_failures: Vec<String>,
}

/// Stops every run of an experiment at its next boundary; runs not yet started are not started.
#[derive(Clone, Debug, Default)]
pub struct CancelHandle(Arc<AtomicBool>);

impl CancelHandle {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// One run's work control: the experiment's cancellation and the run's own deadline.
struct RunControl {
    cancel: CancelHandle,
    deadline: Instant,
}

impl WorkControl for RunControl {
    fn stop_requested(&self) -> Option<StopCause> {
        if self.cancel.is_cancelled() {
            Some(StopCause::Cancelled)
        } else if Instant::now() >= self.deadline {
            Some(StopCause::Deadline)
        } else {
            None
        }
    }
}

/// No model: the call fails with a typed error and nothing leaves the process.
struct NoModel;

#[async_trait::async_trait]
impl ModelProvider for NoModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, FxError> {
        Err(FxError::Configuration(
            "this experiment configures no model; nothing was sent".into(),
        ))
    }
}

/// The task's recorded replies, in order. Recorded replies carry no real usage, so none is
/// reported (zero usage is read by Chip as "not reported").
struct RecordedModel(Mutex<VecDeque<String>>);

#[async_trait::async_trait]
impl ModelProvider for RecordedModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, FxError> {
        let next = self.0.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
        match next {
            Some(reply) => Ok(ModelResponse::new("recorded", reply, Usage::new(0, 0))),
            None => Err(FxError::Provider(
                "the recorded model replies are exhausted; nothing was sent".into(),
            )),
        }
    }
}

/// A disposable project directory and the capabilities offered over it.
struct Disposable {
    id: EnvironmentId,
    root: PathBuf,
    set: Arc<CapabilitySet>,
}

impl WorkEnvironment for Disposable {
    fn id(&self) -> &EnvironmentId {
        &self.id
    }

    fn capabilities(&self) -> Arc<CapabilitySet> {
        self.set.clone()
    }

    fn observation_invariants(&self) -> Vec<Arc<dyn ObservationInvariant>> {
        vec![
            chip_project::path_escape_invariant(&self.root),
            chip_project::out_of_root_write_invariant(&self.root),
            chip_project::host_path_leak_invariant(&self.root),
            chip_project::navigation_mismatch_invariant(&self.root),
        ]
    }

    fn description(&self) -> EnvironmentDescription {
        EnvironmentDescription::default()
    }
}

/// Removes a disposable directory, saying what happened. A directory that is already gone is
/// reported, not ignored: something other than the runner removed it.
pub fn remove_disposable(root: &Path) -> Cleanup {
    match std::fs::remove_dir_all(root) {
        Ok(()) if !root.exists() => Cleanup::Removed,
        Ok(()) => Cleanup::Failed("the directory still exists after removal".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Cleanup::Failed("the directory was already gone before cleanup".into())
        }
        Err(e) => Cleanup::Failed(format!("removal failed ({})", e.kind())),
    }
}

pub struct Runner {
    work_dir: PathBuf,
    cancel: CancelHandle,
    serial: AtomicU64,
}

impl Runner {
    /// Disposable environments are created under `work_dir`, one directory per run.
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        Self {
            work_dir: work_dir.into(),
            cancel: CancelHandle::default(),
            serial: AtomicU64::new(0),
        }
    }

    pub fn cancel_handle(&self) -> CancelHandle {
        self.cancel.clone()
    }

    pub async fn run(&self, resolved: &ResolvedExperiment) -> ExperimentResult {
        let mut runs = Vec::new();
        for task in &resolved.tasks {
            let sides: Vec<(Side, &ResolvedStrategy)> =
                std::iter::once((Side::Baseline, &resolved.baseline))
                    .chain(resolved.candidate.as_ref().map(|c| (Side::Candidate, c)))
                    .collect();
            for (side, strategy) in sides {
                let work_id = format!("{}-{}-{}", resolved.definition.id, side.name(), task.id);
                if self.cancel.is_cancelled() {
                    runs.push(RunRecord::not_run(
                        side,
                        strategy,
                        task,
                        work_id,
                        "the experiment was cancelled before this run started".into(),
                    ));
                    continue;
                }
                runs.push(self.run_one(resolved, side, strategy, task, work_id).await);
            }
        }
        let comparison = compare(resolved, &runs);
        let cleanup_failures = runs
            .iter()
            .filter_map(|r| match &r.cleanup {
                Cleanup::Failed(why) => Some(format!("{}: {why}", r.work_id)),
                _ => None,
            })
            .collect();
        ExperimentResult {
            runs,
            comparison,
            cleanup_failures,
        }
    }

    fn create(
        &self,
        resolved: &ResolvedExperiment,
        task: &TaskFixture,
        side: Side,
    ) -> Result<Disposable, String> {
        let n = self.serial.fetch_add(1, Ordering::SeqCst);
        let name = format!("chip-experiment-{}-{n}-{}", std::process::id(), side.name());
        std::fs::create_dir_all(&self.work_dir)
            .map_err(|e| format!("the experiment directory cannot be created ({})", e.kind()))?;
        let root = self.work_dir.join(name);
        if root.exists() {
            return Err("a disposable directory with this name already exists".into());
        }
        std::fs::create_dir(&root)
            .map_err(|e| format!("the disposable directory cannot be created ({})", e.kind()))?;
        let root = root
            .canonicalize()
            .map_err(|e| format!("the disposable directory cannot be resolved ({})", e.kind()))?;
        for (path, content) in &task.files {
            let file = root.join(path);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("the project cannot be prepared ({})", e.kind()))?;
            }
            std::fs::write(&file, content)
                .map_err(|e| format!("the project cannot be prepared ({})", e.kind()))?;
        }
        let mut set = CapabilitySet::new();
        for fixture in &resolved.fixtures {
            set = set.with(Arc::new(chip_core::AllowlistedBackend::new(
                fixture.backend(&root),
                resolved.definition.capabilities.clone(),
            )));
        }
        Ok(Disposable {
            id: EnvironmentId::new(format!("disposable-{n}")),
            root,
            set: Arc::new(set),
        })
    }

    async fn run_one(
        &self,
        resolved: &ResolvedExperiment,
        side: Side,
        strategy: &ResolvedStrategy,
        task: &TaskFixture,
        work_id: String,
    ) -> RunRecord {
        let definition = &resolved.definition;
        let mode = definition.mode;

        // The permitted execution context.
        let (set, invariants, disposable): (
            Arc<CapabilitySet>,
            Vec<Arc<dyn ObservationInvariant>>,
            Option<Disposable>,
        ) = match mode {
            Mode::Replay => {
                let declared = match descriptors(resolved, Path::new(REPLAY_ROOT)).await {
                    Ok(d) => d,
                    Err(why) => {
                        return RunRecord::not_run(
                            side,
                            strategy,
                            task,
                            work_id,
                            format!("declarations unavailable: {why}"),
                        );
                    }
                };
                let recording = task.recording.clone().expect("checked when resolved");
                let set = CapabilitySet::new().with(Arc::new(Replayed {
                    descriptors: declared,
                    recording,
                }));
                (Arc::new(set), Vec::new(), None)
            }
            Mode::Observe | Mode::Isolated => match self.create(resolved, task, side) {
                Ok(d) => (d.capabilities(), d.observation_invariants(), Some(d)),
                Err(why) => return RunRecord::not_run(side, strategy, task, work_id, why),
            },
        };

        let model: Arc<dyn ModelProvider> = match definition.model {
            ModelSpec::None => Arc::new(NoModel),
            ModelSpec::Recorded => Arc::new(RecordedModel(Mutex::new(
                task.model_replies.iter().cloned().collect(),
            ))),
        };
        let limits = WorkLimits {
            max_turns: definition.limits.max_turns,
            max_executions: definition.limits.max_executions,
        };
        let control = Arc::new(RunControl {
            cancel: self.cancel.clone(),
            deadline: Instant::now() + Duration::from_millis(definition.limits.deadline_ms),
        });
        let mut spec = WorkSpec::new(
            WorkId::new(work_id.clone()),
            WorkGoal::new(task.goal.clone()),
        )
        .with_limits(limits)
        .with_control(control);
        for criterion in &resolved.criteria {
            if let Some(predicate) = criterion.in_loop(task) {
                spec = spec.with_required_observation(predicate);
            }
        }
        for invariant in invariants {
            spec = spec.with_observation_invariant(invariant);
        }
        let declared: Vec<CapabilityId> = definition
            .capabilities
            .iter()
            .filter_map(|c| CapabilityId::new(c).ok())
            .collect();
        // Project state changes between requests: no observation answers a later request.
        for id in &declared {
            spec = spec.with_evidence_reuse_prohibited(id.clone());
        }

        let agent = Agent::with_model(model, definition.model.name())
            .with_capabilities(set.clone())
            .with_executor(set)
            .with_observer(Arc::new(ExecutionObserver));
        let report = agent
            .run_work_with_context_policy(
                &spec,
                &*strategy.strategy.decisions(),
                &ModelDecisionBoundary,
                &*strategy.strategy.context(),
            )
            .await;

        let audit = audit_safety(&report, &spec, &declared);
        let trajectory_violations = verify_trajectory(&report.events, &spec.limits).len();
        let utility = measure_utility(&report, &spec);
        let final_tree = disposable.as_ref().map(|d| d.root.as_path());
        let host = HostObservation {
            mode,
            task,
            report: &report,
            final_tree,
        };
        let criteria: Vec<(String, CriterionResult)> = resolved
            .criteria
            .iter()
            .map(|c| (c.id().to_string(), c.evaluate(&host)))
            .collect();
        let isolation_root_named = disposable.as_ref().is_some_and(|d| {
            let root = d.root.to_string_lossy();
            report.observations.iter().any(|o| {
                o.output
                    .as_deref()
                    .is_some_and(|t| t.contains(root.as_ref()))
            })
        });
        let cleanup = match &disposable {
            Some(d) => remove_disposable(&d.root),
            None => Cleanup::NotNeeded,
        };

        let tokens = match (utility.input_tokens, utility.output_tokens) {
            (Some(i), Some(o)) => Some((i, o)),
            _ => None,
        };
        let audit_clean = audit.is_clean() && trajectory_violations == 0;
        let verdict = classify(
            &report,
            &criteria,
            audit_clean,
            definition.evidence.contains(&EvidenceField::Tokens)
                && utility.model_calls > 0
                && tokens.is_none(),
        );
        RunRecord {
            side,
            strategy: strategy.strategy.id().into(),
            strategy_version: strategy.strategy.version(),
            task: task.id.clone(),
            task_version: task.version,
            task_digest: task.digest(),
            work_id,
            verdict,
            terminal_state: Some(report.outcome.terminal_state().name()),
            outcome_reason: outcome_reason(&report.outcome),
            delivery: match &report.outcome {
                WorkOutcome::Failed { delivery, .. } | WorkOutcome::Cancelled { delivery, .. } => {
                    delivery.map(|d| d.as_str())
                }
                _ => None,
            },
            criteria,
            decisions: report
                .decisions
                .iter()
                .map(|d| {
                    let source = match d.source {
                        chip_core::DecisionSource::Local => "local",
                        chip_core::DecisionSource::Model => "model",
                    };
                    format!("{source}: {}", d.decision.label())
                })
                .collect(),
            executions: utility.executions,
            executions_by_capability: utility.executions_by_capability.clone(),
            failed_observations: utility.failed_observations,
            model_calls: utility.model_calls,
            tokens,
            duration: Some(report.summary.elapsed),
            audit_clean,
            audit_details: audit.details.clone(),
            trajectory_violations,
            evidence: report
                .observations
                .iter()
                .zip(&report.origins)
                .map(|(o, origin)| EvidenceRef {
                    execution_id: o.execution_id.0.clone(),
                    capability: origin.capability.as_str().to_string(),
                    status: if o.status == ExecutionStatus::Success {
                        "success"
                    } else {
                        "failure"
                    },
                    output_sha256: digest(o.output.as_deref().unwrap_or_default().as_bytes()),
                })
                .collect(),
            isolation_root_named,
            cleanup,
        }
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

/// The host's verdict on one run, from the runtime's outcome, the criteria and the audit only.
pub(crate) fn classify(
    report: &WorkReport,
    criteria: &[(String, CriterionResult)],
    audit_clean: bool,
    required_tokens_missing: bool,
) -> Verdict {
    if !audit_clean {
        return Verdict::EvidenceInvalid {
            reason: "the safety audit or trajectory check found violations; the run's evidence cannot be trusted".into(),
        };
    }
    match &report.outcome {
        WorkOutcome::Cancelled {
            cause,
            reason,
            remote,
            ..
        } => Verdict::Cancelled {
            cause: cause.name(),
            reason: reason.clone(),
            termination: match remote {
                Some(RemoteCancellation::Unconfirmed) => Termination::Uncertain(
                    "a model call was abandoned and its remote stop is unconfirmed".into(),
                ),
                _ => Termination::Known,
            },
        },
        WorkOutcome::Failed { reason, delivery } => Verdict::RuntimeFailure {
            reason: reason.clone(),
            delivery: delivery.map(|d| d.as_str()),
        },
        WorkOutcome::Blocked { reason } => Verdict::Incomplete {
            state: "blocked",
            reason: reason.clone(),
        },
        WorkOutcome::Escalated { reason } => Verdict::Incomplete {
            state: "escalated",
            reason: reason.clone(),
        },
        WorkOutcome::LimitReached { limit } => Verdict::Incomplete {
            state: "limit_reached",
            reason: format!("{} limit reached", limit.name()),
        },
        WorkOutcome::Completed { .. } => {
            let failed: Vec<String> = criteria
                .iter()
                .filter(|(_, r)| matches!(r, CriterionResult::NotMet(_)))
                .map(|(id, _)| id.clone())
                .collect();
            let unavailable: Vec<&str> = criteria
                .iter()
                .filter(|(_, r)| matches!(r, CriterionResult::Unavailable(_)))
                .map(|(id, _)| id.as_str())
                .collect();
            if !failed.is_empty() {
                Verdict::CompletedUnverified { failed }
            } else if !unavailable.is_empty() {
                Verdict::EvidenceInvalid {
                    reason: format!("criteria could not be decided: {}", unavailable.join(", ")),
                }
            } else if criteria.is_empty() {
                Verdict::EvidenceInvalid {
                    reason: "no criterion was evaluated".into(),
                }
            } else if required_tokens_missing {
                Verdict::EvidenceInvalid {
                    reason: "token counts are required and the provider reported none".into(),
                }
            } else {
                Verdict::Verified
            }
        }
    }
}

fn compare(resolved: &ResolvedExperiment, runs: &[RunRecord]) -> Comparison {
    let tasks = resolved.tasks.len();
    let verified = |side: Side| {
        runs.iter()
            .filter(|r| r.side == side && r.verdict == Verdict::Verified)
            .count()
    };
    let (vb, vc) = (verified(Side::Baseline), verified(Side::Candidate));
    if resolved.candidate.is_none() {
        return Comparison {
            conclusion: Conclusion::NotApplicable(
                "observe mode executes only the baseline; nothing is compared".into(),
            ),
            verified_baseline: vb,
            verified_candidate: 0,
            tasks,
            differences: Vec::new(),
        };
    }
    let mut differences = Vec::new();
    let mut unjudged = Vec::new();
    for task in &resolved.tasks {
        let pick = |side: Side| runs.iter().find(|r| r.side == side && r.task == task.id);
        let (b, c) = (pick(Side::Baseline), pick(Side::Candidate));
        for (side, run) in [(Side::Baseline, b), (Side::Candidate, c)] {
            match run {
                None => unjudged.push(format!("{}: no {} result", task.id, side.name())),
                Some(r) if !r.verdict.judged() => unjudged.push(format!(
                    "{}: the {} run is {}",
                    task.id,
                    side.name(),
                    r.verdict.class()
                )),
                Some(_) => {}
            }
        }
        if let (Some(b), Some(c)) = (b, c) {
            if b.verdict.class() != c.verdict.class() {
                differences.push(format!(
                    "{}: baseline {}, candidate {}",
                    task.id,
                    b.verdict.class(),
                    c.verdict.class()
                ));
            }
            if b.decisions != c.decisions {
                differences.push(format!(
                    "{}: decisions differ (baseline [{}], candidate [{}])",
                    task.id,
                    b.decisions.join(", "),
                    c.decisions.join(", ")
                ));
            }
            if b.executions != c.executions {
                differences.push(format!(
                    "{}: executions differ (baseline {}, candidate {})",
                    task.id, b.executions, c.executions
                ));
            }
        }
    }
    let conclusion = if !unjudged.is_empty() {
        Conclusion::Incomplete(unjudged)
    } else if vc > vb {
        Conclusion::CandidateVerifiesMore
    } else if vb > vc {
        Conclusion::BaselineVerifiesMore
    } else {
        let set = |side: Side| {
            runs.iter()
                .filter(|r| r.side == side && r.verdict == Verdict::Verified)
                .map(|r| r.task.as_str())
                .collect::<std::collections::BTreeSet<_>>()
        };
        if set(Side::Baseline) == set(Side::Candidate) {
            Conclusion::SameVerifiedOutcomes
        } else {
            Conclusion::DifferentTasksVerified
        }
    };
    Comparison {
        conclusion,
        verified_baseline: vb,
        verified_candidate: vc,
        tasks,
        differences,
    }
}
