//! Capability effect declarations: an interrupted request is performed again only when its
//! capability declares, as an observation, that performing it again is safe.
//!
//! * `Observation`: the validated request recorded with the open intent is performed again through
//!   the durable executor, and what the capability then answers is recorded as a fresh observation.
//!   The earlier response is gone; nothing says it was recovered.
//! * `Consequential`, `Unknown`, or no declaration: the intent stays uncertain
//!   ([`StateError::OutcomeUncertain`]), the refusing attempt gives its lease back, and an operator
//!   settles it, exactly as before.
//!
//! The database is FeltDB's real native agent state. Crashes here are an execution that never
//! returns; the tool-dispatch windows under a real `SIGKILL` of the real binary are in
//! `crates/stack-agent/tests/crash_windows.rs`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chip_agent_state::feltdb::FeltDb;
use chip_agent_state::feltdb::agent_state::{AgentState, AuthorityClock};
use chip_agent_state::feltdb::agent_transition_contract::{EvidenceRef, Operation};
use chip_agent_state::feltdb::{AtomicMutation, RecordPrecondition};
use chip_agent_state::{
    AgentStore, Begun, DurableConfig, DurableWork, OBSERVATION_COLLECTION, OBSERVATION_SCHEMA,
    Restoration, StateError, begin, binding, intent_key, settle,
};
use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityBackend, CapabilityDescriptor,
    CapabilityError, CapabilityId, CapabilityInput, CapabilityProvider, CapabilityRequest,
    DecisionError, Effect, ExecutionError, ExecutionId, ExecutionObserver, ExecutionRequest,
    ExecutionResult, Executor, Idempotency, InputValue, NoLocalPolicy, WorkDecision,
    WorkDecisionBoundary, WorkEvent, WorkGoal, WorkId, WorkReport, WorkSpec,
};
use chip_inference::{FxError, MessageRole, ModelProvider, ModelRequest, ModelResponse, Usage};
use serde_json::{Value, json};
use tempfile::TempDir;

const LEASE_MS: u64 = 60_000;
const GOAL: &str = "read the file, then report what it says";
const READ: &str = "world.read";
const CHARGE: &str = "pay.charge";
const KEYED: &str = "pay.keyed";
const CARRIED: &str = "world.carried";
const MYSTERY: &str = "mystery.act";
const PATH: &str = "src/lib.rs";

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
    next: std::sync::atomic::AtomicUsize,
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

fn read() -> WorkDecision {
    WorkDecision::RequestCapability(
        CapabilityRequest::new(ExecutionId::new("x"), CapabilityId::new(READ).unwrap())
            .with_input("path", InputValue::Text(PATH.into())),
    )
}

fn charge(id: &str, amount: i64) -> WorkDecision {
    WorkDecision::RequestCapability(
        CapabilityRequest::new(ExecutionId::new("x"), CapabilityId::new(id).unwrap())
            .with_input("amount", InputValue::Integer(amount)),
    )
}

fn carried() -> WorkDecision {
    WorkDecision::RequestCapability(
        CapabilityRequest::new(ExecutionId::new("x"), CapabilityId::new(CARRIED).unwrap())
            .with_input("path", InputValue::Text(PATH.into())),
    )
}

fn complete() -> WorkDecision {
    WorkDecision::Complete {
        summary: "done".into(),
    }
}

/// The capabilities, each declaring what its adapter says. `read_effect` is what `world.read`
/// declares at the moment of asking, so a test can change the declaration between attempts.
#[derive(Clone, Copy)]
struct Caps {
    read_effect: Effect,
}

impl Caps {
    fn declared() -> Self {
        Self {
            read_effect: Effect::Observation,
        }
    }
}

fn described(id: &str, input: &str) -> CapabilityDescriptor {
    let mut d = CapabilityDescriptor::new(CapabilityId::new(id).unwrap(), id, "a test capability")
        .without_evidence_reuse();
    d.inputs.push(CapabilityInput {
        name: input.into(),
        description: input.into(),
        required: true,
    });
    d
}

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(vec![
            described(READ, "path").with_effect(self.read_effect),
            described(CHARGE, "amount").with_effect(Effect::Consequential),
            described(KEYED, "amount")
                .with_effect(Effect::Consequential)
                .with_idempotency(Idempotency::Deduplicated),
            described(CARRIED, "path")
                .with_effect(Effect::Observation)
                .with_idempotency(Idempotency::Carried),
            // Declares nothing.
            described(MYSTERY, "amount"),
        ])
    }
    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

/// What the world did, in order: the capability, its typed inputs and the key it was handed.
type Run = (String, BTreeMap<String, InputValue>, Option<String>);

/// A world that records every request it performs (test-only). `crash_after` makes the next call
/// of that capability perform its action and then never return: the response is lost.
#[derive(Default)]
struct World {
    runs: Mutex<Vec<Run>>,
    crash_after: Mutex<Option<String>>,
}

impl World {
    fn runs(&self, capability: &str) -> Vec<Run> {
        self.runs
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.0 == capability)
            .cloned()
            .collect()
    }

    fn count(&self, capability: &str) -> usize {
        self.runs(capability).len()
    }

    fn crash_after(&self, capability: &str) {
        *self.crash_after.lock().unwrap() = Some(capability.into());
    }
}

