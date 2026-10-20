//! An operator settles an intent whose outcome was never observed, through FeltDB's real native
//! agent state; a refused resume gives its lease back.
//!
//! The settlement is a person's conclusion. It is recorded as one (who, what, on what evidence),
//! it reconciles the intent, and it is never an observation, a receipt or a completion's backing.
//! An action concluded to have taken effect is not performed again; one concluded not to have
//! taken effect is performed by the normal path if the work still asks for it.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use chip_agent_state::feltdb::FeltDb;
use chip_agent_state::feltdb::agent_state::AuthorityClock;
use chip_agent_state::{
    AgentStore, Begun, DurableConfig, DurableWork, OBSERVATION_COLLECTION, Restoration,
    SETTLEMENT_EXECUTION_ID, StateError, begin, intent_key, settle,
};
use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, CapabilityRequest, DecisionError, ExecutionError,
    ExecutionId, ExecutionObserver, ExecutionRequest, ExecutionResult, Executor, Idempotency,
    InputValue, NoLocalPolicy, WorkDecision, WorkDecisionBoundary, WorkGoal, WorkId, WorkOutcome,
    WorkReport, WorkSpec,
};
use chip_inference::{FxError, MessageRole, ModelProvider, ModelRequest, ModelResponse, Usage};
use tempfile::TempDir;

const LEASE_MS: u64 = 60_000;
const GOAL: &str = "charge the customer once and report";
const CHARGE: &str = "pay.charge";

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

/// Answers every call, and keeps what each request told the model.
#[derive(Default)]
struct Model(Mutex<Vec<String>>);

#[async_trait::async_trait]
impl ModelProvider for Model {
    async fn complete(&self, r: ModelRequest) -> Result<ModelResponse, FxError> {
        let told: Vec<String> = r
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .map(|m| m.content.clone())
            .collect();
        self.0.lock().unwrap().push(told.join("\n"));
        Ok(ModelResponse::new("r", "reply", Usage::new(1, 1)))
    }
}

struct Script {
    decisions: Vec<WorkDecision>,
    next: AtomicUsize,
}

impl WorkDecisionBoundary for Script {
    fn interpret(
        &self,
        _r: &ModelResponse,
        _c: &[Capability],
    ) -> Result<WorkDecision, DecisionError> {
        let i = self.next.fetch_add(1, Ordering::SeqCst);
        self.decisions
            .get(i)
            .cloned()
            .ok_or_else(|| DecisionError::InvalidDecision("the script ran out".into()))
    }
}

fn charge(amount: i64) -> WorkDecision {
    WorkDecision::RequestCapability(
        CapabilityRequest::new(ExecutionId::new("x"), CapabilityId::new(CHARGE).unwrap())
            .with_input("amount", InputValue::Integer(amount)),
    )
}

fn complete() -> WorkDecision {
    WorkDecision::Complete {
        summary: "charged".into(),
    }
}

struct Caps(Idempotency);

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        let mut d =
            CapabilityDescriptor::new(CapabilityId::new(CHARGE)?, "Charge", "Charges the customer")
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

/// A fake payment service (test-only), with no deduplication. `crash` makes the next call die:
/// before the charge, or after it.
#[derive(Default)]
struct Service {
    charges: AtomicUsize,
    crash_before: AtomicBool,
    crash_after: AtomicBool,
}

