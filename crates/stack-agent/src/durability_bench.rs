//! Durability benchmark: does FeltDB agent state (`work --state-db`) make work more *effective*, or
//! only safer, and what does it cost when nothing goes wrong?
//!
//! A deterministic, paired experiment. Every case runs the same task twice, once per arm:
//!
//! * **A (legacy)**: Chip's work loop with no durable state, as `work` runs without `--state-db`.
//! * **B (durable)**: the same loop composed exactly as `work --state-db FILE --task-id ID` composes
//!   it: `chip_agent_state::begin`, the durable executor wrapper, `DurableWork::finish`, over a real
//!   file-backed FeltDB that is closed and reopened on every restart.
//!
//! Both arms share the task, the initial world, the scripted model, the capabilities, the executor,
//! the limits and the oracle. Only the arm differs. The scripted model decides only from the
//! observations Chip sends it, as a real model would; it never reads the world.
//!
//! The **world** is two append-only files: `effects.log` (observable: `world.inspect` reports it)
//! and `outbox.log` (not observable: a sent notification, like an e-mail or a charge, cannot be
//! read back). In the deduplicated task the notification service (a fake) keeps each
//! notification's idempotency key with it, and writes to `replays.log` each request it answered
//! from an earlier notification instead of sending another. The **oracle** judges the world on disk: each required effect exactly once. A model's
//! claim of success is never the criterion.
//!
//! Faults are injected at fixed lifecycle points: a crash (the process's future is dropped and every
//! handle closed) before an action's effect, after its effect but before its outcome is recorded,
//! or between actions; two workers started together; and a worker that stalls mid-action while
//! another takes over. A restart in arm B waits out the lease on an injected clock; that wait is
//! recorded as `lease_wait_ms` (a modelled cost), not as wall-clock time.
//!
//! This module is a measurement. It changes nothing about production behaviour.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use chip_agent_state::feltdb::FeltDb;
use chip_agent_state::feltdb::agent_state::AuthorityClock;
use chip_agent_state::{AgentStore, Begun, DurableConfig, StateError};
use chip_core::{
    Agent, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, ExecutionError, ExecutionObserver, ExecutionRequest,
    ExecutionResult, Executor, Idempotency, InputValue, ModelDecisionBoundary, NoLocalPolicy,
    Observation, ObservationPredicate, WorkGoal, WorkId, WorkLimits, WorkOutcome, WorkReport,
    WorkSpec,
};
use chip_inference::{FxError, MessageRole, ModelProvider, ModelRequest, ModelResponse, Usage};
use serde_json::{Value, json};
use tokio::sync::Notify;

pub const SCHEMA: &str = "chip.durability-bench.result.v1";
/// `v2` adds `notify-once-deduplicated`; the `v1` tasks are unchanged.
pub const TASK_SET: &str = "chip.durability-bench.tasks.v2";
/// The lease arm B runs under. A restart must wait it out; the wait is modelled, not slept.
pub const LEASE_MS: u64 = 60_000;
/// Worker runs one case may start before it is recorded as not finished.
pub const MAX_RUNS: usize = 5;

// ---- the experiment's axes ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Arm {
    Legacy,
    Durable,
}

impl Arm {
    pub const ALL: [Arm; 2] = [Arm::Legacy, Arm::Durable];
    pub fn name(self) -> &'static str {
        match self {
            Arm::Legacy => "A-legacy",
            Arm::Durable => "B-durable",
        }
    }
}

/// The versioned task set ([`TASK_SET`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Task {
    /// Apply steps 1, 2 and 3, each exactly once. Every effect is observable.
    Steps,
    /// Apply step 1, then send notification 1 exactly once. The notification is not observable.
    Notify,
    /// As [`Task::Notify`], but the notification service applies at most one notification per
    /// idempotency key and declares it (a fake service: the benchmark's stand-in for an external
    /// system that deduplicates). Still not observable.
    NotifyDeduplicated,
}

