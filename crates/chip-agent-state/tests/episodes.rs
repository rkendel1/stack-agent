//! Episodes through FeltDB's real native agent state: what a finished attempt is recorded as, how
//! it survives reopening the database, and what a later work is (and is not) told of it.
//!
//! Every work here is an ordinary Chip work (`Agent::run_work`) over a durable executor on a
//! file-backed FeltDB database, finished through `DurableWork::finish_verified` with the verdict a
//! host would give. The model is scripted and records every request it is sent, so the tests can
//! check what a recalled episode looks like in a decision context.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_agent_state::feltdb::FeltDb;
use chip_agent_state::{
    AgentStore, Begun, DurableConfig, DurableWork, EPISODE_COLLECTION, EPISODE_SCHEMA,
    EpisodeContext, EpisodeOutcome, EpisodeRecord, EpisodeWrite, Exclusion, Finished, StateError,
    Verification, begin, intent_terms,
};
use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, CapabilityRequest, DecisionError, ExecutionError,
    ExecutionId, ExecutionObserver, ExecutionRequest, ExecutionResult, Executor, InputValue,
    NoLocalPolicy, Observation, ObservationPredicate, StopCause, WorkControl, WorkDecision,
    WorkDecisionBoundary, WorkEvent, WorkGoal, WorkId, WorkOutcome, WorkReport, WorkSpec,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use tempfile::TempDir;

const PROJECT: &str = "path:00000000000000aa";
const OTHER_PROJECT: &str = "path:00000000000000bb";
const GOAL: &str = "repair the sensor calibration so the reading is ready";
const SIMILAR: &str = "repair sensor calibration until the reading is ready";
const UNRELATED: &str = "document the deployment checklist for operators";