#[async_trait::async_trait]
impl Executor for World {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let nth = {
            let mut runs = self.runs.lock().unwrap();
            runs.push((
                r.intent.clone(),
                r.inputs.clone(),
                r.idempotency_key.clone(),
            ));
            runs.iter().filter(|x| x.0 == r.intent).count()
        };
        let crash = {
            let mut crash = self.crash_after.lock().unwrap();
            if crash.as_deref() == Some(r.intent.as_str()) {
                crash.take();
                true
            } else {
                false
            }
        };
        if crash {
            std::future::pending::<()>().await;
        }
        // The world changes between requests: every read of it is another answer.
        Ok(ExecutionResult::success(r.id, format!("content v{nth}")))
    }
}

/// What recovery is given: the declarations and the executor, as one backend.
struct Backend {
    caps: Caps,
    world: Arc<World>,
}

#[async_trait::async_trait]
impl CapabilityProvider for Backend {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        self.caps.capabilities().await
    }
    async fn availability(&self, id: &CapabilityId) -> CapabilityAvailability {
        self.caps.availability(id).await
    }
    async fn validate_inputs(
        &self,
        _id: &CapabilityId,
        inputs: &BTreeMap<String, InputValue>,
    ) -> Result<(), CapabilityError> {
        // The adapter's own check, as the product's is: only a declared input is accepted.
        match inputs
            .keys()
            .find(|k| !["path", "amount"].contains(&k.as_str()))
        {
            Some(k) => Err(CapabilityError::InvalidInput(format!(
                "capability does not accept input '{k}'"
            ))),
            None => Ok(()),
        }
    }
}

#[async_trait::async_trait]
impl Executor for Backend {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.world.execute(r).await
    }
}

fn backend(caps: Caps, world: &Arc<World>) -> Arc<dyn CapabilityBackend> {
    Arc::new(Backend {
        caps,
        world: world.clone(),
    })
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

/// What the adapters declare, as the product reads it from the descriptors.
fn declared() -> BTreeMap<String, Effect> {
    [
        (READ, Effect::Observation),
        (CHARGE, Effect::Consequential),
        (KEYED, Effect::Consequential),
        (CARRIED, Effect::Observation),
    ]
    .into_iter()
    .map(|(c, e)| (c.to_string(), e))
    .collect()
}

fn config(actor: &str, effects: BTreeMap<String, Effect>) -> DurableConfig {
    DurableConfig {
        actor: actor.into(),
        lease_ms: LEASE_MS,
        idempotency: [
            (KEYED.to_string(), Idempotency::Deduplicated),
            (CARRIED.to_string(), Idempotency::Carried),
        ]
        .into(),
        effects,
    }
}

fn ready(begun: Result<Begun, StateError>) -> DurableWork {
    match begun {
        Ok(Begun::Ready(work)) => work,
        Ok(Begun::Finished(h)) => panic!("expected a runnable task, found {}", h.task.status),
        Err(e) => panic!("expected a runnable task: {e}"),
    }
}

fn uncertain(task: &str, intents: &[&str]) -> Option<StateError> {
    Some(StateError::OutcomeUncertain {
        task_id: task.into(),
        intents: intents.iter().map(|i| i.to_string()).collect(),
    })
}

async fn run(
    work: &DurableWork,
    model: Arc<Model>,
    world: Arc<World>,
    decisions: Vec<WorkDecision>,
) -> WorkReport {
    Agent::new(model)
        .with_capabilities(Arc::new(Caps::declared()))
        .with_executor(work.executor(world))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &WorkSpec::new(WorkId::new(work.task_id()), WorkGoal::new(GOAL))
                .with_restored(work.restored())
                .with_settled(work.settled_actions()),
            &NoLocalPolicy,
            &Script {
                decisions,
                next: Default::default(),
            },
        )
        .await
}

/// Attempt 1: the work's decisions, with `capability`'s first call performing its action and then
/// never returning. Returns the key of the intent that call left open (the `nth` execution).
async fn crash_in(
    dir: &TempDir,
    clock: &Arc<Clock>,
    task: &str,
    world: &Arc<World>,
    capability: &str,
    decisions: Vec<WorkDecision>,
    nth: usize,
) -> String {
    let store = store(dir, clock);
    let work = ready(begin(&store, task, GOAL, &config("chip-a", declared())));
    world.crash_after(capability);
    let died = tokio::time::timeout(
        Duration::from_millis(300),
        run(&work, Arc::default(), world.clone(), decisions),
    )
    .await;
    assert!(died.is_err(), "the process was meant to die");
    clock.advance(LEASE_MS + 1);
    intent_key(work.attempt(), &format!("{task}-exec-{nth}"))
}

/// A released lease ends 1 ms after it is released.
fn just_after(clock: &Arc<Clock>) {
    clock.advance(2);
}

fn history_intent(store: &AgentStore, task: &str, key: &str) -> chip_agent_state::IntentEntry {
    store
        .history(task)
        .unwrap()
        .unwrap()
        .intents
        .into_iter()
        .find(|i| i.value.key == key)
        .unwrap_or_else(|| panic!("no intent {key}"))
}

