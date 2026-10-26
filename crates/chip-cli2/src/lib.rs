//! cli2 as a Chip capability backend: discovery and governed execution of a project's own tasks.
//!
//! Chip keeps the agent loop, the model boundary, the work state and the completion decision.
//! cli2 keeps discovery, planning, approval-by-plan-digest, execution and its own records. This
//! crate is the narrow adapter between them, and it adds no loop, no model call, no store.
//!
//! Two capabilities, routed by Chip's [`CapabilitySet`](chip_core::CapabilitySet) like any other:
//!
//! * `cli2.tasks`: what the project can run (cli2's discovery of Make/npm/Cargo/just tasks, with
//!   cli2's own account of each task's effects). Read-only.
//! * `cli2.run`: run one *discovered* task, named by cli2's id, never a command line. cli2 plans
//!   it; if its policy requires approval an [`Approver`] (a person; never the model) is shown the
//!   exact plan and approves **its full digest**; cli2 executes it and records the result. Chip is
//!   handed an observation that keeps apart what happened.
//!
//! File reads and edits are deliberately **not** offered here. Chip already has `project.read` and
//! `project.write` (a precondition on the file's hash, stale-base refusal, confinement, protected
//! paths, bounded size) and its completion rules, tamper checks and conflict tracking key off
//! those observations. A second write path would be a way around them.
//!
//! # What an observation means
//!
//! `cli2.run` answers with one canonical JSON line (the first line of the output, like the
//! project capabilities) whose `outcome` is exactly one of:
//!
//! | `outcome` | meaning | Chip status |
//! |---|---|---|
//! | `verified` | the task ran, exited 0, and cli2's host-declared acceptance checks passed | success |
//! | `succeeded_unverified` | the task ran and exited 0; nothing task-level was established | success |
//! | `process_failed` | the task ran and failed (exit status, timeout, signal) | failure |
//! | `verification_failed` | the task ran, but a declared check did not hold | failure |
//! | `rejected` | nothing ran: approval declined, policy forbids, or the plan went stale | failure |
//! | `cancelled` | the run was cancelled | cancelled |
//!
//! An execution whose result cannot be established is not an outcome at all: the backend returns
//! [`ExecutionError::OutcomeUnknown`](chip_core::ExecutionError::OutcomeUnknown), which Chip's
//! durable state keeps unresolved, so it is never presented as a failure and never silently run
//! again.
//!
//! None of these is goal satisfaction. Chip decides completion from PAX; a cli2 result, even
//! `verified`, does not complete a goal. What cli2 results *do* feed is the other direction:
//! [`may_have_changed`] tells the completion predicate that a task may have altered the project,
//! so a test pass from before it no longer counts.

mod approval;
mod client;
mod environment;
mod outcome;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chip_core::{
    CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId, CapabilityInput,
    CapabilityProvider, Effect, ExecutionError, ExecutionRequest, ExecutionResult, Executor,
    Idempotency, InputValue,
};
use serde_json::{Value, json};

pub use approval::{Approver, Decision, DenyAll, PlanReview, StdioApprover};
pub use client::{Client, ClientError, PROTOCOL_VERSION, REQUIRED_OPERATIONS};
pub use environment::Cli2EnvironmentProvider;
pub use outcome::{RunOutcome, RunSummary, may_have_changed, run_summary};

pub const CLI2_TASKS: &str = "cli2.tasks";
pub const CLI2_RUN: &str = "cli2.run";

/// A check the *host* declares for a task. It is attached to cli2's plan so cli2 itself evaluates
/// it against the real output. A model cannot declare, weaken or remove one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expectation {
    StdoutContains(String),
    FileExists(String),
}

impl Expectation {
    fn to_check(&self) -> Value {
        match self {
            Self::StdoutContains(text) => json!({"type": "stdout_contains", "text": text}),
            Self::FileExists(path) => {
                json!({"type": "file_exists", "path": path, "modified_after_start": false})
            }
        }
    }
}

