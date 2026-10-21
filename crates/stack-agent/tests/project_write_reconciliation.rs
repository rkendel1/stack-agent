//! `project.write` reconciled from the project's own files, across real process deaths.
//!
//! Nothing here is a stand-in for the destination: the writes are performed by the real
//! `ProjectExecutor` into a real project directory, and what the project holds (the target and its
//! inode, the staged file, `.stack-agent/writes/`) is what every assertion reads. The only
//! stand-ins are the killer around the executor (it SIGKILLs the process at the window a test
//! names) and the model (a script). A `deliveries` file outside the project counts how often the
//! executor was asked to write.
//!
//! | child mode | where the process dies |
//! | --- | --- |
//! | `die-after-write` | the write is committed in the project; the result never reaches Chip |
//! | `die-before-delivery` | the intent is durable; the executor was never called |

#![cfg(unix)]

use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chip_agent_state::{
    ActionTag, AgentStore, Begun, DestinationStatus, DurableConfig, OperationRef, ReconcileQuery,
    Reconciler, StateError, begin, reconcile, settle,
};
use chip_core::{
    Agent, Capability, CapabilityId, CapabilityRequest, DecisionError, Effect, ExecutionError,
    ExecutionId, ExecutionObserver, ExecutionRequest, ExecutionResult, Executor, Idempotency,
    InputValue, NoLocalPolicy, WorkDecision, WorkDecisionBoundary, WorkGoal, WorkId, WorkOutcome,
    WorkSpec,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use chip_project::{PROJECT_WRITE, ProjectExecutor, ProjectLock};
use stack_agent_reconciler::ProjectWriteReconciler;
use tempfile::TempDir;

// The library is named chip_cli.
mod stack_agent_reconciler {
    pub use chip_cli::project_reconciler::ProjectWriteReconciler;
}

const GOAL: &str = "write the notes file";
const TARGET: &str = "notes.txt";
const CONTENT: &str = "hello from the model\n";
const TASK: &str = "pw-task";
const LEASE_MS: u64 = 4_000;

// ---------------------------------------------------------------------------------------------
// The child
// ---------------------------------------------------------------------------------------------

struct Model;

#[async_trait::async_trait]
impl ModelProvider for Model {
    async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
        Ok(ModelResponse::new("r", "reply", Usage::new(1, 1)))
    }
}

struct Script(Mutex<Vec<WorkDecision>>);

impl WorkDecisionBoundary for Script {
    fn interpret(
        &self,
        _r: &ModelResponse,
        _c: &[Capability],
    ) -> Result<WorkDecision, DecisionError> {
        let mut left = self.0.lock().unwrap();
        if left.is_empty() {
            return Err(DecisionError::InvalidDecision("the script ran out".into()));
        }
        Ok(left.remove(0))
    }
}

fn write_decision() -> WorkDecision {
    WorkDecision::RequestCapability(
        CapabilityRequest::new(
            ExecutionId::new("x"),
            CapabilityId::new(PROJECT_WRITE).unwrap(),
        )
        .with_input("path", InputValue::Text(TARGET.into()))
        .with_input("content", InputValue::Text(CONTENT.into())),
    )
}

fn die() {
    let _ = Command::new("kill")
        .args(["-9", &std::process::id().to_string()])
        .status();
}

/// The real executor, with a log of deliveries outside the project and a kill at the window.
struct Killer {
    inner: ProjectExecutor,
    side: PathBuf,
    mode: String,
}

#[async_trait::async_trait]
impl Executor for Killer {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        if self.mode == "die-before-delivery" {
            die();
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.side.join("deliveries"))
            .unwrap();
        writeln!(log, "key={}", r.idempotency_key.clone().unwrap_or_default()).unwrap();
        log.sync_all().unwrap();
        let result = self.inner.execute(r).await;
        if self.mode == "die-after-write" {
            die();
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        result
    }
}

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

fn report(what: &str) {
    println!("KB_RESULT {what}");
}

