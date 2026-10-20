//! Stable idempotency keys across restarts, through FeltDB's real native agent state.
//!
//! Each logical action of a durable task gets a key derived from the task and the action's place
//! in the trajectory, never from the attempt or the process. A capability that carries the key
//! receives it; one that declares it applies at most one effect per key may be asked again after a
//! crash left its outcome unknown. Every other capability's unknown outcome stays
//! `OutcomeUncertain`. The capability here is a fake payment service, labelled as such: it is the
//! test's stand-in for an external system that deduplicates on the key.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chip_agent_state::feltdb::FeltDb;
use chip_agent_state::feltdb::agent_state::AuthorityClock;
use chip_agent_state::{
    ActionTag, AgentStore, Begun, DurableConfig, DurableWork, Restoration, StateError, action_key,
    begin, binding, intent_key,
};
use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, CapabilityRequest, DecisionError, ExecutionError,
    ExecutionId, ExecutionObserver, ExecutionRequest, ExecutionResult, Executor, Idempotency,
    InputValue, NoLocalPolicy, WorkDecision, WorkDecisionBoundary, WorkGoal, WorkId, WorkOutcome,
    WorkReport, WorkSpec,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use tempfile::TempDir;

const LEASE_MS: u64 = 60_000;
const GOAL: &str = "charge the customer once and report";
const CHARGE: &str = "pay.charge";

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

struct Model;

