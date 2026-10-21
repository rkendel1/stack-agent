//! The boundary between an external effect and its durable result, under a real `SIGKILL`.
//!
//! The existing library tests stand in for a crash with a future that never returns, against an
//! in-memory service. Here the process that performed the action really dies, and everything
//! afterwards is done by other processes against the same FeltDB file and the same destination:
//!
//! * the **destination** (`pay.charge`, a fake payment service) keeps its own state in files that
//!   stack-agent never writes: one line per call it received (`invocations`) and one per charge it
//!   applied (`effects`). Those two files, not any record of stack-agent's, say what happened;
//! * the **child** is this test binary run again (`child_entry`) with `KB_*` variables, so the
//!   process that dies is a real process holding the real database;
//! * the child dies *after the destination applied its effect and returned a successful result to
//!   the executor Chip built, and before the durable executor wrote anything about it*: the
//!   result is in the dead process's memory and nowhere else.
//!
//! | test | capability | what the next process must do |
//! | --- | --- | --- |
//! | result in hand, not committed | not idempotent | refuse as `OutcomeUncertain`; call the destination 0 more times |
//! | result in hand, not committed | deduplicating | ask again under the *same* key; the destination charges once |
//! | read-only call interrupted | observation | perform it again (no operator); the destination is read twice |
//! | two processes resume | not idempotent / deduplicating | at most one dispatches; never a second charge |

#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chip_agent_state::{AgentStore, Begun, DurableConfig, StateError, begin};
use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, CapabilityRequest, DecisionError, Effect, ExecutionError,
    ExecutionId, ExecutionObserver, ExecutionRequest, ExecutionResult, Executor, Idempotency,
    InputValue, NoLocalPolicy, WorkDecision, WorkDecisionBoundary, WorkGoal, WorkId, WorkOutcome,
    WorkSpec,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use tempfile::TempDir;

const GOAL: &str = "charge the customer once";
const CHARGE: &str = "pay.charge";
const TASK: &str = "kill-boundary-task";
const LEASE_MS: u64 = 1_500;

// ---------------------------------------------------------------------------------------------
// The child: a real process that performs the action and may die at the boundary
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

fn charge() -> WorkDecision {
    WorkDecision::RequestCapability(
        CapabilityRequest::new(ExecutionId::new("x"), CapabilityId::new(CHARGE).unwrap())
            .with_input("amount", InputValue::Integer(5)),
    )
}

struct Caps(Idempotency);

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        let mut d =
            CapabilityDescriptor::new(CapabilityId::new(CHARGE)?, "Charge", "Charges the customer")
                .with_effect(Effect::Consequential)
                .without_evidence_reuse()
                .with_idempotency(self.0);
        d.inputs.push(CapabilityInput {
            name: "amount".into(),
            description: "the amount".into(),
            required: true,
        });
        Ok(vec![d])
    }
    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

/// The fake payment service. Its state is the files in `dir`; with `deduplicates`, a key it has
/// already charged under is answered with that charge's answer and charges nothing.
struct Destination {
    dir: PathBuf,
    deduplicates: bool,
    /// Die after applying the effect and before handing the result back.
    die_holding_the_result: bool,
}

impl Destination {
    fn append(&self, file: &str, line: &str) {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(file))
            .unwrap();
        writeln!(f, "{line}").unwrap();
        f.sync_all().unwrap();
    }
}

#[async_trait::async_trait]
impl Executor for Destination {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let key = r.idempotency_key.clone().unwrap_or_default();
        self.append("invocations", &format!("key={key}"));
        let seen = std::fs::read_to_string(self.dir.join("effects")).unwrap_or_default();
        let answer = if self.deduplicates && !key.is_empty() && seen.contains(&format!("key={key}"))
        {
            "charged 5 (replayed)".to_string()
        } else {
            self.append("effects", &format!("key={key}"));
            "charged 5".to_string()
        };
        if self.die_holding_the_result {
            // A real death, not a returned error: no handler runs and nothing is flushed.
            let _ = Command::new("kill")
                .args(["-9", &std::process::id().to_string()])
                .status();
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        Ok(ExecutionResult::success(r.id, answer))
    }
}

const READ: &str = "ledger.read";

/// A read-only capability of the same destination: it changes nothing, and is declared so.
struct Reader {
    dir: PathBuf,
    die_holding_the_result: bool,
}

#[async_trait::async_trait]
impl CapabilityProvider for Reader {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(vec![
            CapabilityDescriptor::new(CapabilityId::new(READ)?, "Read", "Reads the ledger")
                .with_effect(Effect::Observation)
                .without_evidence_reuse(),
        ])
    }
    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

#[async_trait::async_trait]
impl Executor for Reader {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("reads"))
            .unwrap();
        writeln!(f, "read").unwrap();
        f.sync_all().unwrap();
        if self.die_holding_the_result {
            let _ = Command::new("kill")
                .args(["-9", &std::process::id().to_string()])
                .status();
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        Ok(ExecutionResult::success(r.id, "ledger: 1 charge"))
    }
}

fn read() -> WorkDecision {
    WorkDecision::RequestCapability(CapabilityRequest::new(
        ExecutionId::new("x"),
        CapabilityId::new(READ).unwrap(),
    ))
}