/// The observation record Chip wrote for an intent, if it wrote one.
fn observation_record(dir: &TempDir, clock: &Arc<Clock>, task: &str, key: &str) -> Option<Value> {
    let db = FeltDb::open(dir.path().join("agent-state.log")).unwrap();
    AgentState::with_clock(db, clock.clone())
        .read(OBSERVATION_COLLECTION, &format!("{task}~{key}"))
        .unwrap()
        .map(|(_, v)| v)
}

// ---------------------------------------------------------------------------------------------
// An observation is performed again
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_interrupted_observation_is_performed_again_and_the_work_resumes_without_an_operator() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let world = Arc::new(World::default());
    let lost = crash_in(
        &dir,
        &clock,
        "task-r",
        &world,
        READ,
        vec![read(), complete()],
        1,
    )
    .await;
    // The read ran once, and its response is gone.
    assert_eq!(world.count(READ), 1);

    let store = store(&dir, &clock);
    let work = ready(begin(&store, "task-r", GOAL, &config("chip-b", declared())));
    // No refusal and no operator: the open intent is planned to be observed again.
    let plan = work.reobservable();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].intents, [lost.clone()]);
    assert!(work.reissuable().is_empty());
    assert!(work.restored().is_empty(), "nothing has been observed yet");

    let done = work
        .reobserve(backend(Caps::declared(), &world), None)
        .await
        .unwrap();
    // Through the normal executor path, once, with the request the intent recorded.
    assert_eq!(world.count(READ), 2);
    let runs = world.runs(READ);
    assert_eq!(runs[1].0, READ);
    assert_eq!(
        runs[0].1, runs[1].1,
        "the typed inputs are the recorded ones"
    );
    assert_eq!(
        runs[1].1,
        BTreeMap::from([("path".to_string(), InputValue::Text(PATH.into()))])
    );
    assert_eq!(runs[1].2, None, "an observation takes no idempotency key");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].intents, [lost.clone()]);
    assert_eq!(done[0].capability, READ);
    assert_eq!(done[0].status, "success");
    assert!(done[0].execution_id.contains("-reobs-"), "{:?}", done[0]);
    assert_eq!(work.reobserved(), done);
    assert!(work.reobservable().is_empty());

    // The trajectory starts from the fresh observation, and says that it is one.
    let restored = work.restored();
    assert_eq!(restored.len(), 1);
    assert!(restored[0].reobserved);
    assert_eq!(restored[0].result.output, "content v2");

    let model = Arc::new(Model::default());
    let report = run(&work, model.clone(), world.clone(), vec![complete()]).await;
    assert_eq!(world.count(READ), 2, "the work did not read again");
    let events: Vec<_> = report
        .events
        .iter()
        .filter_map(|e| match e {
            WorkEvent::ObservationRestored {
                execution_id,
                reobserved,
                ..
            } => Some((execution_id.to_string(), *reobserved)),
            _ => None,
        })
        .collect();
    assert_eq!(events, [(done[0].execution_id.clone(), true)]);
    let finished = work.finish(&report, true).unwrap();
    assert_eq!(finished.status, "completed");
    assert!(!finished.completion_refused);

    // The records: the old intent is resolved by the new observation, and says nothing of the lost
    // response; the new intent says what it is.
    let history = &finished.history;
    assert_eq!(history.task.unresolved_intents, 0);
    let old = history_intent(&store, "task-r", &lost);
    assert_eq!(old.value.state, "resolved");
    let fresh = &done[0].intent_key;
    assert!(
        old.value
            .evidence
            .iter()
            .any(|c| c.id.as_deref() == Some(format!("task-r~{fresh}").as_str())),
        "{:?}",
        old.value.evidence
    );
    assert!(
        observation_record(&dir, &clock, "task-r", &lost).is_none(),
        "the lost response left no observation, and none was written for it"
    );
    let new = history_intent(&store, "task-r", fresh);
    assert!(
        new.value.action.contains(&format!("reobserves={lost}")),
        "{}",
        new.value.action
    );
    let record = observation_record(&dir, &clock, "task-r", fresh).unwrap();
    assert_eq!(record["output"], "content v2");
    assert_eq!(record["execution_id"], done[0].execution_id.as_str());
    assert_eq!(record["schema"], OBSERVATION_SCHEMA);
    // No operator was involved.
    assert!(store.settlements("task-r").unwrap().is_empty());
}

#[tokio::test]
async fn a_changed_read_result_is_the_fresh_observation_and_never_presented_as_the_lost_one() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let world = Arc::new(World::default());
    let lost = crash_in(
        &dir,
        &clock,
        "task-c",
        &world,
        READ,
        vec![read(), complete()],
        1,
    )
    .await;
    let store = store(&dir, &clock);
    let work = ready(begin(&store, "task-c", GOAL, &config("chip-b", declared())));
    let done = work
        .reobserve(backend(Caps::declared(), &world), None)
        .await
        .unwrap();

    // The first read would have said v1; the world has changed since, and the answer is v2.
    let record = observation_record(&dir, &clock, "task-c", &done[0].intent_key).unwrap();
    assert_eq!(record["output"], "content v2");
    // It is the observation of the new execution, under the new execution's identity and intent,
    // and the lost intent has no observation record of its own.
    assert_ne!(record["intent_key"], lost.as_str());
    assert_ne!(done[0].intent_key, lost);
    assert!(observation_record(&dir, &clock, "task-c", &lost).is_none());
    // The work's trajectory holds the answer now, never an earlier one.
    let report = run(&work, Arc::default(), world.clone(), vec![complete()]).await;
    let outputs: Vec<_> = report
        .observations
        .iter()
        .filter_map(|o| o.output.as_deref())
        .collect();
    assert_eq!(outputs, ["content v2"]);
    work.finish(&report, true).unwrap();
}

