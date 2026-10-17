//! The experiment definition: an immutable, versioned description of one comparison.
//!
//! A definition is data. It names strategies, tasks, capabilities and criteria by identifier and
//! version; the [`Registry`](crate::Registry) resolves those names to implementations the host
//! constructed explicitly. Parsing checks the document's shape; [`Registry::resolve`] checks that
//! every name exists, every version matches and the mode can actually be honoured. Nothing invalid
//! is ever replaced by a default.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The schema every definition document declares.
pub const DEFINITION_SCHEMA: &str = "chip.experiment.v1";

/// The most turns and executions one run may be given, and the longest whole-run deadline. Wider
/// bounds would not be "controlled" any more.
pub const MAX_TURNS: usize = 64;
pub const MAX_EXECUTIONS: usize = 64;
pub const MAX_DEADLINE_MS: u64 = 600_000;

/// How a definition's runs are executed. Only modes with implemented semantics exist; `shadow` and
/// `canary` are reserved names that parsing refuses explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Run the baseline only (a strategy the host registered as its production behavior), in a
    /// disposable isolated environment, and record what it did. No candidate runs; nothing is
    /// compared and no decision is changed.
    Observe,
    /// Run baseline and candidate against recorded capability results and recorded model replies.
    /// Nothing executes: a request that is not in the recording fails. Replay reproduces recorded
    /// inputs, not external effects, timing or a live model.
    Replay,
    /// Run baseline and candidate, each in its own disposable environment, with only capabilities
    /// whose effects the executor confines to that environment.
    Isolated,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Observe => "observe",
            Mode::Replay => "replay",
            Mode::Isolated => "isolated",
        }
    }

    fn parse(text: &str) -> Result<Self, DefinitionError> {
        match text {
            "observe" => Ok(Mode::Observe),
            "replay" => Ok(Mode::Replay),
            "isolated" => Ok(Mode::Isolated),
            "shadow" | "canary" => Err(DefinitionError::UnsupportedMode(format!(
                "`{text}` is reserved for future work and has no implemented semantics"
            ))),
            other => Err(DefinitionError::UnsupportedMode(format!(
                "`{other}` is not a mode (observe, replay, isolated)"
            ))),
        }
    }
}

/// A name and the exact version it must resolve to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Versioned {
    pub id: String,
    pub version: u32,
}

impl fmt::Display for Versioned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.id, self.version)
    }
}

/// The model a run may consult when its strategy escalates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ModelSpec {
    /// No model: a run whose strategy escalates fails with a typed provider error, and nothing is
    /// sent anywhere.
    None,
    /// The task fixture's recorded replies, in order. No network, no credential.
    Recorded,
}

impl ModelSpec {
    pub fn name(&self) -> &'static str {
        match self {
            ModelSpec::None => "none",
            ModelSpec::Recorded => "recorded",
        }
    }
}

/// Bounds for every run of the experiment. The deadline is whole-run wall-clock, enforced through
/// Chip's own work control (checked before every model call and every execution).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunLimits {
    pub max_turns: usize,
    pub max_executions: usize,
    pub deadline_ms: u64,
}

/// Measurements a definition requires in its report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceField {
    Outcome,
    Criteria,
    Decisions,
    Executions,
    ModelCalls,
    Duration,
    Tokens,
    Cost,
}

impl EvidenceField {
    pub fn name(self) -> &'static str {
        match self {
            EvidenceField::Outcome => "outcome",
            EvidenceField::Criteria => "criteria",
            EvidenceField::Decisions => "decisions",
            EvidenceField::Executions => "executions",
            EvidenceField::ModelCalls => "model_calls",
            EvidenceField::Duration => "duration",
            EvidenceField::Tokens => "tokens",
            EvidenceField::Cost => "cost",
        }
    }
}

