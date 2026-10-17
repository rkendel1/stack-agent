//! The reference experiment: two deliberately small, deterministic strategies for one task family,
//! so the framework can be exercised end to end with no model, no network and no credential.
//!
//! The task family: "set `retries` in a config file to the value a notes file requires". The goal
//! names both files in backticks. Two strategies with genuinely different approaches:
//!
//! - `reference.read-then-write@1` reads the notes, reads the config, writes the config.
//! - `reference.search-then-write@1` searches the project for `retries` (one call finds both the
//!   config line and, if it is worded that way, the requirement), reads the config, writes it.
//!
//! The search is cheaper to reason about and brittle: when the notes word the requirement
//! differently ("retry count"), it finds no requirement and blocks rather than guess. The two tasks
//! include one of each wording. The result is whatever the runs show; nothing here scores either.

use std::sync::Arc;

use chip_core::{
    CapabilityBackend, CapabilityId, CapabilityRequest, ExecutionId, ExecutionStatus, InputValue,
    LocalWorkPolicy, Observation, WorkDecision, WorkView,
};
use chip_project::{PROJECT_READ, PROJECT_SEARCH, PROJECT_WRITE, ProjectExecutor};

use crate::criteria::{ExpectedTree, ExpectedWritesObserved, NoUnexpectedChanges};
use crate::definition::{
    EvidenceField, ExperimentDefinition, Mode, ModelSpec, RunLimits, Versioned,
};
use crate::plugin::{
    CapabilityFixture, Confinement, ExtensionPoint, Recording, Strategy, TaskFixture,
};
use crate::registry::Registry;

/// The project capabilities over a run's root. Navigation, reads and writes are confined to the
/// root by `ProjectExecutor`'s own path rules; its Git capabilities run the `git` program and are
/// declared unconfined, so isolated execution refuses them.
pub struct ProjectFixture;

impl CapabilityFixture for ProjectFixture {
    fn id(&self) -> &'static str {
        "chip-project"
    }

    fn version(&self) -> u32 {
        1
    }

    fn provides(&self) -> Vec<(&'static str, Confinement)> {
        let mut out = vec![
            (chip_project::PROJECT_LIST, Confinement::Root),
            (PROJECT_SEARCH, Confinement::Root),
            (PROJECT_READ, Confinement::Root),
            (PROJECT_WRITE, Confinement::Root),
        ];
        out.extend(
            chip_project::GIT_CAPABILITIES
                .iter()
                .map(|c| (*c, Confinement::Unconfined)),
        );
        out
    }

    fn backend(&self, root: &std::path::Path) -> Arc<dyn CapabilityBackend> {
        Arc::new(ProjectExecutor::new(root))
    }
}

fn request(capability: &str, inputs: &[(&str, &str)]) -> WorkDecision {
    let mut r = CapabilityRequest::new(
        ExecutionId::new("proposed"),
        CapabilityId::new(capability).expect("a reference capability id is valid"),
    );
    for (k, v) in inputs {
        r.inputs
            .insert((*k).to_string(), InputValue::Text((*v).to_string()));
    }
    r.inputs_present = !inputs.is_empty();
    WorkDecision::RequestCapability(r)
}

/// The two paths the goal names in backticks: (notes `.md`, config `.toml`).
fn goal_paths(goal: &str) -> Option<(String, String)> {
    let quoted: Vec<&str> = goal.split('`').skip(1).step_by(2).collect();
    let notes = quoted.iter().find(|q| q.ends_with(".md"))?;
    let config = quoted.iter().find(|q| q.ends_with(".toml"))?;
    Some((notes.to_string(), config.to_string()))
}

/// The JSON first line and the body of a successful project observation of `capability`.
fn project_observation<'a>(
    o: &'a Observation,
    capability: &str,
) -> Option<(serde_json::Value, &'a str)> {
    if o.status != ExecutionStatus::Success {
        return None;
    }
    let text = o.output.as_deref()?;
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let head: serde_json::Value = serde_json::from_str(first).ok()?;
    (head["capability"] == capability).then_some((head, rest))
}

fn read_of(observations: &[Observation], path: &str) -> Option<String> {
    observations.iter().rev().find_map(|o| {
        let (head, body) = project_observation(o, PROJECT_READ)?;
        (head["path"] == path && head["complete"] == true).then(|| {
            body.strip_prefix("--- content ---\n")
                .unwrap_or(body)
                .to_string()
        })
    })
}

fn failed_last(observations: &[Observation]) -> bool {
    observations
        .last()
        .is_some_and(|o| o.status != ExecutionStatus::Success)
}

