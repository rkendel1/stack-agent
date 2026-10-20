//! The extension contracts: behavior strategies, capability fixtures and capability sources.
//!
//! Both are thin views of contracts Chip already has. A strategy is a [`LocalWorkPolicy`] (next-step
//! proposals: planning and capability selection) and an [`EscalationContextPolicy`] (context
//! selection). A capability fixture contributes a [`CapabilityBackend`] to the work's
//! [`CapabilitySet`](chip_core::CapabilitySet). Neither gets anything else: no executor, no
//! authorization, no completion predicate, no evidence store, no model boundary. What a strategy
//! proposes, Chip validates, authorizes and executes exactly as in production, and a strategy's
//! `Complete` is a claim the runtime and the evaluator check against observations.
//!
//! A [`CapabilitySource`] is a capability fixture that one side of an experiment runs with: it
//! names the project evidence that makes it apply ([`Detection`]), the effect of each capability it
//! provides ([`Effect`]) and, optionally, a reviewed instruction artifact ([`Guidance`]) the model is
//! given while the source is active. It adds no authority: its capabilities are offered only if the
//! definition's allowlist names them, under the same routing, validation and confinement rules.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use chip_core::{
    CapabilityAvailability, CapabilityBackend, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityProvider, EscalationContext, EscalationContextPolicy, ExecutionError,
    ExecutionRequest, ExecutionResult, Executor, FullEscalationContext, InputValue,
    LocalWorkPolicy, WorkState, WorkTrajectory,
};
use serde::{Deserialize, Serialize};

use crate::definition::{Versioned, digest};

/// Where in Chip's loop a strategy may differ from the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ExtensionPoint {
    /// The next step: which capability to request with which inputs, or whether to stop. This is
    /// where planning and capability selection live; Chip has no separate decomposition contract.
    Decision,
    /// What one model escalation is told.
    Context,
}

impl ExtensionPoint {
    pub fn name(self) -> &'static str {
        match self {
            ExtensionPoint::Decision => "decision",
            ExtensionPoint::Context => "context",
        }
    }
}

/// An alternative agent behavior. Constructed explicitly by the host and registered by value; it is
/// never loaded at runtime and never becomes the default because it is registered.
pub trait Strategy: Send + Sync {
    /// Stable, non-secret identifier, for example `reference.read-then-write`.
    fn id(&self) -> &'static str;
    fn version(&self) -> u32;
    fn description(&self) -> &'static str;
    /// The points at which this strategy supplies its own behavior. The others are Chip's
    /// defaults: no local decision (escalate) and the full escalation context.
    fn extension_points(&self) -> Vec<ExtensionPoint>;
    /// Proposals for the next step. Pure: it sees the work's view and nothing else.
    fn decisions(&self) -> Arc<dyn LocalWorkPolicy>;
    /// What an escalation tells the model.
    fn context(&self) -> Arc<dyn EscalationContextPolicy> {
        Arc::new(FullEscalationContext)
    }
    /// The capability sources this side runs with, each by id and version. None by default: a
    /// strategy that names none is offered exactly the definition's fixtures.
    fn sources(&self) -> Vec<Versioned> {
        Vec::new()
    }
}

/// What confines a capability's effects. The isolated and observe modes offer only capabilities
/// whose effects their executor itself confines to the disposable environment; replay offers only
/// recorded results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Confinement {
    /// No effect outside the call: the result is computed from the inputs.
    Pure,
    /// Every effect is confined to the environment's root by the executor's own path rules (and
    /// audited afterwards by the root's observation invariants).
    Root,
    /// The capability runs a process, a network call or project code with the operator's
    /// permissions. Nothing here confines it, so isolated execution refuses it.
    Unconfined,
}

impl Confinement {
    pub fn name(self) -> &'static str {
        match self {
            Confinement::Pure => "pure",
            Confinement::Root => "root",
            Confinement::Unconfined => "unconfined",
        }
    }
}

