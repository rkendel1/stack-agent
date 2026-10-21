//! A worker whose ownership ended while its action ran returns its result late.
//!
//! The lease expiring does not stop the worker's action, only its authority to record. These tests
//! take the task from a running worker (its lease runs out and another owner begins, then an
//! operator settles the open intent), let the old worker's action return, and read what is
//! authoritative afterwards. The destination is a fake service with its own count.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_agent_state::feltdb::FeltDb;
use chip_agent_state::feltdb::agent_state::AuthorityClock;
use chip_agent_state::{
    AgentStore, Begun, DurableConfig, DurableWork, StateError, begin, intent_key, settle,
};
use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityProvider, CapabilityRequest, DecisionError, Effect, ExecutionError, ExecutionId,
    ExecutionObserver, ExecutionRequest, ExecutionResult, Executor, NoLocalPolicy, WorkDecision,
    WorkDecisionBoundary, WorkGoal, WorkId, WorkOutcome, WorkSpec,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use tempfile::TempDir;

const LEASE_MS: u64 = 60_000;
const GOAL: &str = "charge the customer once";
const CHARGE: &str = "pay.charge";

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
    WorkDecision::RequestCapability(CapabilityRequest::new(
        ExecutionId::new("x"),
        CapabilityId::new(CHARGE).unwrap(),
    ))
}

struct Caps;

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(vec![
            CapabilityDescriptor::new(CapabilityId::new(CHARGE)?, "Charge", "Charges the customer")
                .with_effect(Effect::Consequential)
                .without_evidence_reuse(),
        ])
    }
    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

/// The fake destination: it charges, and while it does, `during` runs (the world moves on).
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
        Ok(ExecutionResult::success(r.id, "charged"))
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
        ..DurableConfig::default()
    }
}

fn ready(begun: Result<Begun, StateError>) -> DurableWork {
    match begun {
        Ok(Begun::Ready(work)) => work,
        other => panic!("expected a runnable task: {:?}", other.err()),
    }
}

#[tokio::test]
async fn an_operator_settlement_made_while_the_old_worker_ran_is_not_overwritten_by_its_late_result()
 {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = AgentStore::with_clock(
        FeltDb::open(dir.path().join("agent-state.log")).unwrap(),
        clock.clone(),
    );
    let work = ready(begin(&store, "task", GOAL, &config("chip-a")));
    let intent = intent_key(work.attempt(), "task-exec-1");
    let service = Arc::new(Service::default());
    let settled: Arc<Mutex<Option<String>>> = Arc::default();
    {
        let (store, clock, settled) = (store.clone(), clock.clone(), settled.clone());
        let intent = intent.clone();
        *service.during.lock().unwrap() = Some(Box::new(move || {
            // Mid-action: the lease runs out, another owner is refused as uncertain (and gives the
            // lease back), and the operator concludes the charge did not happen.
            clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);
            let thief = begin(&store, "task", GOAL, &config("chip-b"));
            assert!(matches!(thief, Err(StateError::OutcomeUncertain { .. })));
            clock.0.fetch_add(2, Ordering::SeqCst);
            let r = settle(
                &store,
                "task",
                &intent,
                false,
                "ops@example",
                "the ledger shows none",
            );
            *settled.lock().unwrap() = Some(format!("{:?}", r.map(|s| s.settlement.executed)));
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
    // The old worker's action ran once, at the destination, whatever its lease said.
    assert_eq!(service.charges.load(Ordering::SeqCst), 1);
    assert_eq!(settled.lock().unwrap().as_deref(), Some("Ok(false)"));
    // Its late result is not a success and not a record: the work is a failure, and the one
    // refused write is the observation, which the operator's settlement already holds the id of.
    match &report.outcome {
        WorkOutcome::Failed { reason, .. } => {
            assert!(reason.contains("could not be made durable"), "{reason}")
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    assert!(
        matches!(work.failures().as_slice(), [StateError::Conflict { .. }]),
        "{:?}",
        work.failures()
    );

    // What is authoritative is the one outcome that was recorded first: the operator's.
    let h = store.history("task").unwrap().unwrap();
    assert_eq!(h.intents.len(), 1);
    let recorded = &h.intents[0].value;
    assert_eq!(
        (
            recorded.state.as_str(),
            recorded.conclusion.as_deref(),
            recorded.result.as_deref()
        ),
        ("resolved", Some("not_executed"), None),
        "the late result did not change the intent"
    );
    clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);
    let later = ready(begin(&store, "task", GOAL, &config("chip-c")));
    assert_eq!(
        later.restored().len(),
        0,
        "the late result was not restored"
    );
    assert_eq!(
        later.settled_actions().len(),
        1,
        "the operator's settlement is what the next owner is given"
    );
    // The limit this pins, not a guarantee: the settlement said "not executed" while the old
    // worker's action was still running, and the charge happened. FeltDB cannot know the action
    // had not stopped; only the operator's reading of the destination could.
}