impl Task {
    pub const ALL: [Task; 3] = [Task::Steps, Task::Notify, Task::NotifyDeduplicated];
    pub fn name(self) -> &'static str {
        match self {
            Task::Steps => "steps-3-observable",
            Task::Notify => "notify-once-unobservable",
            Task::NotifyDeduplicated => "notify-once-deduplicated",
        }
    }
    pub(crate) fn goal(self) -> &'static str {
        match self {
            Task::Steps => "Apply steps 1, 2 and 3 to the world, each exactly once, then report.",
            Task::Notify | Task::NotifyDeduplicated => {
                "Apply step 1 to the world, then send notification 1 exactly once, then report."
            }
        }
    }
    fn steps(self) -> u64 {
        match self {
            Task::Steps => 3,
            Task::Notify | Task::NotifyDeduplicated => 1,
        }
    }
    fn notifies(self) -> bool {
        matches!(self, Task::Notify | Task::NotifyDeduplicated)
    }
    /// What `world.notify` declares it does with an idempotency key.
    fn notify_idempotency(self) -> Idempotency {
        match self {
            Task::NotifyDeduplicated => Idempotency::Deduplicated,
            _ => Idempotency::NotCarried,
        }
    }
    /// Each capability's declaration, as the host passes it to durable state.
    pub(crate) fn idempotency(self) -> BTreeMap<String, Idempotency> {
        [
            ("world.inspect", Idempotency::NotCarried),
            ("world.apply", Idempotency::NotCarried),
            ("world.notify", self.notify_idempotency()),
        ]
        .into_iter()
        .map(|(id, i)| (id.to_string(), i))
        .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scenario {
    /// One worker, no fault: the overhead and outcome-equivalence baseline.
    Normal,
    /// Crash during the 4th execution, before its effect (in B: after the durable intent).
    CrashBeforeEffect,
    /// Crash during the 4th execution, after its effect, before its outcome is recorded.
    CrashAfterEffect,
    /// Crash at the 5th model call: the 4th action ran and its outcome was recorded.
    CrashBetweenActions,
    /// Two workers started together on the same task.
    ConcurrentStart,
    /// Worker 1 stalls before its 4th action's effect; it is presumed dead and a second worker is
    /// started; then worker 1 resumes.
    StaleOwner,
}

impl Scenario {
    pub const ALL: [Scenario; 6] = [
        Scenario::Normal,
        Scenario::CrashBeforeEffect,
        Scenario::CrashAfterEffect,
        Scenario::CrashBetweenActions,
        Scenario::ConcurrentStart,
        Scenario::StaleOwner,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Scenario::Normal => "normal",
            Scenario::CrashBeforeEffect => "restart-crash-before-effect",
            Scenario::CrashAfterEffect => "ambiguous-crash-after-effect",
            Scenario::CrashBetweenActions => "restart-crash-between-actions",
            Scenario::ConcurrentStart => "contention-concurrent-start",
            Scenario::StaleOwner => "contention-stale-owner",
        }
    }
}

/// The execution (1-based, per worker run) and model call at which faults fire.
const FAULT_EXECUTION: usize = 4;
const FAULT_MODEL_CALL: usize = 5;

// ---- the world and its oracle -------------------------------------------------------------------

#[derive(Clone)]
struct World {
    dir: PathBuf,
}

impl World {
    fn create(dir: &Path) -> Self {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        for f in ["effects.log", "outbox.log", "replays.log"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        Self { dir: dir.into() }
    }
    fn lines(&self, file: &str) -> Vec<u64> {
        std::fs::read_to_string(self.dir.join(file))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_whitespace().next()?.parse().ok())
            .collect()
    }
    /// The idempotency keys the notification service sent under, one per notification.
    fn lines_raw(&self, file: &str) -> usize {
        std::fs::read_to_string(self.dir.join(file))
            .unwrap_or_default()
            .lines()
            .count()
    }
    fn keys(&self, file: &str) -> Vec<String> {
        std::fs::read_to_string(self.dir.join(file))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
            .collect()
    }
    fn append(&self, file: &str, line: impl std::fmt::Display) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(self.dir.join(file))
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }
}

/// The oracle's verdict on the world: every required effect exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    pub effects: Vec<u64>,
    pub outbox: Vec<u64>,
    pub duplicate_effects: usize,
    pub missing_effects: usize,
    pub verified_complete: bool,
}

fn judge(task: Task, world: &World) -> Judged {
    let effects = world.lines("effects.log");
    let outbox = world.lines("outbox.log");
    let mut duplicate_effects = 0;
    let mut missing_effects = 0;
    let mut count = |log: &[u64], want: &[u64]| {
        for w in want {
            match log.iter().filter(|x| *x == w).count() {
                0 => missing_effects += 1,
                n => duplicate_effects += n - 1,
            }
        }
        duplicate_effects += log.iter().filter(|x| !want.contains(x)).count();
    };
    let want_effects: Vec<u64> = (1..=task.steps()).collect();
    count(&effects, &want_effects);
    let want_outbox: Vec<u64> = if task.notifies() { vec![1] } else { vec![] };
    count(&outbox, &want_outbox);
    Judged {
        verified_complete: duplicate_effects == 0 && missing_effects == 0,
        effects,
        outbox,
        duplicate_effects,
        missing_effects,
    }
}

// ---- capabilities, executor and faults ----------------------------------------------------------

struct Caps(Task);

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        let step = || CapabilityInput {
            name: "step".into(),
            description: "the step number".into(),
            required: true,
        };
        Ok(vec![
            CapabilityDescriptor::new(
                CapabilityId::new("world.inspect")?,
                "Inspect",
                "Lists the steps applied to the world",
            )
            .without_evidence_reuse(),
            CapabilityDescriptor {
                inputs: vec![step()],
                ..CapabilityDescriptor::new(
                    CapabilityId::new("world.apply")?,
                    "Apply",
                    "Applies one step to the world",
                )
                .without_evidence_reuse()
            },
            CapabilityDescriptor {
                inputs: vec![step()],
                ..CapabilityDescriptor::new(
                    CapabilityId::new("world.notify")?,
                    "Notify",
                    "Sends one notification; it cannot be read back",
                )
                .without_evidence_reuse()
                .with_idempotency(self.0.notify_idempotency())
            },
        ])
    }
    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