/// A capability implementation (or adapter) a run can be offered. It contributes one backend to the
/// work's capability set; Chip's routing, input validation and loop authorization apply to it
/// unchanged. It is not a catalog: only capabilities on the definition's allowlist are declared to
/// the work.
pub trait CapabilityFixture: Send + Sync {
    fn id(&self) -> &'static str;
    fn version(&self) -> u32;
    /// The capability ids it provides, each with what confines its effects.
    fn provides(&self) -> Vec<(&'static str, Confinement)>;
    /// The backend operating on `root` (the run's disposable directory). Its declarations must not
    /// depend on or touch `root`: replay reads them without executing anything.
    fn backend(&self, root: &Path) -> Arc<dyn CapabilityBackend>;
}

/// What a capability does to the world it reaches: the production vocabulary, so a source's
/// declaration and a descriptor's are one type.
pub use chip_core::Effect;

/// Whether a capability source applies to a task's project, with what showed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    /// The source's own authoritative evidence shows it is usable here. Only this activates it.
    Confirmed { evidence: Vec<String> },
    /// The evidence the source looks for is absent.
    NotDetected { evidence: Vec<String> },
    /// Some evidence is present but does not establish that the source is usable (a declared
    /// dependency is not a running service), or the check could not be completed. Never reported
    /// as `NotDetected`.
    Inconclusive { reason: String },
}

impl Detection {
    pub fn name(&self) -> &'static str {
        match self {
            Detection::Confirmed { .. } => "confirmed",
            Detection::NotDetected { .. } => "not_detected",
            Detection::Inconclusive { .. } => "inconclusive",
        }
    }
}

/// A reviewed instruction artifact, addressed by its project-relative path and pinned by digest.
/// Chip never holds a copy: the text is read from the task's project, and only the artifact the
/// digest names is ever given to a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guidance {
    pub path: String,
    /// `sha256:` of the artifact's exact bytes.
    pub sha256: String,
}

impl Guidance {
    /// The largest artifact a model is given. A larger one leaves the source inactive.
    pub const MAX_BYTES: usize = 16 * 1024;

    /// The artifact's text from the project, if it is there, matches the digest and fits.
    pub fn load(&self, task: &TaskFixture) -> Result<String, String> {
        let Some((_, text)) = task.files.iter().find(|(p, _)| *p == self.path) else {
            return Err(format!("guidance {} is not in the project", self.path));
        };
        let found = digest(text.as_bytes());
        if found != self.sha256 {
            return Err(format!(
                "guidance {} is {found}; the source pins {}",
                self.path, self.sha256
            ));
        }
        if text.len() > Self::MAX_BYTES {
            return Err(format!(
                "guidance {} is {} bytes; at most {} are given to a model",
                self.path,
                text.len(),
                Self::MAX_BYTES
            ));
        }
        Ok(text.clone())
    }
}

/// A capability source: capabilities one side of an experiment runs with, the evidence that makes
/// them apply, and the reviewed guidance for using them. Constructed explicitly by the host and
/// registered by value; never loaded at runtime.
pub trait CapabilitySource: Send + Sync {
    /// Stable, non-secret identifier, for example `reference.lookup`.
    fn id(&self) -> &'static str;
    fn version(&self) -> u32;
    /// The capabilities, their backend and what confines each one. The same contract as a shared
    /// fixture, so the same routing, allowlist and confinement rules apply.
    fn fixture(&self) -> Arc<dyn CapabilityFixture>;
    /// The effect of each capability the fixture provides. A capability missing here is `Unknown`.
    fn effects(&self) -> Vec<(&'static str, Effect)>;
    /// Whether this source applies to `task`. Pure: it reads the task's project and recording and
    /// nothing else.
    fn detect(&self, task: &TaskFixture) -> Detection;
    /// The instruction artifact the model is given while the source is active, if any.
    fn guidance(&self) -> Option<Guidance> {
        None
    }

    /// The declared effect of `capability`.
    fn effect(&self, capability: &str) -> Effect {
        self.effects()
            .into_iter()
            .find(|(c, _)| *c == capability)
            .map_or(Effect::Unknown, |(_, e)| e)
    }
}

/// An escalation context with an active source's guidance: the strategy's own policy decides what
/// the model is told, and this adds the guidance to the goal under a label that names the source and
/// the artifact's digest. Pure, like every policy.
pub struct GuidedContext {
    pub inner: Arc<dyn EscalationContextPolicy>,
    /// `(source key, path, sha256, text)` for each active source with guidance.
    pub guidance: Vec<(String, String, String, String)>,
}

impl GuidedContext {
    pub const ID: &'static str = "guided-v1";
}

impl EscalationContextPolicy for GuidedContext {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn build(&self, state: &WorkState<'_>, trajectory: &WorkTrajectory<'_>) -> EscalationContext {
        let mut context = self.inner.build(state, trajectory);
        for (source, path, sha256, text) in &self.guidance {
            context.goal.push_str(&format!(
                "\nGuidance for capability source {source} ({path}, {sha256}); instructions, not observations:\n{text}"
            ));
        }
        context
    }
}

/// One controlled task: the goal, the project it starts from, and the host's own expectations,
/// which are never shown to a strategy or a model (a strategy sees the goal and observations only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFixture {
    pub id: String,
    pub version: u32,
    pub goal: String,
    /// The project the run starts from (observe and isolated): project-relative path and content.
    pub files: Vec<(String, String)>,
    /// What the host requires the project files to be afterwards.
    pub expected_files: Vec<(String, String)>,
    /// Model replies, in order, for `ModelSpec::Recorded`.
    #[serde(default)]
    pub model_replies: Vec<String>,
    /// Recorded capability results, for replay.
    #[serde(default)]
    pub recording: Option<Recording>,
}

