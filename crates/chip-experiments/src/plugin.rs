//! The two extension contracts: behavior strategies and capability fixtures.
//!
//! Both are thin views of contracts Chip already has. A strategy is a [`LocalWorkPolicy`] (next-step
//! proposals: planning and capability selection) and an [`EscalationContextPolicy`] (context
//! selection). A capability fixture contributes a [`CapabilityBackend`] to the work's
//! [`CapabilitySet`](chip_core::CapabilitySet). Neither gets anything else: no executor, no
//! authorization, no completion predicate, no evidence store, no model boundary. What a strategy
//! proposes, Chip validates, authorizes and executes exactly as in production, and a strategy's
//! `Complete` is a claim the runtime and the evaluator check against observations.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use chip_core::{
    CapabilityAvailability, CapabilityBackend, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityProvider, EscalationContextPolicy, ExecutionError, ExecutionRequest, ExecutionResult,
    Executor, FullEscalationContext, InputValue, LocalWorkPolicy,
};
use serde::{Deserialize, Serialize};

use crate::definition::digest;

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

/// Declares and executes only the allowlisted capabilities of `inner`. Chip already refuses a
/// request for an undeclared capability; this keeps an undeclared one from ever reaching the
/// backend even if something else went wrong. It filters; it is not a second registry.
pub(crate) struct Allowlisted {
    pub inner: Arc<dyn CapabilityBackend>,
    pub allowed: Vec<String>,
}

impl Allowlisted {
    fn allows(&self, id: &str) -> bool {
        self.allowed.iter().any(|a| a == id)
    }
}

#[async_trait::async_trait]
impl CapabilityProvider for Allowlisted {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(self
            .inner
            .capabilities()
            .await?
            .into_iter()
            .filter(|d| self.allows(d.id.as_str()))
            .collect())
    }

    async fn availability(&self, id: &CapabilityId) -> CapabilityAvailability {
        if self.allows(id.as_str()) {
            self.inner.availability(id).await
        } else {
            CapabilityAvailability::Unavailable("not on the experiment's allowlist".into())
        }
    }

    async fn validate_inputs(
        &self,
        id: &CapabilityId,
        inputs: &BTreeMap<String, InputValue>,
    ) -> Result<(), CapabilityError> {
        if !self.allows(id.as_str()) {
            return Err(CapabilityError::Unavailable(format!(
                "{id} is not on the experiment's allowlist"
            )));
        }
        self.inner.validate_inputs(id, inputs).await
    }
}

#[async_trait::async_trait]
impl Executor for Allowlisted {
    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        if !self.allows(&request.intent) {
            return Err(ExecutionError::InvalidRequest(format!(
                "{} is not on the experiment's allowlist",
                request.intent
            )));
        }
        self.inner.execute(request).await
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