/// Where, in one worker run, a fault fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    /// Crash in the given execution, before its effect.
    CrashBeforeEffect,
    /// Crash in the given execution, after its effect.
    CrashAfterEffect,
    /// Crash at the given model call.
    CrashAtModelCall,
    /// Stall before the effect until released, then continue.
    StallBeforeEffect,
}

/// What a worker run shares with its harness: the crash signal and the stall gate.
#[derive(Default)]
struct Signals {
    crashed: Notify,
    stalled: Notify,
    release: Notify,
}

struct WorldExecutor {
    world: World,
    /// The notification service deduplicates on the idempotency key (the deduplicated task).
    deduplicates: bool,
    fault: Fault,
    executions: AtomicUsize,
    signals: Arc<Signals>,
}

fn step_of(request: &ExecutionRequest) -> u64 {
    match request.inputs.get("step") {
        Some(InputValue::Integer(n)) => *n as u64,
        Some(InputValue::Text(t)) => t.parse().unwrap_or(0),
        _ => 0,
    }
}

#[async_trait::async_trait]
impl Executor for WorldExecutor {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let n = self.executions.fetch_add(1, Ordering::SeqCst) + 1;
        let at_fault = n == FAULT_EXECUTION;
        if at_fault && self.fault == Fault::CrashBeforeEffect {
            self.signals.crashed.notify_one();
            std::future::pending::<()>().await;
        }
        if at_fault && self.fault == Fault::StallBeforeEffect {
            self.signals.stalled.notify_one();
            self.signals.release.notified().await;
        }
        let output = match r.intent.as_str() {
            "world.inspect" => {
                let applied: Vec<String> = self
                    .world
                    .lines("effects.log")
                    .iter()
                    .map(u64::to_string)
                    .collect();
                format!("applied=[{}]", applied.join(","))
            }
            "world.apply" => {
                let s = step_of(&r);
                self.world.append("effects.log", s);
                format!("applied step {s}")
            }
            "world.notify" => {
                let s = step_of(&r);
                let sent = format!("sent notification {s}");
                match (self.deduplicates, &r.idempotency_key) {
                    // The fake service's own deduplication: a key it has sent under is answered,
                    // not sent again. The key is kept on the notification's own line, so the send
                    // and its key are one append.
                    (true, Some(key)) if self.world.keys("outbox.log").contains(key) => {
                        self.world.append("replays.log", key);
                        sent
                    }
                    (true, Some(key)) => {
                        self.world.append("outbox.log", format!("{s} {key}"));
                        sent
                    }
                    _ => {
                        self.world.append("outbox.log", s);
                        sent
                    }
                }
            }
            other => return Err(ExecutionError::InvalidRequest(format!("unknown {other}"))),
        };
        if at_fault && self.fault == Fault::CrashAfterEffect {
            self.signals.crashed.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(ExecutionResult::success(r.id, output))
    }
}

// ---- the scripted model --------------------------------------------------------------------------

/// Decides only from the observations in the request. Inspects after every action; applies the
/// lowest step the last inspection does not show; then notifies if the task needs it and no
/// notification is among the observations; then completes.
struct ReactiveModel {
    task: Task,
    calls: AtomicUsize,
    request_bytes: AtomicUsize,
    crash_at_call: Option<usize>,
    signals: Arc<Signals>,
}

fn observation_outputs(request: &ModelRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::System && m.content.starts_with("Observation:"))
        .filter_map(|m| m.content.split("\noutput: ").nth(1))
        .map(|o| o.trim().trim_matches('"').to_string())
        .collect()
}

/// The steps and notifications Chip tells the model an operator settled as having taken effect
/// (its "Ruled out" lines). A settlement is a conclusion, not an observation; the model treats it
/// as done and does not ask for it again.
fn settled_effects(request: &ModelRequest) -> (Vec<u64>, bool) {
    let (mut steps, mut notified) = (Vec::new(), false);
    for line in request
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::User)
        .flat_map(|m| m.content.lines())
        .filter(|l| l.contains("an operator (") && l.contains("concluded that its effect occurred"))
    {
        let step = line
            .split_once("(step=")
            .and_then(|(_, rest)| rest.split(')').next())
            .and_then(|n| n.parse::<u64>().ok());
        if line.contains("capability world.apply") {
            steps.extend(step);
        } else if line.contains("capability world.notify") {
            notified = true;
        }
    }
    (steps, notified)
}