impl Service {
    fn charged(&self) -> usize {
        self.charges.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl Executor for Service {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        if self.crash_before.swap(false, Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        self.charges.fetch_add(1, Ordering::SeqCst);
        if self.crash_after.swap(false, Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(ExecutionResult::success(r.id, "charged"))
    }
}

#[derive(Default)]
struct Clock(AtomicU64);

impl Clock {
    fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl AuthorityClock for Clock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + self.0.load(Ordering::SeqCst)
    }
}

fn store(dir: &TempDir, clock: &Arc<Clock>) -> AgentStore {
    let db = FeltDb::open(dir.path().join("agent-state.log")).expect("open FeltDB");
    AgentStore::with_clock(db, clock.clone())
}

fn db_bytes(dir: &TempDir) -> u64 {
    std::fs::metadata(dir.path().join("agent-state.log"))
        .unwrap()
        .len()
}

fn config(actor: &str, idempotency: Idempotency) -> DurableConfig {
    DurableConfig {
        actor: actor.into(),
        lease_ms: LEASE_MS,
        idempotency: [(CHARGE.to_string(), idempotency)].into(),
    }
}

fn ready(begun: Result<Begun, StateError>) -> DurableWork {
    match begun {
        Ok(Begun::Ready(work)) => work,
        Ok(Begun::Finished(h)) => panic!("expected a runnable task, found {}", h.task.status),
        Err(e) => panic!("expected a runnable task: {e}"),
    }
}

async fn run(
    work: &DurableWork,
    model: Arc<Model>,
    service: Arc<Service>,
    decisions: Vec<WorkDecision>,
) -> WorkReport {
    Agent::new(model)
        .with_capabilities(Arc::new(Caps(Idempotency::NotCarried)))
        .with_executor(work.executor(service))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &WorkSpec::new(WorkId::new(work.task_id()), WorkGoal::new(GOAL))
                .with_restored(work.restored())
                .with_settled(work.settled_actions()),
            &NoLocalPolicy,
            &Script {
                decisions,
                next: AtomicUsize::new(0),
            },
        )
        .await
}

/// Attempt 1: charge 5, then die (`before` or after the charge). Returns the open intent's key.
async fn crash(
    dir: &TempDir,
    clock: &Arc<Clock>,
    task: &str,
    before: bool,
    s: &Arc<Service>,
) -> String {
    let store = store(dir, clock);
    let work = ready(begin(
        &store,
        task,
        GOAL,
        &config("chip-a", Idempotency::NotCarried),
    ));
    if before {
        s.crash_before.store(true, Ordering::SeqCst);
    } else {
        s.crash_after.store(true, Ordering::SeqCst);
    }
    let died = tokio::time::timeout(
        Duration::from_millis(300),
        run(
            &work,
            Arc::default(),
            s.clone(),
            vec![charge(5), complete()],
        ),
    )
    .await;
    assert!(died.is_err(), "the process was meant to die");
    clock.advance(LEASE_MS + 1);
    intent_key(work.attempt(), &format!("{task}-exec-1"))
}

/// A released lease ends 1 ms after it is released.
fn just_after(clock: &Arc<Clock>) {
    clock.advance(2);
}

fn uncertain(task: &str, key: &str) -> Option<StateError> {
    Some(StateError::OutcomeUncertain {
        task_id: task.into(),
        intents: vec![key.into()],
    })
}

// ---------------------------------------------------------------------------------------------
// The refusal releases its lease
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_refused_resume_gives_its_lease_back_so_the_task_is_acquirable_at_once() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let service = Arc::new(Service::default());
    let key = crash(&dir, &clock, "task-l", false, &service).await;
    let store = store(&dir, &clock);
    assert_eq!(
        begin(
            &store,
            "task-l",
            GOAL,
            &config("chip-b", Idempotency::NotCarried)
        )
        .err(),
        uncertain("task-l", &key)
    );
    // The refusal took the task (fence 2) and gave the lease back: it ends 1 ms later.
    let task = store.history("task-l").unwrap().unwrap().task;
    assert_eq!((task.fence, task.owner.as_str()), (2, "chip-b"));
    let now = clock.now_ms();
    assert!(task.lease.unwrap().expires_at <= now + 1);
    // Another process takes it straight away, with no lease to wait out: it is refused for the
    // same reason, not because the task is held.
    just_after(&clock);
    assert_eq!(
        begin(
            &store,
            "task-l",
            GOAL,
            &config("chip-c", Idempotency::NotCarried)
        )
        .err(),
        uncertain("task-l", &key)
    );
    assert_eq!(store.history("task-l").unwrap().unwrap().task.fence, 3);
    // And an operator settles it, again with no wait.
    just_after(&clock);
    settle(
        &store,
        "task-l",
        &key,
        true,
        "alice",
        "ledger line 1 shows the charge",
    )
    .unwrap();
    just_after(&clock);
    let work = ready(begin(
        &store,
        "task-l",
        GOAL,
        &config("chip-d", Idempotency::NotCarried),
    ));
    assert_eq!(work.settled().len(), 1);
    assert_eq!(service.charged(), 1);
}