/// The document as written. Field names are the public format (`chip.experiment.v1`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: String,
    id: String,
    version: u32,
    #[serde(default)]
    description: String,
    mode: String,
    baseline: Versioned,
    #[serde(default)]
    candidate: Option<Versioned>,
    tasks: Vec<Versioned>,
    capabilities: Vec<String>,
    limits: RunLimits,
    criteria: Vec<String>,
    model: ModelSpec,
    evidence: Vec<EvidenceField>,
}

/// A parsed definition. Its shape is valid; whether its names resolve is the registry's question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExperimentDefinition {
    pub id: String,
    pub version: u32,
    pub description: String,
    pub mode: Mode,
    pub baseline: Versioned,
    pub candidate: Option<Versioned>,
    pub tasks: Vec<Versioned>,
    /// The only capabilities any run may be offered. Everything else is undeclared to the work.
    pub capabilities: Vec<String>,
    pub limits: RunLimits,
    /// Host-defined acceptance criteria, by registry id, applied identically to every run.
    pub criteria: Vec<String>,
    pub model: ModelSpec,
    pub evidence: Vec<EvidenceField>,
}

/// Why a definition is not accepted. Every variant is a refusal; none is recovered from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefinitionError {
    Malformed(String),
    UnsupportedMode(String),
    UnknownStrategy(String),
    UnknownTask(String),
    UnknownCapability(String),
    UnknownCriterion(String),
    VersionMismatch(String),
    InvalidLimits(String),
    MissingCriteria(String),
    /// The mode's guarantee cannot be honoured with what the definition asks for (an isolated run
    /// offered a capability whose effects nothing confines, a replay fixture with no recording).
    IsolationUnavailable(String),
    /// A task fixture cannot satisfy the experiment's declared input contract.
    FixtureMismatch(String),
    UnavailableEvidence(String),
}

impl DefinitionError {
    /// A stable code for machine-readable output.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Malformed(_) => "malformed",
            Self::UnsupportedMode(_) => "unsupported_mode",
            Self::UnknownStrategy(_) => "unknown_strategy",
            Self::UnknownTask(_) => "unknown_task",
            Self::UnknownCapability(_) => "unknown_capability",
            Self::UnknownCriterion(_) => "unknown_criterion",
            Self::VersionMismatch(_) => "version_mismatch",
            Self::InvalidLimits(_) => "invalid_limits",
            Self::MissingCriteria(_) => "missing_criteria",
            Self::IsolationUnavailable(_) => "isolation_unavailable",
            Self::FixtureMismatch(_) => "fixture_mismatch",
            Self::UnavailableEvidence(_) => "unavailable_evidence",
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::Malformed(d)
            | Self::UnsupportedMode(d)
            | Self::UnknownStrategy(d)
            | Self::UnknownTask(d)
            | Self::UnknownCapability(d)
            | Self::UnknownCriterion(d)
            | Self::VersionMismatch(d)
            | Self::InvalidLimits(d)
            | Self::MissingCriteria(d)
            | Self::IsolationUnavailable(d)
            | Self::FixtureMismatch(d)
            | Self::UnavailableEvidence(d) => d,
        }
    }
}

impl fmt::Display for DefinitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.detail())
    }
}

impl std::error::Error for DefinitionError {}

impl ExperimentDefinition {
    /// Parses a `chip.experiment.v1` document and checks everything that does not need the
    /// registry: the schema, the mode, the limits, and that criteria and tasks are present.
    pub fn from_json(text: &str) -> Result<Self, DefinitionError> {
        let doc: Document =
            serde_json::from_str(text).map_err(|e| DefinitionError::Malformed(e.to_string()))?;
        if doc.schema != DEFINITION_SCHEMA {
            return Err(DefinitionError::Malformed(format!(
                "schema is `{}`; expected `{DEFINITION_SCHEMA}`",
                doc.schema
            )));
        }
        let definition = Self {
            mode: Mode::parse(&doc.mode)?,
            id: doc.id,
            version: doc.version,
            description: doc.description,
            baseline: doc.baseline,
            candidate: doc.candidate,
            tasks: doc.tasks,
            capabilities: doc.capabilities,
            limits: doc.limits,
            criteria: doc.criteria,
            model: doc.model,
            evidence: doc.evidence,
        };
        definition.check_shape()?;
        Ok(definition)
    }