fn wrote(observations: &[Observation], path: &str) -> bool {
    observations
        .iter()
        .any(|o| project_observation(o, PROJECT_WRITE).is_some_and(|(h, _)| h["path"] == path))
}

/// The number after "must be" in a text, if there is one.
fn required_value(text: &str) -> Option<u32> {
    let after = text.split("must be").nth(1)?;
    let digits: String = after
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// The config with its `retries` line set to `value`, or `None` if it has no such line.
fn with_retries(config: &str, value: u32) -> Option<String> {
    let mut found = false;
    let lines: Vec<String> = config
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("retries") && l.contains('=') {
                found = true;
                format!("retries = {value}")
            } else {
                l.to_string()
            }
        })
        .collect();
    found.then(|| lines.join("\n") + if config.ends_with('\n') { "\n" } else { "" })
}

/// The common tail of both strategies: with the requirement known and the config read, write it,
/// then claim completion. Chip decides whether the claim stands.
fn write_then_complete(view: &WorkView<'_>, config_path: &str, value: u32) -> WorkDecision {
    if wrote(view.observations, config_path) {
        return WorkDecision::Complete {
            summary: format!("set retries to {value} in {config_path}"),
        };
    }
    let Some(config) = read_of(view.observations, config_path) else {
        return request(PROJECT_READ, &[("path", config_path)]);
    };
    match with_retries(&config, value) {
        Some(content) => request(
            PROJECT_WRITE,
            &[("path", config_path), ("content", &content)],
        ),
        None => WorkDecision::Block {
            reason: format!("{config_path} has no retries line to set"),
        },
    }
}

struct ReadThenWrite;

impl LocalWorkPolicy for ReadThenWrite {
    fn propose(&self, view: &WorkView<'_>) -> Option<WorkDecision> {
        if failed_last(view.observations) {
            return Some(WorkDecision::Block {
                reason: "the last request failed".into(),
            });
        }
        let Some((notes_path, config_path)) = goal_paths(view.goal.as_str()) else {
            return Some(WorkDecision::Block {
                reason: "the goal names no notes and config file".into(),
            });
        };
        let Some(notes) = read_of(view.observations, &notes_path) else {
            return Some(request(PROJECT_READ, &[("path", &notes_path)]));
        };
        let Some(value) = required_value(&notes) else {
            return Some(WorkDecision::Block {
                reason: format!("{notes_path} states no required value"),
            });
        };
        Some(write_then_complete(view, &config_path, value))
    }
}

struct SearchThenWrite;

impl LocalWorkPolicy for SearchThenWrite {
    fn propose(&self, view: &WorkView<'_>) -> Option<WorkDecision> {
        if failed_last(view.observations) {
            return Some(WorkDecision::Block {
                reason: "the last request failed".into(),
            });
        }
        let Some((notes_path, config_path)) = goal_paths(view.goal.as_str()) else {
            return Some(WorkDecision::Block {
                reason: "the goal names no notes and config file".into(),
            });
        };
        let search = view
            .observations
            .iter()
            .find_map(|o| project_observation(o, PROJECT_SEARCH).map(|(_, body)| body.to_string()));
        let Some(matches) = search else {
            return Some(request(PROJECT_SEARCH, &[("query", "retries")]));
        };
        let requirement = matches
            .lines()
            .filter(|row| row.starts_with(&format!("{notes_path}:")))
            .find_map(required_value);
        let Some(value) = requirement else {
            return Some(WorkDecision::Block {
                reason: "searching for `retries` found no stated requirement".into(),
            });
        };
        Some(write_then_complete(view, &config_path, value))
    }
}

/// A strategy over one decision policy, with Chip's default context policy.
pub struct DecisionStrategy {
    pub id: &'static str,
    pub version: u32,
    pub description: &'static str,
    pub policy: Arc<dyn LocalWorkPolicy>,
}

impl Strategy for DecisionStrategy {
    fn id(&self) -> &'static str {
        self.id
    }

    fn version(&self) -> u32 {
        self.version
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn extension_points(&self) -> Vec<ExtensionPoint> {
        vec![ExtensionPoint::Decision]
    }

    fn decisions(&self) -> Arc<dyn LocalWorkPolicy> {
        self.policy.clone()
    }
}

pub fn read_then_write() -> Arc<dyn Strategy> {
    Arc::new(DecisionStrategy {
        id: "reference.read-then-write",
        version: 1,
        description: "read the notes, read the config, write the config, then claim completion",
        policy: Arc::new(ReadThenWrite),
    })
}