impl ReactiveModel {
    fn decide(&self, outputs: &[String], settled: (Vec<u64>, bool)) -> Value {
        let request = |cap: &str, step: Option<u64>| match step {
            Some(s) => {
                json!({"decision": "request_capability", "capability": cap, "inputs": {"step": s}})
            }
            None => json!({"decision": "request_capability", "capability": cap}),
        };
        let Some(last) = outputs.last().filter(|o| o.starts_with("applied=")) else {
            return request("world.inspect", None);
        };
        let mut applied: Vec<u64> = last
            .trim_start_matches("applied=[")
            .trim_end_matches(']')
            .split(',')
            .filter_map(|s| s.parse().ok())
            .collect();
        applied.extend(settled.0);
        if let Some(next) = (1..=self.task.steps()).find(|s| !applied.contains(s)) {
            return request("world.apply", Some(next));
        }
        let notified = settled.1 || outputs.iter().any(|o| o.starts_with("sent notification"));
        if self.task.notifies() && !notified {
            return request("world.notify", Some(1));
        }
        json!({"decision": "complete", "summary": "every required step is applied"})
    }
}

#[async_trait::async_trait]
impl ModelProvider for ReactiveModel {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, FxError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.request_bytes.fetch_add(
            request.messages.iter().map(|m| m.content.len()).sum(),
            Ordering::SeqCst,
        );
        if self.crash_at_call == Some(call) {
            self.signals.crashed.notify_one();
            std::future::pending::<()>().await;
        }
        // A model call takes time: let a concurrent worker run in between.
        tokio::task::yield_now().await;
        let decision = self.decide(&observation_outputs(&request), settled_effects(&request));
        Ok(ModelResponse::new(
            "scripted",
            decision.to_string(),
            Usage::new(1, 1),
        ))
    }
}

/// Chip's own goal check: the work may complete only once an inspection shows every step.
#[derive(Debug)]
struct AllStepsApplied(u64);

impl ObservationPredicate for AllStepsApplied {
    fn describe(&self) -> String {
        format!("an inspection shows steps 1 to {}", self.0)
    }
    fn satisfied_by(&self, o: &Observation) -> bool {
        o.output.as_deref().is_some_and(|out| {
            out.starts_with("applied=")
                && (1..=self.0).all(|s| {
                    out.trim_start_matches("applied=[")
                        .trim_end_matches(']')
                        .split(',')
                        .any(|x| x == s.to_string())
                })
        })
    }
}

// ---- the injected decision clock (arm B) --------------------------------------------------------

#[derive(Default)]
struct Clock(AtomicU64);

impl AuthorityClock for Clock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + self.0.load(Ordering::SeqCst)
    }
}

// ---- one worker run ---------------------------------------------------------------------------------

/// What one worker run (one process lifetime) did.
#[derive(Debug, Clone)]
pub struct Run {
    pub worker: String,
    /// `ran`, `crashed`, `already_finished`, or a refusal: `lease_held`, `outcome_uncertain`,
    /// `state_error`.
    pub start: String,
    /// Chip's terminal state, when the loop ran to its end.
    pub outcome: Option<String>,
    /// Arm A: Chip completed. Arm B: Chip completed and FeltDB recorded `completed`.
    pub reported_success: bool,
    /// Arm B: what FeltDB holds after the run, or the error that stopped it.
    pub durable: Option<String>,
    pub model_calls: usize,
    pub request_bytes: usize,
    pub executions: usize,
    pub wall_ms: f64,
    /// Arm B: time spent in `begin` and `finish`.
    pub persistence_ms: f64,
    pub persistence_failures: usize,
    pub uncertain_intents: Vec<String>,
    /// Arm B: open intents this run started with that it could settle by performing the same
    /// logical action again under its key.
    pub reissued: usize,
}

struct Case {
    task: Task,
    arm: Arm,
    world: World,
    db_path: PathBuf,
    clock: Arc<Clock>,
    task_id: String,
}

impl Case {
    fn store(&self) -> Result<AgentStore, StateError> {
        let db = FeltDb::open(&self.db_path).map_err(|e| StateError::Storage {
            operation: "open",
            detail: e.to_string(),
        })?;
        Ok(AgentStore::with_clock(db, self.clock.clone()))
    }

    fn spec(&self) -> WorkSpec {
        WorkSpec::new(WorkId::new(&self.task_id), WorkGoal::new(self.task.goal()))
            .with_limits(WorkLimits {
                max_turns: 16,
                max_executions: 12,
            })
            .with_required_observation(Arc::new(AllStepsApplied(self.task.steps())))
    }