fn config() -> DurableConfig {
    DurableConfig {
        actor: format!("chip-{}", std::process::id()),
        lease_ms: LEASE_MS,
        effects: [(PROJECT_WRITE.to_string(), Effect::Consequential)].into(),
        idempotency: [(PROJECT_WRITE.to_string(), Idempotency::Carried)].into(),
    }
}

/// Not a test: the body of the child process. Without `KB_MODE` it does nothing.
#[tokio::test(flavor = "multi_thread")]
async fn child_entry() {
    let Ok(mode) = std::env::var("KB_MODE") else {
        return;
    };
    let root = PathBuf::from(var("KB_ROOT"));
    let side = PathBuf::from(var("KB_SIDE"));
    let task = std::env::var("KB_TASK").unwrap_or_else(|_| TASK.into());
    if let Ok(gate) = std::env::var("KB_GATE") {
        while !Path::new(&gate).exists() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    if mode == "hold-lock" {
        // A live writer: it holds the project until told to let go.
        let _lock = ProjectLock::acquire(&root).expect("the project is free");
        std::fs::write(side.join("holding"), "").unwrap();
        while !side.join("release").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        return report("released");
    }
    let store = match AgentStore::open(var("KB_DB")) {
        Ok(store) => store,
        Err(e) => return report(&format!("open-refused {e}")),
    };
    if mode == "reconcile" {
        let history = store.history(&task).unwrap().unwrap();
        let reconciler = ProjectWriteReconciler::new(&root);
        let mut said = Vec::new();
        for intent in history.unresolved_intents() {
            let r = reconcile(
                &store,
                &task,
                &intent.value.key,
                &format!("recon-{}", std::process::id()),
                Some(&reconciler),
            )
            .await;
            said.push(match r {
                Ok(r) => match &r.finding {
                    chip_agent_state::Finding::Indeterminate { reason } => {
                        format!("indeterminate resolved={} {reason}", r.resolved)
                    }
                    other => format!("{} resolved={}", other.name(), r.resolved),
                },
                Err(e) => format!("refused {e}"),
            });
        }
        return report(&said.join("; "));
    }
    // A work holds its project for its whole run, and dies with it.
    let _lock = match ProjectLock::acquire(&root) {
        Ok(lock) => lock,
        Err(e) => return report(&format!("project-locked {e}")),
    };
    let work = match begin(&store, &task, GOAL, &config()) {
        Ok(Begun::Ready(work)) => work,
        Ok(Begun::Finished(_)) => return report("finished"),
        Err(StateError::OutcomeUncertain { .. }) => return report("uncertain"),
        Err(StateError::LeaseHeld { .. }) => return report("lease-held"),
        Err(e) => return report(&format!("refused {e}")),
    };
    let project = ProjectExecutor::new(&root);
    let capabilities = Arc::new(ProjectExecutor::new(&root));
    let decisions = if work.restored().is_empty() {
        vec![
            write_decision(),
            WorkDecision::Complete {
                summary: "written".into(),
            },
        ]
    } else {
        vec![WorkDecision::Complete {
            summary: "written".into(),
        }]
    };
    let run = Agent::new(Arc::new(Model))
        .with_capabilities(capabilities)
        .with_executor(work.executor(Arc::new(Killer {
            inner: project,
            side,
            mode,
        })))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &WorkSpec::new(WorkId::new(&task), WorkGoal::new(GOAL))
                .with_restored(work.restored())
                .with_settled(work.settled_actions()),
            &NoLocalPolicy,
            &Script(Mutex::new(decisions)),
        )
        .await;
    let _ = work.finish(&run, true);
    report(&format!(
        "ran {}",
        match run.outcome {
            WorkOutcome::Completed { .. } => "completed",
            _ => "not-completed",
        }
    ));
}

// ---------------------------------------------------------------------------------------------
// The parent
// ---------------------------------------------------------------------------------------------

struct Case {
    project: TempDir,
    side: TempDir,
    state: TempDir,
}

impl Case {
    fn new() -> Self {
        Self {
            project: TempDir::new().unwrap(),
            side: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
        }
    }

    fn root(&self) -> PathBuf {
        self.project.path().to_path_buf()
    }

    fn db(&self) -> PathBuf {
        self.state.path().join("agent-state.feltdb")
    }