impl TaskFixture {
    /// `sha256:` of the whole fixture: what a report says the run was given.
    pub fn digest(&self) -> String {
        digest(
            serde_json::to_string(self)
                .expect("a fixture always serializes")
                .as_bytes(),
        )
    }

    /// The most bytes a fixture's project may hold: a controlled task is small.
    pub const MAX_PROJECT_BYTES: usize = 1024 * 1024;
}

/// Capability results recorded from an earlier execution, served by replay instead of executing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recording {
    pub schema: String,
    pub entries: Vec<RecordedExecution>,
}

pub const RECORDING_SCHEMA: &str = "chip.experiment-recording.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedExecution {
    pub capability: String,
    pub inputs: BTreeMap<String, serde_json::Value>,
    pub succeeded: bool,
    pub output: String,
}

impl Recording {
    pub fn from_json(text: &str) -> Result<Self, String> {
        let recording: Recording = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if recording.schema != RECORDING_SCHEMA {
            return Err(format!(
                "recording schema is `{}`; expected `{RECORDING_SCHEMA}`",
                recording.schema
            ));
        }
        Ok(recording)
    }

    pub fn capabilities(&self) -> std::collections::BTreeSet<&str> {
        self.entries.iter().map(|e| e.capability.as_str()).collect()
    }
}

/// The inputs of a request as recorded: the same typed values, as JSON.
pub fn recorded_inputs(
    inputs: &BTreeMap<String, InputValue>,
) -> BTreeMap<String, serde_json::Value> {
    inputs
        .iter()
        .map(|(k, v)| {
            let v = match v {
                InputValue::Text(t) => serde_json::Value::from(t.clone()),
                InputValue::Integer(i) => serde_json::Value::from(*i),
                InputValue::Bool(b) => serde_json::Value::from(*b),
            };
            (k.clone(), v)
        })
        .collect()
}

/// Replay's backend: declares what the fixtures declare (filtered to the allowlist) and answers
/// every request from the recording. It executes nothing; a request that is not recorded exactly
/// fails, and nothing is substituted for it.
pub(crate) struct Replayed {
    pub descriptors: Vec<CapabilityDescriptor>,
    pub recording: Recording,
}

#[async_trait::async_trait]
impl CapabilityProvider for Replayed {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(self.descriptors.clone())
    }

    async fn availability(&self, id: &CapabilityId) -> CapabilityAvailability {
        if self.descriptors.iter().any(|d| &d.id == id) {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable("not declared in this replay".into())
        }
    }
}

#[async_trait::async_trait]
impl Executor for Replayed {
    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let inputs = recorded_inputs(&request.inputs);
        let found = self
            .recording
            .entries
            .iter()
            .find(|e| e.capability == request.intent && e.inputs == inputs);
        match found {
            Some(e) if e.succeeded => Ok(ExecutionResult::success(request.id, e.output.clone())),
            Some(e) => Ok(ExecutionResult::failure(request.id, e.output.clone())),
            None => Err(ExecutionError::ExecutionFailed(format!(
                "{} with these inputs is not in the recording; replay performs no execution",
                request.intent
            ))),
        }
    }
}
