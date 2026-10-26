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