// ---------------------------------------------------------------------------------------------
// Settling
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_effect_settled_as_executed_is_resumed_from_and_never_performed_again() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let service = Arc::new(Service::default());
    let key = crash(&dir, &clock, "task-x", false, &service).await;
    assert_eq!(service.charged(), 1);
    let store = store(&dir, &clock);
    assert_eq!(
        begin(
            &store,
            "task-x",
            GOAL,
            &config("chip-b", Idempotency::NotCarried)
        )
        .err(),
        uncertain("task-x", &key)
    );
    just_after(&clock);
    let settled = settle(
        &store,
        "task-x",
        &key,
        true,
        "alice@example.org",
        "ledger line 1 shows the charge of 5",
    )
    .unwrap();
    assert_eq!(settled.history.task.unresolved_intents, 0);

    // A fresh process: no operator step, no refusal.
    just_after(&clock);
    let store = self::store(&dir, &clock);
    let work = ready(begin(
        &store,
        "task-x",
        GOAL,
        &config("chip-c", Idempotency::NotCarried),
    ));
    assert_eq!(
        work.restoration(),
        &Restoration::Restored { observations: 0 }
    );
    // The model is told, as a ruled-out action, not as an observation.
    let model = Arc::new(Model::default());
    let report = run(
        &work,
        model.clone(),
        service.clone(),
        vec![charge(5), complete()],
    )
    .await;
    let told = model.0.lock().unwrap()[0].clone();
    assert!(
        told.contains(
            "capability pay.charge (amount=5): an operator (alice@example.org) concluded that its effect occurred"
        ),
        "{told}"
    );
    // Asked again anyway, the executor refuses it: nothing is charged a second time.
    assert_eq!(service.charged(), 1);
    match &report.outcome {
        WorkOutcome::Failed { reason, .. } => {
            assert!(
                reason.contains("was settled by alice@example.org"),
                "{reason}"
            )
        }
        other => panic!("{other:?}"),
    }
    let finished = work.finish(&report, false).unwrap();
    assert_eq!(finished.status, "blocked");
    // One intent: the refused request recorded none.
    assert_eq!(finished.history.intents.len(), 1);
}

#[tokio::test]
async fn an_effect_settled_as_not_executed_is_performed_by_the_normal_path() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let service = Arc::new(Service::default());
    let key = crash(&dir, &clock, "task-n", true, &service).await;
    assert_eq!(service.charged(), 0, "it died before the charge");
    let store = store(&dir, &clock);
    let _ = begin(
        &store,
        "task-n",
        GOAL,
        &config("chip-b", Idempotency::NotCarried),
    );
    just_after(&clock);
    settle(
        &store,
        "task-n",
        &key,
        false,
        "alice",
        "no ledger line for this charge",
    )
    .unwrap();
    just_after(&clock);
    let work = ready(begin(
        &store,
        "task-n",
        GOAL,
        &config("chip-c", Idempotency::NotCarried),
    ));
    let report = run(
        &work,
        Arc::default(),
        service.clone(),
        vec![charge(5), complete()],
    )
    .await;
    // The required action is not skipped: it is performed, observed and backs the completion.
    assert!(
        matches!(report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        report.outcome
    );
    assert_eq!(service.charged(), 1);
    let finished = work.finish(&report, true).unwrap();
    assert_eq!(finished.status, "completed");
    assert_eq!(finished.history.intents.len(), 2);
}

