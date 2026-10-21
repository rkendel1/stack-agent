//! Reconciliation in process: how the two answers are read, who may reconcile while another
//! reconciles or works, and what a late worker can no longer change. The real-process deaths are
//! in `crates/stack-agent/tests/reconciliation.rs`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chip_agent_state::feltdb::FeltDb;
use chip_agent_state::feltdb::agent_state::AuthorityClock;
use chip_agent_state::{
    ActionTag, AgentStore, Begun, BusinessCheck, DestinationStatus, DurableConfig, DurableWork,
    Finding, IntentValue, OperationRef, ReconcileQuery, Reconciler, StateError, begin, binding,
    decide, reconcilability, reconcile,
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
use tokio::sync::Notify;

const LEASE_MS: u64 = 60_000;
const GOAL: &str = "charge the customer once";
const CHARGE: &str = "pay.charge";

fn op(key: &str) -> OperationRef {
    OperationRef {
        key: key.into(),
        binding: "b1.x".into(),
    }
}

fn query() -> ReconcileQuery {
    ReconcileQuery {
        task_id: "t".into(),
        intent_key: "a1.t-exec-1".into(),
        capability: CHARGE.into(),
        operation: op("ak1.k"),
        inputs: BTreeMap::new(),
    }
}

fn committed(of: OperationRef) -> DestinationStatus {
    DestinationStatus::Committed {
        operation: of,
        receipt: Some("r".into()),
        reported: "charged 5".into(),
    }
}
fn present() -> BusinessCheck {
    BusinessCheck::Present {
        evidence: "e".into(),
    }
}
fn absent() -> BusinessCheck {
    BusinessCheck::Absent {
        evidence: "e".into(),
    }
}
fn unavailable() -> BusinessCheck {
    BusinessCheck::Unavailable {
        reason: "down".into(),
    }
}
fn not_applied(barred: bool) -> DestinationStatus {
    DestinationStatus::NotApplied {
        barred,
        basis: "b".into(),
    }
}
fn name(f: &Finding) -> &'static str {
    f.name()
}

#[test]
fn only_a_verified_effect_completes_and_only_a_barred_absence_is_not_executed() {
    let q = query();
    let unknown = DestinationStatus::Unknown { reason: "x".into() };
    let cases: Vec<(DestinationStatus, Option<BusinessCheck>, &str)> = vec![
        (committed(op("ak1.k")), Some(present()), "completed"),
        (committed(op("ak1.k")), Some(absent()), "indeterminate"),
        (committed(op("ak1.k")), Some(unavailable()), "indeterminate"),
        (committed(op("ak1.k")), None, "indeterminate"),
        // The report is about another operation: not the original, whatever the effect shows.
        (committed(op("ak1.other")), Some(present()), "indeterminate"),
        (
            committed(OperationRef {
                key: "ak1.k".into(),
                binding: "b1.other".into(),
            }),
            Some(present()),
            "indeterminate",
        ),
        (not_applied(true), Some(absent()), "not_executed"),
        // The destination says "not applied" and its state says "applied": a contradiction.
        (not_applied(true), Some(present()), "indeterminate"),
        (not_applied(true), Some(unavailable()), "indeterminate"),
        // A missing record is not proof unless the destination guarantees it cannot still apply.
        (not_applied(false), Some(absent()), "indeterminate"),
        (not_applied(false), Some(present()), "indeterminate"),
        (unknown.clone(), Some(present()), "indeterminate"),
        (unknown, None, "indeterminate"),
    ];
    for (status, check, want) in cases {
        let got = decide(&q, &status, check.as_ref());
        assert_eq!(name(&got), want, "{status:?} with {check:?}: {got:?}");
    }
}

// ---- an open intent for a keyed capability, left by a worker whose lease ran out -------------

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

struct Caps;

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        let mut d =
            CapabilityDescriptor::new(CapabilityId::new(CHARGE)?, "Charge", "Charges the customer")
                .with_effect(Effect::Consequential)
                .without_evidence_reuse()
                .with_idempotency(Idempotency::Carried);
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

/// The destination: it charges (counted), then while it does `during` runs.
#[derive(Default)]
struct Service {
    charges: AtomicUsize,
    during: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

#[async_trait::async_trait]
impl Executor for Service {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.charges.fetch_add(1, Ordering::SeqCst);
        if let Some(during) = self.during.lock().unwrap().take() {
            during();
        }
        Ok(ExecutionResult::success(r.id, "charged 5"))
    }
}

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

fn config(actor: &str) -> DurableConfig {
    DurableConfig {
        actor: actor.into(),
        lease_ms: LEASE_MS,
        effects: [(CHARGE.to_string(), Effect::Consequential)].into(),
        idempotency: [(CHARGE.to_string(), Idempotency::Carried)].into(),
    }
}