    fn target(&self) -> PathBuf {
        self.root().join(TARGET)
    }

    fn child(&self, mode: &str, gate: Option<&Path>) -> Command {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["--exact", "child_entry", "--nocapture", "--test-threads=1"])
            .env("KB_MODE", mode)
            .env("KB_ROOT", self.root())
            .env("KB_SIDE", self.side.path())
            .env("KB_DB", self.db())
            .stdin(Stdio::null());
        if let Some(gate) = gate {
            c.env("KB_GATE", gate);
        }
        c
    }

    async fn say(&self, mode: &str) -> String {
        result_of(&run(self.child(mode, None)).await)
    }

    async fn died(&self, mode: &str) {
        killed(&run(self.child(mode, None)).await);
        tokio::time::sleep(Duration::from_millis(LEASE_MS + 400)).await;
    }

    /// Times the executor was asked to write.
    fn deliveries(&self) -> usize {
        std::fs::read_to_string(self.side.path().join("deliveries"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    /// The target: (inode, content), if it exists.
    fn file(&self) -> Option<(u64, String)> {
        let meta = std::fs::metadata(self.target()).ok()?;
        Some((meta.ino(), std::fs::read_to_string(self.target()).ok()?))
    }

    /// Names in the project (not the host's own `.stack-agent`).
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.root())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != ".stack-agent")
            .collect();
        names.sort();
        names
    }

    fn journal(&self, name: &str) -> PathBuf {
        self.root().join(".stack-agent/writes").join(name)
    }

    fn history(&self) -> chip_agent_state::History {
        AgentStore::open(self.db())
            .unwrap()
            .history(TASK)
            .unwrap()
            .unwrap()
    }

    /// The original intent's key and action key.
    fn original(&self) -> (String, String) {
        let h = self.history();
        let intent = &h.intents[0].value;
        (
            intent.key.clone(),
            ActionTag::parse(&intent.action).expect("a carried key").key,
        )
    }

    fn open_intents(&self) -> usize {
        self.history().unresolved_intents().len()
    }

    fn query(&self, path: &str, content: &str) -> ReconcileQuery {
        let (intent, key) = self.original();
        ReconcileQuery {
            task_id: TASK.into(),
            intent_key: intent,
            capability: PROJECT_WRITE.into(),
            operation: OperationRef {
                key,
                binding: "b1.unused-by-the-destination".into(),
            },
            inputs: [
                ("path".to_string(), InputValue::Text(path.into())),
                ("content".to_string(), InputValue::Text(content.into())),
            ]
            .into(),
        }
    }

    /// A late delivery of the original write, straight to the executor, as a worker that was
    /// slow would make it.
    async fn late_delivery(&self) -> ExecutionResult {
        let (_, key) = self.original();
        let mut request = ExecutionRequest::new(ExecutionId::new("late"), PROJECT_WRITE)
            .with_inputs(
                [
                    ("path".to_string(), InputValue::Text(TARGET.into())),
                    ("content".to_string(), InputValue::Text(CONTENT.into())),
                ]
                .into(),
            );
        request.idempotency_key = Some(key);
        ProjectExecutor::new(self.root())
            .execute(request)
            .await
            .expect("the executor answers")
    }
}