    /// One process lifetime: start, run the loop unless a fault crashes it, and finish.
    async fn run(&self, worker: &str, fault: Fault, signals: Arc<Signals>) -> Run {
        let model = Arc::new(ReactiveModel {
            task: self.task,
            calls: AtomicUsize::new(0),
            request_bytes: AtomicUsize::new(0),
            crash_at_call: (fault == Fault::CrashAtModelCall).then_some(FAULT_MODEL_CALL),
            signals: signals.clone(),
        });
        let executor = Arc::new(WorldExecutor {
            world: self.world.clone(),
            deduplicates: self.task == Task::NotifyDeduplicated,
            fault,
            executions: AtomicUsize::new(0),
            signals: signals.clone(),
        });
        let mut run = Run {
            worker: worker.into(),
            start: "ran".into(),
            outcome: None,
            reported_success: false,
            durable: None,
            model_calls: 0,
            request_bytes: 0,
            executions: 0,
            wall_ms: 0.0,
            persistence_ms: 0.0,
            persistence_failures: 0,
            uncertain_intents: Vec::new(),
            reissued: 0,
        };
        let started = Instant::now();
        let mut persistence = Duration::ZERO;
        // Arm B: the composition `work --state-db` uses.
        let durable = match self.arm {
            Arm::Legacy => None,
            Arm::Durable => {
                let t = Instant::now();
                let begun = self.store().and_then(|store| {
                    chip_agent_state::begin(
                        &store,
                        &self.task_id,
                        self.task.goal(),
                        &DurableConfig {
                            actor: worker.into(),
                            lease_ms: LEASE_MS,
                            idempotency: self.task.idempotency(),
                            // No effect is declared: every interrupted action stays uncertain.
                            ..Default::default()
                        },
                    )
                });
                persistence += t.elapsed();
                match begun {
                    Ok(Begun::Ready(work)) => {
                        run.reissued = work.reissuable().len();
                        Some(work)
                    }
                    Ok(Begun::Finished(h)) => {
                        run.start = "already_finished".into();
                        run.durable = Some(h.task.status.clone());
                        run.reported_success = h.task.status == "completed";
                        run.wall_ms = ms(started.elapsed());
                        run.persistence_ms = ms(persistence);
                        return run;
                    }
                    Err(e) => {
                        run.start = match &e {
                            StateError::LeaseHeld { .. } => "lease_held",
                            StateError::OutcomeUncertain { intents, .. } => {
                                run.uncertain_intents = intents.clone();
                                "outcome_uncertain"
                            }
                            _ => "state_error",
                        }
                        .into();
                        run.durable = Some(e.to_string());
                        run.wall_ms = ms(started.elapsed());
                        run.persistence_ms = ms(persistence);
                        return run;
                    }
                }
            }
        };
        let exec: Arc<dyn Executor> = match &durable {
            Some(d) => d.executor(executor.clone()),
            None => executor.clone(),
        };
        let agent = Agent::with_model(model.clone(), "scripted")
            .with_capabilities(Arc::new(Caps(self.task)))
            .with_executor(exec)
            .with_observer(Arc::new(ExecutionObserver));
        // As `work --state-db` does: a resumed attempt starts from what FeltDB recorded.
        let spec = match &durable {
            Some(d) => self
                .spec()
                .with_restored(d.restored())
                .with_settled(d.settled_actions()),
            None => self.spec(),
        };
        let report: Option<WorkReport> = tokio::select! {
            r = agent.run_work(&spec, &NoLocalPolicy, &ModelDecisionBoundary) => Some(r),
            _ = signals.crashed.notified() => None,
        };
        run.model_calls = model.calls.load(Ordering::SeqCst);
        run.request_bytes = model.request_bytes.load(Ordering::SeqCst);
        run.executions = executor.executions.load(Ordering::SeqCst);
        match report {
            None => run.start = "crashed".into(),
            Some(report) => {
                let completed = matches!(report.outcome, WorkOutcome::Completed { .. });
                run.outcome = Some(report.outcome.terminal_state().name().into());
                match &durable {
                    None => run.reported_success = completed,
                    Some(d) => {
                        let t = Instant::now();
                        let finished = d.finish(&report, completed);
                        persistence += t.elapsed();
                        run.persistence_failures = d.failures().len();
                        match finished {
                            Ok(f) => {
                                run.reported_success = completed && f.status == "completed";
                                run.durable = Some(f.status);
                            }
                            Err(e) => run.durable = Some(format!("not recorded: {e}")),
                        }
                    }
                }
            }
        }
        run.wall_ms = ms(started.elapsed());
        run.persistence_ms = ms(persistence);
        run
    }

    /// The operator's settlement of an uncertain intent in arm B: verify the outcome outside the
    /// agent (here, from the world on disk) and record it with the shipped settlement
    /// (`stack-agent agent-state settle`), naming who decided and on what evidence.
    fn settle_from_world(&self, key: &str) -> bool {
        let now = judge(self.task, &self.world);
        // The fault always hits the 4th execution: `apply 2` (Steps) or the `notify` (Notify).
        let executed = match self.task {
            Task::Steps => now.effects.contains(&2),
            Task::Notify | Task::NotifyDeduplicated => !now.outbox.is_empty(),
        };
        let Ok(store) = self.store() else {
            return false;
        };
        chip_agent_state::settle(
            &store,
            &self.task_id,
            key,
            executed,
            "bench-operator",
            "read from the world on disk by the benchmark's operator",
        )
        .is_ok()
    }

    /// What a released lease costs: it expires 1 ms after its release (contract v1 has no
    /// release operation; see `chip_agent_state::begin`). Twice: the refusal's, then the
    /// settlement's.
    fn wait_released(&self, waited: &mut u64) {
        self.clock.0.fetch_add(2, Ordering::SeqCst);
        *waited += 2;
    }

    fn wait_out_lease(&self, waited: &mut u64) {
        self.clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);
        *waited += LEASE_MS + 1;
    }
}

fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1_000_000.0).round() / 1000.0
}