/// Records the text of every request; the decision itself comes from the script.
#[derive(Default)]
struct Model {
    requests: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ModelProvider for Model {
    async fn complete(&self, r: ModelRequest) -> Result<ModelResponse, FxError> {
        let text: Vec<String> = r.messages.iter().map(|m| m.content.clone()).collect();
        self.requests.lock().unwrap().push(text.join("\n"));
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

fn request(capability: &str) -> WorkDecision {
    let mut r = CapabilityRequest::new(
        ExecutionId::new("x"),
        CapabilityId::new(capability).unwrap(),
    );
    if capability == "sensor.read" {
        r = r.with_input("path", InputValue::Text("a".into()));
    }
    WorkDecision::RequestCapability(r)
}

fn complete() -> WorkDecision {
    WorkDecision::Complete {
        summary: "the sensor is ready".into(),
    }
}

struct Caps;

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        let mut read = CapabilityDescriptor::new(
            CapabilityId::new("sensor.read")?,
            "Read",
            "reads the sensor",
        )
        .without_evidence_reuse();
        read.inputs.push(CapabilityInput {
            name: "path".into(),
            description: "path".into(),
            required: true,
        });
        let calibrate = CapabilityDescriptor::new(
            CapabilityId::new("sensor.calibrate")?,
            "Calibrate",
            "calibrates the sensor",
        )
        .without_evidence_reuse();
        Ok(vec![read, calibrate])
    }
    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

/// `sensor.calibrate` fixes the sensor (or fails, when `broken`); `sensor.read` says whether it
/// is ready. Counts every execution.
#[derive(Default)]
struct Sensor {
    calibrated: AtomicBool,
    broken: bool,
    executions: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Executor for Sensor {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.executions.lock().unwrap().push(r.intent.clone());
        Ok(match r.intent.as_str() {
            "sensor.calibrate" if self.broken => {
                ExecutionResult::failure(r.id, "calibration failed")
            }
            "sensor.calibrate" => {
                self.calibrated.store(true, Ordering::SeqCst);
                ExecutionResult::success(r.id, "calibrated")
            }
            _ if self.calibrated.load(Ordering::SeqCst) => ExecutionResult::success(r.id, "ready"),
            _ => ExecutionResult::success(r.id, "drifting"),
        })
    }
}

#[derive(Debug)]
struct Ready;

impl ObservationPredicate for Ready {
    fn describe(&self) -> String {
        "a reading that says ready".into()
    }
    fn satisfied_by(&self, o: &Observation) -> bool {
        o.output.as_deref() == Some("ready")
    }
    fn satisfied_by_trajectory(&self, observations: &[Observation]) -> bool {
        observations.last().is_some_and(|o| self.satisfied_by(o))
    }
}

struct Stop;

impl WorkControl for Stop {
    fn stop_requested(&self) -> Option<StopCause> {
        Some(StopCause::Cancelled)
    }
}

struct Rig {
    dir: TempDir,
}

impl Rig {
    fn new() -> Self {
        Self {
            dir: TempDir::new().unwrap(),
        }
    }

    /// A fresh handle on the database file, as a new process would open it.
    fn store(&self) -> AgentStore {
        AgentStore::new(FeltDb::open(self.dir.path().join("agent-state.log")).unwrap())
    }

    fn begin(&self, store: &AgentStore, task: &str, goal: &str) -> DurableWork {
        match begin(store, task, goal, &DurableConfig::default()) {
            Ok(Begun::Ready(w)) => w,
            Ok(Begun::Finished(h)) => panic!("finished: {}", h.task.status),
            Err(e) => panic!("{e}"),
        }
    }
}

fn contracts(version: &str) -> BTreeMap<String, String> {
    ["sensor.read", "sensor.calibrate"]
        .into_iter()
        .map(|c| (c.to_string(), format!("c1.{version}")))
        .collect()
}

fn context(project: &str, goal: &str) -> EpisodeContext {
    EpisodeContext {
        project: project.into(),
        kind: "change".into(),
        intent_terms: intent_terms(goal),
        conditions: [("build_systems", "cargo"), ("tests", "true")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        contracts: contracts("v1"),
        recorded_by: "episodes-test".into(),
    }
}

/// The verdict a host gives: accepted only for a completion whose goal its evidence shows met.
fn verdict(report: &WorkReport, goal: Option<bool>) -> Verification {
    let completed = matches!(report.outcome, WorkOutcome::Completed { .. });
    let accepted = completed && goal == Some(true);
    Verification {
        accepted,
        verified: accepted,
        goal_satisfied: goal,
        grounded: false,
        evidence_invalidated: None,
        exit_status: if accepted { 0 } else { 2 },
    }
}

struct Run {
    work: DurableWork,
    report: WorkReport,
    finished: Finished,
    requests: Vec<String>,
    sensor: Arc<Sensor>,
}

async fn run(
    rig: &Rig,
    store: &AgentStore,
    task: &str,
    goal: &str,
    recall: Option<&EpisodeContext>,
    decisions: Vec<WorkDecision>,
    sensor: Sensor,
    stop: bool,
) -> Run {
    let work = rig.begin(store, task, goal);
    if let Some(context) = recall {
        work.recall(context);
    }
    let mut spec = WorkSpec::new(WorkId::new(task), WorkGoal::new(goal))
        .with_required_observation(Arc::new(Ready))
        .with_recalled(work.recalled());
    spec.limits.max_turns = decisions.len().max(1);
    spec.limits.max_executions = 8;
    if stop {
        spec.control = Some(Arc::new(Stop));
    }
    let sensor = Arc::new(sensor);
    let model = Arc::new(Model::default());
    let report = Agent::new(model.clone())
        .with_capabilities(Arc::new(Caps))
        .with_executor(work.executor(sensor.clone()))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &spec,
            &NoLocalPolicy,
            &Script {
                decisions,
                next: Default::default(),
            },
        )
        .await;
    let goal_level = spec.goal_level(&report.events);
    let finished = work
        .finish_verified(&report, verdict(&report, goal_level))
        .unwrap();
    let requests = model.requests.lock().unwrap().clone();
    Run {
        work,
        report,
        finished,
        requests,
        sensor,
    }
}

fn fixing() -> Vec<WorkDecision> {
    vec![
        request("sensor.calibrate"),
        request("sensor.read"),
        complete(),
    ]
}

/// A work that repairs the sensor and is verified, recorded as an episode of `project`.
async fn verified_success(rig: &Rig, task: &str, project: &str) -> EpisodeRecord {
    let store = rig.store();
    let r = run(
        rig,
        &store,
        task,
        GOAL,
        None,
        fixing(),
        Sensor::default(),
        false,
    )
    .await;
    assert_eq!(r.finished.status, "completed");
    let (episode, written) = r
        .work
        .record_episode(&r.finished, &context(project, GOAL))
        .unwrap();
    assert_eq!(written, EpisodeWrite::Recorded);
    episode
}

fn raw(store: &AgentStore, id: &str) -> Option<serde_json::Value> {
    store
        .database()
        .list_collection(EPISODE_COLLECTION)
        .unwrap()
        .into_iter()
        .find(|row| row.key == format!("{EPISODE_COLLECTION}:{id}"))
        .map(|row| row.value)
}

fn write_raw(store: &AgentStore, id: &str, value: serde_json::Value) {
    use chip_agent_state::feltdb::{AtomicMutation, RecordPrecondition};
    let key = format!("{EPISODE_COLLECTION}:{id}");
    store
        .database()
        .apply_atomic_transaction_guarded(
            &format!("forge-{id}"),
            None,
            None,
            &[],
            &[RecordPrecondition {
                capability: EPISODE_COLLECTION.to_string(),
                key: key.clone(),
                require_absent: true,
                ..Default::default()
            }],
            &[AtomicMutation {
                capability: EPISODE_COLLECTION.to_string(),
                key,
                value: Some(value),
            }],
            None,
        )
        .unwrap();
}

// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_episode_is_versioned_and_round_trips_and_an_unknown_schema_is_refused_not_read() {
    let rig = Rig::new();
    let episode = verified_success(&rig, "t-serial", PROJECT).await;
    let json = serde_json::to_value(&episode).unwrap();
    assert_eq!(json["schema"], EPISODE_SCHEMA);
    assert_eq!(json["outcome"], "verified_success");
    let back: EpisodeRecord = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(back, episode);
    // A field this build does not know is refused, not ignored.
    let mut extra = json.clone();
    extra["lesson"] = "always calibrate first".into();
    assert!(serde_json::from_value::<EpisodeRecord>(extra).is_err());
    // A record under another schema is reported unreadable and never recalled.
    let store = rig.store();
    let mut future = json;
    future["schema"] = "chip.episode.v9".into();
    future["id"] = "t-future~a1".into();
    write_raw(&store, "t-future~a1", future);
    let (episodes, unreadable) = store.episodes().unwrap();
    assert_eq!(episodes.len(), 1);
    assert_eq!(unreadable.len(), 1);
    assert_eq!(unreadable[0].0, "t-future~a1");
    let recall = store
        .recall("t-new", &context(PROJECT, SIMILAR), now(), 3)
        .unwrap();
    assert_eq!(recall.recalled.len(), 1);
    assert!(
        recall
            .excluded
            .iter()
            .any(|(id, why)| id.as_deref() == Some("t-future~a1")
                && matches!(why, Exclusion::Unreadable(_)))
    );
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[tokio::test]
async fn a_verified_success_cites_the_records_feltdb_holds_and_keeps_no_goal_text() {
    let rig = Rig::new();
    let episode = verified_success(&rig, "t-success", PROJECT).await;
    assert_eq!(episode.outcome, EpisodeOutcome::VerifiedSuccess);
    assert_eq!(episode.id, format!("t-success~a{}", episode.attempt));
    assert_eq!(
        episode.result_record,
        format!("chip.WorkResult:{}", episode.id)
    );
    let caps: Vec<&str> = episode
        .procedure
        .iter()
        .map(|s| s.capability.as_str())
        .collect();
    assert_eq!(caps, ["sensor.calibrate", "sensor.read"]);
    assert!(episode.procedure.iter().all(|s| s.status == "success"));
    assert!(
        episode
            .procedure
            .iter()
            .all(|s| s.contract.as_deref() == Some("c1.v1"))
    );
    assert!(episode.failure_evidence.is_empty());
    assert_eq!(episode.executions, 2);
    // Every record it cites is in FeltDB, and its verdict is the result record's.
    let store = rig.store();
    let history = store.history("t-success").unwrap().unwrap();
    assert_eq!(
        Some(&history.results[0].verification.clone().unwrap()),
        episode.verification.as_ref()
    );
    assert_eq!(episode.observation_records.len(), 2);
    for record in &episode.observation_records {
        let id = record.strip_prefix("chip.Observation:").unwrap();
        assert!(
            store
                .database()
                .list_collection("chip.Observation")
                .unwrap()
                .iter()
                .any(|row| row.key == format!("chip.Observation:{id}")),
            "{record}"
        );
    }
    // No goal text, no model text.
    let text = serde_json::to_string(&raw(&store, &episode.id).unwrap()).unwrap();
    for word in [
        "sensor calibration",
        "calibration",
        "repair",
        "the sensor is ready",
    ] {
        assert!(!text.contains(word), "{word} in {text}");
    }
}

#[tokio::test]
async fn a_failed_work_is_a_verified_failure_citing_its_failing_observation() {
    let rig = Rig::new();
    let store = rig.store();
    let broken = Sensor {
        broken: true,
        ..Sensor::default()
    };
    let decisions = vec![
        request("sensor.calibrate"),
        request("sensor.read"),
        WorkDecision::Block {
            reason: "calibration does not take".into(),
        },
    ];
    let r = run(&rig, &store, "t-fail", GOAL, None, decisions, broken, false).await;
    assert_eq!(r.finished.status, "blocked");
    let (episode, _) = r
        .work
        .record_episode(&r.finished, &context(PROJECT, GOAL))
        .unwrap();
    assert_eq!(
        episode.outcome,
        EpisodeOutcome::VerifiedFailure,
        "{}",
        episode.outcome_basis
    );
    assert_eq!(episode.procedure[0].status, "failure");
    assert_eq!(episode.failure_evidence.len(), 1);
    let failing = episode.failure_evidence[0]
        .strip_prefix("chip.Observation:")
        .unwrap();
    let history = store.history("t-fail").unwrap().unwrap();
    assert_eq!(
        history.results[0]
            .verification
            .as_ref()
            .unwrap()
            .goal_satisfied,
        Some(false)
    );
    assert!(
        failing.starts_with(&format!("t-fail~a{}.", episode.attempt)),
        "{failing}"
    );
}

#[tokio::test]
async fn interrupted_and_uncertain_works_are_neither_success_nor_failure() {
    let rig = Rig::new();
    let store = rig.store();
    // Stopped before it did anything: interrupted.
    let r = run(
        &rig,
        &store,
        "t-stopped",
        GOAL,
        None,
        fixing(),
        Sensor::default(),
        true,
    )
    .await;
    assert!(matches!(r.report.outcome, WorkOutcome::Cancelled { .. }));
    let (e, _) = r
        .work
        .record_episode(&r.finished, &context(PROJECT, GOAL))
        .unwrap();
    assert_eq!(e.outcome, EpisodeOutcome::Interrupted);

    // Its tool calls succeeded, but the model blocked before the goal was evaluated: unknown, not
    // success (a successful execution is not a satisfied goal) and not failure.
    let decisions = vec![
        request("sensor.calibrate"),
        WorkDecision::Block {
            reason: "unsure".into(),
        },
    ];
    let r = run(
        &rig,
        &store,
        "t-partial",
        GOAL,
        None,
        decisions,
        Sensor::default(),
        false,
    )
    .await;
    assert!(
        r.report
            .events
            .iter()
            .any(|e| matches!(e, WorkEvent::Execution(_))),
        "the calibration executed"
    );
    let (e, _) = r
        .work
        .record_episode(&r.finished, &context(PROJECT, GOAL))
        .unwrap();
    assert_eq!(e.outcome, EpisodeOutcome::Unknown, "{}", e.outcome_basis);

    // The model said it was done with nothing observed: Chip refuses the completion, and the
    // episode is not a success.
    let r = run(
        &rig,
        &store,
        "t-claimed",
        GOAL,
        None,
        vec![complete()],
        Sensor::default(),
        false,
    )
    .await;
    let (e, _) = r
        .work
        .record_episode(&r.finished, &context(PROJECT, GOAL))
        .unwrap();
    assert_ne!(e.outcome, EpisodeOutcome::VerifiedSuccess);

    // A process that ended mid-work left no result: no episode is invented for it.
    let gone = rig.begin(&store, "t-crashed", GOAL);
    drop(gone);
    let reopened = rig.store();
    let history = reopened.history("t-crashed").unwrap().unwrap();
    assert_eq!(history.status(), "running");
    let attempt = history.task.checkpoint_version;
    let refused = reopened.episode_of(&history, attempt, &context(PROJECT, GOAL), &[]);
    assert!(
        matches!(refused, Err(StateError::InvalidInput(ref why)) if why.contains("no recorded result")),
        "{refused:?}"
    );
    assert!(
        reopened
            .episode(&format!("t-crashed~a{attempt}"))
            .unwrap()
            .is_none()
    );
    // None of these is ever recalled.
    let recall = reopened
        .recall("t-new", &context(PROJECT, SIMILAR), now(), 3)
        .unwrap();
    assert!(recall.recalled.is_empty(), "{recall:?}");
}

#[tokio::test]
async fn recording_the_same_episode_again_writes_nothing_and_a_different_one_is_refused() {
    let rig = Rig::new();
    let store = rig.store();
    let r = run(
        &rig,
        &store,
        "t-dup",
        GOAL,
        None,
        fixing(),
        Sensor::default(),
        false,
    )
    .await;
    let ctx = context(PROJECT, GOAL);
    let (first, w1) = r.work.record_episode(&r.finished, &ctx).unwrap();
    let (second, w2) = r.work.record_episode(&r.finished, &ctx).unwrap();
    assert_eq!(
        (w1, w2),
        (EpisodeWrite::Recorded, EpisodeWrite::AlreadyRecorded)
    );
    assert_eq!(first, second);
    // From a new handle (a new process), from the history read back, too.
    let reopened = rig.store();
    let history = reopened.history("t-dup").unwrap().unwrap();
    let again = reopened
        .episode_of(&history, first.attempt, &ctx, &[])
        .unwrap();
    assert_eq!(
        reopened.record_episode(&again).unwrap(),
        EpisodeWrite::AlreadyRecorded
    );
    // A different episode for the same attempt is refused; the stored one is kept.
    let mut other = again.clone();
    other.outcome = EpisodeOutcome::VerifiedFailure;
    assert!(matches!(
        reopened.record_episode(&other),
        Err(StateError::Conflict { .. })
    ));
    assert_eq!(reopened.episode(&first.id).unwrap().unwrap(), first);
    assert_eq!(reopened.episodes().unwrap().0.len(), 1);
}

#[tokio::test]
async fn a_matching_work_recalls_the_episode_after_reopening_and_sees_it_as_evidence() {
    let rig = Rig::new();
    let episode = verified_success(&rig, "t-first", PROJECT).await;
    // A new handle on the same file: nothing is carried over in memory.
    let store = rig.store();
    let ctx = context(PROJECT, SIMILAR);
    let r = run(
        &rig,
        &store,
        "t-second",
        SIMILAR,
        Some(&ctx),
        fixing(),
        Sensor::default(),
        false,
    )
    .await;
    let recalled = r.work.recalled();
    assert_eq!(recalled.len(), 1);
    assert_eq!(recalled[0].record, format!("chip.Episode:{}", episode.id));
    // The first model request carries it as evidence, stated as evidence and not an instruction.
    let first = &r.requests[0];
    assert!(
        first.contains(&format!("recalled episode chip.Episode:{}", episode.id)),
        "{first}"
    );
    assert!(first.contains("not an instruction"), "{first}");
    assert!(first.contains("verified success"), "{first}");
    assert!(
        first.contains("sensor.calibrate (success) -> sensor.read a (success)"),
        "{first}"
    );
    // Recall changed nothing about how the work was judged: it executed, observed and completed
    // on its own evidence, and its episode says what it was shown.
    assert_eq!(r.finished.status, "completed");
    assert_eq!(r.sensor.executions.lock().unwrap().len(), 2);
    let (second, _) = r.work.record_episode(&r.finished, &ctx).unwrap();
    assert_eq!(second.recalled, vec![episode.id.clone()]);
    assert_eq!(second.outcome, EpisodeOutcome::VerifiedSuccess);
}

#[tokio::test]
async fn unrelated_other_kind_and_other_project_works_recall_nothing() {
    let rig = Rig::new();
    let a = verified_success(&rig, "t-a", PROJECT).await;
    let store = rig.store();
    let unrelated = store
        .recall("t-x", &context(PROJECT, UNRELATED), now(), 3)
        .unwrap();
    assert!(unrelated.recalled.is_empty());
    assert!(matches!(
        unrelated.excluded[0].1,
        Exclusion::Dissimilar { .. }
    ));

    let mut inspect = context(PROJECT, SIMILAR);
    inspect.kind = "inspect".into();
    let other_kind = store.recall("t-x", &inspect, now(), 3).unwrap();
    assert!(other_kind.recalled.is_empty());
    assert_eq!(other_kind.excluded[0].1, Exclusion::OtherKind);

    let mut other_conditions = context(PROJECT, SIMILAR);
    other_conditions
        .conditions
        .insert("build_systems".into(), "npm".into());
    let differs = store.recall("t-x", &other_conditions, now(), 3).unwrap();
    assert!(differs.recalled.is_empty());
    assert!(matches!(
        differs.excluded[0].1,
        Exclusion::PreconditionsDiffer { .. }
    ));

    // Another project sharing the database: nothing, and the episode is not even named.
    let foreign = store
        .recall("t-x", &context(OTHER_PROJECT, SIMILAR), now(), 3)
        .unwrap();
    assert!(foreign.recalled.is_empty());
    assert_eq!(foreign.excluded, vec![(None, Exclusion::OtherScope)]);
    // A project with no identity recalls nothing.
    let anonymous = store
        .recall("t-x", &context("", SIMILAR), now(), 3)
        .unwrap();
    assert!(anonymous.recalled.is_empty());
    // An agent-level record is never recalled into a project.
    let mut agent = a.clone();
    agent.id = "t-agent~a1".into();
    agent.task_id = "t-agent".into();
    agent.scope.level = "agent".into();
    write_raw(&store, "t-agent~a1", serde_json::to_value(&agent).unwrap());
    let recall = store
        .recall("t-x", &context(PROJECT, SIMILAR), now(), 3)
        .unwrap();
    assert_eq!(recall.recalled.len(), 1);
    assert_eq!(recall.recalled[0].episode.id, a.id);
}

#[tokio::test]
async fn an_unverified_or_forged_success_is_never_recalled() {
    let rig = Rig::new();
    let genuine = verified_success(&rig, "t-real", PROJECT).await;
    let store = rig.store();
    // An unknown outcome is not recalled.
    let r = run(
        &rig,
        &store,
        "t-unknown",
        GOAL,
        None,
        vec![request("sensor.calibrate")],
        Sensor::default(),
        false,
    )
    .await;
    let (unknown, _) = r
        .work
        .record_episode(&r.finished, &context(PROJECT, GOAL))
        .unwrap();
    assert_eq!(unknown.outcome, EpisodeOutcome::Unknown);
    // A record that claims success without the verdict for it, and one whose task FeltDB does not
    // hold completed, are not recalled either: an outcome is never promoted.
    let mut no_verdict = genuine.clone();
    no_verdict.id = "t-forged~a1".into();
    no_verdict.task_id = "t-forged".into();
    no_verdict.verification = None;
    write_raw(
        &store,
        "t-forged~a1",
        serde_json::to_value(&no_verdict).unwrap(),
    );
    let mut promoted = unknown.clone();
    promoted.id = "t-unknown~a9".into();
    promoted.outcome = EpisodeOutcome::VerifiedSuccess;
    promoted.verification = Some(Verification {
        accepted: true,
        verified: true,
        goal_satisfied: Some(true),
        grounded: false,
        evidence_invalidated: None,
        exit_status: 0,
    });
    write_raw(
        &store,
        "t-unknown~a9",
        serde_json::to_value(&promoted).unwrap(),
    );
    let recall = store
        .recall("t-x", &context(PROJECT, SIMILAR), now(), 3)
        .unwrap();
    let ids: Vec<&str> = recall
        .recalled
        .iter()
        .map(|r| r.episode.id.as_str())
        .collect();
    assert_eq!(ids, [genuine.id.as_str()], "{recall:?}");
    let why = |id: &str| {
        recall
            .excluded
            .iter()
            .find(|(i, _)| i.as_deref() == Some(id))
            .map(|(_, w)| w.clone())
            .unwrap()
    };
    assert!(matches!(why(&unknown.id), Exclusion::Unverified { .. }));
    assert!(matches!(why("t-forged~a1"), Exclusion::Unverified { .. }));
    assert!(
        matches!(why("t-unknown~a9"), Exclusion::Unbacked(_)),
        "{:?}",
        why("t-unknown~a9")
    );

    // A work that recalls a success and then fails is recorded as the failure it was.
    let broken = Sensor {
        broken: true,
        ..Sensor::default()
    };
    let decisions = vec![
        request("sensor.calibrate"),
        request("sensor.read"),
        WorkDecision::Block {
            reason: "no".into(),
        },
    ];
    let ctx = context(PROJECT, SIMILAR);
    let r = run(
        &rig,
        &store,
        "t-after",
        SIMILAR,
        Some(&ctx),
        decisions,
        broken,
        false,
    )
    .await;
    assert_eq!(r.work.recalled().len(), 1);
    let (after, _) = r.work.record_episode(&r.finished, &ctx).unwrap();
    assert_eq!(after.outcome, EpisodeOutcome::VerifiedFailure);
    assert_eq!(after.recalled, vec![genuine.id.clone()]);
}

#[tokio::test]
async fn a_procedure_whose_capability_changed_or_went_away_is_stale_and_not_recalled() {
    let rig = Rig::new();
    verified_success(&rig, "t-old", PROJECT).await;
    let store = rig.store();
    let mut changed = context(PROJECT, SIMILAR);
    changed.contracts = contracts("v2");
    let recall = store.recall("t-x", &changed, now(), 3).unwrap();
    assert!(recall.recalled.is_empty());
    assert!(
        matches!(recall.excluded[0].1, Exclusion::StaleProcedure { ref capability } if capability == "sensor.calibrate")
    );
    let mut gone = context(PROJECT, SIMILAR);
    gone.contracts.remove("sensor.read");
    let recall = store.recall("t-x", &gone, now(), 3).unwrap();
    assert!(
        matches!(recall.excluded[0].1, Exclusion::StaleProcedure { ref capability } if capability == "sensor.read")
    );
    // Expired: older than the support window.
    let recall = store
        .recall(
            "t-x",
            &context(PROJECT, SIMILAR),
            now() + chip_agent_state::EPISODE_VALID_MS + 1,
            3,
        )
        .unwrap();
    assert_eq!(recall.excluded[0].1, Exclusion::Expired);
}

#[tokio::test]
async fn recall_neither_authorizes_nor_completes_anything() {
    let rig = Rig::new();
    verified_success(&rig, "t-prior", PROJECT).await;
    let store = rig.store();
    let ctx = context(PROJECT, SIMILAR);
    // The model asks for a capability the work does not declare, then claims completion with
    // nothing observed. Recall says calibration worked before; neither request is honoured.
    let decisions = vec![request("sensor.replace"), complete(), complete()];
    let r = run(
        &rig,
        &store,
        "t-bypass",
        SIMILAR,
        Some(&ctx),
        decisions,
        Sensor::default(),
        false,
    )
    .await;
    assert_eq!(r.work.recalled().len(), 1);
    assert!(
        r.sensor.executions.lock().unwrap().is_empty(),
        "nothing executed"
    );
    assert!(
        r.report
            .events
            .iter()
            .any(|e| matches!(e, WorkEvent::DecisionRejected { .. })),
        "{:?}",
        r.report.events
    );
    assert!(!matches!(r.report.outcome, WorkOutcome::Completed { .. }));
    assert_ne!(r.finished.status, "completed");
    let (e, _) = r.work.record_episode(&r.finished, &ctx).unwrap();
    assert_ne!(e.outcome, EpisodeOutcome::VerifiedSuccess);
}

#[tokio::test]
async fn an_unreadable_episode_store_degrades_to_no_recall_and_the_work_runs_unchanged() {
    let rig = Rig::new();
    let store = rig.store();
    write_raw(
        &store,
        "t-garbage~a1",
        serde_json::json!({"schema": EPISODE_SCHEMA, "id": 7}),
    );
    let ctx = context(PROJECT, SIMILAR);
    let r = run(
        &rig,
        &store,
        "t-runs",
        SIMILAR,
        Some(&ctx),
        fixing(),
        Sensor::default(),
        false,
    )
    .await;
    assert!(r.work.recalled().is_empty());
    assert!(r.requests.iter().all(|q| !q.contains("recalled episode")));
    assert_eq!(r.finished.status, "completed");
    assert!(r.work.failures().is_empty());
    // The task's durable state is exactly what a work without recall leaves.
    let history = store.history("t-runs").unwrap().unwrap();
    assert_eq!(history.results.len(), 1);
    assert_eq!(history.task.unresolved_intents, 0);
}

#[tokio::test]
async fn an_episode_whose_cited_result_is_missing_or_another_attempts_is_never_recalled() {
    let rig = Rig::new();
    let genuine = verified_success(&rig, "t-cites", PROJECT).await;
    let store = rig.store();
    // A copy citing a result record that does not exist.
    let mut missing = genuine.clone();
    missing.id = "t-ghost~a2".into();
    missing.task_id = "t-ghost".into();
    missing.result_record = "chip.WorkResult:t-ghost~a2".into();
    write_raw(&store, &missing.id, serde_json::to_value(&missing).unwrap());
    // One citing another attempt's result.
    let mut borrowed = genuine.clone();
    borrowed.id = "t-borrow~a2".into();
    borrowed.task_id = "t-borrow".into();
    write_raw(
        &store,
        &borrowed.id,
        serde_json::to_value(&borrowed).unwrap(),
    );
    // One citing something that is not a work result at all.
    let mut odd = genuine.clone();
    odd.id = "t-odd~a2".into();
    odd.result_record = "chip.Observation:t-cites~a2.x".into();
    write_raw(&store, &odd.id, serde_json::to_value(&odd).unwrap());

    let recall = store
        .recall("t-x", &context(PROJECT, SIMILAR), now(), 3)
        .unwrap();
    let ids: Vec<&str> = recall
        .recalled
        .iter()
        .map(|r| r.episode.id.as_str())
        .collect();
    assert_eq!(ids, [genuine.id.as_str()], "{recall:?}");
    for id in ["t-ghost~a2", "t-borrow~a2", "t-odd~a2"] {
        let why = recall
            .excluded
            .iter()
            .find(|(i, _)| i.as_deref() == Some(id))
            .map(|(_, w)| w.clone());
        assert!(matches!(why, Some(Exclusion::Unbacked(_))), "{id}: {why:?}");
        assert!(store.backing(&store.episode(id).unwrap().unwrap()).is_err());
    }
    assert!(store.backing(&genuine).is_ok());
}

#[tokio::test]
async fn a_failed_episode_write_changes_nothing_and_fabricates_no_success() {
    let rig = Rig::new();
    let store = rig.store();
    let broken = Sensor {
        broken: true,
        ..Sensor::default()
    };
    let decisions = vec![
        request("sensor.calibrate"),
        request("sensor.read"),
        WorkDecision::Block {
            reason: "no".into(),
        },
    ];
    let r = run(
        &rig,
        &store,
        "t-failwrite",
        GOAL,
        None,
        decisions,
        broken,
        false,
    )
    .await;
    assert_eq!(r.finished.status, "blocked");
    let ctx = context(PROJECT, GOAL);
    // Something already holds the episode's id, claiming success: the write is refused.
    let mut forged = store
        .episode_of(&r.finished.history, r.work.attempt(), &ctx, &[])
        .unwrap();
    assert_eq!(forged.outcome, EpisodeOutcome::VerifiedFailure);
    forged.outcome = EpisodeOutcome::VerifiedSuccess;
    forged.verification = Some(Verification {
        accepted: true,
        verified: true,
        goal_satisfied: Some(true),
        grounded: false,
        evidence_invalidated: None,
        exit_status: 0,
    });
    write_raw(&store, &forged.id, serde_json::to_value(&forged).unwrap());
    let refused = r.work.record_episode(&r.finished, &ctx);
    assert!(
        matches!(refused, Err(StateError::Conflict { .. })),
        "{refused:?}"
    );
    // The task is exactly as finish left it: blocked, its verdict unverified.
    let history = store.history("t-failwrite").unwrap().unwrap();
    assert_eq!(history.task.status, "blocked");
    assert_eq!(history, r.finished.history);
    assert!(!history.results[0].verification.as_ref().unwrap().accepted);
    // And the record claiming success is never recalled: FeltDB does not back it.
    let recall = store
        .recall("t-x", &context(PROJECT, SIMILAR), now(), 3)
        .unwrap();
    assert!(recall.recalled.is_empty(), "{recall:?}");
    assert!(matches!(recall.excluded[0].1, Exclusion::Unbacked(_)));
}

#[tokio::test]
async fn concurrent_recording_of_one_attempt_writes_one_episode() {
    let rig = Rig::new();
    let store = rig.store();
    let r = run(
        &rig,
        &store,
        "t-race",
        GOAL,
        None,
        fixing(),
        Sensor::default(),
        false,
    )
    .await;
    let episode = store
        .episode_of(
            &r.finished.history,
            r.work.attempt(),
            &context(PROJECT, GOAL),
            &[],
        )
        .unwrap();
    let writes: Vec<EpisodeWrite> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let (store, episode) = (store.clone(), episode.clone());
                scope.spawn(move || store.record_episode(&episode).unwrap())
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(
        writes
            .iter()
            .filter(|w| **w == EpisodeWrite::Recorded)
            .count(),
        1,
        "{writes:?}"
    );
    assert_eq!(store.episodes().unwrap().0, vec![episode]);
}

// ---- eligibility, conflicts and feedback ----------------------------------------------------

use chip_agent_state::{
    Asking, Check, Dimension, FeedbackClass, FeedbackRecord, ProcedureStep, assess, feedback_of,
    select,
};

/// `base` with this procedure (capability, target, status), one observation record per step.
fn with_procedure(
    base: &EpisodeRecord,
    id: &str,
    outcome: EpisodeOutcome,
    steps: &[(&str, Option<&str>, &str)],
) -> EpisodeRecord {
    let mut e = base.clone();
    e.id = id.into();
    e.task_id = id.split('~').next().unwrap().into();
    e.outcome = outcome;
    e.procedure = steps
        .iter()
        .map(|(c, t, s)| ProcedureStep {
            capability: c.to_string(),
            target: t.map(str::to_string),
            contract: Some("c1.v1".into()),
            status: s.to_string(),
        })
        .collect();
    e.observation_records = (0..steps.len())
        .map(|i| format!("chip.Observation:{id}.o{i}"))
        .collect();
    e
}

const W_LIB: (&str, Option<&str>, &str) = ("project.write", Some("src/lib.rs"), "success");
const W_TWO: (&str, Option<&str>, &str) = ("project.write", Some("src/two.rs"), "success");
const PASS: (&str, Option<&str>, &str) = ("pax.test", None, "success");
const FAIL: (&str, Option<&str>, &str) = ("pax.test", None, "failure");

#[tokio::test]
async fn feedback_classifies_only_what_the_host_observed_and_otherwise_says_inconclusive() {
    let rig = Rig::new();
    let base = verified_success(&rig, "t-base", PROJECT).await;
    use EpisodeOutcome::*;
    let recalled = with_procedure(&base, "t-old~a2", VerifiedSuccess, &[W_LIB, PASS]);
    let case = |outcome, steps: &[(&str, Option<&str>, &str)]| {
        feedback_of(
            &with_procedure(&base, "t-new~a2", outcome, steps),
            &recalled,
            0,
        )
    };
    // The recalled write first, the next test passed, verified: helpful (the procedure held).
    let f = case(VerifiedSuccess, &[W_LIB, PASS]);
    assert_eq!(f.classification, FeedbackClass::Helpful, "{}", f.basis);
    assert_eq!(f.attempted, Some(true));
    assert_eq!(f.verification_after.as_deref(), Some("success"));
    assert_eq!(
        f.attempt_evidence.as_deref(),
        Some("chip.Observation:t-new~a2.o0")
    );
    assert!(f.basis.contains("not established"), "{}", f.basis);
    // The recalled write first and the next test failed: misleading here, whatever came after.
    let f = case(VerifiedSuccess, &[W_LIB, FAIL, W_TWO, PASS]);
    assert_eq!(f.classification, FeedbackClass::Misleading, "{}", f.basis);
    assert_eq!(f.corrective_writes, 1);
    let f = case(VerifiedFailure, &[W_LIB, FAIL]);
    assert_eq!(f.classification, FeedbackClass::Misleading);
    // Verified by another first write: not helpful.
    let f = case(VerifiedSuccess, &[W_TWO, PASS]);
    assert_eq!(f.classification, FeedbackClass::NotHelpful, "{}", f.basis);
    assert_eq!(f.attempted, Some(false));
    // A failed work that never tried the recalled write: no evidence about the episode.
    let f = case(VerifiedFailure, &[W_TWO, FAIL]);
    assert_eq!(f.classification, FeedbackClass::Inconclusive, "{}", f.basis);
    // Ambiguous: no write at all; the write and no test after it; a pass in an unverified work;
    // a recalled episode that names no write.
    for f in [
        case(Unknown, &[("project.read", Some("src/lib.rs"), "success")]),
        case(Unknown, &[W_LIB]),
        case(Interrupted, &[W_LIB, PASS]),
        feedback_of(
            &with_procedure(&base, "t-new~a2", VerifiedSuccess, &[W_LIB, PASS]),
            &with_procedure(&base, "t-old~a2", VerifiedFailure, &[W_LIB, FAIL]),
            0,
        ),
    ] {
        assert_eq!(f.classification, FeedbackClass::Inconclusive, "{}", f.basis);
    }
    assert_eq!(
        case(Unknown, &[("project.read", None, "success")]).attempted,
        None
    );
}

#[tokio::test]
async fn feedback_is_written_once_never_changes_the_episode_and_a_failed_write_changes_nothing() {
    let rig = Rig::new();
    let prior = verified_success(&rig, "t-prior", PROJECT).await;
    let store = rig.store();
    let before = raw(&store, &prior.id).unwrap();
    let ctx = context(PROJECT, SIMILAR);
    let r = run(
        &rig,
        &store,
        "t-fb",
        SIMILAR,
        Some(&ctx),
        fixing(),
        Sensor::default(),
        false,
    )
    .await;
    assert_eq!(r.work.recalled().len(), 1);
    let (own, _) = r.work.record_episode(&r.finished, &ctx).unwrap();
    let first: Vec<FeedbackRecord> = r
        .work
        .record_feedback(&own)
        .into_iter()
        .map(|f| {
            let (f, written) = f.unwrap();
            assert_eq!(written, EpisodeWrite::Recorded);
            f
        })
        .collect();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].episode, prior.id);
    assert_eq!(first[0].attempt_episode, own.id);
    // The sensor procedure has no project write: whether it was used cannot be observed.
    assert_eq!(first[0].classification, FeedbackClass::Inconclusive);
    // Again, from this handle and from a new one: nothing new is written.
    for f in r.work.record_feedback(&own) {
        assert_eq!(f.unwrap().1, EpisodeWrite::AlreadyRecorded);
    }
    let reopened = rig.store();
    assert_eq!(
        reopened.record_feedback(&first[0]).unwrap(),
        EpisodeWrite::AlreadyRecorded
    );
    assert_eq!(reopened.feedback().unwrap().0, first);
    // The recalled episode is byte for byte what it was.
    assert_eq!(raw(&reopened, &prior.id).unwrap(), before);
    // A different record under the same id is refused, the stored one kept, and the task's
    // outcome is exactly as finish left it.
    let mut other = first[0].clone();
    other.classification = FeedbackClass::Helpful;
    assert!(matches!(
        reopened.record_feedback(&other),
        Err(StateError::Conflict { .. })
    ));
    assert_eq!(reopened.feedback().unwrap().0, first);
    let history = reopened.history("t-fb").unwrap().unwrap();
    assert_eq!(history, r.finished.history);
    assert_eq!(history.task.status, "completed");
}

