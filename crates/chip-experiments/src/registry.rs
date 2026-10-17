//! The registry: what the host explicitly constructed and made available to experiments.
//!
//! It is a value the host builds and passes in, never a global. Registering a strategy makes it
//! resolvable by experiments that name it; it does not make it a default and does not touch any
//! production path. Only the host can mark a strategy as its production behavior (which `observe`
//! requires); a strategy cannot mark itself.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use chip_core::CapabilityDescriptor;

use crate::criteria::Criterion;
use crate::definition::{DefinitionError, ExperimentDefinition, Mode, ModelSpec, Versioned};
use crate::plugin::{CapabilityFixture, Confinement, Strategy, TaskFixture};

struct Registered {
    strategy: Arc<dyn Strategy>,
    production: bool,
}

#[derive(Default)]
pub struct Registry {
    strategies: BTreeMap<String, Registered>,
    fixtures: Vec<Arc<dyn CapabilityFixture>>,
    criteria: BTreeMap<String, Arc<dyn Criterion>>,
    tasks: BTreeMap<String, TaskFixture>,
    experiments: BTreeMap<String, ExperimentDefinition>,
}

/// A strategy as an experiment resolved it.
#[derive(Clone)]
pub struct ResolvedStrategy {
    pub strategy: Arc<dyn Strategy>,
    /// The host registered it as its current production behavior.
    pub production: bool,
}

/// A definition whose every name resolved and whose mode can be honoured.
pub struct ResolvedExperiment {
    pub definition: ExperimentDefinition,
    pub baseline: ResolvedStrategy,
    pub candidate: Option<ResolvedStrategy>,
    pub tasks: Vec<TaskFixture>,
    /// The fixtures providing the allowlisted capabilities, each once.
    pub fixtures: Vec<Arc<dyn CapabilityFixture>>,
    /// Each allowlisted capability and what confines it.
    pub confinement: Vec<(String, Confinement)>,
    pub criteria: Vec<Arc<dyn Criterion>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    fn insert_strategy(&mut self, strategy: Arc<dyn Strategy>, production: bool) {
        let id = strategy.id().to_string();
        assert!(
            !self.strategies.contains_key(&id),
            "strategy {id} registered twice: registration is explicit and must be unambiguous"
        );
        self.strategies.insert(
            id,
            Registered {
                strategy,
                production,
            },
        );
    }

    pub fn with_strategy(mut self, strategy: Arc<dyn Strategy>) -> Self {
        self.insert_strategy(strategy, false);
        self
    }

    /// Registers the host's current production behavior. Only `observe` requires one.
    pub fn with_production_strategy(mut self, strategy: Arc<dyn Strategy>) -> Self {
        self.insert_strategy(strategy, true);
        self
    }

    pub fn with_capability_fixture(mut self, fixture: Arc<dyn CapabilityFixture>) -> Self {
        let mine: Vec<&str> = fixture.provides().iter().map(|(c, _)| *c).collect();
        for other in &self.fixtures {
            for (c, _) in other.provides() {
                assert!(
                    !mine.contains(&c),
                    "capability {c} is provided by both {} and {}: routing must be unambiguous",
                    other.id(),
                    fixture.id()
                );
            }
        }
        self.fixtures.push(fixture);
        self
    }

    pub fn with_criterion(mut self, criterion: Arc<dyn Criterion>) -> Self {
        let id = criterion.id().to_string();
        assert!(
            !self.criteria.contains_key(&id),
            "criterion {id} registered twice"
        );
        self.criteria.insert(id, criterion);
        self
    }

    pub fn with_task(mut self, task: TaskFixture) -> Self {
        assert!(
            !self.tasks.contains_key(&task.id),
            "task {} registered twice",
            task.id
        );
        self.tasks.insert(task.id.clone(), task);
        self
    }

    pub fn with_experiment(mut self, definition: ExperimentDefinition) -> Self {
        assert!(
            !self.experiments.contains_key(&definition.id),
            "experiment {} registered twice",
            definition.id
        );
        self.experiments.insert(definition.id.clone(), definition);
        self
    }

    pub fn experiment(&self, id: &str) -> Option<&ExperimentDefinition> {
        self.experiments.get(id)
    }

    pub fn experiments(&self) -> impl Iterator<Item = &ExperimentDefinition> {
        self.experiments.values()
    }

    pub fn strategies(&self) -> impl Iterator<Item = (&Arc<dyn Strategy>, bool)> {
        self.strategies
            .values()
            .map(|r| (&r.strategy, r.production))
    }

    fn strategy(&self, wanted: &Versioned) -> Result<ResolvedStrategy, DefinitionError> {
        let r = self.strategies.get(&wanted.id).ok_or_else(|| {
            DefinitionError::UnknownStrategy(format!("{} is not registered", wanted.id))
        })?;
        if r.strategy.version() != wanted.version {
            return Err(DefinitionError::VersionMismatch(format!(
                "{wanted} was asked for; the registered {} is version {}",
                wanted.id,
                r.strategy.version()
            )));
        }
        Ok(ResolvedStrategy {
            strategy: r.strategy.clone(),
            production: r.production,
        })
    }