// ---- one paired case ----------------------------------------------------------------------------------

/// Runs one (scenario, task, arm) case to its end and returns its record.
pub async fn run_case(scenario: Scenario, task: Task, arm: Arm, root: &Path, repeat: u32) -> Value {
    let dir = root.join(format!(
        "{}-{}-{}-{repeat}",
        scenario.name(),
        task.name(),
        arm.name()
    ));
    let world = World::create(&dir.join("world"));
    let case = Case {
        task,
        arm,
        world,
        db_path: dir.join("agent-state.log"),
        clock: Arc::new(Clock::default()),
        task_id: format!("bench-{}-{repeat}", task.name()),
    };
    let started = Instant::now();
    let mut runs: Vec<Run> = Vec::new();
    let mut lease_wait_ms = 0;
    let mut operator_interventions = 0;
    let mut safe_refusals = 0usize;

    // The fault phase.
    match scenario {
        Scenario::Normal => runs.push(case.run("w1", Fault::None, Arc::default()).await),
        Scenario::CrashBeforeEffect
        | Scenario::CrashAfterEffect
        | Scenario::CrashBetweenActions => {
            let fault = match scenario {
                Scenario::CrashBeforeEffect => Fault::CrashBeforeEffect,
                Scenario::CrashAfterEffect => Fault::CrashAfterEffect,
                _ => Fault::CrashAtModelCall,
            };
            runs.push(case.run("w1", fault, Arc::default()).await);
        }
        Scenario::ConcurrentStart => {
            let (a, b) = tokio::join!(
                case.run("w1", Fault::None, Arc::default()),
                case.run("w2", Fault::None, Arc::default()),
            );
            runs.push(a);
            runs.push(b);
        }
        Scenario::StaleOwner => {
            let signals = Arc::new(Signals::default());
            let w1 = case.run("w1", Fault::StallBeforeEffect, signals.clone());
            let takeover = async {
                signals.stalled.notified().await;
                // Worker 1 is presumed dead. Arm B must wait for its lease; arm A need not.
                if arm == Arm::Durable {
                    case.wait_out_lease(&mut lease_wait_ms);
                }
                let w2 = case.run("w2", Fault::None, Arc::default()).await;
                signals.release.notify_one();
                w2
            };
            let (a, b) = tokio::join!(w1, takeover);
            runs.push(b);
            runs.push(a);
        }
    }

    // The recovery phase: the operator restarts the work until FeltDB (B) or the world (A) says it
    // is done, or the runs run out. Arm A restarts what did not report success; arm B restarts
    // what FeltDB does not hold as finished, reconciling an uncertain intent from the world.
    while runs.len() < MAX_RUNS {
        let last = runs.last().unwrap();
        let any_success = runs.iter().any(|r| r.reported_success);
        if any_success {
            break;
        }
        match arm {
            Arm::Legacy => {}
            Arm::Durable => {
                if last.start == "outcome_uncertain" {
                    // The refusal released its lease and the settlement releases its own, so
                    // nothing is waited out; a released lease ends 1 ms later.
                    for key in last.uncertain_intents.clone() {
                        if case.settle_from_world(&key) {
                            operator_interventions += 1;
                        }
                    }
                    case.wait_released(&mut lease_wait_ms);
                } else {
                    // Any other restart in B waits for the previous owner's lease.
                    case.wait_out_lease(&mut lease_wait_ms);
                }
            }
        }
        let n = runs.len() + 1;
        runs.push(
            case.run(&format!("w{n}"), Fault::None, Arc::default())
                .await,
        );
    }

    let judged = judge(task, &case.world);
    safe_refusals += runs
        .iter()
        .filter(|r| r.start == "outcome_uncertain")
        .count();
    let reported_success = runs.iter().any(|r| r.reported_success);
    let false_successes = runs
        .iter()
        .filter(|r| r.reported_success && r.start == "ran")
        .count()
        .saturating_sub(usize::from(judged.verified_complete));
    let classification = if judged.verified_complete && reported_success {
        "success"
    } else if reported_success {
        "false_success"
    } else if judged.verified_complete {
        "completed_unproven"
    } else if runs.iter().any(|r| r.start == "outcome_uncertain") {
        "unresolved"
    } else {
        "failure"
    };
    let interrupted = scenario != Scenario::Normal;
    let first = &runs[0];
    json!({
        "schema": SCHEMA,
        "task_set": TASK_SET,
        "scenario": scenario.name(),
        "task": task.name(),
        "arm": arm.name(),
        "repeat": repeat,
        "classification": classification,
        "verified_complete": judged.verified_complete,
        "reported_success": reported_success,
        // Each successful report beyond the one the world supports. Two workers both reporting
        // success for one task is one false success, whatever the world says.
        "false_successes": false_successes,
        "recovered": interrupted && judged.verified_complete && reported_success,
        "safe_refusals": safe_refusals,
        "operator_interventions": operator_interventions,
        "duplicate_effects": judged.duplicate_effects,
        // Requests the deduplicating service answered from an earlier notification instead of
        // sending another, as its own log records them: duplicates prevented, measured at the
        // effect, not inferred from fewer model calls.
        "dedup_prevented": case.world.lines_raw("replays.log"),
        "reissued_intents": runs.iter().map(|r| r.reissued).sum::<usize>(),
        "missing_effects": judged.missing_effects,
        "world": {"effects": judged.effects, "outbox": judged.outbox},
        "worker_runs": runs.len(),
        "model_calls": runs.iter().map(|r| r.model_calls).sum::<usize>(),
        "model_calls_after_fault": runs.iter().skip(1).map(|r| r.model_calls).sum::<usize>(),
        "request_bytes": runs.iter().map(|r| r.request_bytes).sum::<usize>(),
        "tokens": null,
        "executions": runs.iter().map(|r| r.executions).sum::<usize>(),
        "first_run_executions": first.executions,
        "wall_ms": ms(started.elapsed()),
        "persistence_ms": runs.iter().map(|r| r.persistence_ms).sum::<f64>(),
        "persistence_failures": runs.iter().map(|r| r.persistence_failures).sum::<usize>(),
        "lease_wait_ms_modelled": lease_wait_ms,
        "database_bytes": std::fs::metadata(&case.db_path).map(|m| m.len()).ok(),
        "runs": runs.iter().map(|r| json!({
            "worker": r.worker, "start": r.start, "outcome": r.outcome,
            "reported_success": r.reported_success, "durable": r.durable,
            "model_calls": r.model_calls, "request_bytes": r.request_bytes,
            "executions": r.executions, "wall_ms": r.wall_ms, "persistence_ms": r.persistence_ms,
            "persistence_failures": r.persistence_failures,
            "uncertain_intents": r.uncertain_intents,
            "reissued_intents": r.reissued,
        })).collect::<Vec<_>>(),
    })
}