#[tokio::test]
async fn a_failed_feedback_write_leaves_the_task_and_its_episode_as_they_were() {
    let rig = Rig::new();
    let prior = verified_success(&rig, "t-p2", PROJECT).await;
    let store = rig.store();
    let ctx = context(PROJECT, SIMILAR);
    let broken = Sensor {
        broken: true,
        ..Sensor::default()
    };
    let decisions = vec![
        request("sensor.calibrate"),
        request("sensor.read"),
        WorkDecision::Block {
            reason: "no".into(),
        },
    ];
    let r = run(
        &rig,
        &store,
        "t-fw",
        SIMILAR,
        Some(&ctx),
        decisions,
        broken,
        false,
    )
    .await;
    let (own, _) = r.work.record_episode(&r.finished, &ctx).unwrap();
    // Something else already holds the feedback's id.
    let mut squatter = feedback_of(&own, &prior, 0);
    squatter.basis = "not what this attempt established".into();
    {
        use chip_agent_state::feltdb::{AtomicMutation, RecordPrecondition};
        let key = format!("{}:{}", chip_agent_state::FEEDBACK_COLLECTION, squatter.id);
        store
            .database()
            .apply_atomic_transaction_guarded(
                "squat",
                None,
                None,
                &[],
                &[RecordPrecondition {
                    capability: chip_agent_state::FEEDBACK_COLLECTION.into(),
                    key: key.clone(),
                    require_absent: true,
                    ..Default::default()
                }],
                &[AtomicMutation {
                    capability: chip_agent_state::FEEDBACK_COLLECTION.into(),
                    key,
                    value: Some(serde_json::to_value(&squatter).unwrap()),
                }],
                None,
            )
            .unwrap();
    }
    let written = r.work.record_feedback(&own);
    assert!(
        matches!(written[0], Err(StateError::Conflict { .. })),
        "{written:?}"
    );
    let history = store.history("t-fw").unwrap().unwrap();
    assert_eq!(history, r.finished.history);
    assert_eq!(history.task.status, "blocked");
    assert_eq!(store.episode(&own.id).unwrap().unwrap(), own);
}

