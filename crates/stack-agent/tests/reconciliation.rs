//! Resolving an uncertain consequential operation from the destination's own record, across real
//! process deaths, without dispatching it again.
//!
//! The **destination** (`pay.charge`, a fake payment service) keeps its state in files that
//! stack-agent never writes, and they are the ground truth of every assertion:
//!
//! * `invocations`: one line per delivery it received;
//! * `ledger`: its record of operations it applied (`key|binding|receipt|reported`) - what its
//!   status query reads;
//! * `accounts`: the business effect (`key|amount`) - what its verification reads, separately from
//!   the operation record;
//! * `tombstones`: keys it has barred from ever applying (its own expiry; a test plays that clock);
//! * switches the tests set: `status-unavailable`, `verify-unavailable`, `skip-business-effect`.
//!
//! The **child** is this test binary run again (`child_entry`), so the process that dies, and every
//! process that recovers, is a real one holding the real database. It dies *after the destination
//! applied its effect and returned a success, before the durable executor wrote anything*, or
//! *before the delivery reached the destination at all*.
//!
//! This proves the protocol works against a destination that provides the contract. It says nothing
//! about whether any real destination does.

#![cfg(unix)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chip_agent_state::{
    ActionTag, AgentStore, Begun, BusinessCheck, DestinationStatus, DurableConfig, OperationRef,
    ReconcileQuery, Reconciler, StateError, begin, binding, reconcile, settle,
};
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
const TASK: &str = "recon-task";
const LEASE_MS: u64 = 4_000;

// ---------------------------------------------------------------------------------------------
// The destination
// ---------------------------------------------------------------------------------------------

fn lines(dir: &Path, file: &str) -> Vec<String> {
    std::fs::read_to_string(dir.join(file))
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

fn append(dir: &Path, file: &str, line: &str) {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(file))
        .unwrap();
    writeln!(f, "{line}").unwrap();
    f.sync_all().unwrap();
}

fn die() {
    // A real death: no handler runs and nothing is flushed.
    let _ = Command::new("kill")
        .args(["-9", &std::process::id().to_string()])
        .status();
}

struct Destination {
    dir: PathBuf,
    mode: String,
}

#[async_trait::async_trait]
impl Executor for Destination {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        if self.mode == "die-before-delivery" {
            die();
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        let key = r.idempotency_key.clone().unwrap_or_default();
        append(&self.dir, "invocations", &format!("key={key}"));
        if lines(&self.dir, "tombstones").contains(&key) {
            return Err(ExecutionError::ExecutionFailed(
                "the destination refuses a key it has barred".into(),
            ));
        }
        let amount = match r.inputs.get("amount") {
            Some(InputValue::Integer(n)) => *n,
            _ => return Err(ExecutionError::InvalidRequest("no amount".into())),
        };
        let reported = format!("charged {amount}");
        append(
            &self.dir,
            "ledger",
            &format!(
                "{key}|{}|rcpt-{}|{reported}",
                binding(&r),
                key.chars().rev().take(6).collect::<String>()
            ),
        );
        if !self.dir.join("skip-business-effect").exists() {
            append(&self.dir, "accounts", &format!("{key}|{amount}"));
        }
        if self.mode == "die-holding-the-result" {
            die();
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        Ok(ExecutionResult::success(r.id, reported))
    }
}

/// What the destination answers when asked, from its own files. Reads only.
struct Fixture {
    dir: PathBuf,
}

#[async_trait::async_trait]
impl Reconciler for Fixture {
    fn handles(&self, capability: &str) -> bool {
        capability == CHARGE
    }

    async fn status(&self, q: &ReconcileQuery) -> DestinationStatus {
        if self.dir.join("status-unavailable").exists() {
            return DestinationStatus::Unknown {
                reason: "the status service is unavailable".into(),
            };
        }
        for line in lines(&self.dir, "ledger") {
            let parts: Vec<&str> = line.splitn(4, '|').collect();
            if parts[0] == q.operation.key {
                return DestinationStatus::Committed {
                    operation: OperationRef {
                        key: parts[0].into(),
                        binding: parts[1].into(),
                    },
                    receipt: Some(parts[2].into()),
                    reported: parts[3].into(),
                };
            }
        }
        if lines(&self.dir, "tombstones").contains(&q.operation.key) {
            return DestinationStatus::NotApplied {
                barred: true,
                basis: "the key was barred at the destination before it applied anything".into(),
            };
        }
        DestinationStatus::NotApplied {
            barred: false,
            basis: "no record of the key".into(),
        }
    }