#[async_trait::async_trait]
impl ModelProvider for Model {
    async fn complete(&self, _r: ModelRequest) -> Result<ModelResponse, FxError> {
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

fn block() -> WorkDecision {
    WorkDecision::Block {
        reason: "stopping here".into(),
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

/// A fake payment service (test-only). With `deduplicates`, a key it has already charged under is
/// answered with that charge's outcome and charged nothing. `crash_after_effect` makes the next
/// call charge, then never return: the process dies after the effect, before its observation.
#[derive(Default)]
struct Service {
    deduplicates: bool,
    charges: Mutex<Vec<(Option<String>, i64)>>,
    answers: Mutex<BTreeMap<String, String>>,
    replays: AtomicUsize,
    keys: Mutex<Vec<Option<String>>>,
    crash_after_effect: AtomicBool,
}

impl Service {
    fn deduplicating() -> Arc<Self> {
        Arc::new(Self {
            deduplicates: true,
            ..Self::default()
        })
    }
    fn charged(&self) -> usize {
        self.charges.lock().unwrap().len()
    }
    fn keys(&self) -> Vec<Option<String>> {
        self.keys.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Executor for Service {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.keys.lock().unwrap().push(r.idempotency_key.clone());
        let amount = match r.inputs.get("amount") {
            Some(InputValue::Integer(n)) => *n,
            _ => return Err(ExecutionError::InvalidRequest("no amount".into())),
        };
        if self.deduplicates {
            if let Some(key) = &r.idempotency_key {
                if let Some(answer) = self.answers.lock().unwrap().get(key) {
                    self.replays.fetch_add(1, Ordering::SeqCst);
                    return Ok(ExecutionResult::success(r.id, answer.clone()));
                }
            }
        }
        let answer = format!("charged {amount}");
        self.charges
            .lock()
            .unwrap()
            .push((r.idempotency_key.clone(), amount));
        if let (true, Some(key)) = (self.deduplicates, &r.idempotency_key) {
            self.answers
                .lock()
                .unwrap()
                .insert(key.clone(), answer.clone());
        }
        if self.crash_after_effect.swap(false, Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(ExecutionResult::success(r.id, answer))
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

/// One "process": its own handle on the database file.
fn store(dir: &TempDir, clock: &Arc<Clock>) -> AgentStore {
    let db = FeltDb::open(dir.path().join("agent-state.log")).expect("open FeltDB");
    AgentStore::with_clock(db, clock.clone())
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
    idempotency: Idempotency,
    service: Arc<dyn Executor>,
    decisions: Vec<WorkDecision>,
) -> WorkReport {
    Agent::new(Arc::new(Model))
        .with_capabilities(Arc::new(Caps(idempotency)))
        .with_executor(work.executor(service))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &WorkSpec::new(WorkId::new(work.task_id()), WorkGoal::new(GOAL))
                .with_restored(work.restored()),
            &NoLocalPolicy,
            &Script {
                decisions,
                next: AtomicUsize::new(0),
            },
        )
        .await
}

/// Attempt 1 in its own process: charge, and die after the charge, before its observation.
/// Returns the attempt's intent key for the charge.
async fn crash_after_charge(
    dir: &TempDir,
    clock: &Arc<Clock>,
    task: &str,
    idempotency: Idempotency,
    service: &Arc<Service>,
) -> String {
    let store = store(dir, clock);
    let work = ready(begin(&store, task, GOAL, &config("chip-a", idempotency)));
    service.crash_after_effect.store(true, Ordering::SeqCst);
    let crashed = tokio::time::timeout(
        Duration::from_millis(300),
        run(
            &work,
            idempotency,
            service.clone(),
            vec![charge(5), complete()],
        ),
    )
    .await;
    assert!(crashed.is_err(), "the process was meant to die");
    assert_eq!(service.charged(), 1);
    let key = intent_key(work.attempt(), &format!("{task}-exec-1"));
    clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);
    key
}

// ---------------------------------------------------------------------------------------------
// The key
// ---------------------------------------------------------------------------------------------

#[test]
fn a_logical_key_names_the_task_and_the_action_and_nothing_about_the_attempt() {
    let k = action_key("task-1", "task-1-exec-2");
    // Deterministic: every process derives it again.
    assert_eq!(k, action_key("task-1", "task-1-exec-2"));
    assert!(k.starts_with("ak1.") && k.len() == 36, "{k}");
    assert!(chip_agent_state::is_identifier(&k));
    // Another action, or the same position in another task, is another key.
    assert_ne!(k, action_key("task-1", "task-1-exec-3"));
    assert_ne!(k, action_key("task-2", "task-1-exec-2"));
    // The attempt-scoped intent key is a different thing and stays as it was.
    assert_eq!(intent_key(2, "task-1-exec-2"), "a2.task-1-exec-2");
    assert_eq!(intent_key(7, "task-1-exec-2"), "a7.task-1-exec-2");

    // The binding is the request's capability and typed inputs, and nothing else.
    let request = |amount| {
        ExecutionRequest::new(ExecutionId::new("e"), CHARGE)
            .with_inputs([("amount".to_string(), InputValue::Integer(amount))].into())
    };
    assert_eq!(binding(&request(5)), binding(&request(5)));
    assert_ne!(binding(&request(5)), binding(&request(6)));
    let mut keyed = request(5);
    keyed.id = ExecutionId::new("other");
    keyed.idempotency_key = Some("ak1.x".into());
    assert_eq!(binding(&keyed), binding(&request(5)));

    // The intent's action text round-trips; a bare capability id carries no tag.
    let tag = ActionTag {
        capability: CHARGE.into(),
        key: k.clone(),
        binding: binding(&request(5)),
        idempotency: Idempotency::Deduplicated,
    };
    assert_eq!(ActionTag::parse(&tag.to_action()), Some(tag));
    assert_eq!(ActionTag::parse(CHARGE), None);
    assert_eq!(ActionTag::parse("op.perform"), None);
}

#[tokio::test]
async fn a_capability_that_carries_the_key_receives_it_and_one_that_cannot_does_not() {
    for idempotency in [
        Idempotency::Carried,
        Idempotency::Deduplicated,
        Idempotency::NotCarried,
    ] {
        let dir = TempDir::new().unwrap();
        let clock = Arc::new(Clock::default());
        let work = ready(begin(
            &store(&dir, &clock),
            "task-k",
            GOAL,
            &config("chip-a", idempotency),
        ));
        let service = Arc::new(Service::default());
        let report = run(
            &work,
            idempotency,
            service.clone(),
            vec![charge(5), charge(7), complete()],
        )
        .await;
        assert!(matches!(report.outcome, WorkOutcome::Completed { .. }));
        let keys = service.keys();
        if idempotency == Idempotency::NotCarried {
            assert_eq!(keys, vec![None, None], "{idempotency:?}");
        } else {
            // Each action its own key, the one derived from the task and its place.
            assert_eq!(
                keys,
                vec![
                    Some(action_key("task-k", "task-k-exec-1")),
                    Some(action_key("task-k", "task-k-exec-2")),
                ],
                "{idempotency:?}"
            );
        }
        // The intent records the key for a capability that carries it, and no key for one that
        // cannot. Either way it records the request's binding and inputs, so an operator's
        // settlement names exactly the request it settles.
        let history = store(&dir, &clock).history("task-k").unwrap().unwrap();
        assert_eq!(history.task.status, "running");
        let actions: Vec<Option<ActionTag>> = history
            .intents
            .iter()
            .map(|i| ActionTag::parse(&i.value.action))
            .collect();
        assert_eq!(actions.len(), 2);
        for (i, tag) in actions.iter().enumerate() {
            match idempotency {
                Idempotency::NotCarried => {
                    assert_eq!(tag, &None);
                    let amount = if i == 0 { 5 } else { 7 };
                    let request = ExecutionRequest::new(ExecutionId::new("e"), CHARGE)
                        .with_inputs([("amount".to_string(), InputValue::Integer(amount))].into());
                    assert_eq!(
                        history.intents[i].value.action,
                        format!(
                            "{CHARGE} binding={} inputs={{\"amount\":{amount}}}",
                            binding(&request)
                        )
                    );
                }
                _ => assert_eq!(tag.as_ref().unwrap().idempotency, idempotency),
            }
        }
        assert_eq!(work.finish(&report, true).unwrap().status, "completed");
    }
}

// ---------------------------------------------------------------------------------------------
// After a crash: deduplicated, and not
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_deduplicating_capability_is_asked_again_under_the_same_key_and_charges_once() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let service = Service::deduplicating();
    let first_intent =
        crash_after_charge(&dir, &clock, "task-d", Idempotency::Deduplicated, &service).await;

    // A new process, a new attempt: the open intent does not block it.
    let store = store(&dir, &clock);
    let work = ready(begin(
        &store,
        "task-d",
        GOAL,
        &config("chip-b", Idempotency::Deduplicated),
    ));
    assert_eq!(work.attempt(), 3);
    let open = work.reissuable();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].intent_key, first_intent);
    assert_eq!(
        store
            .history("task-d")
            .unwrap()
            .unwrap()
            .task
            .unresolved_intents,
        1
    );

    let report = run(
        &work,
        Idempotency::Deduplicated,
        service.clone(),
        vec![charge(5), complete()],
    )
    .await;
    assert!(
        matches!(report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        report.outcome
    );
    // The same key in both processes and both attempts; the service charged once and answered
    // the second request with the first charge's outcome.
    let key = action_key("task-d", "task-d-exec-1");
    assert_eq!(service.keys(), vec![Some(key.clone()), Some(key.clone())]);
    assert_eq!(service.charged(), 1);
    assert_eq!(service.replays.load(Ordering::SeqCst), 1);
    assert!(work.reissuable().is_empty());

    let finished = work.finish(&report, true).unwrap();
    assert_eq!(finished.status, "completed");
    assert_eq!(finished.history.task.unresolved_intents, 0);
    // The first intent is settled from the second execution's observation, and says so.
    let settled = finished
        .history
        .intents
        .iter()
        .find(|i| i.value.key == first_intent)
        .unwrap();
    assert_eq!(settled.value.state, "resolved");
    assert_eq!(settled.value.conclusion.as_deref(), Some("executed"));
    let cited: Vec<_> = settled
        .value
        .evidence
        .iter()
        .filter_map(|c| c.id.clone())
        .collect();
    assert_eq!(
        cited,
        vec![format!("task-d~{}", intent_key(3, "task-d-exec-1"))]
    );
    assert_eq!(service.charged(), 1);
}

#[tokio::test]
async fn a_reissued_action_restores_once_and_the_superseded_intent_is_not_an_observation() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let service = Service::deduplicating();
    crash_after_charge(&dir, &clock, "task-r", Idempotency::Deduplicated, &service).await;
    let work = ready(begin(
        &store(&dir, &clock),
        "task-r",
        GOAL,
        &config("chip-b", Idempotency::Deduplicated),
    ));
    let report = run(
        &work,
        Idempotency::Deduplicated,
        service.clone(),
        vec![charge(5), block()],
    )
    .await;
    assert_eq!(work.finish(&report, false).unwrap().status, "blocked");
    clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);

    // Attempt 3 restores the one charge that was observed, and nothing for the intent it settled.
    let work = ready(begin(
        &store(&dir, &clock),
        "task-r",
        GOAL,
        &config("chip-c", Idempotency::Deduplicated),
    ));
    assert_eq!(
        work.restoration(),
        &Restoration::Restored { observations: 1 }
    );
    assert_eq!(work.restored()[0].result.output, "charged 5");
    let report = run(
        &work,
        Idempotency::Deduplicated,
        service.clone(),
        vec![complete()],
    )
    .await;
    assert_eq!(work.finish(&report, true).unwrap().status, "completed");
    assert_eq!(service.charged(), 1);
}

#[tokio::test]
async fn reusing_the_key_for_a_different_payload_is_refused_before_anything_runs() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let service = Service::deduplicating();
    crash_after_charge(&dir, &clock, "task-c", Idempotency::Deduplicated, &service).await;
    let work = ready(begin(
        &store(&dir, &clock),
        "task-c",
        GOAL,
        &config("chip-b", Idempotency::Deduplicated),
    ));
    // The model now asks for another amount at the same place in the trajectory: the key it
    // would get already names the charge of 5.
    let report = run(
        &work,
        Idempotency::Deduplicated,
        service.clone(),
        vec![charge(9), complete()],
    )
    .await;
    assert_eq!(service.keys().len(), 1, "the service was not called again");
    assert_eq!(service.charged(), 1);
    let refused = report
        .events
        .iter()
        .any(|e| format!("{e:?}").contains("already names another request"));
    assert!(refused, "{:#?}", report.events);
    // Nothing was recorded for the refused request, the first intent is still open, and the task
    // cannot be completed while it is.
    let finished = work.finish(&report, true).unwrap();
    assert_ne!(finished.status, "completed");
    assert_eq!(finished.history.task.unresolved_intents, 1);
    assert_eq!(finished.history.intents.len(), 1);
    assert_eq!(work.reissuable().len(), 1);
}

#[tokio::test]
async fn a_capability_that_only_carries_the_key_gets_no_guarantee_after_a_crash() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    // The service even deduplicates; it did not declare it, so nothing relies on it.
    let service = Service::deduplicating();
    let key = crash_after_charge(&dir, &clock, "task-n", Idempotency::Carried, &service).await;
    assert_eq!(
        begin(
            &store(&dir, &clock),
            "task-n",
            GOAL,
            &config("chip-b", Idempotency::Carried)
        )
        .err(),
        Some(StateError::OutcomeUncertain {
            task_id: "task-n".into(),
            intents: vec![key.clone()],
        })
    );
    // Declaring deduplication now does not make the earlier, undeclared execution deduplicated.
    clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);
    assert_eq!(
        begin(
            &store(&dir, &clock),
            "task-n",
            GOAL,
            &config("chip-b", Idempotency::Deduplicated)
        )
        .err(),
        Some(StateError::OutcomeUncertain {
            task_id: "task-n".into(),
            intents: vec![key],
        })
    );
    assert_eq!(service.charged(), 1);
}