#[tokio::test]
async fn no_feedback_without_recall_or_without_a_finished_attempt() {
    let rig = Rig::new();
    verified_success(&rig, "t-none", PROJECT).await;
    let store = rig.store();
    // No recall: the attempt was told of nothing, so there is nothing to give feedback on.
    let r = run(
        &rig,
        &store,
        "t-blind",
        SIMILAR,
        None,
        fixing(),
        Sensor::default(),
        false,
    )
    .await;
    let (own, _) = r
        .work
        .record_episode(&r.finished, &context(PROJECT, SIMILAR))
        .unwrap();
    assert!(r.work.record_feedback(&own).is_empty());
    assert!(store.feedback().unwrap().0.is_empty());
}

#[tokio::test]
async fn an_incompatible_episode_is_excluded_with_a_stable_inspectable_reason() {
    let rig = Rig::new();
    let e = verified_success(&rig, "t-compat", PROJECT).await;
    let store = rig.store();
    let mut changed = context(PROJECT, SIMILAR);
    changed
        .conditions
        .insert("test_definitions".into(), "sha256:changed".into());
    let mut original = context(PROJECT, SIMILAR);
    original
        .conditions
        .insert("test_definitions".into(), "sha256:before".into());
    // Recorded under one set of tests, asked under another.
    let mut recorded = e.clone();
    recorded.conditions = original.conditions.clone();
    let asking = Asking {
        context: &changed,
        task_id: Some("t-x"),
        work_known: true,
    };
    let decision = assess(&recorded, &asking, now(), &|e| store.backing(e));
    let excluded = decision.excluded.clone().unwrap();
    assert_eq!(excluded.dimension(), Dimension::ContextCompatibility);
    assert_eq!(excluded.code(), "preconditions_differ");
    assert!(
        excluded.describe().contains("test definitions changed"),
        "{}",
        excluded.describe()
    );
    assert!(matches!(decision.hard_exclusion, Check::Passed(_)));
    assert!(matches!(decision.evidence_quality, Check::Passed(_)));
    assert!(matches!(decision.relevance, Check::Passed(_)));
    let json = decision.to_json();
    assert_eq!(json["decision"], "excluded");
    assert_eq!(json["dimension"], "context_compatibility");
    assert_eq!(json["checks"]["context_compatibility"]["status"], "failed");
    // The same answer every time, and none of it is goal text.
    assert_eq!(
        assess(&recorded, &asking, now(), &|e| store.backing(e)),
        decision
    );
    assert!(!json.to_string().contains("calibration"));
    // A hard exclusion is decided first and nothing else is asked.
    let other = Asking {
        context: &context(OTHER_PROJECT, SIMILAR),
        task_id: Some("t-x"),
        work_known: true,
    };
    let foreign = assess(&recorded, &other, now(), &|e| store.backing(e));
    assert_eq!(foreign.episode, None);
    assert_eq!(foreign.excluded, Some(Exclusion::OtherScope));
    assert!(matches!(foreign.relevance, Check::NotEvaluated(_)));
}