#[tokio::test]
async fn the_next_attempt_restores_a_fresh_observation_like_any_recorded_one() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let world = Arc::new(World::default());
    crash_in(
        &dir,
        &clock,
        "task-n",
        &world,
        READ,
        vec![read(), complete()],
        1,
    )
    .await;
    let work = ready(begin(
        &store(&dir, &clock),
        "task-n",
        GOAL,
        &config("chip-b", declared()),
    ));
    work.reobserve(backend(Caps::declared(), &world), None)
        .await
        .unwrap();
    // This attempt dies too, before it finishes.
    clock.advance(LEASE_MS + 1);
    let third = ready(begin(
        &store(&dir, &clock),
        "task-n",
        GOAL,
        &config("chip-c", declared()),
    ));
    // Nothing is open and nothing is performed: the fresh observation is a recorded one now.
    assert!(third.reobservable().is_empty());
    assert_eq!(
        third.restoration(),
        &Restoration::Restored { observations: 1 }
    );
    let restored = third.restored();
    assert_eq!(restored.len(), 1);
    assert!(!restored[0].reobserved, "this attempt performed nothing");
    assert_eq!(restored[0].result.output, "content v2");
    assert_eq!(world.count(READ), 2);
    let report = run(&third, Arc::default(), world.clone(), vec![complete()]).await;
    assert_eq!(third.finish(&report, true).unwrap().status, "completed");
}