#[tokio::test]
async fn the_settlement_records_who_decided_what_on_which_evidence_and_survives_a_restart() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let service = Arc::new(Service::default());
    let key = crash(&dir, &clock, "task-p", false, &service).await;
    let store = store(&dir, &clock);
    let _ = begin(
        &store,
        "task-p",
        GOAL,
        &config("chip-b", Idempotency::NotCarried),
    );
    just_after(&clock);
    let settled = settle(
        &store,
        "task-p",
        &key,
        true,
        "alice",
        "ledger line 1 shows it",
    )
    .unwrap();
    assert_eq!(settled.record_id, format!("task-p~{key}"));
    drop(store);

    // A new process reads it back.
    let store = self::store(&dir, &clock);
    let s = store.settlements("task-p").unwrap();
    assert_eq!(s.len(), 1);
    let s = &s[0];
    assert_eq!(s.intent_key, key);
    assert_eq!(s.capability, CHARGE);
    assert!(s.binding.as_deref().unwrap().starts_with("b1."));
    assert_eq!(
        s.inputs,
        Some([("amount".to_string(), InputValue::Integer(5))].into())
    );
    assert!(s.executed);
    assert_eq!(s.decided_by.as_deref(), Some("alice"));
    assert_eq!(s.evidence, "ledger line 1 shows it");

    let h = store.history("task-p").unwrap().unwrap();
    // The intent is kept, resolved by the operator's conclusion, citing the settlement record.
    let intent = &h.intents[0].value;
    assert_eq!(
        (intent.key.as_str(), intent.state.as_str()),
        (key.as_str(), "resolved")
    );
    assert_eq!(intent.conclusion.as_deref(), Some("executed"));
    assert_eq!(intent.result, None, "no outcome was ever reported for it");
    assert!(
        intent
            .evidence
            .iter()
            .any(|c| c.collection.as_deref() == Some(OBSERVATION_COLLECTION)
                && c.id.as_deref() == Some(format!("task-p~{key}").as_str()))
    );
    // A checkpoint by the operator says what they concluded; the task's status is unchanged.
    let last = h.checkpoints.last().unwrap();
    assert_eq!(last.actor, "alice");
    assert_eq!(last.status, "running");
    assert!(last.summary.contains("settled intent"), "{}", last.summary);
    assert!(last.summary.contains("ledger line 1 shows it"));
    assert!(last.summary.contains("not an execution observation"));
    // The lease is given back.
    assert!(h.task.lease.unwrap().expires_at <= clock.now_ms() + 1);
}

#[tokio::test]
async fn a_settlement_is_not_a_receipt_an_observation_or_a_completions_backing() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let service = Arc::new(Service::default());
    let key = crash(&dir, &clock, "task-r", false, &service).await;
    let store = store(&dir, &clock);
    let _ = begin(
        &store,
        "task-r",
        GOAL,
        &config("chip-b", Idempotency::NotCarried),
    );
    just_after(&clock);
    settle(&store, "task-r", &key, true, "alice", "ledger line 1").unwrap();
    // The record: execution `operator` of attempt 0, no request, no receipt, and not `success`.
    let (_, value) = store
        .database()
        .get_value(&format!("{OBSERVATION_COLLECTION}:task-r~{key}"))
        .unwrap()
        .map(|v| ((), v))
        .unwrap();
    assert_eq!(value["execution_id"], SETTLEMENT_EXECUTION_ID);
    assert_eq!(value["attempt"], 0);
    assert_eq!(value["status"], "error");
    assert_eq!(value["receipt_id"], serde_json::Value::Null);
    assert!(value.get("request").is_none());
    assert!(value["output"].as_str().unwrap().starts_with(
        "operator settlement (an operator's conclusion, not an execution observation)"
    ));
    // Resumed and asked to complete at once: nothing observed backs it, so it is not completed.
    just_after(&clock);
    let work = ready(begin(
        &store,
        "task-r",
        GOAL,
        &config("chip-c", Idempotency::NotCarried),
    ));
    assert!(work.restored().is_empty());
    let report = run(&work, Arc::default(), service.clone(), vec![complete()]).await;
    let finished = work.finish(&report, true).unwrap();
    assert!(finished.completion_refused);
    assert_eq!(finished.status, "blocked");
}