pub fn search_then_write() -> Arc<dyn Strategy> {
    Arc::new(DecisionStrategy {
        id: "reference.search-then-write",
        version: 1,
        description: "search the project for `retries`, read the config, write it, then claim completion",
        policy: Arc::new(SearchThenWrite),
    })
}

const GOAL: &str = "Set `retries` in `config/settings.toml` to the value `docs/notes.md` requires.";
const CONFIG: &str = "name = \"courier\"\nretries = 1\ntimeout_ms = 500\n";

fn task(id: &str, notes: &str, value: u32, recording: &str, replies: &[&str]) -> TaskFixture {
    TaskFixture {
        id: id.into(),
        version: 1,
        goal: GOAL.into(),
        files: vec![
            ("config/settings.toml".into(), CONFIG.into()),
            ("docs/notes.md".into(), notes.into()),
            ("README.md".into(), "# courier\n".into()),
        ],
        expected_files: vec![(
            "config/settings.toml".into(),
            CONFIG.replace("retries = 1", &format!("retries = {value}")),
        )],
        model_replies: replies.iter().map(|r| r.to_string()).collect(),
        recording: Some(Recording::from_json(recording).expect("the committed recording parses")),
    }
}

/// The requirement worded with the word `retries`, which a search for it finds.
pub fn explicit_task() -> TaskFixture {
    task(
        "retries.explicit",
        "# Notes\n\nThe number of retries must be 3 for the upstream contract.\n",
        3,
        include_str!("../fixtures/retries.explicit.recording.json"),
        &[
            r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"docs/notes.md"}}"#,
            r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"config/settings.toml"}}"#,
            r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"config/settings.toml","content":"name = \"courier\"\nretries = 3\ntimeout_ms = 500\n"}}"#,
            r#"{"decision":"complete","summary":"retries is now 3"}"#,
        ],
    )
}

/// The same requirement worded "retry count", which a search for `retries` does not find.
pub fn worded_task() -> TaskFixture {
    task(
        "retries.worded",
        "# Notes\n\nThe retry count must be 4 for the upstream contract.\n",
        4,
        include_str!("../fixtures/retries.worded.recording.json"),
        &[],
    )
}

fn definition(id: &str, mode: Mode, criteria: &[&str]) -> ExperimentDefinition {
    ExperimentDefinition {
        id: id.into(),
        version: 1,
        description: "read-then-write versus search-then-write on the retries task family".into(),
        mode,
        baseline: Versioned {
            id: "reference.read-then-write".into(),
            version: 1,
        },
        candidate: Some(Versioned {
            id: "reference.search-then-write".into(),
            version: 1,
        }),
        tasks: vec![
            Versioned {
                id: "retries.explicit".into(),
                version: 1,
            },
            Versioned {
                id: "retries.worded".into(),
                version: 1,
            },
        ],
        capabilities: vec![
            PROJECT_READ.into(),
            PROJECT_SEARCH.into(),
            PROJECT_WRITE.into(),
        ],
        limits: RunLimits {
            max_turns: 8,
            max_executions: 6,
            deadline_ms: 30_000,
        },
        criteria: criteria.iter().map(|c| c.to_string()).collect(),
        model: ModelSpec::None,
        evidence: vec![
            EvidenceField::Outcome,
            EvidenceField::Criteria,
            EvidenceField::Decisions,
            EvidenceField::Executions,
            EvidenceField::ModelCalls,
            EvidenceField::Duration,
            EvidenceField::Tokens,
        ],
    }
}

pub fn isolated_experiment() -> ExperimentDefinition {
    definition(
        "reference.retries-isolated",
        Mode::Isolated,
        &[
            "observed.expected-writes",
            "tree.expected-files",
            "tree.no-unexpected-changes",
        ],
    )
}

pub fn replay_experiment() -> ExperimentDefinition {
    definition(
        "reference.retries-replay",
        Mode::Replay,
        &["observed.expected-writes"],
    )
}

/// Everything the reference experiments need, registered explicitly. A host adds its own
/// strategies (production ones included) to this.
pub fn registry() -> Registry {
    Registry::new()
        .with_strategy(read_then_write())
        .with_strategy(search_then_write())
        .with_capability_fixture(Arc::new(ProjectFixture))
        .with_criterion(Arc::new(ExpectedWritesObserved))
        .with_criterion(Arc::new(ExpectedTree))
        .with_criterion(Arc::new(NoUnexpectedChanges))
        .with_task(explicit_task())
        .with_task(worded_task())
        .with_experiment(isolated_experiment())
        .with_experiment(replay_experiment())
}