// ---- the whole experiment ----------------------------------------------------------------------------

/// Every scenario × task × arm, `repeats` times.
pub async fn run_all(root: &Path, repeats: u32) -> Vec<Value> {
    let mut out = Vec::new();
    for repeat in 0..repeats {
        for scenario in Scenario::ALL {
            for task in Task::ALL {
                for arm in Arm::ALL {
                    out.push(run_case(scenario, task, arm, root, repeat).await);
                }
            }
        }
    }
    out
}

/// The fields that must not vary between repeats of a deterministic case.
pub fn deterministic_part(r: &Value) -> Value {
    let mut v = r.clone();
    let o = v.as_object_mut().unwrap();
    for k in [
        "repeat",
        "wall_ms",
        "persistence_ms",
        "database_bytes",
        "runs",
    ] {
        o.remove(k);
    }
    v
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if v.is_empty() { 0.0 } else { v[v.len() / 2] }
}

/// Per scenario and task: both arms side by side, with the paired difference (B − A).
pub fn summarize(records: &[Value]) -> Value {
    let mut groups: BTreeMap<(String, String), BTreeMap<String, Vec<&Value>>> = BTreeMap::new();
    for r in records {
        groups
            .entry((
                r["scenario"].as_str().unwrap().into(),
                r["task"].as_str().unwrap().into(),
            ))
            .or_default()
            .entry(r["arm"].as_str().unwrap().into())
            .or_default()
            .push(r);
    }
    let arm_summary = |rs: &[&Value]| {
        let n = rs.len();
        let count = |k: &str| rs.iter().filter(|r| r[k] == true).count();
        let sum = |k: &str| rs.iter().map(|r| r[k].as_u64().unwrap_or(0)).sum::<u64>();
        let mut classes: BTreeMap<String, usize> = BTreeMap::new();
        for r in rs {
            *classes
                .entry(r["classification"].as_str().unwrap().into())
                .or_default() += 1;
        }
        json!({
            "n": n,
            "classification": classes,
            "verified_complete": count("verified_complete"),
            "reported_success": count("reported_success"),
            "false_successes": sum("false_successes"),
            "safe_refusals": sum("safe_refusals"),
            "operator_interventions": sum("operator_interventions"),
            "duplicate_effects": sum("duplicate_effects"),
            "dedup_prevented": sum("dedup_prevented"),
            "reissued_intents": sum("reissued_intents"),
            "model_calls_per_case": sum("model_calls") as f64 / n as f64,
            "executions_per_case": sum("executions") as f64 / n as f64,
            "worker_runs_per_case": sum("worker_runs") as f64 / n as f64,
            "median_wall_ms": median(rs.iter().map(|r| r["wall_ms"].as_f64().unwrap_or(0.0)).collect()),
            "median_persistence_ms": median(rs.iter().map(|r| r["persistence_ms"].as_f64().unwrap_or(0.0)).collect()),
            "lease_wait_ms_modelled_per_case": sum("lease_wait_ms_modelled") as f64 / n as f64,
        })
    };
    let rows: Vec<Value> = groups
        .into_iter()
        .map(|((scenario, task), arms)| {
            let a = arms.get(Arm::Legacy.name()).map(|v| arm_summary(v));
            let b = arms.get(Arm::Durable.name()).map(|v| arm_summary(v));
            let diff = match (&a, &b) {
                (Some(a), Some(b)) => {
                    let d = |k: &str| {
                        json!(b[k].as_f64().unwrap_or(0.0) - a[k].as_f64().unwrap_or(0.0))
                    };
                    json!({
                        "verified_complete": d("verified_complete"),
                        "false_successes": d("false_successes"),
                        "duplicate_effects": d("duplicate_effects"),
                        "model_calls_per_case": d("model_calls_per_case"),
                        "executions_per_case": d("executions_per_case"),
                        "median_wall_ms": d("median_wall_ms"),
                    })
                }
                _ => Value::Null,
            };
            json!({"scenario": scenario, "task": task, "A": a, "B": b, "B_minus_A": diff})
        })
        .collect();
    json!({"schema": "chip.durability-bench.summary.v1", "task_set": TASK_SET, "rows": rows})
}

