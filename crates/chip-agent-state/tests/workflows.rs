//! Workflow candidates through FeltDB's real native agent state: extraction from verified
//! episodes, its refusals, determinism and idempotency, conflicts, and validation against the
//! runtime as it is now.
//!
//! Every work is an ordinary Chip work over a durable executor on a file-backed database,
//! finished with the verdict a host gives, recorded as an episode, then extracted from. The
//! capabilities have the product's names and shapes (`project.write` with `path` and `content`,
//! `pax.test` with none) so that extraction reads real recorded requests.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_agent_state::feltdb::FeltDb;
use chip_agent_state::{
    AgentStore, Begun, Current, Declared, DurableConfig, EPISODE_COLLECTION, EpisodeContext,
    EpisodeOutcome, EpisodeRecord, EpisodeWrite, InputSource, StateError, Verification,
    WORKFLOW_COLLECTION, WorkflowCandidate, begin, extract, intent_terms, validate,
    workflow_codes as codes,
};
use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, CapabilityRequest, DecisionError, ExecutionError,
    ExecutionId, ExecutionObserver, ExecutionRequest, ExecutionResult, Executor, InputValue,
    NoLocalPolicy, Observation, ObservationPredicate, WorkDecision, WorkDecisionBoundary, WorkGoal,
    WorkId, WorkOutcome, WorkReport, WorkSpec,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use tempfile::TempDir;

const PROJECT: &str = "path:00000000000000aa";
const GOAL: &str = "add the function two to the library";
const CONTENT: &str = "pub fn two() -> u8 { 2 } // sentinel-model-written-content";

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

fn write(path: &str) -> WorkDecision {
    WorkDecision::RequestCapability(
        CapabilityRequest::new(
            ExecutionId::new("x"),
            CapabilityId::new("project.write").unwrap(),
        )
        .with_input("path", InputValue::Text(path.into()))
        .with_input("content", InputValue::Text(CONTENT.into())),
    )
}

fn test() -> WorkDecision {
    WorkDecision::RequestCapability(CapabilityRequest::new(
        ExecutionId::new("x"),
        CapabilityId::new("pax.test").unwrap(),
    ))
}

fn complete() -> WorkDecision {
    WorkDecision::Complete {
        summary: "done".into(),
    }
}

fn descriptors() -> Vec<CapabilityDescriptor> {
    let mut w = CapabilityDescriptor::new(
        CapabilityId::new("project.write").unwrap(),
        "Write",
        "writes a file",
    )
    .without_evidence_reuse();
    for name in ["path", "content"] {
        w.inputs.push(CapabilityInput {
            name: name.into(),
            description: name.into(),
            required: true,
        });
    }
    let t = CapabilityDescriptor::new(CapabilityId::new("pax.test").unwrap(), "Test", "tests")
        .without_evidence_reuse();
    vec![w, t]
}

struct Caps;

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(descriptors())
    }
    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

/// `pax.test` passes once `src/lib.rs` has been written (or `checks` was, when given).
struct Project {
    checks: &'static str,
    written: Mutex<Vec<String>>,
    ok: AtomicBool,
}