#[derive(Clone)]
pub struct Cli2Config {
    /// The socket of a `cli2 serve` the user started.
    pub socket: PathBuf,
    /// The project root Chip works in; cli2 operates on exactly this directory.
    pub project: PathBuf,
    pub approver: Arc<dyn Approver>,
    /// Acceptance checks per cli2 task id (`project-task:make:test`), declared by the host.
    pub expectations: BTreeMap<String, Vec<Expectation>>,
    /// Longest a single protocol request may take.
    pub io_timeout: Duration,
    /// Longest a run may take before the backend stops waiting.
    pub max_wait: Duration,
    /// Delay between status checks of a running task.
    pub poll: Duration,
}

impl Cli2Config {
    pub fn new(socket: impl Into<PathBuf>, project: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            project: project.into(),
            approver: Arc::new(DenyAll),
            expectations: BTreeMap::new(),
            io_timeout: Duration::from_secs(30),
            max_wait: Duration::from_secs(30 * 60),
            poll: Duration::from_millis(100),
        }
    }

    pub fn with_approver(mut self, approver: Arc<dyn Approver>) -> Self {
        self.approver = approver;
        self
    }

    pub fn expecting(mut self, task: &str, checks: Vec<Expectation>) -> Self {
        self.expectations.insert(task.to_string(), checks);
        self
    }
}

pub struct Cli2Backend {
    cfg: Arc<Cli2Config>,
}

impl Cli2Backend {
    pub fn new(cfg: Cli2Config) -> Self {
        Self { cfg: Arc::new(cfg) }
    }

    fn connect(cfg: &Cli2Config) -> Result<Client, ClientError> {
        Client::connect(&cfg.socket, cfg.io_timeout)
    }
}

fn input(name: &str, description: &str) -> CapabilityInput {
    CapabilityInput {
        name: name.into(),
        description: description.into(),
        required: true,
    }
}

/// One task cli2 discovered, reduced to what Chip shows and decides on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub id: String,
    pub name: String,
    pub purpose: Option<String>,
    /// cli2's own account of what it does: `unknown`, `declared read-only`, ...
    pub effects: String,
    /// False only when cli2 has the task *declared* read-only. Everything else may change the project.
    pub may_change: bool,
}