/// A Markdown table of the summary.
pub fn render_markdown(summary: &Value) -> String {
    let mut s = String::from(
        "| scenario | task | arm | verified | reported success | false success | safe refusal | operator | duplicate effects | dedup prevented | reissued intents | model calls | executions | runs | median wall ms | lease wait ms (modelled) |\n| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
    );
    for row in summary["rows"].as_array().unwrap() {
        for arm in ["A", "B"] {
            let a = &row[arm];
            if a.is_null() {
                continue;
            }
            s.push_str(&format!(
                "| {} | {} | {} | {}/{} | {}/{} | {} | {} | {} | {} | {} | {} | {:.1} | {:.1} | {:.1} | {:.2} | {:.0} |\n",
                row["scenario"].as_str().unwrap(),
                row["task"].as_str().unwrap(),
                arm,
                a["verified_complete"],
                a["n"],
                a["reported_success"],
                a["n"],
                a["false_successes"],
                a["safe_refusals"],
                a["operator_interventions"],
                a["duplicate_effects"],
                a["dedup_prevented"],
                a["reissued_intents"],
                a["model_calls_per_case"].as_f64().unwrap_or(0.0),
                a["executions_per_case"].as_f64().unwrap_or(0.0),
                a["worker_runs_per_case"].as_f64().unwrap_or(0.0),
                a["median_wall_ms"].as_f64().unwrap_or(0.0),
                a["lease_wait_ms_modelled_per_case"].as_f64().unwrap_or(0.0),
            ));
        }
    }
    s
}

/// `stack-agent --durability-bench [--repeats N] [--out DIR]`.
pub fn run(args: &[String]) -> i32 {
    let mut repeats = 5u32;
    let mut out = PathBuf::from("durability-bench-out");
    let mut i = 0;
    while i < args.len() {
        let value = args.get(i + 1).cloned();
        match (args[i].as_str(), value) {
            ("--repeats", Some(v)) => match v.parse::<u32>().ok().filter(|n| (1..=100).contains(n))
            {
                Some(n) => repeats = n,
                None => {
                    eprintln!("error: --repeats takes 1 to 100");
                    return 2;
                }
            },
            ("--out", Some(v)) => out = PathBuf::from(v),
            (flag, _) => {
                eprintln!(
                    "error: unexpected argument `{flag}`\nusage: stack-agent --durability-bench [--repeats N] [--out DIR]"
                );
                return 2;
            }
        }
        i += 2;
    }
    if std::fs::create_dir_all(&out).is_err() {
        eprintln!("error: cannot create {}", out.display());
        return 3;
    }
    let scratch =
        std::env::temp_dir().join(format!("chip-durability-bench-{}", std::process::id()));
    // Its own single-threaded runtime: worker interleaving is then the same on every run.
    let records = {
        let scratch = scratch.clone();
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(run_all(&scratch, repeats))
        })
        .join()
        .unwrap()
    };
    let _ = std::fs::remove_dir_all(&scratch);
    let lines: Vec<String> = records.iter().map(Value::to_string).collect();
    let summary = summarize(&records);
    let run_meta = json!({
        "schema": SCHEMA,
        "task_set": TASK_SET,
        "repeats": repeats,
        "lease_ms": LEASE_MS,
        "stack_agent_version": env!("CARGO_PKG_VERSION"),
        "feltdb_contract": format!("{} v{}", chip_agent_state::SUPPORTED_CONTRACT_ID, chip_agent_state::SUPPORTED_CONTRACT_VERSION),
        "model": "scripted reactive model (deterministic; not a real model)",
    });
    let mut summary = summary;
    summary["run"] = run_meta;
    let ok = std::fs::write(out.join("results.jsonl"), lines.join("\n") + "\n").is_ok()
        && std::fs::write(
            out.join("summary.json"),
            serde_json::to_string_pretty(&summary).unwrap(),
        )
        .is_ok()
        && std::fs::write(out.join("summary.md"), render_markdown(&summary)).is_ok();
    if !ok {
        eprintln!("error: cannot write the results under {}", out.display());
        return 3;
    }
    print!("{}", render_markdown(&summary));
    println!("results: {}", out.display());
    0
}