    /// Validates `definition` against what is registered. Refuses rather than substitutes.
    pub fn resolve(
        &self,
        definition: &ExperimentDefinition,
    ) -> Result<ResolvedExperiment, DefinitionError> {
        definition.check_shape()?;
        let mode = definition.mode;
        let baseline = self.strategy(&definition.baseline)?;
        if mode == Mode::Observe && !baseline.production {
            return Err(DefinitionError::UnsupportedMode(format!(
                "observe runs the host's production behavior, and {} is not registered as production",
                definition.baseline
            )));
        }
        let candidate = definition
            .candidate
            .as_ref()
            .map(|c| self.strategy(c))
            .transpose()?;

        let mut tasks = Vec::new();
        for wanted in &definition.tasks {
            let task = self.tasks.get(&wanted.id).ok_or_else(|| {
                DefinitionError::UnknownTask(format!("{} is not registered", wanted.id))
            })?;
            if task.version != wanted.version {
                return Err(DefinitionError::VersionMismatch(format!(
                    "{wanted} was asked for; the registered task is version {}",
                    task.version
                )));
            }
            check_fixture(task, definition)?;
            tasks.push(task.clone());
        }

        let mut fixtures: Vec<Arc<dyn CapabilityFixture>> = Vec::new();
        let mut confinement = Vec::new();
        for capability in &definition.capabilities {
            let found = self.fixtures.iter().find_map(|f| {
                f.provides()
                    .into_iter()
                    .find(|(c, _)| c == capability)
                    .map(|(_, conf)| (f, conf))
            });
            let Some((fixture, conf)) = found else {
                return Err(DefinitionError::UnknownCapability(format!(
                    "no registered fixture provides {capability}"
                )));
            };
            if matches!(mode, Mode::Observe | Mode::Isolated) && conf == Confinement::Unconfined {
                return Err(DefinitionError::IsolationUnavailable(format!(
                    "{capability} is unconfined (it runs a process, network call or project code with the operator's permissions); {} execution can only offer capabilities whose effects the executor confines to the disposable environment",
                    mode.name()
                )));
            }
            if !fixtures.iter().any(|f| Arc::ptr_eq(f, fixture)) {
                fixtures.push(fixture.clone());
            }
            confinement.push((capability.clone(), conf));
        }

        if mode == Mode::Replay {
            for task in &tasks {
                let recording = task.recording.as_ref().expect("checked by check_fixture");
                for recorded in recording.capabilities() {
                    if !definition.capabilities.iter().any(|c| c == recorded) {
                        return Err(DefinitionError::FixtureMismatch(format!(
                            "task {}'s recording holds {recorded}, which is not on the allowlist",
                            task.id
                        )));
                    }
                }
            }
        }

        let mut criteria = Vec::new();
        for id in &definition.criteria {
            let c = self.criteria.get(id).ok_or_else(|| {
                DefinitionError::UnknownCriterion(format!("{id} is not registered"))
            })?;
            if !c.supports(mode) {
                return Err(DefinitionError::UnsupportedMode(format!(
                    "criterion {id} cannot be decided in {} mode",
                    mode.name()
                )));
            }
            criteria.push(c.clone());
        }

        Ok(ResolvedExperiment {
            definition: definition.clone(),
            baseline,
            candidate,
            tasks,
            fixtures,
            confinement,
            criteria,
        })
    }
}

/// A task fixture against the experiment's input contract.
fn check_fixture(
    task: &TaskFixture,
    definition: &ExperimentDefinition,
) -> Result<(), DefinitionError> {
    let mismatch = |why: String| {
        Err(DefinitionError::FixtureMismatch(format!(
            "task {}: {why}",
            task.id
        )))
    };
    if task.goal.trim().is_empty() {
        return mismatch("the goal is empty".into());
    }
    if definition.model == ModelSpec::Recorded && task.model_replies.is_empty() {
        return mismatch("the experiment uses recorded model replies and the task has none".into());
    }
    match definition.mode {
        Mode::Replay => {
            if task.recording.is_none() {
                return mismatch(
                    "replay needs recorded capability results and the task has none".into(),
                );
            }
        }
        Mode::Observe | Mode::Isolated => {
            if task.files.is_empty() {
                return mismatch("an isolated run needs a project to start from".into());
            }
            let bytes: usize = task.files.iter().map(|(_, c)| c.len()).sum();
            if bytes > TaskFixture::MAX_PROJECT_BYTES {
                return mismatch(format!(
                    "the project is {bytes} bytes; at most {} are allowed",
                    TaskFixture::MAX_PROJECT_BYTES
                ));
            }
            for (path, _) in task.files.iter().chain(&task.expected_files) {
                if !safe_relative(path) {
                    return mismatch(format!("{path:?} is not a plain project-relative path"));
                }
            }
        }
    }
    Ok(())
}

/// Relative, normal components only: no root, no `..`, no empty or `.` segment.
pub(crate) fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

/// The descriptors the allowlisted capabilities have, read from the fixtures' declarations.
pub(crate) async fn descriptors(
    resolved: &ResolvedExperiment,
    root: &Path,
) -> Result<Vec<CapabilityDescriptor>, String> {
    let mut out = Vec::new();
    for fixture in &resolved.fixtures {
        let declared = fixture
            .backend(root)
            .capabilities()
            .await
            .map_err(|e| e.to_string())?;
        out.extend(declared.into_iter().filter(|d| {
            resolved
                .definition
                .capabilities
                .iter()
                .any(|c| c == d.id.as_str())
        }));
    }
    Ok(out)
}
