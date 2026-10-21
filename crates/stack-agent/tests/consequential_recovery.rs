//! A consequential capability whose executor started the action and cannot say how it ended must
//! leave its intent unresolved: recorded as a definite failure, a later attempt would be free to
//! perform it again.
//!
//! `pax.test` is consequential (it runs the project's own code). Its stand-in here appends one line
//! to a ledger of its own each time it actually runs, outside anything stack-agent records, and
//! then ends the way each test says. Before this change the executor reported a timeout, an
//! unreadable result and an oversized one as `ExecutionFailed`, which durable state records as a
//! resolved, failed outcome: the next attempt began and the model could ask for the run again.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chip_agent_state::{AgentStore, Begun, DurableConfig, DurableWork, StateError, begin};
use chip_core::{
    Agent, Capability, CapabilityId, CapabilityRequest, DecisionError, Effect, ExecutionId,
    ExecutionObserver, NoLocalPolicy, WorkDecision, WorkDecisionBoundary, WorkGoal, WorkId,
    WorkOutcome, WorkSpec,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use chip_pax::PaxExecutor;
use tempfile::TempDir;

const GOAL: &str = "check that the tests pass";
const LEASE_MS: u64 = 1_000;

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

fn test() -> WorkDecision {
    WorkDecision::RequestCapability(CapabilityRequest::new(
        ExecutionId::new("x"),
        CapabilityId::new("pax.test").unwrap(),
    ))
}

/// A `pax` that runs (counted in its own ledger) and then does `ending`.
struct Pax {
    dir: TempDir,
}

impl Pax {
    fn new(ending: &str) -> Self {
        let dir = TempDir::new().unwrap();
        let d = dir.path().display();
        let script = format!(
            "#!/bin/sh\n\
             if [ \"$1\" = \"--version\" ]; then echo 'pax 0.4.1'; exit 0; fi\n\
             echo run >> '{d}/ledger'\n\
             {ending}\n"
        );
        let path = dir.path().join("pax");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    fn binary(&self) -> PathBuf {
        self.dir.path().join("pax")
    }

    /// Runs the stand-in actually performed.
    fn performed(&self) -> usize {
        std::fs::read_to_string(self.dir.path().join("ledger"))
            .map(|l| l.lines().count())
            .unwrap_or(0)
    }
}

fn config() -> DurableConfig {
    DurableConfig {
        lease_ms: LEASE_MS,
        effects: [("pax.test".to_string(), Effect::Consequential)].into(),
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

async fn attempt(store: &AgentStore, pax: &Pax, project: &TempDir) -> WorkOutcome {
    let executor = Arc::new(
        PaxExecutor::new(project.path())
            .with_binary(pax.binary())
            .with_timeout(Duration::from_millis(400)),
    );
    let work = ready(begin(store, "task", GOAL, &config()));
    let report = Agent::new(Arc::new(Model))
        .with_capabilities(executor.clone())
        .with_executor(work.executor(executor))
        .with_observer(Arc::new(ExecutionObserver))
        .run_work(
            &WorkSpec::new(WorkId::new("task"), WorkGoal::new(GOAL))
                .with_restored(work.restored())
                .with_settled(work.settled_actions()),
            &NoLocalPolicy,
            &Script(Mutex::new(vec![
                test(),
                test(),
                WorkDecision::Complete {
                    summary: "the tests pass".into(),
                },
            ])),
        )
        .await;
    let _ = work.finish(&report, false);
    report.outcome
}

/// The run started and the executor could not establish how it ended.
async fn an_ending_the_executor_cannot_vouch_for_leaves_the_run_uncertain(ending: &str) {
    let state = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let store = AgentStore::open(state.path().join("state.feltdb")).unwrap();
    let pax = Pax::new(ending);

    let outcome = attempt(&store, &pax, &project).await;
    assert!(
        matches!(outcome, WorkOutcome::Failed { ref reason, .. } if reason.contains("outcome unknown")),
        "{outcome:?}"
    );
    assert_eq!(pax.performed(), 1, "the run happened once");
    let task = store
        .tasks()
        .unwrap()
        .into_iter()
        .find(|t| t.task_id == "task")
        .unwrap();
    assert_eq!(
        task.unresolved_intents, 1,
        "no outcome was established, so none is recorded"
    );

    tokio::time::sleep(Duration::from_millis(LEASE_MS + 300)).await;
    match begin(&store, "task", GOAL, &config()) {
        Err(StateError::OutcomeUncertain { intents, .. }) => assert_eq!(intents.len(), 1),
        Err(e) => panic!("expected OutcomeUncertain, got {e}"),
        Ok(_) => panic!("a later attempt began over a run whose outcome is unknown"),
    }
    assert_eq!(pax.performed(), 1, "and nothing ran it again");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pax_test_that_outlasts_its_time_limit_has_an_unknown_outcome_not_a_failed_one() {
    an_ending_the_executor_cannot_vouch_for_leaves_the_run_uncertain("sleep 30").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pax_test_whose_result_cannot_be_read_has_an_unknown_outcome_not_a_failed_one() {
    an_ending_the_executor_cannot_vouch_for_leaves_the_run_uncertain("echo 'not a result'").await;
}