#[tokio::test]
async fn a_capability_that_no_longer_deduplicates_is_not_asked_again() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let service = Service::deduplicating();
    let key = crash_after_charge(&dir, &clock, "task-w", Idempotency::Deduplicated, &service).await;
    for now in [Idempotency::Carried, Idempotency::NotCarried] {
        clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);
        assert_eq!(
            begin(&store(&dir, &clock), "task-w", GOAL, &config("chip-b", now)).err(),
            Some(StateError::OutcomeUncertain {
                task_id: "task-w".into(),
                intents: vec![key.clone()],
            }),
            "{now:?}"
        );
    }
}

#[tokio::test]
async fn a_crash_after_the_effect_stays_uncertain_without_deduplication() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let service = Arc::new(Service::default());
    let key = crash_after_charge(&dir, &clock, "task-u", Idempotency::NotCarried, &service).await;
    assert_eq!(service.keys(), vec![None]);
    assert_eq!(
        begin(
            &store(&dir, &clock),
            "task-u",
            GOAL,
            &config("chip-b", Idempotency::NotCarried)
        )
        .err(),
        Some(StateError::OutcomeUncertain {
            task_id: "task-u".into(),
            intents: vec![key],
        })
    );
    assert_eq!(service.charged(), 1);
}

#[tokio::test]
async fn a_crash_during_the_reissue_leaves_both_intents_for_one_more_reissue() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let service = Service::deduplicating();
    let first =
        crash_after_charge(&dir, &clock, "task-t", Idempotency::Deduplicated, &service).await;
    // Attempt 3 asks again and dies too, after the service answered, before the observation.
    let second = {
        let work = ready(begin(
            &store(&dir, &clock),
            "task-t",
            GOAL,
            &config("chip-b", Idempotency::Deduplicated),
        ));
        let crashed = tokio::time::timeout(
            Duration::from_millis(300),
            run(
                &work,
                Idempotency::Deduplicated,
                Arc::new(Hang(service.clone())),
                vec![charge(5), complete()],
            ),
        )
        .await;
        assert!(crashed.is_err());
        intent_key(work.attempt(), "task-t-exec-1")
    };
    clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);
    let work = ready(begin(
        &store(&dir, &clock),
        "task-t",
        GOAL,
        &config("chip-c", Idempotency::Deduplicated),
    ));
    let mut open: Vec<String> = work
        .reissuable()
        .into_iter()
        .map(|r| r.intent_key)
        .collect();
    open.sort();
    assert_eq!(open, vec![first, second]);
    let report = run(
        &work,
        Idempotency::Deduplicated,
        service.clone(),
        vec![charge(5), complete()],
    )
    .await;
    let finished = work.finish(&report, true).unwrap();
    assert_eq!(finished.status, "completed");
    assert_eq!(finished.history.task.unresolved_intents, 0);
    assert_eq!(service.charged(), 1);
    let key = action_key("task-t", "task-t-exec-1");
    assert_eq!(service.keys(), vec![Some(key.clone()); 3]);
}