#[tokio::test]
async fn an_invalid_settlement_is_refused_and_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let service = Arc::new(Service::default());
    let key = crash(&dir, &clock, "task-i", false, &service).await;
    let store = store(&dir, &clock);
    let _ = begin(
        &store,
        "task-i",
        GOAL,
        &config("chip-b", Idempotency::NotCarried),
    );
    just_after(&clock);
    let before = (store.history("task-i").unwrap(), db_bytes(&dir));
    let long = "x".repeat(chip_agent_state::MAX_SETTLEMENT_EVIDENCE_BYTES + 1);
    for (task, intent, who, evidence, why) in [
        ("task-i", key.as_str(), "alice", "", "no evidence"),
        ("task-i", key.as_str(), "alice", "   ", "blank evidence"),
        (
            "task-i",
            key.as_str(),
            "alice",
            long.as_str(),
            "evidence too long",
        ),
        (
            "task-i",
            key.as_str(),
            "alice",
            "line\nbreak",
            "evidence with a control character",
        ),
        ("task-i", key.as_str(), "", "ledger", "no identity"),
        (
            "task-i",
            key.as_str(),
            "alice smith",
            "ledger",
            "an identity with a space",
        ),
        ("task-i", "a1.nope", "alice", "ledger", "no such intent"),
        (
            "task-i",
            "bad key",
            "alice",
            "ledger",
            "a malformed intent key",
        ),
        ("task-none", key.as_str(), "alice", "ledger", "no such task"),
    ] {
        match settle(&store, task, intent, true, who, evidence) {
            Err(StateError::SettlementRefused { .. }) => {}
            other => panic!("{why}: {other:?}"),
        }
        assert_eq!(
            (store.history("task-i").unwrap(), db_bytes(&dir)),
            before,
            "{why}: nothing may be written"
        );
    }
    // Settled once; a second settlement, either way, is refused and changes nothing.
    settle(&store, "task-i", &key, true, "alice", "ledger line 1").unwrap();
    just_after(&clock);
    let settled = (store.history("task-i").unwrap(), db_bytes(&dir));
    for executed in [true, false] {
        match settle(&store, "task-i", &key, executed, "bob", "I disagree") {
            Err(StateError::SettlementRefused { reason, .. }) => {
                assert!(reason.contains("resolved already"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
        assert_eq!((store.history("task-i").unwrap(), db_bytes(&dir)), settled);
    }
    assert_eq!(
        store.settlements("task-i").unwrap()[0]
            .decided_by
            .as_deref(),
        Some("alice")
    );
}

#[tokio::test]
async fn an_intent_with_its_own_observation_or_a_held_task_is_not_settled() {
    // A worker holds the task: the operator waits for it to stop.
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let work = ready(begin(
        &store,
        "task-h",
        GOAL,
        &config("chip-a", Idempotency::NotCarried),
    ));
    let hung = tokio::time::timeout(
        Duration::from_millis(200),
        run(
            &work,
            Arc::default(),
            Arc::new(Service {
                crash_before: AtomicBool::new(true),
                ..Service::default()
            }),
            vec![charge(5)],
        ),
    )
    .await;
    assert!(hung.is_err());
    let key = intent_key(work.attempt(), "task-h-exec-1");
    let before = db_bytes(&dir);
    match settle(&store, "task-h", &key, true, "alice", "ledger") {
        Err(StateError::SettlementRefused { reason, .. }) => {
            assert!(reason.contains("a worker holds the task"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(db_bytes(&dir), before);

    // An intent whose execution was observed is reconciled from that, not by an operator.
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = self::store(&dir, &clock);
    let work = ready(begin(
        &store,
        "task-o",
        GOAL,
        &config("chip-a", Idempotency::NotCarried),
    ));
    let report = run(
        &work,
        Arc::default(),
        Arc::default(),
        vec![charge(5), complete()],
    )
    .await;
    work.finish(&report, true).unwrap();
    let key = intent_key(work.attempt(), "task-o-exec-1");
    match settle(&store, "task-o", &key, false, "alice", "ledger") {
        Err(StateError::SettlementRefused { reason, .. }) => {
            assert!(reason.contains("completed"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn two_operators_settling_one_intent_at_once_cannot_both_be_recorded() {
    for _ in 0..5 {
        let dir = TempDir::new().unwrap();
        let clock = Arc::new(Clock::default());
        let service = Arc::new(Service::default());
        let key = crash(&dir, &clock, "task-c", false, &service).await;
        let store = store(&dir, &clock);
        let _ = begin(
            &store,
            "task-c",
            GOAL,
            &config("chip-b", Idempotency::NotCarried),
        );
        just_after(&clock);
        let barrier = Arc::new(Barrier::new(2));
        let settlers: Vec<_> = [("alice", true), ("bob", false)]
            .into_iter()
            .map(|(who, executed)| {
                let (store, barrier, key) = (store.clone(), barrier.clone(), key.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    (
                        who,
                        executed,
                        settle(&store, "task-c", &key, executed, who, "my reading"),
                    )
                })
            })
            .collect();
        let results: Vec<_> = settlers.into_iter().map(|t| t.join().unwrap()).collect();
        let won: Vec<_> = results.iter().filter(|r| r.2.is_ok()).collect();
        assert_eq!(won.len(), 1, "{results:?}");
        for (_, _, r) in results.iter().filter(|r| r.2.is_err()) {
            assert!(
                matches!(r, Err(StateError::SettlementRefused { .. })),
                "{r:?}"
            );
        }
        // What is recorded is exactly the one that won.
        let (who, executed, _) = won[0];
        let recorded = &store.settlements("task-c").unwrap()[0];
        assert_eq!(
            (recorded.decided_by.as_deref(), recorded.executed),
            (Some(*who), *executed)
        );
        let h = store.history("task-c").unwrap().unwrap();
        assert_eq!(h.task.unresolved_intents, 0);
        assert_eq!(
            h.intents[0].value.conclusion.as_deref(),
            Some(if *executed {
                "executed"
            } else {
                "not_executed"
            })
        );
    }
}

#[tokio::test]
async fn a_settled_deduplicated_action_is_not_reissued_and_the_rest_of_the_trajectory_restores() {
    // P2's re-issue is for intents left open; a settled one is closed, and its action is not
    // performed again under any key.
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let work = ready(begin(
        &store,
        "task-d",
        GOAL,
        &config("chip-a", Idempotency::Deduplicated),
    ));
    let service = Arc::new(Service::default());
    // The first charge is observed; the second dies after its effect.
    let died = tokio::time::timeout(Duration::from_millis(300), async {
        let report = Agent::new(Arc::new(Model::default()))
            .with_capabilities(Arc::new(Caps(Idempotency::Deduplicated)))
            .with_executor(
                work.executor(Arc::new(CrashSecond(service.clone(), AtomicUsize::new(0)))),
            )
            .with_observer(Arc::new(ExecutionObserver))
            .run_work(
                &WorkSpec::new(WorkId::new("task-d"), WorkGoal::new(GOAL)),
                &NoLocalPolicy,
                &Script {
                    decisions: vec![charge(3), charge(5), complete()],
                    next: AtomicUsize::new(0),
                },
            )
            .await;
        report
    })
    .await;
    assert!(died.is_err());
    assert_eq!(service.charged(), 2);
    clock.advance(LEASE_MS + 1);
    let key = intent_key(work.attempt(), "task-d-exec-2");
    // Settled before any restart could re-issue it.
    settle(&store, "task-d", &key, true, "alice", "ledger line 2").unwrap();
    just_after(&clock);
    let work = ready(begin(
        &store,
        "task-d",
        GOAL,
        &config("chip-b", Idempotency::Deduplicated),
    ));
    assert!(work.reissuable().is_empty());
    assert_eq!(
        work.restoration(),
        &Restoration::Restored { observations: 1 }
    );
    assert_eq!(work.settled().len(), 1);
}

/// Charges through the service; the second call dies after its charge.
struct CrashSecond(Arc<Service>, AtomicUsize);

#[async_trait::async_trait]
impl Executor for CrashSecond {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let result = self.0.execute(r).await;
        if self.1.fetch_add(1, Ordering::SeqCst) == 1 {
            std::future::pending::<()>().await;
        }
        result
    }
}