fn ready(begun: Result<Begun, StateError>) -> DurableWork {
    match begun {
        Ok(Begun::Ready(work)) => work,
        other => panic!("expected a runnable task: {:?}", other.err()),
    }
}

fn store(dir: &TempDir, clock: &Arc<Clock>) -> AgentStore {
    AgentStore::with_clock(
        FeltDb::open(dir.path().join("agent-state.log")).unwrap(),
        clock.clone(),
    )
}

/// What the destination knows, and how often it was asked.
#[derive(Default)]
struct Dest {
    committed: Mutex<Option<OperationRef>>,
    status_calls: AtomicUsize,
    /// When set, `status` waits here, so a reconciliation can be held in the middle of its query.
    hold: Mutex<Option<Arc<Notify>>>,
    entered: Notify,
}

#[async_trait::async_trait]
impl Reconciler for Dest {
    fn handles(&self, capability: &str) -> bool {
        capability == CHARGE
    }
    async fn status(&self, _q: &ReconcileQuery) -> DestinationStatus {
        self.status_calls.fetch_add(1, Ordering::SeqCst);
        let hold = self.hold.lock().unwrap().clone();
        if let Some(hold) = hold {
            self.entered.notify_one();
            hold.notified().await;
        }
        match self.committed.lock().unwrap().clone() {
            Some(operation) => committed(operation),
            None => not_applied(false),
        }
    }
    async fn verify(&self, _q: &ReconcileQuery) -> BusinessCheck {
        present()
    }
}

/// A task whose worker ran the charge and was then taken over: the intent is open and carries its
/// key. Returns the work (the old owner), the service, and the intent's key and operation.
async fn open_intent(
    store: &AgentStore,
    clock: &Arc<Clock>,
    service: Arc<Service>,
) -> (DurableWork, String, OperationRef) {
    let work = ready(begin(store, "task", GOAL, &config("chip-a")));
    let hold = Arc::new(Notify::new());
    let blocked = hold.clone();
    // The worker's action never returns in this attempt: the process is gone, as far as the
    // database is concerned.
    let died = tokio::time::timeout(
        Duration::from_millis(300),
        Agent::new(Arc::new(Model))
            .with_capabilities(Arc::new(Caps))
            .with_executor(work.executor(Arc::new(Hang(service))))
            .with_observer(Arc::new(ExecutionObserver))
            .run_work(
                &WorkSpec::new(WorkId::new("task"), WorkGoal::new(GOAL)),
                &NoLocalPolicy,
                &Script(Mutex::new(vec![charge()])),
            ),
    )
    .await;
    drop(blocked);
    assert!(died.is_err(), "the worker was meant to hang");
    clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);
    let h = store.history("task").unwrap().unwrap();
    let intent = &h.intents[0].value;
    let tag = ActionTag::parse(&intent.action).unwrap();
    (
        work,
        intent.key.clone(),
        OperationRef {
            key: tag.key,
            binding: tag.binding,
        },
    )
}

/// Performs the charge at the destination, then never returns.
struct Hang(Arc<Service>);

#[async_trait::async_trait]
impl Executor for Hang {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let _ = self.0.execute(r).await;
        std::future::pending().await
    }
}