/// Passes the request to the service, then never returns: the service has answered, and the
/// process dies before the observation is written.
struct Hang(Arc<Service>);

#[async_trait::async_trait]
impl Executor for Hang {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let _ = self.0.execute(r).await;
        std::future::pending().await
    }
}

#[tokio::test]
async fn an_error_is_no_answer_and_the_intent_stays_open() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let service = Service::deduplicating();
    crash_after_charge(&dir, &clock, "task-e", Idempotency::Deduplicated, &service).await;
    let work = ready(begin(
        &store(&dir, &clock),
        "task-e",
        GOAL,
        &config("chip-b", Idempotency::Deduplicated),
    ));
    let report = run(
        &work,
        Idempotency::Deduplicated,
        Arc::new(Fails),
        vec![charge(5), complete()],
    )
    .await;
    assert_eq!(work.reissuable().len(), 1);
    let finished = work.finish(&report, true).unwrap();
    assert_ne!(finished.status, "completed");
    assert_eq!(finished.history.task.unresolved_intents, 1);
}

struct Fails;

#[async_trait::async_trait]
impl Executor for Fails {
    async fn execute(&self, _r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        Err(ExecutionError::ExecutionFailed(
            "the service is down".into(),
        ))
    }
}

#[tokio::test]
async fn a_completion_with_an_intent_left_open_is_refused() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let service = Service::deduplicating();
    crash_after_charge(&dir, &clock, "task-o", Idempotency::Deduplicated, &service).await;
    let work = ready(begin(
        &store(&dir, &clock),
        "task-o",
        GOAL,
        &config("chip-b", Idempotency::Deduplicated),
    ));
    // The model does not ask again and claims completion: the charge may or may not have happened.
    let report = run(
        &work,
        Idempotency::Deduplicated,
        service.clone(),
        vec![complete()],
    )
    .await;
    let finished = work.finish(&report, true).unwrap();
    assert_eq!(finished.status, "blocked");
    assert!(finished.completion_refused);
    let summary = &finished.history.checkpoints.last().unwrap().summary;
    assert!(summary.contains("still unresolved"), "{summary}");
    // It is still open for the next attempt.
    clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);
    let work = ready(begin(
        &store(&dir, &clock),
        "task-o",
        GOAL,
        &config("chip-c", Idempotency::Deduplicated),
    ));
    assert_eq!(work.reissuable().len(), 1);
    assert_eq!(service.charged(), 1);
}