#[tokio::test]
async fn an_interrupted_recovery_is_recovered_by_one_more_observation() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let world = Arc::new(World::default());
    let lost = crash_in(
        &dir,
        &clock,
        "task-i",
        &world,
        READ,
        vec![read(), complete()],
        1,
    )
    .await;

    // Attempt 2 dies inside the re-observation: the read ran, its response was lost again.
    let second = ready(begin(
        &store(&dir, &clock),
        "task-i",
        GOAL,
        &config("chip-b", declared()),
    ));
    world.crash_after(READ);
    let died = tokio::time::timeout(
        Duration::from_millis(300),
        second.reobserve(backend(Caps::declared(), &world), None),
    )
    .await;
    assert!(died.is_err());
    assert_eq!(world.count(READ), 2);
    clock.advance(LEASE_MS + 1);

    let store = store(&dir, &clock);
    let third = ready(begin(&store, "task-i", GOAL, &config("chip-c", declared())));
    // Both open intents name one request; one fresh observation settles both.
    let plan = third.reobservable();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].intents.len(), 2);
    assert_eq!(plan[0].intents[0], lost);
    let done = third
        .reobserve(backend(Caps::declared(), &world), None)
        .await
        .unwrap();
    assert_eq!(world.count(READ), 3, "one more, not one for each intent");
    assert_eq!(done[0].intents, plan[0].intents);
    let report = run(&third, Arc::default(), world.clone(), vec![complete()]).await;
    let finished = third.finish(&report, true).unwrap();
    assert_eq!(finished.status, "completed");
    assert_eq!(finished.history.task.unresolved_intents, 0);
    let fresh = &done[0].intent_key;
    for key in &plan[0].intents {
        let intent = history_intent(&store, "task-i", key);
        assert_eq!(intent.value.state, "resolved");
        assert!(
            intent
                .value
                .evidence
                .iter()
                .any(|c| c.id.as_deref() == Some(format!("task-i~{fresh}").as_str())),
            "{key}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Anything else stays uncertain
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_interrupted_consequential_action_is_not_performed_again_and_stays_settleable() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let world = Arc::new(World::default());
    let key = crash_in(
        &dir,
        &clock,
        "task-w",
        &world,
        CHARGE,
        vec![charge(CHARGE, 5), complete()],
        1,
    )
    .await;
    assert_eq!(world.count(CHARGE), 1);
    let store = store(&dir, &clock);
    assert_eq!(
        begin(&store, "task-w", GOAL, &config("chip-b", declared())).err(),
        uncertain("task-w", &[&key])
    );
    assert_eq!(world.count(CHARGE), 1, "nothing was performed again");
    // The refusal gave its lease back: the task is acquirable at once.
    let task = store.history("task-w").unwrap().unwrap().task;
    assert_eq!((task.fence, task.owner.as_str()), (2, "chip-b"));
    assert!(task.lease.unwrap().expires_at <= clock.now_ms() + 1);
    just_after(&clock);
    // The operator settles it, as before, and it is a settlement and never an observation.
    settle(
        &store,
        "task-w",
        &key,
        true,
        "alice",
        "the ledger has the charge",
    )
    .unwrap();
    just_after(&clock);
    let work = ready(begin(&store, "task-w", GOAL, &config("chip-c", declared())));
    assert_eq!(work.settled().len(), 1);
    assert!(
        work.restored().is_empty(),
        "a settlement is not an observation"
    );
    assert!(work.reobservable().is_empty() && work.reobserved().is_empty());
    assert_eq!(world.count(CHARGE), 1);
}

#[tokio::test]
async fn an_interrupted_action_with_no_declaration_or_an_unknown_one_stays_uncertain() {
    // The capability is a read in every case; only what it declares differs. Absent, `unknown` and
    // `consequential` each keep the refusal; none is read as an observation.
    for (name, effects) in [
        ("none", BTreeMap::new()),
        (
            "unknown",
            BTreeMap::from([(READ.to_string(), Effect::Unknown)]),
        ),
        (
            "consequential",
            BTreeMap::from([(READ.to_string(), Effect::Consequential)]),
        ),
        // Declarations of other capabilities say nothing about this one.
        (
            "others only",
            BTreeMap::from([(CARRIED.to_string(), Effect::Observation)]),
        ),
    ] {
        let dir = TempDir::new().unwrap();
        let clock = Arc::new(Clock::default());
        let world = Arc::new(World::default());
        let task = format!("task-u-{}", name.replace(' ', "-"));
        let key = crash_in(
            &dir,
            &clock,
            &task,
            &world,
            READ,
            vec![read(), complete()],
            1,
        )
        .await;
        let store = store(&dir, &clock);
        assert_eq!(
            begin(&store, &task, GOAL, &config("chip-b", effects)).err(),
            uncertain(&task, &[&key]),
            "{name}"
        );
        assert_eq!(world.count(READ), 1, "{name}: nothing was performed again");
    }
    // A capability nothing declares at all, whose action was recorded.
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let world = Arc::new(World::default());
    let key = crash_in(
        &dir,
        &clock,
        "task-m",
        &world,
        MYSTERY,
        vec![charge(MYSTERY, 1), complete()],
        1,
    )
    .await;
    assert_eq!(
        begin(
            &store(&dir, &clock),
            "task-m",
            GOAL,
            &config("chip-b", declared())
        )
        .err(),
        uncertain("task-m", &[&key])
    );
    assert_eq!(world.count(MYSTERY), 1);
}

#[tokio::test]
async fn recovery_re_validates_against_what_the_capability_declares_now() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let world = Arc::new(World::default());
    let key = crash_in(
        &dir,
        &clock,
        "task-d",
        &world,
        READ,
        vec![read(), complete()],
        1,
    )
    .await;
    let store = store(&dir, &clock);

    // The intent was recorded when the capability was an observation; it no longer says so.
    for read_effect in [Effect::Unknown, Effect::Consequential] {
        let work = ready(begin(&store, "task-d", GOAL, &config("chip-b", declared())));
        let refused = work
            .reobserve(backend(Caps { read_effect }, &world), None)
            .await
            .err();
        assert_eq!(refused, uncertain("task-d", &[&key]), "{read_effect:?}");
        assert_eq!(
            world.count(READ),
            1,
            "{read_effect:?}: nothing was performed"
        );
        let reasons: Vec<String> = work.failures().iter().map(|e| e.to_string()).collect();
        assert!(
            reasons.iter().any(|r| r.contains("not an observation")),
            "{reasons:?}"
        );
        // The refusal gave its lease back.
        just_after(&clock);
    }

    // The task is still recoverable once the declaration holds.
    let work = ready(begin(&store, "task-d", GOAL, &config("chip-b", declared())));
    work.reobserve(backend(Caps::declared(), &world), None)
        .await
        .unwrap();
    assert_eq!(world.count(READ), 2);
}

// ---------------------------------------------------------------------------------------------
// The request is the one the intent recorded
// ---------------------------------------------------------------------------------------------