#[tokio::test]
async fn two_eligible_episodes_with_different_procedures_are_ordered_and_flagged_deterministically()
{
    let rig = Rig::new();
    let base = verified_success(&rig, "t-cbase", PROJECT).await;
    use EpisodeOutcome::*;
    let mut a = with_procedure(&base, "t-ca~a2", VerifiedSuccess, &[W_LIB, PASS]);
    let mut b = with_procedure(
        &base,
        "t-cb~a2",
        VerifiedSuccess,
        &[W_LIB, FAIL, W_TWO, PASS],
    );
    a.created_at_ms = 1_000;
    b.created_at_ms = 2_000;
    let mut query = context(PROJECT, GOAL);
    for capability in ["project.write", "pax.test"] {
        query.contracts.insert(capability.into(), "c1.v1".into());
    }
    let pick = |episodes: Vec<EpisodeRecord>| {
        select(episodes, Vec::new(), "t-x", &query, 3_000, 3, &|_| Ok(()))
    };
    let first = pick(vec![a.clone(), b.clone()]);
    let second = pick(vec![b.clone(), a.clone()]);
    assert_eq!(first, second, "the order they are read in changes nothing");
    let ids: Vec<&str> = first
        .recalled
        .iter()
        .map(|r| r.episode.id.as_str())
        .collect();
    assert_eq!(
        ids,
        ["t-cb~a2", "t-ca~a2"],
        "equally similar: the most recent first"
    );
    assert_eq!(first.recalled[0].conflicts_with, ["t-ca~a2"]);
    assert_eq!(first.recalled[1].conflicts_with, ["t-cb~a2"]);
    let statement = first.recalled[0].statement();
    assert!(statement.contains("chip.Episode:t-ca~a2"), "{statement}");
    assert!(
        statement.contains("conflicts with recalled episode"),
        "{statement}"
    );
    assert!(
        statement.contains("not proof the procedure applies now"),
        "{statement}"
    );
    let rendered = chip_core::RecalledEpisode {
        record: String::new(),
        statement: statement.clone(),
    }
    .render();
    assert!(
        rendered.contains("conflicts with recalled episode chip.Episode:t-ca~a2"),
        "the conflict survives clipping: {rendered}"
    );
    // Two episodes that ended with the same write do not conflict.
    let c = with_procedure(&base, "t-cc~a2", VerifiedSuccess, &[W_LIB, PASS]);
    let agree = pick(vec![a.clone(), c]);
    assert!(agree.recalled.iter().all(|r| r.conflicts_with.is_empty()));
}