fn parse_tasks(list: &Value) -> Vec<Task> {
    let mut tasks: Vec<Task> = list["capabilities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["kind"] == "project_task")
        .filter_map(|c| {
            let classes: Vec<&str> = c["effects"]["classes"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let certainty = c["effects"]["certainty"].as_str().unwrap_or("unknown");
            let read_only = certainty == "declared" && classes == ["read_only"];
            Some(Task {
                id: c["id"].as_str()?.to_string(),
                name: c["name"].as_str()?.to_string(),
                purpose: c["purpose"]["value"].as_str().map(String::from),
                effects: if read_only {
                    "declared read-only".into()
                } else {
                    format!(
                        "{certainty}{}",
                        if classes.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", classes.join(", "))
                        }
                    )
                },
                may_change: !read_only,
            })
        })
        .collect();
    tasks.sort_by(|a, b| a.id.cmp(&b.id));
    tasks
}

fn discover(client: &mut Client, project: &str) -> Result<Vec<Task>, ClientError> {
    client.call("project.scan", json!({"project": project}))?;
    // Only the project's own tasks: the unfiltered list also holds every executable on PATH,
    // which can be larger than a protocol message. A cli2 too old to filter by kind is refused
    // with a message that says so, rather than answered with something else.
    let list = client
        .call(
            "capabilities.list",
            json!({"project": project, "kinds": ["project_task"]}),
        )
        .map_err(|e| match e {
            ClientError::Server { code, .. } if code == "invalid_params" => {
                ClientError::Unavailable(
                    "this cli2 cannot list capabilities by kind; use a cli2 that supports capabilities.list `kinds`".into(),
                )
            }
            other => other,
        })?;
    Ok(parse_tasks(&list))
}

fn project_str(cfg: &Cli2Config) -> String {
    cfg.project.display().to_string()
}

#[async_trait::async_trait]
impl CapabilityProvider for Cli2Backend {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(vec![
            CapabilityDescriptor::new(
                CapabilityId::new(CLI2_TASKS).expect("a valid id"),
                "List the project's runnable tasks",
                "List the tasks cli2 discovered in this project (Make targets, npm scripts, cargo and just tasks), by id, with what each is for and what cli2 knows of its effects. Read-only.",
            )
            .with_effect(Effect::Observation)
            .without_evidence_reuse(),
            {
                let mut d = CapabilityDescriptor::new(
                    CapabilityId::new(CLI2_RUN).expect("a valid id"),
                    "Run a discovered project task",
                    "Run ONE task listed by cli2.tasks, named by its id in `task`. cli2 plans it and a person may have to approve the exact plan first; the result says whether it ran, whether it failed or its check failed, or that it was rejected. A result is not proof the goal is met.",
                )
                .with_max_input_bytes(256)
                // Whether a project task changes anything is cli2's to say per task, not this
                // adapter's: it declares nothing, so an interrupted run stays uncertain.
                .with_effect(Effect::Unknown)
                // The action key reaches cli2, which answers a repeated key with the plan and the
                // execution that key already produced (see `run_task`).
                .with_idempotency(Idempotency::Deduplicated)
                .without_evidence_reuse();
                d.inputs = vec![input(
                    "task",
                    "the id of a task from cli2.tasks, e.g. project-task:make:test",
                )];
                d
            },
        ])
    }

    async fn availability(&self, id: &CapabilityId) -> CapabilityAvailability {
        if id.as_str() != CLI2_TASKS && id.as_str() != CLI2_RUN {
            return CapabilityAvailability::Unavailable(format!("{id} is not a cli2 capability"));
        }
        let cfg = self.cfg.clone();
        match tokio::task::spawn_blocking(move || Self::connect(&cfg).map(|_| ())).await {
            Ok(Ok(())) => CapabilityAvailability::Available,
            Ok(Err(e)) => CapabilityAvailability::Unavailable(e.to_string()),
            Err(_) => CapabilityAvailability::Unavailable("the availability check failed".into()),
        }
    }

    async fn validate_inputs(
        &self,
        id: &CapabilityId,
        inputs: &BTreeMap<String, InputValue>,
    ) -> Result<(), CapabilityError> {
        for name in inputs.keys() {
            if id.as_str() != CLI2_RUN || name != "task" {
                return Err(CapabilityError::InvalidInput(format!(
                    "capability does not accept input '{name}'"
                )));
            }
        }
        if id.as_str() != CLI2_RUN {
            return Ok(());
        }
        let task = match inputs.get("task") {
            Some(InputValue::Text(t)) if !t.trim().is_empty() && t.len() <= 128 => t.clone(),
            _ => {
                return Err(CapabilityError::InvalidInput(
                    "task must be the id of a discovered task".into(),
                ));
            }
        };
        // The task must exist, as cli2 discovered it right now. If cli2 cannot be asked, this
        // fails closed: nothing is run on the strength of an id nobody could check.
        let cfg = self.cfg.clone();
        let found = tokio::task::spawn_blocking(move || {
            let mut c = Self::connect(&cfg)?;
            discover(&mut c, &project_str(&cfg))
        })
        .await
        .map_err(|_| CapabilityError::Unavailable("discovery did not complete".into()))?
        .map_err(|e| CapabilityError::Unavailable(e.to_string()))?;
        if found.iter().any(|t| t.id == task) {
            Ok(())
        } else {
            Err(CapabilityError::InvalidInput(format!(
                "{task:?} is not a task cli2 discovered in this project; list them with cli2.tasks"
            )))
        }
    }
}

#[async_trait::async_trait]
impl Executor for Cli2Backend {
    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let cfg = self.cfg.clone();
        match request.intent.as_str() {
            CLI2_TASKS => {
                let id = request.id.clone();
                tokio::task::spawn_blocking(move || list_tasks(&cfg, id))
                    .await
                    .map_err(|_| {
                        ExecutionError::ExecutionFailed("discovery did not complete".into())
                    })?
            }
            CLI2_RUN => {
                let task = match request.inputs.get("task") {
                    Some(InputValue::Text(t)) => t.clone(),
                    _ => {
                        return Err(ExecutionError::InvalidRequest("task is required".into()));
                    }
                };
                let key = request.idempotency_key.clone();
                let id = request.id.clone();
                // The run may outlive a caller that stops waiting; what it left is cli2's record.
                let handle =
                    tokio::task::spawn_blocking(move || run_task(&cfg, id, &task, key.as_deref()));
                handle.await.map_err(|_| {
                    ExecutionError::OutcomeUnknown(
                        "the cli2 run did not complete in this process; it may have taken effect"
                            .into(),
                    )
                })?
            }
            other => Err(ExecutionError::InvalidRequest(format!(
                "{other} is not a cli2 capability"
            ))),
        }
    }
}