/// Attempt 3's own records, written through FeltDB's contract exactly as `chip-agent-state` writes
/// them, up to the point where the process dies: the charge was performed again under its key,
/// observed and its outcome recorded, and the intent it performed again is not yet settled.
fn reissued_then_died(dir: &TempDir, clock: &Arc<Clock>, task: &str, key: &str) -> String {
    use chip_agent_state::OBSERVATION_COLLECTION;
    use chip_agent_state::feltdb::agent_state::AgentState;
    use chip_agent_state::feltdb::agent_transition_contract::{EvidenceRef, Operation};
    use chip_agent_state::feltdb::{AtomicMutation, RecordPrecondition};
    use serde_json::json;

    let db = FeltDb::open(dir.path().join("agent-state.log")).unwrap();
    let state = AgentState::with_clock(db.clone(), clock.clone());
    let actor = "chip-b";
    let owner = |op: &str, fence: u64| Operation {
        op: op.into(),
        task_id: Some(task.into()),
        actor: Some(actor.into()),
        fence: Some(fence),
        lease_id: Some("lease-b".into()),
        ..Operation::default()
    };
    let read_task = || {
        let (_, v) = state.read("_feltdb.AgentTask", task).unwrap().unwrap();
        v
    };
    state
        .apply(&Operation {
            op: "acquire".into(),
            task_id: Some(task.into()),
            actor: Some(actor.into()),
            lease_id: Some("lease-b".into()),
            lease_ms: Some(LEASE_MS),
            ..Operation::default()
        })
        .unwrap();
    let t = read_task();
    let fence = t["fence"].as_u64().unwrap();
    state
        .apply(&Operation {
            expected_version: t["checkpoint_version"].as_u64(),
            summary: Some("resumed".into()),
            evidence: Some(vec![]),
            status: Some("running".into()),
            ..owner("checkpoint", fence)
        })
        .unwrap();
    let attempt = t["checkpoint_version"].as_u64().unwrap() + 1;
    let intent = intent_key(attempt, &format!("{task}-exec-1"));
    let request = ExecutionRequest::new(ExecutionId::new(format!("{task}-exec-1")), CHARGE)
        .with_inputs([("amount".to_string(), InputValue::Integer(5))].into());
    let tag = ActionTag {
        capability: CHARGE.into(),
        key: key.into(),
        binding: binding(&request),
        idempotency: Idempotency::Deduplicated,
    };
    state
        .apply(&Operation {
            idempotency_key: Some(intent.clone()),
            action: Some(tag.to_action()),
            ..owner("record_intent", fence)
        })
        .unwrap();
    let id = format!("{task}~{intent}");
    let record_key = format!("{OBSERVATION_COLLECTION}:{id}");
    db.apply_atomic_transaction_guarded(
        "test-reissue-observation",
        None,
        None,
        &[],
        &[RecordPrecondition {
            capability: OBSERVATION_COLLECTION.into(),
            key: record_key.clone(),
            require_absent: true,
            ..Default::default()
        }],
        &[AtomicMutation {
            capability: OBSERVATION_COLLECTION.into(),
            key: record_key,
            value: Some(json!({
                "schema": chip_agent_state::OBSERVATION_SCHEMA, "task_id": task,
                "intent_key": intent, "execution_id": format!("{task}-exec-1"),
                "attempt": attempt, "status": "success", "executed": true,
                "output": "charged 5", "receipt_id": null,
                "request": {"capability": CHARGE, "inputs": {"amount": 5}},
            })),
        }],
        None,
    )
    .unwrap();
    state
        .apply(&Operation {
            idempotency_key: Some(intent.clone()),
            result: Some("succeeded".into()),
            evidence: Some(vec![EvidenceRef {
                kind: "record".into(),
                collection: Some(OBSERVATION_COLLECTION.into()),
                id: Some(id),
                uri: None,
            }]),
            ..owner("record_outcome", fence)
        })
        .unwrap();
    clock.0.fetch_add(LEASE_MS + 1, Ordering::SeqCst);
    intent
}