/// An attempt that recorded intents and died, written through FeltDB's contract as
/// `chip-agent-state` writes them. Each step is an intent's `action` text and, when the action was
/// observed, the execution id and output of its observation and outcome. Returns each intent's key.
fn raw_attempt(
    dir: &TempDir,
    clock: &Arc<Clock>,
    task: &str,
    steps: &[(String, Option<(&str, &str)>)],
) -> Vec<String> {
    let db = FeltDb::open(dir.path().join("agent-state.log")).unwrap();
    let state = AgentState::with_clock(db.clone(), clock.clone());
    static RAW: AtomicU64 = AtomicU64::new(0);
    let n = RAW.fetch_add(1, Ordering::SeqCst);
    let actor = format!("chip-raw-{n}");
    let lease = format!("lease-raw-{n}");
    let (actor, lease) = (actor.as_str(), lease.as_str());
    let created = state
        .read("_feltdb.AgentTask", task)
        .unwrap()
        .map(|_| ())
        .is_some();
    state
        .apply(&Operation {
            op: if created { "acquire" } else { "create_task" }.into(),
            task_id: Some(task.into()),
            actor: Some(actor.into()),
            objective: (!created).then(|| GOAL.to_string()),
            lease_id: Some(lease.into()),
            lease_ms: Some(LEASE_MS),
            ..Operation::default()
        })
        .unwrap();
    let owner = |op: &str, fence: u64| Operation {
        op: op.into(),
        task_id: Some(task.into()),
        actor: Some(actor.into()),
        fence: Some(fence),
        lease_id: Some(lease.into()),
        ..Operation::default()
    };
    let t = state.read("_feltdb.AgentTask", task).unwrap().unwrap().1;
    let fence = t["fence"].as_u64().unwrap();
    state
        .apply(&Operation {
            expected_version: t["checkpoint_version"].as_u64(),
            summary: Some("raw attempt".into()),
            evidence: Some(vec![]),
            status: Some("running".into()),
            ..owner("checkpoint", fence)
        })
        .unwrap();
    let attempt = t["checkpoint_version"].as_u64().unwrap() + 1;
    let mut keys = Vec::new();
    for (n, (action, observed)) in steps.iter().enumerate() {
        let execution = format!("{task}-exec-{}", n + 1);
        let key = intent_key(attempt, &execution);
        let action = action.replace("<own>", &key);
        state
            .apply(&Operation {
                idempotency_key: Some(key.clone()),
                action: Some(action.clone()),
                ..owner("record_intent", fence)
            })
            .unwrap();
        if let Some((execution_id, output)) = observed {
            let id = format!("{task}~{key}");
            let record_key = format!("{OBSERVATION_COLLECTION}:{id}");
            let (capability, inputs) = request_of(&action);
            db.apply_atomic_transaction_guarded(
                "test-raw-observation",
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
                        "schema": OBSERVATION_SCHEMA, "task_id": task,
                        "intent_key": key, "execution_id": execution_id,
                        "attempt": attempt, "status": "success", "executed": true,
                        "output": output, "receipt_id": null,
                        "request": {"capability": capability, "inputs": inputs},
                    })),
                }],
                None,
            )
            .unwrap();
            state
                .apply(&Operation {
                    idempotency_key: Some(key.clone()),
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
        }
        keys.push(key);
    }
    clock.advance(LEASE_MS + 1);
    keys
}

/// The capability and inputs an action text records.
fn request_of(action: &str) -> (String, Value) {
    let capability = action.split(' ').next().unwrap().to_string();
    let inputs = action
        .split_once(" inputs=")
        .map_or(json!({}), |(_, j)| serde_json::from_str(j).unwrap());
    (capability, inputs)
}

fn read_request(path: &str) -> ExecutionRequest {
    ExecutionRequest::new(ExecutionId::new("x"), READ)
        .with_inputs([("path".to_string(), InputValue::Text(path.into()))].into())
}

fn read_binding() -> String {
    binding(&read_request(PATH))
}