#[tokio::test]
async fn a_reconciliation_in_progress_refuses_a_second_and_dispatches_nothing() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let service = Arc::new(Service::default());
    let (_old, intent, operation) = open_intent(&store, &clock, service.clone()).await;
    assert_eq!(service.charges.load(Ordering::SeqCst), 1);

    let dest = Arc::new(Dest::default());
    *dest.committed.lock().unwrap() = Some(operation);
    let release = Arc::new(Notify::new());
    *dest.hold.lock().unwrap() = Some(release.clone());

    let first = {
        let (store, dest, intent) = (store.clone(), dest.clone(), intent.clone());
        tokio::spawn(async move {
            reconcile(&store, "task", &intent, "recon-1", Some(&*dest))
                .await
                .unwrap()
        })
    };
    dest.entered.notified().await;
    // The first holds the task's lease and is waiting on the destination.
    let second = reconcile(&store, "task", &intent, "recon-2", Some(&*dest)).await;
    match second {
        Err(StateError::ReconciliationRefused { reason, .. }) => {
            assert!(reason.contains("holds the task's lease"), "{reason}")
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(
        dest.status_calls.load(Ordering::SeqCst),
        1,
        "the refused one asked nothing"
    );
    // Nor can a worker begin over it meanwhile.
    assert!(matches!(
        begin(&store, "task", GOAL, &config("chip-b")),
        Err(StateError::LeaseHeld { .. })
    ));

    release.notify_one();
    let done = first.await.unwrap();
    assert!(done.resolved);
    assert_eq!(
        service.charges.load(Ordering::SeqCst),
        1,
        "nothing was dispatched"
    );
    let h = store.history("task").unwrap().unwrap();
    assert_eq!(h.reconciliations.len(), 1);
    assert_eq!(h.intents[0].value.conclusion.as_deref(), Some("executed"));
}

#[tokio::test]
async fn a_worker_returning_after_reconciliation_cannot_change_what_it_recorded() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let service = Arc::new(Service::default());
    let dest = Arc::new(Dest::default());
    let work = ready(begin(&store, "task", GOAL, &config("chip-a")));
    let reconciled: Arc<Mutex<Option<String>>> = Arc::default();
    {
        // Mid-action: the lease runs out and a reconciliation resolves the intent from the
        // destination, which by then has the charge. The old worker's action is still running.
        let (store, clock, dest, service2, reconciled) = (
            store.clone(),
            clock.clone(),
            dest.clone(),
            service.clone(),
            reconciled.clone(),
        );
        *service.during.lock().unwrap() = Some(Box::new(move || {
            clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);
            let h = store.history("task").unwrap().unwrap();
            let tag = ActionTag::parse(&h.intents[0].value.action).unwrap();
            *dest.committed.lock().unwrap() = Some(OperationRef {
                key: tag.key,
                binding: tag.binding,
            });
            let key = h.intents[0].value.key.clone();
            let _ = &service2;
            let rt = tokio::runtime::Handle::current();
            let r = std::thread::scope(|s| {
                s.spawn(|| rt.block_on(reconcile(&store, "task", &key, "recon-1", Some(&*dest))))
                    .join()
                    .unwrap()
            });
            *reconciled.lock().unwrap() = Some(format!("{:?}", r.map(|r| r.resolved)));
        }));
    }
    let report = Agent::new(Arc::new(Model))
        .with_capabilities(Arc::new(Caps))
        .with_executor(work.executor(service.clone()))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &WorkSpec::new(WorkId::new("task"), WorkGoal::new(GOAL)),
            &NoLocalPolicy,
            &Script(Mutex::new(vec![charge()])),
        )
        .await;
    assert_eq!(reconciled.lock().unwrap().as_deref(), Some("Ok(true)"));
    // The old worker's own result arrives second: its observation id is taken, so it is refused
    // and the work is a failure, not a success.
    match &report.outcome {
        WorkOutcome::Failed { reason, .. } => {
            assert!(reason.contains("could not be made durable"), "{reason}")
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    assert_eq!(service.charges.load(Ordering::SeqCst), 1);
    let h = store.history("task").unwrap().unwrap();
    assert_eq!(h.intents[0].value.conclusion.as_deref(), Some("executed"));
    assert_eq!(
        h.intents[0].value.result, None,
        "resolved by reconciliation, not by an outcome"
    );
    // The next owner restores the reconciled observation: one execution, one charge.
    clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);
    let next = ready(begin(&store, "task", GOAL, &config("chip-c")));
    let restored = next.restored();
    assert_eq!(restored.len(), 1);
    assert!(
        restored[0]
            .result
            .output
            .starts_with("reconciled from the destination's own record")
    );
}

#[test]
fn an_intent_whose_recorded_request_does_not_hash_to_its_binding_is_not_reconciled() {
    let genuine = chip_agent_state::ActionTag {
        capability: CHARGE.into(),
        key: "ak1.k".into(),
        binding: binding(
            &ExecutionRequest::new(ExecutionId::new("e"), CHARGE)
                .with_inputs([("amount".to_string(), InputValue::Integer(5))].into()),
        ),
        idempotency: Idempotency::Carried,
    };
    let intent = |action: String| IntentValue {
        action,
        conclusion: None,
        evidence: Vec::new(),
        intended_at_ms: 0,
        intended_fence: 1,
        key: "a1.t-exec-1".into(),
        result: None,
        state: "open".into(),
        task_id: "t".into(),
        verified: false,
    };
    let with = |inputs: &str| intent(format!("{} inputs={inputs}", genuine.to_action()));
    assert_eq!(reconcilability(&with(r#"{"amount":5}"#)), Ok(()));
    // The same capability, similar inputs: a different request is not the original.
    let err = reconcilability(&with(r#"{"amount":6}"#)).unwrap_err();
    assert!(err.contains("do not hash to the recorded binding"), "{err}");
    let err = reconcilability(&intent(CHARGE.into())).unwrap_err();
    assert!(err.contains("does not carry an action key"), "{err}");
}