impl Project {
    fn checking(checks: &'static str) -> Self {
        Self {
            checks,
            written: Mutex::new(Vec::new()),
            ok: AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl Executor for Project {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        Ok(match r.intent.as_str() {
            "project.write" => {
                let path = match r.inputs.get("path") {
                    Some(InputValue::Text(p)) => p.clone(),
                    _ => String::new(),
                };
                if path == self.checks {
                    self.ok.store(true, Ordering::SeqCst);
                }
                self.written.lock().unwrap().push(path);
                ExecutionResult::success(r.id, "written")
            }
            _ if self.ok.load(Ordering::SeqCst) => ExecutionResult::success(r.id, "passed"),
            _ => ExecutionResult::failure(r.id, "failed"),
        })
    }
}

#[derive(Debug)]
struct Passed;

impl ObservationPredicate for Passed {
    fn describe(&self) -> String {
        "a passing test run".into()
    }
    fn satisfied_by(&self, o: &Observation) -> bool {
        o.output.as_deref() == Some("passed")
    }
    fn satisfied_by_trajectory(&self, observations: &[Observation]) -> bool {
        observations.last().is_some_and(|o| self.satisfied_by(o))
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
    fn store(&self) -> AgentStore {
        AgentStore::new(FeltDb::open(self.dir.path().join("agent-state.log")).unwrap())
    }
}

fn contracts() -> BTreeMap<String, String> {
    ["project.write", "pax.test"]
        .into_iter()
        .map(|c| (c.to_string(), "c1.v1".to_string()))
        .collect()
}

fn context(goal: &str) -> EpisodeContext {
    EpisodeContext {
        project: PROJECT.into(),
        kind: "change".into(),
        intent_terms: intent_terms(goal),
        conditions: [("build_systems", "cargo"), ("test_definitions", "none")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        contracts: contracts(),
        recorded_by: "workflows-test".into(),
    }
}

fn verdict(report: &WorkReport, goal: Option<bool>) -> Verification {
    let accepted = matches!(report.outcome, WorkOutcome::Completed { .. }) && goal == Some(true);
    Verification {
        accepted,
        verified: accepted,
        goal_satisfied: goal,
        grounded: false,
        evidence_invalidated: None,
        exit_status: if accepted { 0 } else { 2 },
    }
}

/// One work, finished and recorded as an episode.
async fn work(
    store: &AgentStore,
    task: &str,
    decisions: Vec<WorkDecision>,
    project: Project,
) -> EpisodeRecord {
    let durable = match begin(store, task, GOAL, &DurableConfig::default()) {
        Ok(Begun::Ready(w)) => w,
        _ => panic!("begin"),
    };
    let mut spec = WorkSpec::new(WorkId::new(task), WorkGoal::new(GOAL))
        .with_required_observation(Arc::new(Passed));
    spec.limits.max_turns = decisions.len().max(1);
    spec.limits.max_executions = 8;
    let report = Agent::new(Arc::new(Model))
        .with_capabilities(Arc::new(Caps))
        .with_executor(durable.executor(Arc::new(project)))
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
    let finished = durable
        .finish_verified(&report, verdict(&report, spec.goal_level(&report.events)))
        .unwrap();
    durable.record_episode(&finished, &context(GOAL)).unwrap().0
}

async fn verified(store: &AgentStore, task: &str) -> EpisodeRecord {
    let e = work(
        store,
        task,
        vec![write("src/lib.rs"), test(), complete()],
        Project::checking("src/lib.rs"),
    )
    .await;
    assert_eq!(
        e.outcome,
        EpisodeOutcome::VerifiedSuccess,
        "{}",
        e.outcome_basis
    );
    e
}

fn declared() -> BTreeMap<String, Declared> {
    let mut m = BTreeMap::new();
    m.insert(
        "project.write".to_string(),
        Declared {
            contract: "c1.v1".into(),
            inputs: vec![("path".into(), true), ("content".into(), true)],
        },
    );
    m.insert(
        "pax.test".to_string(),
        Declared {
            contract: "c1.v1".into(),
            inputs: vec![],
        },
    );
    m
}

/// Validation of `c` among `all` against the project as `context(GOAL)` describes it.
fn check(
    store: &AgentStore,
    c: &WorkflowCandidate,
    all: &[WorkflowCandidate],
    capabilities: &BTreeMap<String, Declared>,
    conditions: &BTreeMap<String, String>,
) -> chip_agent_state::Validation {
    let episodes = store.episodes().unwrap().0;
    validate(
        c,
        all,
        &Current {
            project: PROJECT,
            conditions,
            capabilities,
            task: None,
            episode: &|id| episodes.iter().find(|e| e.id == id).cloned(),
            backing: &|e| store.backing(e),
        },
    )
}

fn raw_rows(store: &AgentStore) -> Vec<(String, serde_json::Value)> {
    let mut rows = Vec::new();
    for collection in [
        EPISODE_COLLECTION,
        WORKFLOW_COLLECTION,
        "chip.WorkResult",
        "chip.Observation",
    ] {
        for row in store.database().list_collection(collection).unwrap() {
            rows.push((row.key, row.value));
        }
    }
    rows
}

// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_verified_episode_yields_a_single_observation_candidate_with_references_not_content() {
    let rig = Rig::new();
    let store = rig.store();
    let e = verified(&store, "t-one").await;
    let done = store.extract_workflows(PROJECT, "test").unwrap();
    assert!(done.rejected.is_empty(), "{:?}", done.rejected);
    assert_eq!(done.written.len(), 1);
    let c = store.workflow(&done.written[0]).unwrap().unwrap();
    assert_eq!(c.schema, "chip.workflow_candidate.v1");
    assert_eq!(c.strength, "single_observation");
    assert_eq!(c.support.len(), 1);
    assert_eq!(c.support[0].episode, e.id);
    assert_eq!(c.support[0].result_record, e.result_record);
    assert_eq!(c.kind, "change");
    assert!(c.goal_signature.starts_with("g1."));
    // The procedure, in capability terms: write a fixed path with a parameter content, then test.
    assert_eq!(c.steps.len(), 2);
    assert_eq!(c.steps[0].capability, "project.write");
    assert_eq!(c.steps[0].role, "change");
    let inputs: BTreeMap<&str, &InputSource> = c.steps[0]
        .inputs
        .iter()
        .map(|i| (i.name.as_str(), &i.source))
        .collect();
    assert_eq!(
        inputs["path"],
        &InputSource::Fixed {
            value: "src/lib.rs".into()
        }
    );
    assert_eq!(inputs["content"], &InputSource::Parameter);
    assert_eq!(c.steps[1].capability, "pax.test");
    assert!(c.steps[1].inputs.is_empty());
    assert_eq!(c.steps[0].evidence, vec![e.observation_records[0].clone()]);
    let v = c.verification.as_ref().unwrap();
    assert_eq!(
        (v.kind.as_str(), v.step),
        ("test_run_after_last_change", Some(1))
    );
    assert!(c.unresolved.is_empty());
    assert_eq!(c.preconditions, context(GOAL).conditions);
    assert_eq!(
        c.required_capabilities["project.write"].as_deref(),
        Some("c1.v1")
    );
    // The model-written content is in the observation record, never in the candidate.
    let text = serde_json::to_string(&c).unwrap();
    assert!(!text.contains("sentinel-model-written-content"), "{text}");
    assert!(!text.contains("function"), "no goal words");
    // And it validates against the same runtime.
    let all = store.workflows().unwrap().0;
    let valid = check(&store, &c, &all, &declared(), &context(GOAL).conditions);
    assert!(valid.eligible, "{:?}", valid.checks);
}

#[tokio::test]
async fn unverified_unsupported_and_unreconstructable_episodes_yield_no_candidate() {
    let rig = Rig::new();
    let store = rig.store();
    // Blocked after the write: not a verified success.
    let blocked = work(
        &store,
        "t-blocked",
        vec![
            write("src/lib.rs"),
            WorkDecision::Block {
                reason: "no".into(),
            },
        ],
        Project::checking("src/lib.rs"),
    )
    .await;
    assert_ne!(blocked.outcome, EpisodeOutcome::VerifiedSuccess);
    let good = verified(&store, "t-good").await;
    let all = store.episodes().unwrap().0;
    // As the store reads them, except the good episode's first observation, which is treated as
    // a record with no request (a v1 observation, or an operator's settlement).
    let first = good.observation_records[0].trim_start_matches("chip.Observation:");
    let recorded = |id: &str| (id != first).then(|| store.recorded_step(id)).flatten();
    let out = extract(&all, &recorded, &|_| Ok(()), "test");
    let codes_of = |id: &str| {
        out.rejected
            .iter()
            .find(|r| r.episode == id)
            .map(|r| r.code)
    };
    assert_eq!(codes_of(&blocked.id), Some("not_verified_success"));
    assert_eq!(codes_of(&good.id), Some("inputs_unrecorded"));
    assert!(out.candidates.is_empty());
    // An episode whose cited records do not back it.
    let out = extract(
        &all,
        &recorded,
        &|_| Err("its result record does not exist".into()),
        "t",
    );
    assert_eq!(
        out.rejected
            .iter()
            .find(|r| r.episode == good.id)
            .map(|r| r.code),
        Some("evidence_missing")
    );
    // A procedure cut short: more executions than recorded steps.
    let mut cut = good.clone();
    cut.executions = 5;
    let out = extract(&[cut], &|_| None, &|_| Ok(()), "t");
    assert_eq!(out.rejected[0].code, "procedure_incomplete");
}

#[tokio::test]
async fn extraction_is_deterministic_idempotent_and_survives_reopening() {
    let rig = Rig::new();
    let store = rig.store();
    verified(&store, "t-a").await;
    verified(&store, "t-b").await;
    let first = store.extract_workflows(PROJECT, "test").unwrap();
    assert_eq!(
        first.written.len(),
        1,
        "two episodes, one procedure, one candidate"
    );
    let again = store.extract_workflows(PROJECT, "test").unwrap();
    assert!(again.written.is_empty());
    assert_eq!(again.already_recorded, first.written);
    let reopened = rig.store();
    let c = reopened.workflow(&first.written[0]).unwrap().unwrap();
    assert_eq!(c.strength, "repeated");
    assert_eq!(c.support.len(), 2);
    assert_eq!(
        c.steps[0].evidence.len(),
        2,
        "each step cites both episodes"
    );
    assert_eq!(
        reopened
            .extract_workflows(PROJECT, "test")
            .unwrap()
            .already_recorded,
        first.written
    );
    // The same answer whatever order the episodes are read in.
    let mut episodes = reopened.episodes().unwrap().0;
    let run = |episodes: &[EpisodeRecord]| {
        extract(
            episodes,
            &|id| reopened.recorded_step(id),
            &|e| reopened.backing(e),
            "test",
        )
    };
    let forward = run(&episodes);
    episodes.reverse();
    assert_eq!(run(&episodes), forward);
    assert_eq!(forward.candidates, reopened.workflows().unwrap().0);
}

#[tokio::test]
async fn more_support_writes_a_new_candidate_and_the_older_one_is_superseded() {
    let rig = Rig::new();
    let store = rig.store();
    verified(&store, "t-1").await;
    let one = store.extract_workflows(PROJECT, "test").unwrap().written;
    verified(&store, "t-2").await;
    let two = store.extract_workflows(PROJECT, "test").unwrap().written;
    assert_eq!((one.len(), two.len()), (1, 1));
    assert_ne!(one, two);
    let all = store.workflows().unwrap().0;
    let old = all.iter().find(|c| c.id == one[0]).unwrap();
    let new = all.iter().find(|c| c.id == two[0]).unwrap();
    assert_eq!(old.procedure_key, new.procedure_key);
    assert_eq!(old.support.len(), 1, "the earlier record is unchanged");
    let v_old = check(&store, old, &all, &declared(), &context(GOAL).conditions);
    let v_new = check(&store, new, &all, &declared(), &context(GOAL).conditions);
    assert_eq!(v_old.codes(), vec![codes::SUPERSEDED]);
    assert!(v_new.eligible, "{:?}", v_new.checks);
}

#[tokio::test]
async fn different_procedures_for_one_task_stay_separate_and_conflict() {
    let rig = Rig::new();
    let store = rig.store();
    verified(&store, "t-lib").await;
    let two = work(
        &store,
        "t-two",
        vec![write("src/two.rs"), test(), complete()],
        Project::checking("src/two.rs"),
    )
    .await;
    assert_eq!(two.outcome, EpisodeOutcome::VerifiedSuccess);
    let done = store.extract_workflows(PROJECT, "test").unwrap();
    assert_eq!(done.written.len(), 2, "never merged");
    let all = store.workflows().unwrap().0;
    for c in &all {
        assert_eq!(c.support.len(), 1);
        let v = check(&store, c, &all, &declared(), &context(GOAL).conditions);
        assert_eq!(
            v.codes(),
            vec![codes::CONFLICTING_PROCEDURE],
            "{:?}",
            v.checks
        );
        assert!(!v.eligible);
    }
}

#[tokio::test]
async fn validation_reports_each_current_incompatibility_with_a_stable_code() {
    let rig = Rig::new();
    let store = rig.store();
    verified(&store, "t-v").await;
    let id = store.extract_workflows(PROJECT, "test").unwrap().written[0].clone();
    let all = store.workflows().unwrap().0;
    let c = store.workflow(&id).unwrap().unwrap();
    let conditions = context(GOAL).conditions;
    let codes_with = |caps: &BTreeMap<String, Declared>, conds: &BTreeMap<String, String>| {
        check(&store, &c, &all, caps, conds).codes()
    };
    // A changed contract; a capability no longer offered; a newly required input.
    let mut changed = declared();
    changed.get_mut("pax.test").unwrap().contract = "c1.v2".into();
    assert_eq!(
        codes_with(&changed, &conditions),
        vec![codes::CONTRACT_CHANGED]
    );
    let mut gone = declared();
    gone.remove("project.write");
    assert_eq!(
        codes_with(&gone, &conditions),
        vec![codes::CAPABILITY_UNAVAILABLE]
    );
    let mut stricter = declared();
    stricter
        .get_mut("project.write")
        .unwrap()
        .inputs
        .push(("mode".into(), true));
    assert_eq!(
        codes_with(&stricter, &conditions),
        vec![codes::INPUT_UNSATISFIABLE]
    );
    let mut narrower = declared();
    narrower
        .get_mut("project.write")
        .unwrap()
        .inputs
        .retain(|(n, _)| n != "content");
    assert_eq!(
        codes_with(&narrower, &conditions),
        vec![codes::INPUT_UNSATISFIABLE]
    );
    // A precondition no longer met.
    let mut other = conditions.clone();
    other.insert("test_definitions".into(), "sha256:changed".into());
    assert_eq!(
        codes_with(&declared(), &other),
        vec![codes::PRECONDITION_UNMET]
    );
    // Missing evidence: the supporting episode can no longer be read, or is no longer backed.
    let episodes = store.episodes().unwrap().0;
    let gone_episode = validate(
        &c,
        &all,
        &Current {
            project: PROJECT,
            conditions: &conditions,
            capabilities: &declared(),
            task: None,
            episode: &|_| None,
            backing: &|e| store.backing(e),
        },
    );
    assert_eq!(gone_episode.codes(), vec![codes::EVIDENCE_MISSING]);
    let unbacked = validate(
        &c,
        &all,
        &Current {
            project: PROJECT,
            conditions: &conditions,
            capabilities: &declared(),
            task: None,
            episode: &|id| episodes.iter().find(|e| e.id == id).cloned(),
            backing: &|_| Err("its result record does not exist".into()),
        },
    );
    assert_eq!(unbacked.codes(), vec![codes::EVIDENCE_MISSING]);
    // Another project; another task.
    let foreign = validate(
        &c,
        &all,
        &Current {
            project: "path:00000000000000bb",
            conditions: &conditions,
            capabilities: &declared(),
            task: None,
            episode: &|id| episodes.iter().find(|e| e.id == id).cloned(),
            backing: &|e| store.backing(e),
        },
    );
    assert_eq!(foreign.codes(), vec![codes::OTHER_PROJECT]);
    let unrelated = intent_terms("document the deployment checklist for operators");
    let task = validate(
        &c,
        &all,
        &Current {
            project: PROJECT,
            conditions: &conditions,
            capabilities: &declared(),
            task: Some(("change", &unrelated)),
            episode: &|id| episodes.iter().find(|e| e.id == id).cloned(),
            backing: &|e| store.backing(e),
        },
    );
    assert_eq!(task.codes(), vec![codes::TASK_MISMATCH]);
    // The same answer every time.
    assert_eq!(
        check(&store, &c, &all, &changed, &conditions),
        check(&store, &c, &all, &changed, &conditions)
    );
    assert!(
        check(&store, &c, &all, &changed, &conditions).to_json()["advisory"]
            .as_str()
            .unwrap()
            .contains("grants no authority")
    );
}

#[tokio::test]
async fn unresolved_steps_and_missing_verification_prevent_eligibility() {
    let rig = Rig::new();
    let store = rig.store();
    // A wrong write, a failing test, the right write, a passing test: verified, but which earlier
    // steps the result depends on is not established.
    let mixed = work(
        &store,
        "t-mixed",
        vec![
            write("src/two.rs"),
            test(),
            write("src/lib.rs"),
            test(),
            complete(),
        ],
        Project::checking("src/lib.rs"),
    )
    .await;
    assert_eq!(mixed.outcome, EpisodeOutcome::VerifiedSuccess);
    let done = store.extract_workflows(PROJECT, "test").unwrap();
    let c = store.workflow(&done.written[0]).unwrap().unwrap();
    assert_eq!(c.unresolved[0].code, "failed_steps");
    assert_eq!(
        c.steps[1].expected_status, "failure",
        "what happened, not what was hoped"
    );
    let all = store.workflows().unwrap().0;
    let v = check(&store, &c, &all, &declared(), &context(GOAL).conditions);
    assert_eq!(v.codes(), vec![codes::UNRESOLVED_STEP]);
    // No test run after the last change: no verification requirement can be stated.
    let mut untested = mixed.clone();
    untested.id = "t-untested~a2".into();
    untested.procedure.truncate(3);
    untested.observation_records.truncate(3);
    untested.executions = 3;
    let records: BTreeMap<String, chip_agent_state::RecordedStep> =
        [("src/two.rs", 0usize), ("x", 1), ("src/lib.rs", 2)]
            .into_iter()
            .map(|(path, i)| {
                let id = untested.observation_records[i]
                    .trim_start_matches("chip.Observation:")
                    .to_string();
                let step = &untested.procedure[i];
                let mut inputs = BTreeMap::new();
                if step.capability == "project.write" {
                    inputs.insert("path".to_string(), serde_json::Value::from(path));
                }
                (
                    id,
                    chip_agent_state::RecordedStep {
                        capability: step.capability.clone(),
                        inputs,
                    },
                )
            })
            .collect();
    let out = extract(
        &[untested],
        &|id| records.get(id).cloned(),
        &|_| Ok(()),
        "t",
    );
    let c = &out.candidates[0];
    assert!(c.verification.is_none());
    assert!(
        c.unresolved
            .iter()
            .any(|u| u.code == "verification_undefined")
    );
}

#[tokio::test]
async fn a_failed_candidate_write_changes_nothing_else() {
    let rig = Rig::new();
    let store = rig.store();
    let e = verified(&store, "t-f").await;
    let history = store.history("t-f").unwrap().unwrap();
    // Something already holds the id the candidate will be written under, with other content.
    let episodes = store.episodes().unwrap().0;
    let proposed = extract(
        &episodes,
        &|id| store.recorded_step(id),
        &|e| store.backing(e),
        "test",
    );
    let mut squatter = proposed.candidates[0].clone();
    squatter.extracted_by = "something else".into();
    assert_eq!(
        store.record_workflow(&squatter).unwrap(),
        EpisodeWrite::Recorded
    );
    // Extraction is refused for it, and keeps the stored record.
    let refused = store.extract_workflows(PROJECT, "test");
    assert!(
        matches!(refused, Err(StateError::Conflict { .. })),
        "{refused:?}"
    );
    assert_eq!(store.workflow(&squatter.id).unwrap().unwrap(), squatter);
    // The task, its episode and its records are exactly as they were.
    assert_eq!(store.history("t-f").unwrap().unwrap(), history);
    assert_eq!(store.episode(&e.id).unwrap().unwrap(), e);
}

#[tokio::test]
async fn extraction_writes_only_candidates_and_never_changes_episodes_or_evidence() {
    let rig = Rig::new();
    let store = rig.store();
    verified(&store, "t-r").await;
    let before: Vec<_> = raw_rows(&store)
        .into_iter()
        .filter(|(k, _)| !k.starts_with(WORKFLOW_COLLECTION))
        .collect();
    store.extract_workflows(PROJECT, "test").unwrap();
    let after: Vec<_> = raw_rows(&store)
        .into_iter()
        .filter(|(k, _)| !k.starts_with(WORKFLOW_COLLECTION))
        .collect();
    assert_eq!(before, after);
    assert_eq!(store.workflows().unwrap().0.len(), 1);
}

#[tokio::test]
async fn an_execution_record_is_written_once_and_a_failed_write_changes_nothing_else() {
    use chip_agent_state::{ExecutedStep, WORKFLOW_EXECUTION_SCHEMA, WorkflowExecutionRecord};
    let rig = Rig::new();
    let store = rig.store();
    let e = verified(&store, "t-x").await;
    let history = store.history("t-x").unwrap().unwrap();
    let record = WorkflowExecutionRecord {
        schema: WORKFLOW_EXECUTION_SCHEMA.into(),
        id: format!("t-x~a{}~wf", e.attempt),
        candidate: "wfp1.k.s".into(),
        candidate_record: "chip.WorkflowCandidate:wfp1.k.s".into(),
        procedure_key: "wfp1.k".into(),
        support: vec!["t-old~a2".into()],
        considered: 1,
        eligible: 1,
        selection_rule: "the one eligible candidate".into(),
        task_id: "t-x".into(),
        attempt: e.attempt,
        work_id: e.work_id.clone(),
        steps: vec![ExecutedStep {
            index: 0,
            capability: "project.write".into(),
            role: "change".into(),
            status: "completed".into(),
            decided_by: Some("model".into()),
            execution_id: Some("t-x-exec-1".into()),
            observation_record: Some(e.observation_records[0].clone()),
        }],
        completed_steps: 1,
        stopped: None,
        fallback: false,
        outcome: e.outcome,
        verification: e.verification.clone(),
        result_record: e.result_record.clone(),
        episode: e.id.clone(),
        created_at_ms: e.created_at_ms,
        recorded_by: "test".into(),
    };
    assert_eq!(
        store.record_workflow_execution(&record).unwrap(),
        EpisodeWrite::Recorded
    );
    assert_eq!(
        rig.store().record_workflow_execution(&record).unwrap(),
        EpisodeWrite::AlreadyRecorded
    );
    // A different record for the same attempt is refused; the stored one, the task, its episode
    // and its verdict are exactly as they were.
    let mut other = record.clone();
    other.outcome = EpisodeOutcome::VerifiedFailure;
    assert!(matches!(
        store.record_workflow_execution(&other),
        Err(StateError::Conflict { .. })
    ));
    assert_eq!(store.workflow_executions().unwrap().0, vec![record]);
    assert_eq!(store.history("t-x").unwrap().unwrap(), history);
    assert_eq!(store.episode(&e.id).unwrap().unwrap(), e);
}