#[tokio::test]
async fn an_intent_another_attempt_already_performed_again_is_settled_at_the_next_start() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let service = Service::deduplicating();
    let first =
        crash_after_charge(&dir, &clock, "task-s", Idempotency::Deduplicated, &service).await;
    let key = action_key("task-s", "task-s-exec-1");
    let second = reissued_then_died(&dir, &clock, "task-s", &key);

    let store = store(&dir, &clock);
    let work = ready(begin(
        &store,
        "task-s",
        GOAL,
        &config("chip-c", Idempotency::Deduplicated),
    ));
    // Nothing is left to perform again: the first intent is settled from the second's observation,
    // which is restored as the one charge.
    assert!(work.reissuable().is_empty());
    assert_eq!(
        work.restoration(),
        &Restoration::Restored { observations: 1 }
    );
    let history = store.history("task-s").unwrap().unwrap();
    assert_eq!(history.task.unresolved_intents, 0);
    let settled = history
        .intents
        .iter()
        .find(|i| i.value.key == first)
        .unwrap();
    assert_eq!(settled.value.conclusion.as_deref(), Some("executed"));
    assert!(
        settled
            .value
            .evidence
            .iter()
            .any(|c| c.id.as_deref() == Some(format!("task-s~{second}").as_str()))
    );
    let report = run(
        &work,
        Idempotency::Deduplicated,
        service.clone(),
        vec![complete()],
    )
    .await;
    assert_eq!(work.finish(&report, true).unwrap().status, "completed");
    assert_eq!(service.charged(), 1);
}