    async fn verify(&self, q: &ReconcileQuery) -> BusinessCheck {
        if self.dir.join("verify-unavailable").exists() {
            return BusinessCheck::Unavailable {
                reason: "the accounts service is unavailable".into(),
            };
        }
        let want = match q.inputs.get("amount") {
            Some(InputValue::Integer(n)) => *n,
            _ => {
                return BusinessCheck::Unavailable {
                    reason: "no expected amount".into(),
                };
            }
        };
        match lines(&self.dir, "accounts")
            .iter()
            .find_map(|l| l.split_once('|').filter(|(k, _)| *k == q.operation.key))
        {
            Some((_, amount)) if amount == want.to_string() => BusinessCheck::Present {
                evidence: format!("account shows {amount} under the key"),
            },
            Some((_, amount)) => BusinessCheck::Absent {
                evidence: format!("account shows {amount}, not {want}"),
            },
            None => BusinessCheck::Absent {
                evidence: "the account shows no charge under the key".into(),
            },
        }
    }
}

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

fn config(idempotency: Idempotency) -> DurableConfig {
    DurableConfig {
        actor: format!("chip-{}", std::process::id()),
        lease_ms: LEASE_MS,
        effects: [(CHARGE.to_string(), Effect::Consequential)].into(),
        idempotency: [(CHARGE.to_string(), idempotency)].into(),
    }
}

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

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
        "carried" => Idempotency::Carried,
        _ => Idempotency::NotCarried,
    };
    if let Ok(gate) = std::env::var("KB_GATE") {
        while !Path::new(&gate).exists() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    let store = match AgentStore::open(var("KB_DB")) {
        Ok(store) => store,
        Err(e) => return report(&format!("open-refused {e}")),
    };
    if mode == "reconcile" {
        let history = store.history(TASK).unwrap().unwrap();
        let fixture = Fixture { dir };
        let mut said = Vec::new();
        for intent in history.unresolved_intents() {
            let r = reconcile(
                &store,
                TASK,
                &intent.value.key,
                &format!("recon-{}", std::process::id()),
                Some(&fixture),
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
    let work = match begin(&store, TASK, GOAL, &config(idempotency)) {
        Ok(Begun::Ready(work)) => work,
        Ok(Begun::Finished(_)) => return report("finished"),
        Err(StateError::OutcomeUncertain { .. }) => return report("uncertain"),
        Err(StateError::LeaseHeld { .. }) => return report("lease-held"),
        Err(e) => return report(&format!("refused {e}")),
    };
    let destination = Arc::new(Destination { dir, mode });
    let decisions = if work.restored().is_empty() {
        vec![
            charge(),
            WorkDecision::Complete {
                summary: "charged".into(),
            },
        ]
    } else {
        vec![WorkDecision::Complete {
            summary: "charged".into(),
        }]
    };
    let run = Agent::new(Arc::new(Model))
        .with_capabilities(Arc::new(Caps(idempotency)))
        .with_executor(work.executor(destination))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &WorkSpec::new(WorkId::new(TASK), WorkGoal::new(GOAL))
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

    async fn say(&self, mode: &str) -> String {
        result_of(&run(self.child(mode, None)).await)
    }

    /// Deliveries the destination received / operations it applied / business effects it holds.
    fn invocations(&self) -> usize {
        lines(self.dest.path(), "invocations").len()
    }
    fn applied(&self) -> usize {
        lines(self.dest.path(), "ledger").len()
    }
    fn charged(&self) -> usize {
        lines(self.dest.path(), "accounts").len()
    }

    fn switch(&self, name: &str) {
        std::fs::write(self.dest.path().join(name), "").unwrap();
    }

    /// The destination bars the original key (its own expiry; this test plays that clock).
    fn bar_the_original_key(&self) {
        append(self.dest.path(), "tombstones", &self.original().0);
    }

    /// The original intent's key, action key and binding, from FeltDB's record.
    fn original(&self) -> (String, String, String) {
        let store = AgentStore::open(self.db()).unwrap();
        let h = store.history(TASK).unwrap().unwrap();
        let intent = &h.intents[0].value;
        let tag = ActionTag::parse(&intent.action).expect("a carried key");
        (tag.key, intent.key.clone(), tag.binding)
    }

    /// (intents with an outcome, intents without).
    fn intents(&self) -> (usize, usize) {
        let store = AgentStore::open(self.db()).unwrap();
        let h = store.history(TASK).unwrap().unwrap();
        let open = h.unresolved_intents().len();
        (h.intents.len() - open, open)
    }

    fn history(&self) -> chip_agent_state::History {
        AgentStore::open(self.db())
            .unwrap()
            .history(TASK)
            .unwrap()
            .unwrap()
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

async fn lease_runs_out() {
    tokio::time::sleep(Duration::from_millis(LEASE_MS + 400)).await;
}

/// A first process that dies, a lease that runs out.
async fn died(case: &Case, mode: &str) {
    killed(&run(case.child(mode, None)).await);
    lease_runs_out().await;
}

// A -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_committed_effect_whose_result_was_lost_is_reconciled_without_dispatching_it_again() {
    let case = Case::new("carried");
    died(&case, "die-holding-the-result").await;
    assert_eq!(
        (case.invocations(), case.applied(), case.charged()),
        (1, 1, 1)
    );
    assert_eq!(case.intents(), (0, 1), "no outcome was committed");

    // Refused as uncertain: success is not available yet.
    assert_eq!(case.say("work").await, "uncertain");
    assert_eq!(case.invocations(), 1);

    assert_eq!(case.say("reconcile").await, "completed resolved=true");
    assert_eq!(case.invocations(), 1, "reconciliation dispatched nothing");
    assert_eq!(case.intents(), (1, 0));

    // The record names the original operation and says what was checked.
    let (action_key, intent, binding) = case.original();
    let h = case.history();
    let r = &h.reconciliations[0];
    assert_eq!(
        (
            r.intent_key.as_str(),
            r.action_key.as_str(),
            r.binding.as_str()
        ),
        (intent.as_str(), action_key.as_str(), binding.as_str())
    );
    assert_eq!(
        (
            r.finding.as_str(),
            r.destination.as_str(),
            r.business_effect.as_str()
        ),
        ("completed", "committed", "present")
    );
    assert!(
        !serde_json::to_string(r).unwrap().contains("amount"),
        "the request's inputs are not copied into the record"
    );
    assert_eq!(h.intents[0].value.conclusion.as_deref(), Some("executed"));

    // Only now does the run reach success, on the reconciled observation, with no second charge.
    assert_eq!(case.say("work").await, "ran completed");
    assert_eq!(
        case.invocations(),
        1,
        "the destination was never called again"
    );
    assert_eq!((case.applied(), case.charged()), (1, 1));
    assert_eq!(case.history().task.status, "completed");
}

// B -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn an_operation_the_destination_barred_before_it_applied_is_resolved_as_not_executed() {
    let case = Case::new("carried");
    died(&case, "die-before-delivery").await;
    assert_eq!(
        (case.invocations(), case.applied(), case.charged()),
        (0, 0, 0)
    );
    assert_eq!(case.say("work").await, "uncertain", "absence is not proof");

    // Without the destination's guarantee that it can no longer apply, nothing is concluded.
    assert!(
        case.say("reconcile")
            .await
            .starts_with("indeterminate resolved=false")
    );
    assert_eq!(case.intents(), (0, 1));

    case.bar_the_original_key();
    assert_eq!(case.say("reconcile").await, "not_executed resolved=true");
    assert_eq!(
        (case.invocations(), case.applied(), case.charged()),
        (0, 0, 0),
        "reconciliation applied and dispatched nothing"
    );
    assert_eq!(case.intents(), (1, 0));
    let h = case.history();
    assert_eq!(
        h.intents[0].value.conclusion.as_deref(),
        Some("not_executed")
    );
    assert_eq!(h.reconciliations.len(), 2, "both queries are on record");

    // Continuing is the normal path under the normal rules: the work is no longer refused and the
    // model's next request is dispatched like any other. This destination barred the key, so it
    // refuses the delivery: a barred key is final for that logical action (see recovery-matrix.md).
    assert_eq!(case.say("work").await, "ran not-completed");
    assert_eq!(case.invocations(), 1, "dispatched by the normal path, once");
    assert_eq!((case.applied(), case.charged()), (0, 0));
}

// C -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn evidence_that_cannot_decide_leaves_the_operation_uncertain_however_often_it_is_asked() {
    // The destination cannot say.
    let case = Case::new("carried");
    died(&case, "die-holding-the-result").await;
    case.switch("status-unavailable");
    for n in 1..=2 {
        let said = case.say("reconcile").await;
        assert!(said.starts_with("indeterminate resolved=false"), "{said}");
        assert_eq!(case.say("work").await, "uncertain");
        assert_eq!(
            case.history().reconciliations.len(),
            n,
            "every query is on record"
        );
    }
    assert_eq!(
        case.invocations(),
        1,
        "and nothing was dispatched meanwhile"
    );
    assert_eq!(case.intents(), (0, 1));
    let h = case.history();
    assert!(
        h.reconciliations
            .iter()
            .all(|r| r.destination == "unknown" && r.needs_settlement())
    );

    // The destination answers about another operation under the same key.
    let case = Case::new("carried");
    died(&case, "die-holding-the-result").await;
    let ledger = std::fs::read_to_string(case.dest.path().join("ledger")).unwrap();
    let (key, _, original) = case.original();
    std::fs::write(
        case.dest.path().join("ledger"),
        ledger.replace(&original, "b1.00000000000000000000000000000000"),
    )
    .unwrap();
    assert!(key.starts_with("ak1."));
    let said = case.say("reconcile").await;
    assert!(said.starts_with("indeterminate resolved=false"), "{said}");
    assert_eq!(case.history().reconciliations[0].destination, "mismatched");
    assert_eq!(case.intents(), (0, 1));

    // A capability that carries no key has no identity to ask about: nothing is written.
    let case = Case::new("not_carried");
    died(&case, "die-holding-the-result").await;
    let said = case.say("reconcile").await;
    assert!(said.contains("does not carry an action key"), "{said}");
    assert!(case.history().reconciliations.is_empty());
    assert_eq!(case.say("work").await, "uncertain");
    assert_eq!((case.invocations(), case.intents()), (1, (0, 1)));
}

// D -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_destination_that_reports_completion_without_its_business_effect_is_not_success() {
    let case = Case::new("carried");
    case.switch("skip-business-effect");
    died(&case, "die-holding-the-result").await;
    assert_eq!(
        (case.applied(), case.charged()),
        (1, 0),
        "recorded, but no charge landed"
    );

    let said = case.say("reconcile").await;
    assert!(said.starts_with("indeterminate resolved=false"), "{said}");
    let r = &case.history().reconciliations[0];
    assert_eq!(
        (r.destination.as_str(), r.business_effect.as_str()),
        ("committed", "absent")
    );
    assert!(
        r.reason
            .as_deref()
            .unwrap()
            .contains("business effect is absent")
    );
    assert_eq!(
        case.say("work").await,
        "uncertain",
        "the task is not successful"
    );
    assert_ne!(case.history().task.status, "completed");
    assert_eq!(case.invocations(), 1, "and the operation was not repeated");

    // The operator keeps the last word, with the evidence in front of them.
    let store = AgentStore::open(case.db()).unwrap();
    let (_, intent, _) = case.original();
    settle(
        &store,
        TASK,
        &intent,
        true,
        "ops@example",
        "ledger shows it; account does not",
    )
    .unwrap();
    drop(store);
    assert_eq!(case.intents(), (1, 0));
}

// E -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn reconciling_again_changes_nothing_and_never_overwrites_an_operators_settlement() {
    let case = Case::new("carried");
    died(&case, "die-holding-the-result").await;
    assert_eq!(case.say("reconcile").await, "completed resolved=true");
    for _ in 0..2 {
        // Across restarts: the intent is resolved, so there is nothing open to ask about.
        assert_eq!(case.say("reconcile").await, "");
    }
    let before = case.history();
    assert_eq!(before.reconciliations.len(), 1);
    assert_eq!(case.invocations(), 1);

    // An operator settled first: a later reconciliation cannot rewrite it.
    let case = Case::new("carried");
    died(&case, "die-holding-the-result").await;
    let (_, intent, _) = case.original();
    {
        let store = AgentStore::open(case.db()).unwrap();
        settle(&store, TASK, &intent, false, "ops@example", "they said so").unwrap();
    }
    assert_eq!(
        case.say("reconcile").await,
        "",
        "no intent is open to ask about"
    );
    let h = case.history();
    assert_eq!(
        h.intents[0].value.conclusion.as_deref(),
        Some("not_executed")
    );
    assert!(h.reconciliations.is_empty());
    assert_eq!(case.invocations(), 1);
}

// F -------------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn two_processes_reconciling_one_operation_resolve_it_once_and_dispatch_nothing() {
    for _ in 0..3 {
        let case = Case::new("carried");
        died(&case, "die-holding-the-result").await;
        let gate = case.dest.path().join("go");
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
        assert_eq!(resolved, 1, "exactly one resolved it: {a} / {b}");
        assert_eq!(case.invocations(), 1, "{a} / {b}");
        assert_eq!((case.applied(), case.charged()), (1, 1));
        let h = case.history();
        assert_eq!(h.intents[0].value.conclusion.as_deref(), Some("executed"));
        assert!(
            h.reconciliations
                .iter()
                .filter(|r| r.finding == "completed")
                .count()
                == 1
        );
        assert_eq!(case.say("work").await, "ran completed");
        assert_eq!(case.invocations(), 1);
    }
}
