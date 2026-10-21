//! An execution whose executor cannot say how it ended ([`ExecutionError::OutcomeUnknown`]: the
//! request was sent and no answer came back) leaves its intent unresolved. A later attempt is then
//! refused as uncertain, exactly as after a crash at that point, and nothing is performed again.
//!
//! An executor's definite failure ([`ExecutionError::ExecutionFailed`]) is still an outcome: it is
//! recorded, and a later attempt may start.
//!
//! Found by the live MCP experiment (`chip-appport-source`, `examples/mcp_live.rs`): a write the
//! application performed, whose answer was lost, was recorded as failed, and the resumed attempt
//! sent it again.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chip_agent_state::feltdb::FeltDb;
use chip_agent_state::feltdb::agent_state::AuthorityClock;
use chip_agent_state::{AgentStore, Begun, DurableConfig, DurableWork, StateError, begin};
use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, CapabilityRequest, DecisionError, Effect, ExecutionError,
    ExecutionId, ExecutionObserver, ExecutionRequest, ExecutionResult, Executor, InputValue,
    NoLocalPolicy, WorkDecision, WorkDecisionBoundary, WorkGoal, WorkId, WorkOutcome, WorkSpec,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use tempfile::TempDir;

const GOAL: &str = "charge the order once";
const CHARGE: &str = "pay.charge";
const LEASE_MS: u64 = 60_000;

/// Wall time plus whatever a test advances it by: a lease lapses when the test says so, never
/// because the machine was slow.
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
    AgentStore::with_clock(
        FeltDb::open(dir.path().join("state.feltdb")).unwrap(),
        clock.clone(),
    )
}

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

/// `pay.charge`, consequential, that runs (counted) and then answers as `answer` says.
struct Payments {
    sent: Mutex<u32>,
    answer: fn() -> Result<String, ExecutionError>,
}

#[async_trait::async_trait]
impl CapabilityProvider for Payments {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        let mut d =
            CapabilityDescriptor::new(CapabilityId::new(CHARGE).unwrap(), CHARGE, "charges a card")
                .with_effect(Effect::Consequential)
                .without_evidence_reuse();
        d.inputs.push(CapabilityInput {
            name: "amount".into(),
            description: "amount".into(),
            required: true,
        });
        Ok(vec![d])
    }
    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

#[async_trait::async_trait]
impl Executor for Payments {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        *self.sent.lock().unwrap() += 1;
        (self.answer)().map(|output| ExecutionResult::success(r.id, output))
    }
}

fn config() -> DurableConfig {
    DurableConfig {
        lease_ms: LEASE_MS,
        effects: [(CHARGE.to_string(), Effect::Consequential)].into(),
        ..DurableConfig::default()
    }
}

fn ready(begun: Result<Begun, StateError>) -> DurableWork {
    match begun {
        Ok(Begun::Ready(work)) => work,
        Ok(Begun::Finished(_)) => panic!("the task is already finished"),
        Err(e) => panic!("begin refused: {e}"),
    }
}

async fn attempt(store: &AgentStore, payments: &Arc<Payments>) -> (DurableWork, WorkOutcome) {
    let work = ready(begin(store, "task", GOAL, &config()));
    let report = Agent::new(Arc::new(Model))
        .with_capabilities(payments.clone())
        .with_executor(work.executor(payments.clone()))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &WorkSpec::new(WorkId::new("task"), WorkGoal::new(GOAL))
                .with_restored(work.restored())
                .with_settled(work.settled_actions()),
            &NoLocalPolicy,
            &Script(Mutex::new(vec![
                charge(),
                charge(),
                WorkDecision::Complete {
                    summary: "charged".into(),
                },
            ])),
        )
        .await;
    let _ = work.finish(&report, false);
    (work, report.outcome)
}

#[tokio::test]
async fn an_unknown_outcome_leaves_the_intent_open_and_the_next_attempt_is_refused_as_uncertain() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let payments = Arc::new(Payments {
        sent: Mutex::new(0),
        answer: || {
            Err(ExecutionError::OutcomeUnknown(
                "sent; no answer came back".into(),
            ))
        },
    });
    let (_, outcome) = attempt(&store, &payments).await;
    // The work stops at the unknown outcome; the charge is not tried again in the same attempt.
    assert!(
        matches!(outcome, WorkOutcome::Failed { ref reason, .. } if reason.contains("outcome unknown")),
        "{outcome:?}"
    );
    assert_eq!(*payments.sent.lock().unwrap(), 1);
    let task = store
        .tasks()
        .unwrap()
        .into_iter()
        .find(|t| t.task_id == "task")
        .unwrap();
    assert_eq!(
        task.unresolved_intents, 1,
        "the intent was not resolved by an outcome it does not have"
    );

    clock.advance(LEASE_MS + 1);
    match begin(&store, "task", GOAL, &config()) {
        Err(StateError::OutcomeUncertain { intents, .. }) => assert_eq!(intents.len(), 1),
        Err(e) => panic!("expected OutcomeUncertain, got {e}"),
        Ok(_) => panic!("a later attempt began over an uncertain charge"),
    }
    assert_eq!(
        *payments.sent.lock().unwrap(),
        1,
        "the uncertain charge was not sent again"
    );
}

#[tokio::test]
async fn a_definite_failure_is_an_outcome_and_a_later_attempt_may_begin() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let store = store(&dir, &clock);
    let payments = Arc::new(Payments {
        sent: Mutex::new(0),
        answer: || {
            Err(ExecutionError::ExecutionFailed(
                "the card was declined".into(),
            ))
        },
    });
    let (_, outcome) = attempt(&store, &payments).await;
    assert!(matches!(outcome, WorkOutcome::Failed { .. }), "{outcome:?}");
    assert_eq!(
        store
            .tasks()
            .unwrap()
            .into_iter()
            .find(|t| t.task_id == "task")
            .unwrap()
            .unresolved_intents,
        0
    );
    clock.advance(LEASE_MS + 1);
    assert!(matches!(
        begin(&store, "task", GOAL, &config()),
        Ok(Begun::Ready(_))
    ));
}
