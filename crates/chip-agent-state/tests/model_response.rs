//! Durable model responses: a response is recorded (`chip.ModelResponse`) before anything acts on
//! it, and an attempt interrupted after the response arrived and before anything it asked for was
//! attempted is answered from that record on resume, instead of a second, paid model call.
//!
//! What it does not change, and these tests hold it to:
//!
//! * A response is not evidence. Given back, it passes the same interpretation, validation and
//!   authorization as a fresh one, and an action it asks for is performed through the same durable
//!   executor: an intent first.
//! * A response that was acted on (an intent recorded after it) is never given back: the intent is
//!   the authority, and an interrupted action stays `OutcomeUncertain` until it is settled.
//! * A model call that never returned is not replayed: its outcome (received, answered, charged)
//!   is unknown, the resumed attempt asks again, and the history says the call was in flight.
//!
//! Every test counts the effects the fake payment service applied in its own ledger, independently
//! of anything recorded in FeltDB, and counts the model calls the fake provider received.
//!
//! A crash is the attempt's task aborted at a gate (no `finish`, no cleanup), and a restart is the
//! database closed and reopened from its file. The same window under a real `SIGKILL` of the real
//! binary is in `crates/stack-agent/tests/crash_windows.rs`.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_agent_state::feltdb::AtomicMutation;
use chip_agent_state::feltdb::FeltDb;
use chip_agent_state::feltdb::agent_state::AuthorityClock;
use chip_agent_state::{
    AgentStore, Begun, DurableConfig, DurableWork, MODEL_RESPONSE_COLLECTION, ResponseRestoration,
    StateError, begin, settle,
};
use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, CapabilityRequest, DecisionError, Effect, ExecutionError,
    ExecutionId, ExecutionObserver, ExecutionRequest, ExecutionResult, Executor, InputValue,
    NoLocalPolicy, StopCause, WorkControl, WorkDecision, WorkDecisionBoundary, WorkEvent, WorkGoal,
    WorkId, WorkReport, WorkSpec,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use tempfile::TempDir;
use tokio::sync::Notify;

const GOAL: &str = "charge the order once, then report";
const CHARGE: &str = "pay.charge";
const MODEL: &str = "scripted-model";
const LEASE_MS: u64 = 60_000;

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

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

/// The world outside the agent: one database file and one payment ledger, each surviving every
/// crash; and one clock, so a dead owner's lease can run out.
struct World {
    dir: TempDir,
    clock: Arc<Clock>,
    payments: Arc<Payments>,
    model: Arc<Model>,
}

impl World {
    fn new(script: Vec<&'static str>) -> Self {
        Self {
            dir: TempDir::new().unwrap(),
            clock: Arc::new(Clock::default()),
            payments: Arc::new(Payments::default()),
            model: Arc::new(Model {
                script,
                calls: AtomicUsize::new(0),
                hang_on: Mutex::new(None),
                entered: Notify::new(),
            }),
        }
    }

    /// The database, opened from its file as a restarted process opens it.
    fn store(&self) -> AgentStore {
        AgentStore::with_clock(
            FeltDb::open(self.dir.path().join("agent-state.log")).unwrap(),
            self.clock.clone(),
        )
    }

    /// The dead owner's lease runs out.
    fn lease_expires(&self) {
        self.clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);
    }
}

/// The provider: call `n` answers `script[n]` (the last entry once exhausted), counted; the call
/// numbered `hang_on` never returns.
struct Model {
    script: Vec<&'static str>,
    calls: AtomicUsize,
    hang_on: Mutex<Option<usize>>,
    entered: Notify,
}

impl Model {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl ModelProvider for Model {
    async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if *self.hang_on.lock().unwrap() == Some(n) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        let text = self.script.get(n).or(self.script.last()).unwrap();
        Ok(ModelResponse::new(
            format!("resp-{n}"),
            *text,
            Usage::new(10, 2),
        ))
    }
}