fn list_tasks(
    cfg: &Cli2Config,
    id: chip_core::ExecutionId,
) -> Result<ExecutionResult, ExecutionError> {
    let mut c = Cli2Backend::connect(cfg)
        .map_err(|e| ExecutionError::ExecutorUnavailable(e.to_string()))?;
    let tasks = discover(&mut c, &project_str(cfg)).map_err(|e| match e {
        ClientError::Unavailable(m) => ExecutionError::ExecutorUnavailable(m),
        other => ExecutionError::ExecutionFailed(other.to_string()),
    })?;
    let head = json!({"capability": CLI2_TASKS, "count": tasks.len()});
    let mut out = format!("{head}\n");
    for t in &tasks {
        out.push_str(&format!(
            "{} | {} | effects: {}\n",
            t.id,
            t.purpose.as_deref().unwrap_or("(no description)"),
            t.effects
        ));
    }
    if tasks.is_empty() {
        out.push_str("cli2 discovered no runnable tasks in this project\n");
    }
    Ok(ExecutionResult::success(id, bounded(&out, 8 * 1024)))
}

/// At most `max` bytes of `s`, on a character boundary.
fn bounded(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[output cut at {max} bytes]", &s[..end])
}

fn rejected(id: chip_core::ExecutionId, task: &Task, why: &str, extra: Value) -> ExecutionResult {
    let mut v = json!({
        "capability": CLI2_RUN, "task": task.id, "outcome": "rejected", "ran": false,
        "may_change": task.may_change, "reason": why,
    });
    if let (Some(base), Some(extra)) = (v.as_object_mut(), extra.as_object()) {
        base.extend(extra.clone());
    }
    ExecutionResult::failure(id, format!("{v}\nnothing was run: {why}"))
}