fn result_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.split_once("KB_RESULT ").map(|(_, r)| r))
        .map(str::to_string)
        .unwrap_or_else(|| {
            panic!(
                "the child reported nothing; status {:?}\nstdout:\n{}\nstderr:\n{}",
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
}

fn killed(out: &Output) {
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(out.status.signal(), Some(9), "expected SIGKILL: {out:?}");
}

async fn run(mut c: Command) -> Output {
    tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap()
}

// A -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_committed_write_whose_result_was_lost_is_reconciled_by_the_file_it_staged() {
    let case = Case::new();
    case.died("die-after-write").await;
    let (inode, content) = case.file().expect("the write committed in the project");
    assert_eq!(content, CONTENT);
    assert_eq!(case.deliveries(), 1);
    assert_eq!(case.entries(), [TARGET], "no staged file is left behind");
    assert_eq!(case.open_intents(), 1, "no outcome was committed");
    assert_eq!(case.say("work").await, "uncertain");

    // The real binary's own command, against the real database and project.
    let out = Command::new(env!("CARGO_BIN_EXE_stack-agent"))
        .args(["agent-state", "reconcile", "--json", "--state-db"])
        .arg(case.db())
        .args([
            "--task-id",
            TASK,
            "--decided-by",
            "ops@example",
            "--project-dir",
        ])
        .arg(case.root())
        .args(["--intent", &case.original().0])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let said: serde_json::Value = serde_json::from_str(
        stdout
            .lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or_else(|| panic!("no output; status {:?}; stderr: {stderr}", out.status)),
    )
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "{said} {stderr}");
    assert_eq!(
        (said["finding"].as_str(), said["resolved"].as_bool()),
        (Some("completed"), Some(true))
    );

    // The original write was not performed again: same file, same inode, one delivery.
    assert_eq!(case.file(), Some((inode, content)));
    assert_eq!(case.deliveries(), 1);
    // The record names the project's own evidence: the write's key, in the project's journal.
    let (_, key) = case.original();
    assert!(case.journal(&format!("{key}.prepared")).exists());
    assert!(!case.journal(&format!("{key}.barred")).exists());
    let h = case.history();
    let r = &h.reconciliations[0];
    assert_eq!(
        (
            r.finding.as_str(),
            r.destination.as_str(),
            r.business_effect.as_str()
        ),
        ("completed", "committed", "present")
    );
    assert_eq!(r.action_key, key);
    let shown = serde_json::to_string(&h.reconciliations).unwrap();
    assert!(
        !shown.contains(TARGET) && !shown.contains("hello"),
        "{shown}"
    );

    // Only now does the run succeed, with no second write.
    assert_eq!(case.say("work").await, "ran completed");
    assert_eq!(case.deliveries(), 1);
    assert_eq!(case.file().unwrap().0, inode);
    assert_eq!(case.history().task.status, "completed");

    // A late delivery of the same write changes nothing.
    let late = case.late_delivery().await;
    assert!(late.output.contains("already_applied"), "{}", late.output);
    assert_eq!(case.file().unwrap().0, inode);
}