fn config(idempotency: Idempotency) -> DurableConfig {
    DurableConfig {
        actor: format!("chip-{}", std::process::id()),
        lease_ms: LEASE_MS,
        effects: [
            (CHARGE.to_string(), Effect::Consequential),
            (READ.to_string(), Effect::Observation),
        ]
        .into(),
        idempotency: [(CHARGE.to_string(), idempotency)].into(),
    }
}

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

/// What a child reports on its last stdout line, for the parent to read.
fn report(what: &str) {
    println!("KB_RESULT {what}");
}

/// Not a test: the body of the child process. Without `KB_MODE` it does nothing.
#[tokio::test(flavor = "multi_thread")]
async fn child_entry() {
    let Ok(mode) = std::env::var("KB_MODE") else {
        return;
    };
    let dir = PathBuf::from(var("KB_DEST"));
    let idempotency = match var("KB_IDEMPOTENCY").as_str() {
        "deduplicated" => Idempotency::Deduplicated,
        _ => Idempotency::NotCarried,
    };
    // Resumers wait for the starting gun so that they really do begin together.
    if let Ok(gate) = std::env::var("KB_GATE") {
        while !Path::new(&gate).exists() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    let store = match AgentStore::open(var("KB_DB")) {
        Ok(store) => store,
        Err(e) => return report(&format!("open-refused {e}")),
    };
    let work = match begin(&store, TASK, GOAL, &config(idempotency)) {
        Ok(Begun::Ready(work)) => work,
        Ok(Begun::Finished(_)) => return report("finished"),
        Err(StateError::OutcomeUncertain { .. }) => return report("uncertain"),
        Err(StateError::LeaseHeld { .. }) => return report("lease-held"),
        Err(e) => return report(&format!("refused {e}")),
    };
    if mode.ends_with("reading") {
        let reader = Arc::new(Reader {
            dir,
            die_holding_the_result: mode == "die-reading",
        });
        let observed = match work.reobserve(reader.clone(), None).await {
            Ok(done) => done.len(),
            Err(e) => return report(&format!("reobserve-refused {e}")),
        };
        let run = Agent::new(Arc::new(Model))
            .with_capabilities(reader.clone())
            .with_executor(work.executor(reader))
            .with_observer(Arc::new(ExecutionObserver))
            .run_work(
                &WorkSpec::new(WorkId::new(TASK), WorkGoal::new(GOAL))
                    .with_restored(work.restored())
                    .with_settled(work.settled_actions()),
                &NoLocalPolicy,
                &Script(Mutex::new(vec![
                    // A fresh task reads; a resumed one has the read restored and only completes.
                    if work.restored().is_empty() {
                        read()
                    } else {
                        WorkDecision::Complete {
                            summary: "read".into(),
                        }
                    },
                    WorkDecision::Complete {
                        summary: "read".into(),
                    },
                ])),
            )
            .await;
        let _ = work.finish(&run, true);
        return report(&format!("reobserved {observed}"));
    }
    let destination = Arc::new(Destination {
        dir,
        deduplicates: idempotency == Idempotency::Deduplicated,
        die_holding_the_result: mode == "die-holding-the-result",
    });
    let caps = Arc::new(Caps(idempotency));
    let run = Agent::new(Arc::new(Model))
        .with_capabilities(caps)
        .with_executor(work.executor(destination))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &WorkSpec::new(WorkId::new(TASK), WorkGoal::new(GOAL))
                .with_restored(work.restored())
                .with_settled(work.settled_actions()),
            &NoLocalPolicy,
            &Script(Mutex::new(vec![
                charge(),
                WorkDecision::Complete {
                    summary: "charged".into(),
                },
            ])),
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
// The parent: what the destination and the database say afterwards
// ---------------------------------------------------------------------------------------------

struct Case {
    dest: TempDir,
    state: TempDir,
    idempotency: &'static str,
}

impl Case {
    fn new(idempotency: &'static str) -> Self {
        Self {
            dest: TempDir::new().unwrap(),
            state: TempDir::new().unwrap(),
            idempotency,
        }
    }

    fn db(&self) -> PathBuf {
        self.state.path().join("agent-state.feltdb")
    }

    fn child(&self, mode: &str, gate: Option<&Path>) -> Command {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["--exact", "child_entry", "--nocapture", "--test-threads=1"])
            .env("KB_MODE", mode)
            .env("KB_DEST", self.dest.path())
            .env("KB_DB", self.db())
            .env("KB_IDEMPOTENCY", self.idempotency)
            .stdin(Stdio::null());
        if let Some(gate) = gate {
            c.env("KB_GATE", gate);
        }
        c
    }

    fn lines(&self, file: &str) -> Vec<String> {
        std::fs::read_to_string(self.dest.path().join(file))
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// Calls the destination received.
    fn invocations(&self) -> Vec<String> {
        self.lines("invocations")
    }

    /// Charges the destination applied.
    fn effects(&self) -> usize {
        self.lines("effects").len()
    }

    /// Intents that have a durable outcome / that do not.
    fn intents(&self) -> (usize, usize) {
        let store = AgentStore::open(self.db()).unwrap();
        let history = store.history(TASK).unwrap().unwrap();
        let open = history.unresolved_intents().len();
        (history.intents.len() - open, open)
    }
}

fn result_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        // The harness prints its own "test child_entry ... " on the same line.
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

async fn lease_runs_out() {
    tokio::time::sleep(Duration::from_millis(LEASE_MS + 400)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_result_received_and_lost_with_the_process_is_not_charged_again_without_a_key() {
    let case = Case::new("not_carried");
    let died = run(case.child("die-holding-the-result", None)).await;
    killed(&died);
    assert_eq!(case.effects(), 1, "the destination applied the charge");
    assert_eq!(
        case.intents(),
        (0, 1),
        "the result was in the dead process's memory only: no outcome was committed"
    );

    lease_runs_out().await;
    for _ in 0..2 {
        let out = run(case.child("resume", None)).await;
        assert_eq!(result_of(&out), "uncertain", "{out:?}");
    }
    assert_eq!(
        case.invocations().len(),
        1,
        "no process called the destination again"
    );
    assert_eq!(case.effects(), 1);
    assert_eq!(case.intents(), (0, 1), "the uncertainty is still recorded");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_result_received_and_lost_with_the_process_is_asked_again_under_the_same_key_when_deduplicated()
 {
    let case = Case::new("deduplicated");
    let died = run(case.child("die-holding-the-result", None)).await;
    killed(&died);
    assert_eq!(case.effects(), 1);
    assert_eq!(case.intents(), (0, 1));

    lease_runs_out().await;
    let out = run(case.child("resume", None)).await;
    assert_eq!(result_of(&out), "ran completed", "{out:?}");

    let calls = case.invocations();
    assert_eq!(calls.len(), 2, "the destination was asked a second time");
    assert_eq!(
        calls[0], calls[1],
        "under the same logical operation key: {calls:?}"
    );
    assert!(calls[0].len() > "key=".len(), "and a key was sent");
    assert_eq!(case.effects(), 1, "and it charged once");
    assert_eq!(case.intents(), (2, 0), "both intents now have an outcome");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_interrupted_by_a_kill_is_read_again_by_the_next_process_without_an_operator() {
    let case = Case::new("not_carried");
    killed(&run(case.child("die-reading", None)).await);
    assert_eq!(case.lines("reads").len(), 1, "the destination was read");
    assert_eq!(case.intents(), (0, 1), "and no outcome was committed");

    lease_runs_out().await;
    let out = run(case.child("resume-reading", None)).await;
    assert_eq!(result_of(&out), "reobserved 1", "{out:?}");
    assert_eq!(
        case.lines("reads").len(),
        2,
        "read-only, so repeating it is the declared recovery"
    );
    assert_eq!(case.effects(), 0, "and nothing was changed");
    assert_eq!(case.intents().1, 0, "no intent is left open");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_processes_resuming_one_uncertain_charge_never_dispatch_it_again() {
    let case = Case::new("not_carried");
    killed(&run(case.child("die-holding-the-result", None)).await);
    lease_runs_out().await;

    let gate = case.dest.path().join("go");
    let (a, b) = (
        case.child("resume", Some(&gate)),
        case.child("resume", Some(&gate)),
    );
    let (a, b) = (tokio::spawn(run(a)), tokio::spawn(run(b)));
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(&gate, "").unwrap();
    let (a, b) = (result_of(&a.await.unwrap()), result_of(&b.await.unwrap()));
    eprintln!("resumers reported: {a} / {b}");

    for r in [&a, &b] {
        assert!(
            r == "uncertain" || r == "lease-held" || r.starts_with("open-refused"),
            "a resumer may only be refused: {a} / {b}"
        );
    }
    assert_eq!(case.invocations().len(), 1, "{a} / {b}");
    assert_eq!(case.effects(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_processes_resuming_one_deduplicated_charge_dispatch_it_at_most_once_between_them() {
    let case = Case::new("deduplicated");
    killed(&run(case.child("die-holding-the-result", None)).await);
    lease_runs_out().await;

    let gate = case.dest.path().join("go");
    let (a, b) = (
        case.child("resume", Some(&gate)),
        case.child("resume", Some(&gate)),
    );
    let (a, b) = (tokio::spawn(run(a)), tokio::spawn(run(b)));
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(&gate, "").unwrap();
    let (a, b) = (result_of(&a.await.unwrap()), result_of(&b.await.unwrap()));
    eprintln!("resumers reported: {a} / {b}");

    let ran = [&a, &b].iter().filter(|r| r.starts_with("ran")).count();
    assert_eq!(ran, 1, "exactly one resumer owned the task: {a} / {b}");
    for r in [&a, &b] {
        assert!(
            r.starts_with("ran")
                || r == "lease-held"
                || r == "finished"
                || r.starts_with("open-refused"),
            "{a} / {b}"
        );
    }
    assert_eq!(
        case.invocations().len(),
        2,
        "the original call and one reissue: {a} / {b}"
    );
    assert_eq!(case.effects(), 1, "{a} / {b}");
}