fn run_task(
    cfg: &Cli2Config,
    id: chip_core::ExecutionId,
    task_id: &str,
    key: Option<&str>,
) -> Result<ExecutionResult, ExecutionError> {
    let project = project_str(cfg);
    let unavailable = |e: ClientError| match e {
        ClientError::Unavailable(m) => ExecutionError::ExecutorUnavailable(m),
        other => ExecutionError::ExecutionFailed(other.to_string()),
    };
    let mut c = Cli2Backend::connect(cfg).map_err(unavailable)?;
    let tasks = discover(&mut c, &project).map_err(unavailable)?;
    let Some(task) = tasks.iter().find(|t| t.id == task_id).cloned() else {
        return Err(ExecutionError::InvalidRequest(format!(
            "{task_id:?} is not a task cli2 discovered"
        )));
    };

    // 1. Plan. cli2 resolves the task (pinned to this id) and decides what approval it needs.
    let mut params = json!({
        "project": project,
        "intent": format!("run {}", task.name),
        "selection": task.id,
    });
    if let Some(k) = key {
        params["idempotency_key"] = json!(format!("chip:{k}"));
    }
    if let Some(checks) = cfg.expectations.get(&task.id) {
        params["expectations"] = Value::Array(checks.iter().map(Expectation::to_check).collect());
    }
    let planned = c.call("intent.plan", params).map_err(|e| match e {
        ClientError::Server { code, message } => ExecutionError::ExecutionFailed(format!(
            "cli2 would not plan {task_id}: {code}: {message}"
        )),
        other => unavailable(other),
    })?;
    let plan = planned["outcome"]["plan"].clone();
    if planned["outcome"]["status"] != "planned" || plan.is_null() {
        let why = planned["outcome"]["message"]
            .as_str()
            .unwrap_or("cli2 produced no plan")
            .to_string();
        return Ok(rejected(
            id,
            &task,
            &why,
            json!({"cli2_status": planned["outcome"]["status"]}),
        ));
    }
    if planned["approvability"] == "forbidden" {
        return Ok(rejected(
            id,
            &task,
            "cli2's policy forbids this task; no approval can authorize it",
            json!({}),
        ));
    }
    let plan_id = plan["plan_id"].as_str().unwrap_or_default().to_string();
    let digest = plan["digest"].as_str().unwrap_or_default().to_string();
    if plan_id.is_empty() || digest.is_empty() {
        return Err(ExecutionError::ExecutionFailed(
            "cli2's plan has no id or digest".into(),
        ));
    }

    // 2. A repeated action key finds what the key already did: never a second execution.
    let view = c
        .call("plan.get", json!({"plan_id": plan_id}))
        .map_err(|e| {
            ExecutionError::OutcomeUnknown(format!(
                "could not check whether {plan_id} already ran: {e}"
            ))
        })?;
    let existing = view["executions"]
        .as_array()
        .and_then(|a| a.last())
        .and_then(|e| e["execution_id"].as_str())
        .map(String::from);
    let execution_id = match existing {
        Some(existing) => existing,
        None => {
            // 3. Approval, by the plan's full digest. Only when cli2's policy says it is needed.
            let needs = planned["approvability"] == "requires_approval";
            if needs {
                let review = PlanReview {
                    task: task.id.clone(),
                    plan_id: plan_id.clone(),
                    digest: digest.clone(),
                    commands: plan["steps"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(display_step)
                        .collect(),
                    working_directory: plan["steps"][0]["working_directory"]
                        .as_str()
                        .unwrap_or(&project)
                        .to_string(),
                    approval_level: plan["approval"]["level"]
                        .as_str()
                        .unwrap_or("confirm")
                        .to_string(),
                    reasons: plan["approval"]["requirements"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|r| r["reason"].as_str().map(String::from))
                        .collect(),
                    effects: task.effects.clone(),
                    risk: plan["risk"].as_str().unwrap_or("unknown").to_string(),
                };
                if let Decision::Deny(why) = cfg.approver.review(&review) {
                    // Retire the plan so it can never be approved and run later.
                    let _ = c.call("plan.reject", json!({"plan_id": plan_id, "reason": why}));
                    return Ok(rejected(
                        id,
                        &task,
                        &format!("approval was declined: {why}"),
                        json!({"plan_id": plan_id, "plan_digest": digest}),
                    ));
                }
            }
            // Approving is only a decision: nothing runs until it is started. cli2 checks the
            // digest against the stored plan, so this approves exactly what was reviewed.
            let approval = match c.call(
                "plan.approve",
                json!({"plan_id": plan_id, "plan_digest": digest}),
            ) {
                Ok(a) => a,
                Err(ClientError::Server { code, message }) => {
                    return Ok(rejected(
                        id,
                        &task,
                        &format!("cli2 refused the approval ({code}): {message}"),
                        json!({"plan_id": plan_id, "plan_digest": digest}),
                    ));
                }
                Err(other) => return Err(ExecutionError::ExecutionFailed(other.to_string())),
            };
            let approval_id = approval["approval"]["approval_id"]
                .as_str()
                .unwrap_or_default()
                .to_string();

            // 4. Start. If the answer is lost the run may exist: ask, do not guess.
            match c.call(
                "execution.start",
                json!({"plan_id": plan_id, "approval_id": approval_id}),
            ) {
                Ok(started) => started["execution_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                Err(ClientError::Server { code, message }) => {
                    return Ok(rejected(
                        id,
                        &task,
                        &format!("cli2 would not start it ({code}): {message}"),
                        json!({"plan_id": plan_id, "plan_digest": digest}),
                    ));
                }
                Err(lost) => match recover_start(cfg, &plan_id) {
                    Some(existing) => existing,
                    None => {
                        return Err(ExecutionError::OutcomeUnknown(format!(
                            "the request to start {plan_id} got no answer ({lost}); the run may exist"
                        )));
                    }
                },
            }
        }
    };
    if execution_id.is_empty() {
        return Err(ExecutionError::ExecutionFailed(
            "cli2 returned no execution id".into(),
        ));
    }

    // 5. Observe until cli2 records an outcome.
    observe(cfg, id, &task, &plan_id, &digest, &execution_id)
}

fn display_step(step: &Value) -> String {
    let argv: Vec<String> = std::iter::once(step["executable"].as_str().unwrap_or("?").to_string())
        .chain(
            step["arguments"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str().map(String::from)),
        )
        .collect();
    argv.join(" ")
}

/// After a start whose answer was lost: did an execution come into being for this plan?
fn recover_start(cfg: &Cli2Config, plan_id: &str) -> Option<String> {
    let mut c = Cli2Backend::connect(cfg).ok()?;
    let view = c.call("plan.get", json!({"plan_id": plan_id})).ok()?;
    view["executions"]
        .as_array()?
        .last()?
        .get("execution_id")?
        .as_str()
        .map(String::from)
}

fn observe(
    cfg: &Cli2Config,
    id: chip_core::ExecutionId,
    task: &Task,
    plan_id: &str,
    digest: &str,
    execution_id: &str,
) -> Result<ExecutionResult, ExecutionError> {
    let started = Instant::now();
    let mut c = Cli2Backend::connect(cfg).map_err(|e| {
        ExecutionError::OutcomeUnknown(format!("cannot watch execution {execution_id}: {e}"))
    })?;
    let mut lost_in_a_row = 0u32;
    let snapshot = loop {
        match c.call("execution.get", json!({"execution_id": execution_id})) {
            Ok(snap) => {
                lost_in_a_row = 0;
                match snap["lifecycle"].as_str() {
                    Some("finished") => break snap,
                    Some("interrupted_unknown") => {
                        return Err(ExecutionError::OutcomeUnknown(format!(
                            "execution {execution_id} (plan {plan_id}) was interrupted and its outcome was not recorded; it may have changed the project"
                        )));
                    }
                    Some("running") | Some("cancelling") => {}
                    other => {
                        return Err(ExecutionError::OutcomeUnknown(format!(
                            "execution {execution_id} is in an unrecognized state {other:?}"
                        )));
                    }
                }
            }
            Err(ClientError::Server { code, message }) => {
                return Err(ExecutionError::OutcomeUnknown(format!(
                    "cli2 cannot say how execution {execution_id} ended ({code}: {message})"
                )));
            }
            Err(lost) => {
                lost_in_a_row += 1;
                // Reconnect and keep asking a few times; the run itself is cli2's.
                match Cli2Backend::connect(cfg) {
                    Ok(fresh) if lost_in_a_row < 5 => c = fresh,
                    _ => {
                        return Err(ExecutionError::OutcomeUnknown(format!(
                            "lost contact with cli2 while execution {execution_id} was running ({lost}); it may have completed"
                        )));
                    }
                }
            }
        }
        if started.elapsed() >= cfg.max_wait {
            // Stop it, then see what cli2 recorded; a run still going is unknown, not a failure.
            let _ = c.call("execution.cancel", json!({"execution_id": execution_id}));
            let settle = Instant::now();
            while settle.elapsed() < Duration::from_secs(10) {
                if let Ok(snap) = c.call("execution.get", json!({"execution_id": execution_id}))
                    && snap["lifecycle"] == "finished"
                {
                    return outcome::result(id, task, plan_id, digest, &snap, |r| fetch(&mut c, r));
                }
                std::thread::sleep(cfg.poll);
            }
            return Err(ExecutionError::OutcomeUnknown(format!(
                "execution {execution_id} was still running after {}s and did not stop when cancelled",
                cfg.max_wait.as_secs()
            )));
        }
        std::thread::sleep(cfg.poll);
    };
    outcome::result(id, task, plan_id, digest, &snapshot, |r| fetch(&mut c, r))
}

/// A bounded tail of a stored output, by cli2's evidence reference. The reference stays the record.
fn fetch(c: &mut Client, reference: &str) -> Option<String> {
    c.call(
        "evidence.read",
        json!({"reference": reference, "max_bytes": 64 * 1024}),
    )
    .ok()
    .and_then(|r| r["content"].as_str().map(String::from))
}