// B -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_write_that_never_happened_is_barred_and_its_key_is_never_reused() {
    let case = Case::new();
    case.died("die-before-delivery").await;
    assert_eq!((case.deliveries(), case.file()), (0, None));
    let (_, key) = case.original();

    assert_eq!(case.say("reconcile").await, "not_executed resolved=true");
    assert_eq!(
        (case.deliveries(), case.file()),
        (0, None),
        "nothing was written"
    );
    assert!(case.journal(&format!("{key}.barred")).exists());
    assert_eq!(case.open_intents(), 0);
    let r = &case.history().reconciliations[0];
    assert_eq!(
        (r.destination.as_str(), r.business_effect.as_str()),
        ("not_applied_barred", "absent")
    );

    // The original write can never apply, even delivered late.
    let late = case.late_delivery().await;
    assert!(late.output.contains("operation_barred"), "{}", late.output);
    assert_eq!(case.file(), None);

    // The work is no longer refused. Its next request for the same place carries the same key, so
    // the project refuses it: the executor is asked once, nothing is written, and the outcome
    // recorded is a failed execution, not a success (whether the work then completes is the goal
    // evaluation's call, not this write's).
    case.say("work").await;
    assert_eq!(case.deliveries(), 1);
    assert_eq!(case.file(), None);
    let h = case.history();
    let second = &h.intents[1].value;
    assert_eq!(second.result.as_deref(), Some("failed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_task_is_a_new_logical_write_with_its_own_key() {
    let case = Case::new();
    case.died("die-before-delivery").await;
    assert_eq!(case.say("reconcile").await, "not_executed resolved=true");
    let (_, barred) = case.original();

    // Explicitly: another task, hence another action key, performed by the normal path.
    let mut c = case.child("work", None);
    c.env("KB_TASK", "pw-task-2");
    assert_eq!(result_of(&run(c).await), "ran completed");
    assert_eq!(case.file().unwrap().1, CONTENT);
    assert_eq!(case.deliveries(), 1);
    assert!(
        case.journal(&format!("{barred}.barred")).exists(),
        "the old key stays barred"
    );
    let store = AgentStore::open(case.db()).unwrap();
    let second = store.history("pw-task-2").unwrap().unwrap();
    let new_key = ActionTag::parse(&second.intents[0].value.action)
        .unwrap()
        .key;
    assert_ne!(new_key, barred);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_staged_file_that_was_never_renamed_is_removed_so_it_can_never_be() {
    let case = Case::new();
    case.died("die-before-delivery").await;
    let (_, key) = case.original();
    // The state a kill between staging and renaming leaves: the content staged under the key.
    let staged = case.root().join(format!(".chip-write-{key}.tmp"));
    std::fs::write(&staged, CONTENT).unwrap();

    assert_eq!(case.say("reconcile").await, "not_executed resolved=true");
    assert!(
        !staged.exists(),
        "the rename it was waiting for can no longer happen"
    );
    assert_eq!(case.file(), None);
    assert!(case.journal(&format!("{key}.barred")).exists());
}

// C -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn evidence_the_project_cannot_give_leaves_the_write_uncertain() {
    // A live writer holds the project: nothing can be ruled out.
    let case = Case::new();
    case.died("die-before-delivery").await;
    let holder = tokio::spawn(run(case.child("hold-lock", None)));
    while !case.side.path().join("holding").exists() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let said = case.say("reconcile").await;
    assert!(said.starts_with("indeterminate resolved=false"), "{said}");
    assert!(said.contains("holds the project lock"), "{said}");
    assert_eq!(case.open_intents(), 1);
    assert!(
        !case
            .journal(&format!("{}.barred", case.original().1))
            .exists(),
        "nothing was barred"
    );
    std::fs::write(case.side.path().join("release"), "").unwrap();
    assert_eq!(result_of(&holder.await.unwrap()), "released");
    // And it can be asked again once the project is free.
    assert_eq!(case.say("reconcile").await, "not_executed resolved=true");

    // The target was replaced after the write: its identity is gone, so matching contents prove
    // nothing.
    let case = Case::new();
    case.died("die-after-write").await;
    let (inode, _) = case.file().unwrap();
    std::fs::remove_file(case.target()).unwrap();
    std::fs::write(case.target(), CONTENT).unwrap();
    assert_ne!(
        case.file().unwrap().0,
        inode,
        "a different file with the same contents"
    );
    let said = case.say("reconcile").await;
    assert!(said.starts_with("indeterminate resolved=false"), "{said}");
    assert!(
        !case
            .journal(&format!("{}.barred", case.original().1))
            .exists()
    );
    assert_eq!(case.say("work").await, "uncertain");
    assert_eq!(case.deliveries(), 1);
    let h = case.history();
    assert_eq!(h.reconciliations[0].destination, "unknown");
}

// D -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn contents_that_predate_the_write_are_not_evidence_that_it_happened() {
    let case = Case::new();
    std::fs::write(case.target(), CONTENT).unwrap();
    let before = case.file().unwrap();
    case.died("die-before-delivery").await;

    let said = case.say("reconcile").await;
    assert!(said.starts_with("indeterminate resolved=false"), "{said}");
    assert!(said.contains("business effect is present"), "{said}");
    assert_eq!(case.file(), Some(before), "the file was not touched");
    assert_eq!(case.open_intents(), 1);
    assert_eq!(case.say("work").await, "uncertain");
    assert_eq!(case.deliveries(), 0);
}