/// Reads the decision from the response's own text, so a response given back decides exactly
/// what it decided when it was received.
struct ByText;

impl WorkDecisionBoundary for ByText {
    fn interpret(
        &self,
        r: &ModelResponse,
        _c: &[Capability],
    ) -> Result<WorkDecision, DecisionError> {
        match r.output.as_str() {
            "charge" => Ok(WorkDecision::RequestCapability(
                CapabilityRequest::new(ExecutionId::new("x"), CapabilityId::new(CHARGE).unwrap())
                    .with_input("amount", InputValue::Integer(5)),
            )),
            "complete" => Ok(WorkDecision::Complete {
                summary: "charged once".into(),
            }),
            other => Err(DecisionError::InvalidDecision(format!(
                "unscripted: {other}"
            ))),
        }
    }
}

/// `pay.charge`, consequential and not keyed: its ledger is the ground truth of what was charged.
/// A gate can hold the request while it is validated (the second availability check; the first is
/// the work's discovery, before any model call), when nothing has been attempted yet, or inside the
/// effect (after the charge was applied, before its answer).
#[derive(Default)]
struct Payments {
    ledger: Mutex<Vec<String>>,
    hold_validation: std::sync::atomic::AtomicBool,
    availability_checks: AtomicUsize,
    hold_after_effect: std::sync::atomic::AtomicBool,
    entered: Notify,
}

impl Payments {
    fn charged(&self) -> usize {
        self.ledger.lock().unwrap().len()
    }
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
        let n = self.availability_checks.fetch_add(1, Ordering::SeqCst);
        if n == 1 && self.hold_validation.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        CapabilityAvailability::Available
    }
}

#[async_trait::async_trait]
impl Executor for Payments {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.ledger.lock().unwrap().push(r.id.0.clone());
        if self.hold_after_effect.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(ExecutionResult::success(r.id, "charged 5"))
    }
}

/// The run's owner, as `stack-agent work` composes it: every event and every model response goes
/// to FeltDB as the loop records it.
struct Recorder(DurableWork);