    /// The canonical document (the format [`from_json`](Self::from_json) reads).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(Document {
            schema: DEFINITION_SCHEMA.into(),
            id: self.id.clone(),
            version: self.version,
            description: self.description.clone(),
            mode: self.mode.name().into(),
            baseline: self.baseline.clone(),
            candidate: self.candidate.clone(),
            tasks: self.tasks.clone(),
            capabilities: self.capabilities.clone(),
            limits: self.limits,
            criteria: self.criteria.clone(),
            model: self.model.clone(),
            evidence: self.evidence.clone(),
        })
        .expect("a definition always serializes")
    }

    /// `sha256:` of the canonical document: what a report says it ran.
    pub fn digest(&self) -> String {
        digest(self.to_json().to_string().as_bytes())
    }

    /// The checks that need nothing but the definition itself.
    pub fn check_shape(&self) -> Result<(), DefinitionError> {
        let malformed = |what: &str| Err(DefinitionError::Malformed(what.to_string()));
        if self.id.trim().is_empty() || self.id.len() > 100 {
            return malformed("id must be 1 to 100 characters");
        }
        if self.version == 0 {
            return malformed("version starts at 1");
        }
        if self.tasks.is_empty() {
            return malformed("at least one task is required");
        }
        if self.capabilities.is_empty() {
            return malformed("the capability allowlist is empty; a run could do nothing");
        }
        let mut seen = std::collections::BTreeSet::new();
        for task in &self.tasks {
            if !seen.insert(&task.id) {
                return malformed(&format!("task {} is listed twice", task.id));
            }
        }
        let l = self.limits;
        if !(1..=MAX_TURNS).contains(&l.max_turns) {
            return Err(DefinitionError::InvalidLimits(format!(
                "max_turns must be 1 to {MAX_TURNS}"
            )));
        }
        if !(1..=MAX_EXECUTIONS).contains(&l.max_executions) {
            return Err(DefinitionError::InvalidLimits(format!(
                "max_executions must be 1 to {MAX_EXECUTIONS}"
            )));
        }
        if !(1..=MAX_DEADLINE_MS).contains(&l.deadline_ms) {
            return Err(DefinitionError::InvalidLimits(format!(
                "deadline_ms must be 1 to {MAX_DEADLINE_MS}"
            )));
        }
        if self.criteria.is_empty() {
            return Err(DefinitionError::MissingCriteria(
                "no acceptance criterion: nothing could establish an outcome".into(),
            ));
        }
        match (self.mode, &self.candidate) {
            (Mode::Observe, Some(_)) => {
                return Err(DefinitionError::UnsupportedMode(
                    "observe runs the baseline only; a candidate is never executed in observe mode"
                        .into(),
                ));
            }
            (Mode::Replay | Mode::Isolated, None) => {
                return Err(DefinitionError::Malformed(format!(
                    "{} mode compares a candidate with the baseline; no candidate is named",
                    self.mode.name()
                )));
            }
            (Mode::Replay | Mode::Isolated, Some(c)) if *c == self.baseline => {
                return malformed("the candidate is the baseline; there is nothing to compare");
            }
            _ => {}
        }
        if self.evidence.contains(&EvidenceField::Cost) {
            return Err(DefinitionError::UnavailableEvidence(
                "cost: no provider in this workspace reports cost, and it is never estimated"
                    .into(),
            ));
        }
        for required in [EvidenceField::Outcome, EvidenceField::Criteria] {
            if !self.evidence.contains(&required) {
                return Err(DefinitionError::Malformed(format!(
                    "evidence must include `{}`",
                    required.name()
                )));
            }
        }
        Ok(())
    }
}

/// `sha256:<hex>` of some bytes.
pub fn digest(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