// E -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn two_processes_recovering_one_write_resolve_it_once_and_write_nothing() {
    for _ in 0..2 {
        let case = Case::new();
        case.died("die-after-write").await;
        let (inode, _) = case.file().unwrap();
        let gate = case.side.path().join("go");
        let (a, b) = (
            tokio::spawn(run(case.child("reconcile", Some(&gate)))),
            tokio::spawn(run(case.child("reconcile", Some(&gate)))),
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        std::fs::write(&gate, "").unwrap();
        let (a, b) = (result_of(&a.await.unwrap()), result_of(&b.await.unwrap()));
        eprintln!("reconcilers reported: {a} / {b}");
        let resolved = [&a, &b]
            .iter()
            .filter(|r| r.as_str() == "completed resolved=true")
            .count();
        assert_eq!(resolved, 1, "{a} / {b}");
        assert_eq!(case.deliveries(), 1);
        assert_eq!(case.file().unwrap().0, inode, "no second write");
        assert_eq!(
            case.history()
                .reconciliations
                .iter()
                .filter(|r| r.finding == "completed")
                .count(),
            1
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_operators_settlement_stands_against_a_later_reconciliation_and_a_late_write() {
    let case = Case::new();
    case.died("die-before-delivery").await;
    let (intent, _) = case.original();
    {
        let store = AgentStore::open(case.db()).unwrap();
        settle(
            &store,
            TASK,
            &intent,
            false,
            "ops@example",
            "the file is absent",
        )
        .unwrap();
    }
    assert_eq!(
        case.say("reconcile").await,
        "",
        "nothing is open to ask about"
    );
    let h = case.history();
    assert_eq!(
        h.intents[0].value.conclusion.as_deref(),
        Some("not_executed")
    );
    assert!(h.reconciliations.is_empty());
    assert_eq!(case.file(), None);
}

// F -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_different_target_or_content_under_the_same_key_is_refused_and_touches_nothing() {
    let case = Case::new();
    case.died("die-after-write").await;
    let before = case.file().unwrap();
    let (_, key) = case.original();
    let reconciler = ProjectWriteReconciler::new(case.root());

    for (path, content) in [("other.txt", CONTENT), (TARGET, "different\n")] {
        let status = reconciler.status(&case.query(path, content)).await;
        match status {
            DestinationStatus::Unknown { reason } => {
                assert!(reason.contains("binds this key to another"), "{reason}")
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }
    assert_eq!(case.file(), Some(before));
    assert!(
        !case.journal(&format!("{key}.barred")).exists(),
        "the destination was not mutated"
    );
    assert_eq!(case.entries(), [TARGET]);
    assert_eq!(case.open_intents(), 1);

    // The destination itself refuses the key for a different write.
    let mut request = ExecutionRequest::new(ExecutionId::new("e"), PROJECT_WRITE).with_inputs(
        [
            ("path".to_string(), InputValue::Text(TARGET.into())),
            (
                "content".to_string(),
                InputValue::Text("different\n".into()),
            ),
        ]
        .into(),
    );
    request.idempotency_key = Some(key);
    let r = ProjectExecutor::new(case.root())
        .execute(request)
        .await
        .unwrap();
    assert!(r.output.contains("key_bound_elsewhere"), "{}", r.output);
    assert_eq!(case.file().unwrap().1, CONTENT);
}

// Inspectability ------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn show_explains_what_is_unresolved_what_was_asked_and_why_it_was_not_enough() {
    let case = Case::new();
    std::fs::write(case.target(), CONTENT).unwrap();
    case.died("die-before-delivery").await;
    assert!(case.say("reconcile").await.starts_with("indeterminate"));

    let out = Command::new(env!("CARGO_BIN_EXE_stack-agent"))
        .args(["agent-state", "show", "--json", "--state-db"])
        .arg(case.db())
        .args(["--task-id", TASK])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let shown: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    let u = &shown["unresolved"][0];
    let (_, key) = case.original();
    assert_eq!(u["capability"], PROJECT_WRITE);
    assert_eq!(u["action_key"], key);
    assert_eq!(u["capability_has_a_reconciler"], true);
    assert_eq!(u["last_finding"], "indeterminate");
    assert_eq!(u["destination_reported"], "not_applied_barred");
    assert_eq!(u["key_barred"], true);
    assert!(
        u["why_not_resolved"]
            .as_str()
            .unwrap()
            .contains("business effect is present")
    );
    assert_eq!(u["manual_settlement_required"], true);
    assert!(
        !text.contains("hello") && !text.contains(TARGET),
        "no input of the request is shown"
    );
}