impl WorkControl for Recorder {
    fn stop_requested(&self) -> Option<StopCause> {
        None
    }
    fn on_event(&self, event: &WorkEvent) {
        self.0.record_event("task", event);
    }
    fn on_model_response(
        &self,
        turn: usize,
        model: &str,
        response: &ModelResponse,
        observed: &[ExecutionId],
    ) {
        self.0
            .record_model_response("task", turn, model, response, observed);
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

/// One attempt of the work, as `stack-agent work` runs it, against `model_name`.
async fn run(world: &World, work: DurableWork, model_name: &str) -> WorkReport {
    let spec = WorkSpec::new(WorkId::new("task"), WorkGoal::new(GOAL))
        .with_restored(work.restored())
        .with_settled(work.settled_actions())
        .with_restored_response(work.restored_response())
        .with_control(Arc::new(Recorder(work.clone())));
    Agent::with_model(world.model.clone(), model_name)
        .with_capabilities(world.payments.clone())
        .with_executor(work.executor(world.payments.clone()))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(&spec, &NoLocalPolicy, &ByText)
        .await
}

/// An attempt that is killed once `gate` is entered: nothing after that point happens, and the
/// attempt is never finished. Its database handle goes with it.
async fn killed_at(world: &World, gate: &Notify) {
    let store = world.store();
    let work = ready(begin(&store, "task", GOAL, &config()));
    let (model, payments) = (world.model.clone(), world.payments.clone());
    let task = tokio::spawn(async move {
        let spec = WorkSpec::new(WorkId::new("task"), WorkGoal::new(GOAL))
            .with_restored(work.restored())
            .with_restored_response(work.restored_response())
            .with_control(Arc::new(Recorder(work.clone())));
        Agent::with_model(model, MODEL)
            .with_capabilities(payments.clone())
            .with_executor(work.executor(payments))
            .with_observer(Arc::new(ExecutionObserver))
            .run_work(&spec, &NoLocalPolicy, &ByText)
            .await
    });
    gate.notified().await;
    task.abort();
    let _ = task.await;
    drop(store);
}

fn kinds(report: &WorkReport) -> Vec<&'static str> {
    report
        .events
        .iter()
        .filter_map(|e| match e {
            WorkEvent::ModelCalled { .. } => Some("ModelCalled"),
            WorkEvent::ModelResponseRestored { .. } => Some("ModelResponseRestored"),
            WorkEvent::ModelResponseDiscarded { .. } => Some("ModelResponseDiscarded"),
            WorkEvent::ObservationRestored { .. } => Some("ObservationRestored"),
            WorkEvent::ObservationRecorded { .. } => Some("ObservationRecorded"),
            WorkEvent::WorkCompleted { .. } => Some("WorkCompleted"),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The window this adds: the response arrived, nothing was attempted
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_response_received_and_never_acted_on_is_given_back_instead_of_asking_again() {
    let world = World::new(vec!["charge", "complete"]);
    // Attempt 1: the model answers "charge"; the process dies while the request is validated,
    // before any intent is recorded.
    world.payments.hold_validation.store(true, Ordering::SeqCst);
    killed_at(&world, &world.payments.entered).await;
    assert_eq!(world.model.calls(), 1);
    assert_eq!(
        world.payments.charged(),
        0,
        "nothing was charged before the crash"
    );

    // Attempt 2, in a "new process".
    world.lease_expires();
    let store = world.store();
    let work = ready(begin(&store, "task", GOAL, &config()));
    assert_eq!(
        work.response_restoration(),
        &ResponseRestoration::Offered {
            record: "task~a2~m000001".into()
        }
    );
    assert_eq!(work.interrupted_model_call(), None);
    let report = run(&world, work.clone(), MODEL).await;
    let finished = work.finish(&report, true).unwrap();

    // One model call in all for the "charge" decision: turn 1 of attempt 3 was answered from the
    // record. The second call is the "complete" the work had not asked for yet.
    assert_eq!(
        world.model.calls(),
        2,
        "the recorded response was not bought again"
    );
    assert_eq!(
        kinds(&report),
        [
            "ModelResponseRestored",
            "ObservationRecorded",
            "ModelCalled",
            "WorkCompleted"
        ]
    );
    // It was performed as any request is: through an intent, then the effect, exactly once.
    assert_eq!(world.payments.charged(), 1);
    let history = finished.history;
    assert_eq!(history.task.status, "completed");
    assert_eq!(history.intents.len(), 1);
    assert_eq!(history.task.unresolved_intents, 0);
    // Both responses are durable: attempt 2's (given back) and attempt 3's (the completion).
    let turns: Vec<(u64, u64)> = history
        .model_responses
        .iter()
        .map(|r| (r.attempt, r.turn))
        .collect();
    assert_eq!(turns, [(2, 1), (3, 2)]);
    assert!(history.model_calls_in_flight().is_empty());
    // The given-back turn is a runtime decision in the durable trajectory, and the decision
    // itself stays the model's claim.
    let a3: Vec<(&str, &str)> = history
        .events
        .iter()
        .filter(|e| e.attempt == 3)
        .map(|e| (e.event["kind"].as_str().unwrap(), e.provenance.as_str()))
        .collect();
    assert!(
        a3.contains(&("ModelResponseRestored", "runtime_decided")),
        "{a3:?}"
    );
    assert!(a3.contains(&("DecisionMade", "model_inferred")), "{a3:?}");

    // A third start finds the task finished: nothing is offered or run again.
    assert!(matches!(
        begin(&store, "task", GOAL, &config()),
        Ok(Begun::Finished(_))
    ));
    assert_eq!((world.model.calls(), world.payments.charged()), (2, 1));
}

#[tokio::test]
async fn a_response_from_another_model_is_discarded_and_the_configured_model_is_asked() {
    let world = World::new(vec!["charge", "charge", "complete"]);
    world.payments.hold_validation.store(true, Ordering::SeqCst);
    killed_at(&world, &world.payments.entered).await;
    world.lease_expires();
    let store = world.store();
    let work = ready(begin(&store, "task", GOAL, &config()));
    assert!(matches!(
        work.response_restoration(),
        ResponseRestoration::Offered { .. }
    ));
    // The work now asks another model: its judgment is not the recorded one's.
    let report = run(&world, work.clone(), "another-model").await;
    work.finish(&report, true).unwrap();
    assert_eq!(world.model.calls(), 3, "the configured model was asked");
    let discarded = report.events.iter().find_map(|e| match e {
        WorkEvent::ModelResponseDiscarded { reason, .. } => Some(reason.clone()),
        _ => None,
    });
    assert!(
        discarded.is_some_and(|r| r.contains("another-model")),
        "{:?}",
        kinds(&report)
    );
    assert_eq!(world.payments.charged(), 1);
}

// ---------------------------------------------------------------------------------------------
// What it must not change
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_response_that_was_acted_on_is_never_given_back_and_the_uncertain_charge_is_not_repeated()
{
    let world = World::new(vec!["charge", "charge", "complete"]);
    // Attempt 1: the intent is recorded, the charge is applied, and the process dies before the
    // answer: the outcome is unknown.
    world
        .payments
        .hold_after_effect
        .store(true, Ordering::SeqCst);
    killed_at(&world, &world.payments.entered).await;
    assert_eq!(world.payments.charged(), 1);

    world.lease_expires();
    let store = world.store();
    match begin(&store, "task", GOAL, &config()) {
        Err(StateError::OutcomeUncertain { intents, .. }) => assert_eq!(intents.len(), 1),
        Err(e) => panic!("expected OutcomeUncertain, got {e}"),
        Ok(_) => panic!("began over an uncertain charge"),
    }
    // The operator finds the charge in the payment service's ledger.
    let intent = store.history("task").unwrap().unwrap().intents[0]
        .value
        .key
        .clone();
    settle(
        &store,
        "task",
        &intent,
        true,
        "ops@example",
        "ledger shows the charge",
    )
    .unwrap();

    let work = ready(begin(&store, "task", GOAL, &config()));
    // The response that asked for the charge was acted on: an intent followed it.
    assert_eq!(
        work.response_restoration(),
        &ResponseRestoration::NotOffered {
            record: "task~a2~m000001".into(),
            reason: "the attempt recorded an intent after it".into(),
        }
    );
    assert_eq!(work.restored_response(), None);
    let report = run(&world, work.clone(), MODEL).await;
    let _ = work.finish(&report, false);
    // The model was asked again and asked for the charge again; the settled charge was refused
    // before an intent, so the ledger still has exactly one.
    assert_eq!(world.model.calls(), 2);
    assert!(!kinds(&report).contains(&"ModelResponseRestored"));
    assert_eq!(world.payments.charged(), 1, "the charge was not repeated");
}

#[tokio::test]
async fn a_model_call_that_never_returned_is_reported_in_flight_and_asked_again_not_replayed() {
    let world = World::new(vec!["charge", "charge", "complete"]);
    *world.model.hang_on.lock().unwrap() = Some(0);
    killed_at(&world, &world.model.entered).await;
    assert_eq!(world.model.calls(), 1);

    world.lease_expires();
    let store = world.store();
    let history = store.history("task").unwrap().unwrap();
    let in_flight = history.model_calls_in_flight();
    assert_eq!(in_flight.len(), 1);
    assert_eq!((in_flight[0].attempt, in_flight[0].turn), (2, 1));
    assert!(
        history.model_responses.is_empty(),
        "no response was received"
    );

    let work = ready(begin(&store, "task", GOAL, &config()));
    assert_eq!(work.response_restoration(), &ResponseRestoration::None);
    assert_eq!(
        work.interrupted_model_call().map(|c| (c.attempt, c.turn)),
        Some((2, 1))
    );
    let report = run(&world, work.clone(), MODEL).await;
    assert!(work.finish(&report, true).unwrap().history.task.status == "completed");
    // The lost call is not replayed (there is nothing to replay); it is asked again.
    assert_eq!(world.model.calls(), 3);
    assert_eq!(world.payments.charged(), 1);
}

#[tokio::test]
async fn a_completed_charge_and_a_crash_in_the_next_model_call_resume_without_charging_again() {
    let world = World::new(vec!["charge", "complete"]);
    // Attempt 1: the charge completes and is recorded; the process dies in the next model call.
    *world.model.hang_on.lock().unwrap() = Some(1);
    killed_at(&world, &world.model.entered).await;
    assert_eq!(world.payments.charged(), 1);

    world.lease_expires();
    let store = world.store();
    let work = ready(begin(&store, "task", GOAL, &config()));
    // The first response was acted on and its observation is restored; it is not offered.
    assert!(matches!(
        work.response_restoration(),
        ResponseRestoration::NotOffered { .. }
    ));
    let report = run(&world, work.clone(), MODEL).await;
    let finished = work.finish(&report, true).unwrap();
    assert_eq!(finished.history.task.status, "completed");
    assert_eq!(
        kinds(&report),
        ["ObservationRestored", "ModelCalled", "WorkCompleted"]
    );
    assert_eq!(
        world.payments.charged(),
        1,
        "the recorded charge was not repeated"
    );
    assert_eq!(world.model.calls(), 3);
}

#[tokio::test]
async fn a_finished_attempt_offers_nothing() {
    let world = World::new(vec!["charge", "complete"]);
    let store = world.store();
    let work = ready(begin(&store, "task", GOAL, &config()));
    let report = run(&world, work.clone(), MODEL).await;
    // Blocked by the host (not accepted), so a later attempt may begin: its predecessor finished.
    work.finish(&report, false).unwrap();
    world.lease_expires();
    let again = ready(begin(&store, "task", GOAL, &config()));
    assert_eq!(again.response_restoration(), &ResponseRestoration::None);
    assert_eq!(again.restored_response(), None);
}

#[tokio::test]
async fn a_tampered_response_record_is_invalid_history_and_nothing_runs() {
    let world = World::new(vec!["charge", "complete"]);
    world.payments.hold_validation.store(true, Ordering::SeqCst);
    killed_at(&world, &world.payments.entered).await;
    world.lease_expires();
    let store = world.store();
    // Overwrite the recorded output without its digest.
    let key = format!("{MODEL_RESPONSE_COLLECTION}:task~a2~m000001");
    let mut value = store
        .database()
        .list_collection(MODEL_RESPONSE_COLLECTION)
        .unwrap()
        .into_iter()
        .find(|r| r.key == key)
        .unwrap()
        .value;
    value["output"] = "complete".into();
    store
        .database()
        .apply_atomic_transaction_guarded(
            "tamper",
            None,
            None,
            &[],
            &[],
            &[AtomicMutation {
                capability: MODEL_RESPONSE_COLLECTION.into(),
                key,
                value: Some(value),
            }],
            None,
        )
        .unwrap();
    match begin(&store, "task", GOAL, &config()) {
        Err(StateError::InvalidHistory { detail, .. }) => {
            assert!(detail.contains("model response"), "{detail}")
        }
        Err(e) => panic!("expected InvalidHistory, got {e}"),
        Ok(_) => panic!("began over a tampered response"),
    }
    assert!(store.history("task").is_err());
    assert_eq!((world.model.calls(), world.payments.charged()), (1, 0));
}