#[tokio::test]
async fn a_recorded_request_that_cannot_be_reconstructed_exactly_is_refused_not_repaired() {
    let good = read_binding();
    let other = binding(&read_request("src/other.rs"));
    // Each is an open intent of `world.read` whose recorded action does not describe, exactly, the
    // request its binding names.
    for (name, action) in [
        (
            "binding of another request",
            format!(r#"{READ} binding={other} inputs={{"path":"{PATH}"}}"#),
        ),
        (
            "binding that is no digest",
            format!(r#"{READ} binding=b1.deadbeef inputs={{"path":"{PATH}"}}"#),
        ),
        ("inputs not recorded", format!("{READ} binding={good}")),
        ("neither recorded", READ.to_string()),
        (
            "inputs that are not JSON",
            format!("{READ} binding={good} inputs={{path: src}}"),
        ),
        (
            "an input that is not a typed value",
            format!(r#"{READ} binding={good} inputs={{"path":["a"]}}"#),
        ),
        (
            "inputs that are not an object",
            format!(r#"{READ} binding={good} inputs="src/lib.rs""#),
        ),
        (
            "extra inputs under the old binding",
            format!(r#"{READ} binding={good} inputs={{"path":"{PATH}","mode":"x"}}"#),
        ),
    ] {
        let dir = TempDir::new().unwrap();
        let clock = Arc::new(Clock::default());
        let world = Arc::new(World::default());
        let task = "task-v";
        let keys = raw_attempt(&dir, &clock, task, &[(action, None)]);
        let store = store(&dir, &clock);
        assert_eq!(
            begin(&store, task, GOAL, &config("chip-b", declared())).err(),
            uncertain(task, &[&keys[0]]),
            "{name}"
        );
        assert_eq!(world.count(READ), 0, "{name}: nothing was executed");
        // And it is refused with its lease given back.
        just_after(&clock);
        assert_eq!(
            begin(&store, task, GOAL, &config("chip-c", declared())).err(),
            uncertain(task, &[&keys[0]]),
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_recorded_request_the_capability_no_longer_accepts_is_refused_and_nothing_runs() {
    // The record is exact (its inputs hash to its binding), but the request is not valid against
    // the capability as it is declared now: an input it does not take.
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let world = Arc::new(World::default());
    let request = ExecutionRequest::new(ExecutionId::new("x"), READ).with_inputs(
        [
            ("path".to_string(), InputValue::Text(PATH.into())),
            ("mode".to_string(), InputValue::Text("x".into())),
        ]
        .into(),
    );
    let action = format!(
        r#"{READ} binding={} inputs={{"mode":"x","path":"{PATH}"}}"#,
        binding(&request)
    );
    let keys = raw_attempt(&dir, &clock, "task-x", &[(action, None)]);
    let store = store(&dir, &clock);
    let work = ready(begin(&store, "task-x", GOAL, &config("chip-b", declared())));
    assert_eq!(work.reobservable().len(), 1);
    assert_eq!(
        work.reobserve(backend(Caps::declared(), &world), None)
            .await
            .err(),
        uncertain("task-x", &[&keys[0]])
    );
    assert_eq!(world.count(READ), 0);
    assert!(
        work.failures()
            .iter()
            .any(|e| e.to_string().contains("no longer valid")),
        "{:?}",
        work.failures()
    );
    // Nothing was written for the request: it has no new intent, and the old one is still open.
    let history = store.history("task-x").unwrap().unwrap();
    assert_eq!(history.intents.len(), 1);
    assert_ne!(history.intents[0].value.state, "resolved");
}

// ---------------------------------------------------------------------------------------------
// Windows inside the recovery
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_fresh_observation_recorded_before_its_intent_was_settled_settles_it_without_running() {
    // A recovery performed the read, recorded its observation and outcome, and died before it
    // reconciled the old intent. The old intent is settled from that observation at the next start.
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let world = Arc::new(World::default());
    let task = "task-t";
    let good = read_binding();
    let lost = raw_attempt(
        &dir,
        &clock,
        task,
        &[(
            format!(r#"{READ} binding={good} inputs={{"path":"{PATH}"}}"#),
            None,
        )],
    )[0]
    .clone();
    let again = raw_attempt(
        &dir,
        &clock,
        task,
        &[(
            format!(r#"{READ} binding={good} reobserves={lost} inputs={{"path":"{PATH}"}}"#),
            Some((&format!("{task}-exec-1"), "content v9")),
        )],
    )[0]
    .clone();
    let store = store(&dir, &clock);
    let work = ready(begin(&store, task, GOAL, &config("chip-c", declared())));
    assert!(work.reobservable().is_empty());
    assert_eq!(
        work.restoration(),
        &Restoration::Restored { observations: 1 }
    );
    assert_eq!(work.restored()[0].result.output, "content v9");
    let history = store.history(task).unwrap().unwrap();
    assert_eq!(history.task.unresolved_intents, 0);
    let old = history_intent(&store, task, &lost);
    assert_eq!(old.value.state, "resolved");
    assert!(
        old.value
            .evidence
            .iter()
            .any(|c| c.id.as_deref() == Some(format!("{task}~{again}").as_str())),
        "{:?}",
        old.value.evidence
    );
    assert_eq!(world.count(READ), 0);
}

// ---------------------------------------------------------------------------------------------
// P2: the declaration is independent of the idempotency key
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_consequential_capability_that_deduplicates_is_still_not_performed_by_recovery() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let world = Arc::new(World::default());
    crash_in(
        &dir,
        &clock,
        "task-k",
        &world,
        KEYED,
        vec![charge(KEYED, 5), complete()],
        1,
    )
    .await;
    let work = ready(begin(
        &store(&dir, &clock),
        "task-k",
        GOAL,
        &config("chip-b", declared()),
    ));
    // P2's rule is untouched: it may be asked again, under its key, when the work asks for exactly
    // that request. Recovery performs nothing.
    assert_eq!(work.reissuable().len(), 1);
    assert!(work.reobservable().is_empty());
    assert!(
        work.reobserve(backend(Caps::declared(), &world), None)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(world.count(KEYED), 1);
}

#[tokio::test]
async fn an_observation_that_carries_the_key_is_performed_again_without_one() {
    // `world.carried` declares an observation and that the key reaches it. The declaration of the
    // effect is what lets recovery perform it; the key is not part of that, and a re-observation
    // takes none, so it cannot be mistaken for the logical action it observes again.
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let world = Arc::new(World::default());
    let lost = crash_in(
        &dir,
        &clock,
        "task-p",
        &world,
        CARRIED,
        vec![carried(), complete()],
        1,
    )
    .await;
    let original = world.runs(CARRIED);
    assert!(original[0].2.is_some(), "the original carried a key");
    let store = store(&dir, &clock);
    let work = ready(begin(&store, "task-p", GOAL, &config("chip-b", declared())));
    assert_eq!(work.reobservable().len(), 1);
    assert!(work.reissuable().is_empty());
    let done = work
        .reobserve(backend(Caps::declared(), &world), None)
        .await
        .unwrap();
    let runs = world.runs(CARRIED);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].2, None);
    assert_eq!(runs[0].1, runs[1].1);
    let report = run(&work, Arc::default(), world.clone(), vec![complete()]).await;
    assert_eq!(work.finish(&report, true).unwrap().status, "completed");
    assert_eq!(
        history_intent(&store, "task-p", &lost).value.state,
        "resolved"
    );
    assert!(
        history_intent(&store, "task-p", &done[0].intent_key)
            .value
            .action
            .contains("reobserves=")
    );
}

#[tokio::test]
async fn a_declaration_does_not_move_an_intent_to_another_capability() {
    // An open consequential intent beside an open observation: the observation is performed again,
    // the other still refuses the whole resume, and nothing is performed for either.
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let world = Arc::new(World::default());
    let good = read_binding();
    let charged = binding(
        &ExecutionRequest::new(ExecutionId::new("x"), CHARGE)
            .with_inputs([("amount".to_string(), InputValue::Integer(5))].into()),
    );
    let keys = raw_attempt(
        &dir,
        &clock,
        "task-b",
        &[
            (
                format!(r#"{READ} binding={good} inputs={{"path":"{PATH}"}}"#),
                None,
            ),
            (
                format!(r#"{CHARGE} binding={charged} inputs={{"amount":5}}"#),
                None,
            ),
        ],
    );
    let store = store(&dir, &clock);
    assert_eq!(
        begin(&store, "task-b", GOAL, &config("chip-b", declared())).err(),
        uncertain("task-b", &[&keys[1]])
    );
    assert_eq!(world.count(READ) + world.count(CHARGE), 0);
}

// ---------------------------------------------------------------------------------------------
// Cancellation and deadlines apply to a re-observation as to any execution
// ---------------------------------------------------------------------------------------------

/// The work owner's control: it answers `None` for the first `quiet` questions, then `cause`.
struct Control {
    cause: chip_core::StopCause,
    quiet: usize,
    asked: std::sync::atomic::AtomicUsize,
}

impl Control {
    fn after(quiet: usize, cause: chip_core::StopCause) -> Arc<dyn chip_core::WorkControl> {
        Arc::new(Self {
            cause,
            quiet,
            asked: Default::default(),
        })
    }
}

impl chip_core::WorkControl for Control {
    fn stop_requested(&self) -> Option<chip_core::StopCause> {
        let n = self.asked.fetch_add(1, Ordering::SeqCst);
        (n >= self.quiet).then_some(self.cause)
    }
}

#[tokio::test]
async fn a_stop_requested_before_recovery_prevents_it_and_leaves_the_intent_open() {
    for cause in [
        chip_core::StopCause::Cancelled,
        chip_core::StopCause::Deadline,
    ] {
        let dir = TempDir::new().unwrap();
        let clock = Arc::new(Clock::default());
        let world = Arc::new(World::default());
        let task = format!("task-s-{}", cause.name());
        let key = crash_in(
            &dir,
            &clock,
            &task,
            &world,
            READ,
            vec![read(), complete()],
            1,
        )
        .await;
        let store = store(&dir, &clock);
        let work = ready(begin(&store, &task, GOAL, &config("chip-b", declared())));
        let refused = work
            .reobserve(
                backend(Caps::declared(), &world),
                Some(Control::after(0, cause)),
            )
            .await
            .err();
        assert_eq!(refused, uncertain(&task, &[&key]), "{}", cause.name());
        assert_eq!(
            world.count(READ),
            1,
            "{}: nothing was executed",
            cause.name()
        );
        assert!(work.restored().is_empty() && work.reobserved().is_empty());
        let reasons: Vec<String> = work.failures().iter().map(|e| e.to_string()).collect();
        assert!(
            reasons.iter().any(|r| r.contains(cause.name())),
            "{reasons:?}"
        );
        // No intent was recorded for it, and the lease was given back.
        let history = store.history(&task).unwrap().unwrap();
        assert_eq!(history.intents.len(), 1);
        assert_ne!(history.intents[0].value.state, "resolved");
        just_after(&clock);
        // Nothing retried by itself; a later attempt that is not told to stop recovers.
        let work = ready(begin(&store, &task, GOAL, &config("chip-c", declared())));
        work.reobserve(backend(Caps::declared(), &world), None)
            .await
            .unwrap();
        assert_eq!(world.count(READ), 2);
    }
}

#[tokio::test]
async fn a_stop_reached_before_the_executor_begins_runs_nothing() {
    // The first question (before the intent) is answered "go on"; the second, at the executor's
    // door, is a stop. The intent was recorded, nothing ran, and the old intent stays open.
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(Clock::default());
    let world = Arc::new(World::default());
    let key = crash_in(
        &dir,
        &clock,
        "task-g",
        &world,
        READ,
        vec![read(), complete()],
        1,
    )
    .await;
    let store = store(&dir, &clock);
    let work = ready(begin(&store, "task-g", GOAL, &config("chip-b", declared())));
    let refused = work
        .reobserve(
            backend(Caps::declared(), &world),
            Some(Control::after(1, chip_core::StopCause::Cancelled)),
        )
        .await
        .err();
    assert_eq!(refused, uncertain("task-g", &[&key]));
    assert_eq!(world.count(READ), 1, "the capability was never called");
    assert!(work.restored().is_empty() && work.reobserved().is_empty());
    let history = store.history("task-g").unwrap().unwrap();
    let old = history.intents.iter().find(|i| i.value.key == key).unwrap();
    assert_ne!(old.value.state, "resolved");
    // The recovery's own intent is recorded as what it was: it did not run.
    assert_eq!(history.intents.len(), 2);
    just_after(&clock);
    let work = ready(begin(&store, "task-g", GOAL, &config("chip-c", declared())));
    work.reobserve(backend(Caps::declared(), &world), None)
        .await
        .unwrap();
    assert_eq!(world.count(READ), 2);
    let after = store.history("task-g").unwrap().unwrap();
    assert_eq!(after.task.unresolved_intents, 0);
}
