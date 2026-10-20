use crate::authorization::{AuthorizationRequest, Subject};
use crate::{FeltDb, FlowError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

pub const DEFAULT_DECISION_COLLECTION: &str = "semantic_decisions";

#[cfg(target_arch = "wasm32")]
fn semantic_now_ms() -> u128 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
fn semantic_now_ms() -> u128 {
    crate::now_ms()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Active,
    Superseded,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Binary,
    Choice,
    Score,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BinaryDecisionResult {
    pub kind: DecisionKind,
    pub decision: bool,
    pub option_mass: f64,
    #[serde(default)]
    pub supporting_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WeightedOption {
    pub value: String,
    pub probability: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChoiceDecisionResult {
    pub kind: DecisionKind,
    pub selected: String,
    pub options: Vec<WeightedOption>,
    pub option_mass: f64,
    #[serde(default)]
    pub supporting_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoreDecisionResult {
    pub kind: DecisionKind,
    pub selected: String,
    pub levels: Vec<WeightedOption>,
    pub option_mass: f64,
    #[serde(default)]
    pub supporting_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionResult {
    Binary {
        decision: bool,
        option_mass: f64,
        #[serde(default)]
        supporting_fields: Vec<String>,
    },
    Choice {
        selected: String,
        options: Vec<WeightedOption>,
        option_mass: f64,
        #[serde(default)]
        supporting_fields: Vec<String>,
    },
    Score {
        selected: String,
        levels: Vec<WeightedOption>,
        option_mass: f64,
        #[serde(default)]
        supporting_fields: Vec<String>,
    },
}

impl DecisionResult {
    pub fn kind(&self) -> DecisionKind {
        match self {
            Self::Binary { .. } => DecisionKind::Binary,
            Self::Choice { .. } => DecisionKind::Choice,
            Self::Score { .. } => DecisionKind::Score,
        }
    }

    pub fn option_mass(&self) -> f64 {
        match self {
            Self::Binary { option_mass, .. }
            | Self::Choice { option_mass, .. }
            | Self::Score { option_mass, .. } => *option_mass,
        }
    }

    pub fn supporting_fields(&self) -> &[String] {
        match self {
            Self::Binary {
                supporting_fields, ..
            }
            | Self::Choice {
                supporting_fields, ..
            }
            | Self::Score {
                supporting_fields, ..
            } => supporting_fields.as_slice(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionDefinition {
    Binary {
        predicate: String,
    },
    Choice {
        question: String,
        options: Vec<String>,
    },
    Score {
        question: String,
        levels: Vec<String>,
    },
}

impl DecisionDefinition {
    pub fn kind(&self) -> DecisionKind {
        match self {
            Self::Binary { .. } => DecisionKind::Binary,
            Self::Choice { .. } => DecisionKind::Choice,
            Self::Score { .. } => DecisionKind::Score,
        }
    }

    pub fn prompt(&self) -> &str {
        match self {
            Self::Binary { predicate } => predicate,
            Self::Choice { question, .. } | Self::Score { question, .. } => question,
        }
    }

    pub fn schema_values(&self) -> &[String] {
        match self {
            Self::Binary { .. } => &[],
            Self::Choice { options, .. } => options,
            Self::Score { levels, .. } => levels,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionRequest {
    Binary {
        predicate: String,
        context: Value,
    },
    Choice {
        question: String,
        options: Vec<String>,
        context: Value,
    },
    Score {
        question: String,
        levels: Vec<String>,
        context: Value,
    },
}

impl DecisionRequest {
    pub fn definition(&self) -> DecisionDefinition {
        match self {
            Self::Binary { predicate, .. } => DecisionDefinition::Binary {
                predicate: predicate.clone(),
            },
            Self::Choice {
                question, options, ..
            } => DecisionDefinition::Choice {
                question: question.clone(),
                options: options.clone(),
            },
            Self::Score {
                question, levels, ..
            } => DecisionDefinition::Score {
                question: question.clone(),
                levels: levels.clone(),
            },
        }
    }

    pub fn context(&self) -> &Value {
        match self {
            Self::Binary { context, .. }
            | Self::Choice { context, .. }
            | Self::Score { context, .. } => context,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionRecordRef {
    pub collection: String,
    pub record_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionPrincipal {
    pub subject: Subject,
    pub tenant_id: String,
    pub application_id: String,
    #[serde(default = "default_read_capability")]
    pub capability: String,
}

fn default_read_capability() -> String {
    "state:read".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionAuthorizationDecision {
    pub allowed: bool,
    pub decision_id: Option<String>,
}

pub trait DecisionAuthorizationGate {
    fn check(
        &self,
        request: &AuthorizationRequest,
    ) -> Result<DecisionAuthorizationDecision, DecisionError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DecisionAuthorizationConfig {
    pub principal: Option<DecisionPrincipal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionRuntimeMetadata {
    pub runtime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_method: Option<DecisionExecutionMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_schema_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DecisionExecutionMethod {
    Logit,
    Structured,
    Generated,
    Head,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionRuntimeCapabilities {
    pub binary: bool,
    pub choice: bool,
    pub score: bool,
    pub batch: bool,
    pub probability_distribution: bool,
    pub execution_methods: BTreeSet<DecisionExecutionMethod>,
}

impl Default for DecisionRuntimeCapabilities {
    fn default() -> Self {
        Self {
            binary: true,
            choice: true,
            score: true,
            batch: true,
            probability_distribution: true,
            execution_methods: BTreeSet::new(),
        }
    }
}

pub trait DecisionRuntime {
    fn metadata(&self) -> DecisionRuntimeMetadata;
    fn decide(&self, request: &DecisionRequest) -> Result<DecisionResult, DecisionError>;

    fn capabilities(&self) -> DecisionRuntimeCapabilities {
        DecisionRuntimeCapabilities::default()
    }

    fn execute(&self, request: &DecisionRequest) -> Result<DecisionResult, DecisionError> {
        self.decide(request)
    }

    fn execute_batch(
        &self,
        requests: &[DecisionRequest],
    ) -> Result<Vec<DecisionResult>, DecisionError> {
        requests
            .iter()
            .map(|request| self.execute(request))
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionProvenance {
    pub runtime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_method: Option<DecisionExecutionMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_schema_revision: Option<String>,
    pub context_hash: String,
    pub relevant_state_version: String,
    pub cache_key: String,
    pub source: String,
    pub evaluated_at: u128,
    pub served_at: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionEvidenceField {
    pub field: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionEvidence {
    pub decision_id: String,
    pub kind: DecisionKind,
    pub input: DecisionDefinition,
    pub result: DecisionResult,
    pub supporting_context: Vec<DecisionEvidenceField>,
    pub runtime: DecisionRuntimeMetadata,
    pub state: DecisionStateReference,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionStateReference {
    pub collection: String,
    pub record_id: String,
    pub record_version: Option<u64>,
    pub reference: String,
    pub relevant_state_version: String,
    pub withheld_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionEvaluation {
    pub result: DecisionResult,
    pub cached: bool,
    pub evidence: Vec<DecisionEvidence>,
    pub provenance: DecisionProvenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionJudgmentRecord {
    pub id: String,
    pub lineage_key: String,
    pub kind: DecisionKind,
    pub collection: String,
    pub record_id: String,
    pub input: DecisionDefinition,
    pub schema_version: String,
    pub relevant_state_version: String,
    pub runtime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_method: Option<DecisionExecutionMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_schema_revision: Option<String>,
    pub result: DecisionResult,
    pub option_mass: f64,
    pub context_fields: Vec<String>,
    pub supporting_fields: Vec<String>,
    pub status: DecisionStatus,
    pub state: DecisionStateReference,
    pub evidence: DecisionEvidence,
    pub evaluated_at: u128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_at: Option<u128>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionOptions {
    #[serde(default)]
    pub fields: Option<Vec<String>>,
    #[serde(default)]
    pub application_id: Option<String>,
    #[serde(default)]
    pub decision_collection: Option<String>,
    #[serde(default)]
    pub schema_version: Option<String>,
}

impl Default for DecisionOptions {
    fn default() -> Self {
        Self {
            fields: None,
            application_id: None,
            decision_collection: None,
            schema_version: Some("semantic-decision-v1".into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DecisionTransportAuthorization {
    #[serde(default)]
    pub allowed_fields: Vec<String>,
    #[serde(default)]
    pub principal: Option<DecisionPrincipal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionTransportRuntime {
    Deterministic {
        metadata: DecisionRuntimeMetadata,
        responses: BTreeMap<String, DecisionResult>,
    },
    Recording {
        metadata: DecisionRuntimeMetadata,
        result: DecisionResult,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionTransportExecutionRequest {
    #[serde(default)]
    pub target: Option<DecisionRecordRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
    pub definition: DecisionDefinition,
    #[serde(default)]
    pub options: DecisionOptions,
    #[serde(default)]
    pub authorization: Option<DecisionTransportAuthorization>,
    pub runtime: DecisionTransportRuntime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionTransportExecutionResponse {
    pub evaluation: DecisionEvaluation,
    #[serde(default)]
    pub runtime_requests: Vec<DecisionRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionBatchDecision {
    pub id: String,
    pub definition: DecisionDefinition,
    #[serde(default = "empty_object")]
    pub context: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionBatch {
    #[serde(default = "empty_object")]
    pub context: Value,
    pub decisions: Vec<DecisionBatchDecision>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionBatchEvaluation {
    pub id: String,
    pub result: DecisionResult,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChoicePermutationObservation {
    pub order: Vec<String>,
    pub selected: String,
    pub distribution: Vec<WeightedOption>,
    pub rank: usize,
    pub probability: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChoiceExpansionObservation {
    pub baseline: ChoicePermutationObservation,
    pub expanded: ChoicePermutationObservation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChoiceDiagnosticsReport {
    pub permutations: Vec<ChoicePermutationObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expansion: Option<ChoiceExpansionObservation>,
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionContextPacket {
    pub collection: String,
    pub record_id: String,
    pub record_version: Option<u64>,
    pub reference: String,
    pub fields: Vec<String>,
    pub values: BTreeMap<String, Value>,
    pub withheld_fields: Vec<String>,
    pub relevant_state_version: String,
    pub context_hash: String,
    pub authorization_decisions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionBenchmarkCase {
    pub collection: String,
    pub record_id: String,
    pub input: DecisionDefinition,
    pub mutate: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionBenchmarkReport {
    pub binary: usize,
    pub choice: usize,
    pub score: usize,
    pub runtime_invocations_first_pass: usize,
    pub runtime_invocations_second_pass: usize,
    pub cache_hits_second_pass: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionError {
    pub code: String,
    pub message: String,
}

impl DecisionError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DecisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for DecisionError {}

pub(crate) struct TransportAuthorizationGate {
    allowed_fields: BTreeSet<String>,
}

impl TransportAuthorizationGate {
    pub(crate) fn new(allowed_fields: &[String]) -> Self {
        Self {
            allowed_fields: allowed_fields.iter().cloned().collect(),
        }
    }
}

impl DecisionAuthorizationGate for TransportAuthorizationGate {
    fn check(
        &self,
        request: &AuthorizationRequest,
    ) -> Result<DecisionAuthorizationDecision, DecisionError> {
        let field = request
            .resource
            .uri
            .split('/')
            .next_back()
            .unwrap_or_default()
            .to_string();
        Ok(DecisionAuthorizationDecision {
            allowed: self.allowed_fields.contains(&field),
            decision_id: Some(digest(&[
                "semantic-decision-authorization-v1".into(),
                request.application_id.clone(),
                request.resource.uri.clone(),
                field,
            ])),
        })
    }
}

struct RecordingDecisionRuntime {
    metadata: DecisionRuntimeMetadata,
    result: DecisionResult,
    requests: Mutex<Vec<DecisionRequest>>,
}

impl RecordingDecisionRuntime {
    fn new(metadata: DecisionRuntimeMetadata, result: DecisionResult) -> Self {
        Self {
            metadata,
            result,
            requests: Mutex::new(Vec::new()),
        }
    }

    fn recorded_requests(&self) -> Vec<DecisionRequest> {
        self.requests
            .lock()
            .expect("recording runtime mutex")
            .clone()
    }
}

impl DecisionRuntime for RecordingDecisionRuntime {
    fn metadata(&self) -> DecisionRuntimeMetadata {
        self.metadata.clone()
    }

    fn decide(&self, request: &DecisionRequest) -> Result<DecisionResult, DecisionError> {
        self.requests
            .lock()
            .expect("recording runtime mutex")
            .push(request.clone());
        Ok(self.result.clone())
    }
}

enum TransportRuntimeHandle {
    Deterministic(DeterministicDecisionRuntime),
    Recording(RecordingDecisionRuntime),
}

impl TransportRuntimeHandle {
    fn new(runtime: &DecisionTransportRuntime) -> Self {
        match runtime {
            DecisionTransportRuntime::Deterministic {
                metadata,
                responses,
            } => Self::Deterministic(DeterministicDecisionRuntime::new(
                metadata.clone(),
                responses.clone(),
            )),
            DecisionTransportRuntime::Recording { metadata, result } => Self::Recording(
                RecordingDecisionRuntime::new(metadata.clone(), result.clone()),
            ),
        }
    }

    fn recorded_requests(&self) -> Vec<DecisionRequest> {
        match self {
            Self::Deterministic(_) => Vec::new(),
            Self::Recording(runtime) => runtime.recorded_requests(),
        }
    }
}

impl DecisionRuntime for TransportRuntimeHandle {
    fn metadata(&self) -> DecisionRuntimeMetadata {
        match self {
            Self::Deterministic(runtime) => runtime.metadata(),
            Self::Recording(runtime) => runtime.metadata(),
        }
    }

    fn decide(&self, request: &DecisionRequest) -> Result<DecisionResult, DecisionError> {
        match self {
            Self::Deterministic(runtime) => runtime.execute(request),
            Self::Recording(runtime) => runtime.execute(request),
        }
    }
}

fn digest(parts: &[String]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0x1f]);
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn canonical_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn require_object(value: &Value, message: &str) -> Result<Map<String, Value>, DecisionError> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| DecisionError::new("DECISION_INVALID_RESULT", message))
}

fn canonical_values(values: &[String]) -> Result<Vec<String>, DecisionError> {
    if values.is_empty() {
        return Err(DecisionError::new(
            "DECISION_INVALID_OPTION",
            "closed-set decisions must declare at least one value",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        let canonical = canonical_text(value);
        if canonical.is_empty() {
            return Err(DecisionError::new(
                "DECISION_INVALID_OPTION",
                "closed-set decisions cannot declare an empty value",
            ));
        }
        if !seen.insert(canonical.clone()) {
            return Err(DecisionError::new(
                "DECISION_INVALID_OPTION",
                format!("closed-set decision declared \"{canonical}\" more than once"),
            ));
        }
        normalized.push(canonical);
    }
    Ok(normalized)
}

pub fn validate_decision_definition(
    input: &DecisionDefinition,
) -> Result<DecisionDefinition, DecisionError> {
    Ok(match input {
        DecisionDefinition::Binary { predicate } => {
            let predicate = canonical_text(predicate);
            if predicate.is_empty() {
                return Err(DecisionError::new(
                    "DECISION_INVALID_RESULT",
                    "binary decisions require a non-empty predicate",
                ));
            }
            DecisionDefinition::Binary { predicate }
        }
        DecisionDefinition::Choice { question, options } => DecisionDefinition::Choice {
            question: {
                let question = canonical_text(question);
                if question.is_empty() {
                    return Err(DecisionError::new(
                        "DECISION_INVALID_RESULT",
                        "choice decisions require a non-empty question",
                    ));
                }
                question
            },
            options: canonical_values(options)?,
        },
        DecisionDefinition::Score { question, levels } => DecisionDefinition::Score {
            question: {
                let question = canonical_text(question);
                if question.is_empty() {
                    return Err(DecisionError::new(
                        "DECISION_INVALID_RESULT",
                        "score decisions require a non-empty question",
                    ));
                }
                question
            },
            levels: canonical_values(levels)?,
        },
    })
}

fn distribution_by_values(
    values: &[String],
    observed: &[WeightedOption],
) -> Result<Vec<WeightedOption>, DecisionError> {
    let mut probabilities = BTreeMap::new();
    let mut total = 0.0;
    for entry in observed {
        if !values.contains(&entry.value) {
            return Err(DecisionError::new(
                "DECISION_INVALID_OPTION",
                format!("runtime returned undeclared option \"{}\"", entry.value),
            ));
        }
        if entry.probability.is_nan()
            || !entry.probability.is_finite()
            || entry.probability < 0.0
            || entry.probability > 1.0
        {
            return Err(DecisionError::new(
                "DECISION_INVALID_DISTRIBUTION",
                format!(
                    "runtime returned an invalid probability for \"{}\"",
                    entry.value
                ),
            ));
        }
        if probabilities
            .insert(entry.value.clone(), entry.probability)
            .is_some()
        {
            return Err(DecisionError::new(
                "DECISION_INVALID_DISTRIBUTION",
                format!(
                    "runtime returned duplicate probability for \"{}\"",
                    entry.value
                ),
            ));
        }
        total += entry.probability;
    }
    if probabilities.len() != values.len() {
        return Err(DecisionError::new(
            "DECISION_INVALID_DISTRIBUTION",
            "runtime did not return probabilities for every declared value",
        ));
    }
    if (total - 1.0).abs() > 1e-6 {
        return Err(DecisionError::new(
            "DECISION_INVALID_DISTRIBUTION",
            "runtime returned a distribution that does not sum to 1",
        ));
    }
    Ok(values
        .iter()
        .map(|value| WeightedOption {
            value: value.clone(),
            probability: *probabilities.get(value).unwrap_or(&0.0),
        })
        .collect())
}

pub fn validate_decision_result(
    request: &DecisionRequest,
    result: &DecisionResult,
) -> Result<DecisionResult, DecisionError> {
    let supporting_fields = result.supporting_fields().to_vec();
    let option_mass = result.option_mass();
    if option_mass.is_nan() || !option_mass.is_finite() || !(0.0..=1.0).contains(&option_mass) {
        return Err(DecisionError::new(
            "DECISION_INVALID_RESULT",
            "runtime returned an optionMass outside [0, 1]",
        ));
    }

    Ok(match (request, result) {
        (DecisionRequest::Binary { .. }, DecisionResult::Binary { decision, .. }) => {
            DecisionResult::Binary {
                decision: *decision,
                option_mass,
                supporting_fields,
            }
        }
        (
            DecisionRequest::Choice { options, .. },
            DecisionResult::Choice {
                selected,
                options: returned,
                ..
            },
        ) => {
            if !options.contains(selected) {
                return Err(DecisionError::new(
                    "DECISION_INVALID_OPTION",
                    format!("runtime selected undeclared option \"{selected}\""),
                ));
            }
            DecisionResult::Choice {
                selected: selected.clone(),
                options: distribution_by_values(options, returned)?,
                option_mass,
                supporting_fields,
            }
        }
        (
            DecisionRequest::Score { levels, .. },
            DecisionResult::Score {
                selected,
                levels: returned,
                ..
            },
        ) => {
            if !levels.contains(selected) {
                return Err(DecisionError::new(
                    "DECISION_INVALID_OPTION",
                    format!("runtime selected undeclared level \"{selected}\""),
                ));
            }
            DecisionResult::Score {
                selected: selected.clone(),
                levels: distribution_by_values(levels, returned)?,
                option_mass,
                supporting_fields,
            }
        }
        _ => {
            return Err(DecisionError::new(
                "DECISION_INVALID_RESULT",
                "runtime returned a result for the wrong decision kind",
            ))
        }
    })
}

fn selected_fields(record: &Map<String, Value>, fields: &Option<Vec<String>>) -> Vec<String> {
    let mut readable: Vec<String> = record
        .keys()
        .filter(|key| !matches!(key.as_str(), "id" | "__version"))
        .cloned()
        .collect();
    readable.sort();
    match fields {
        None => readable,
        Some(fields) => {
            let selected: BTreeSet<_> = fields.iter().cloned().collect();
            readable
                .into_iter()
                .filter(|field| selected.contains(field))
                .collect()
        }
    }
}

fn state_reference(application_id: Option<&str>, collection: &str, record_id: &str) -> String {
    match application_id {
        Some(application_id) if !application_id.is_empty() => {
            format!("flow://{application_id}/{collection}/{record_id}")
        }
        _ => format!("{collection}:{record_id}"),
    }
}

pub fn authorize_context(
    collection: &str,
    record_id: &str,
    record: &Value,
    options: &DecisionOptions,
    authorization: Option<(&dyn DecisionAuthorizationGate, &DecisionAuthorizationConfig)>,
) -> Result<DecisionContextPacket, DecisionError> {
    let object = record.as_object().ok_or_else(|| {
        DecisionError::new(
            "DECISION_INVALID_RESULT",
            "semantic decisions require object-shaped records",
        )
    })?;
    let record_version = object.get("__version").and_then(Value::as_u64);
    let selected = selected_fields(object, &options.fields);
    let deselected: BTreeSet<_> = object
        .keys()
        .filter(|field| !matches!(field.as_str(), "id" | "__version"))
        .filter(|field| !selected.contains(field))
        .cloned()
        .collect();
    let application_id = options.application_id.as_deref().or_else(|| {
        authorization
            .and_then(|(_, config)| config.principal.as_ref())
            .map(|principal| principal.application_id.as_str())
    });
    let mut authorized = Vec::new();
    let mut withheld: BTreeSet<String> = deselected;
    let mut decisions = Vec::new();

    for field in &selected {
        let allowed = if let Some((gate, config)) = authorization {
            let principal = config.principal.as_ref().ok_or_else(|| {
                DecisionError::new(
                    "DECISION_INVALID_RESULT",
                    "authorization config requires a principal",
                )
            })?;
            let uri = format!(
                "flow://{}/{}/{}/{}",
                principal.application_id, collection, record_id, field
            );
            let decision = gate.check(&AuthorizationRequest {
                subject: principal.subject.clone(),
                tenant_id: principal.tenant_id.clone(),
                application_id: principal.application_id.clone(),
                resource: crate::authorization::Resource {
                    uri,
                    attributes: BTreeMap::new(),
                },
                capability: principal.capability.clone(),
                context: crate::authorization::AuthorizationContext::default(),
                grant_id: None,
                subject_attributes: BTreeMap::new(),
            })?;
            if let Some(decision_id) = decision.decision_id {
                decisions.push(decision_id);
            }
            decision.allowed
        } else {
            true
        };

        if allowed {
            authorized.push(field.clone());
        } else {
            withheld.insert(field.clone());
        }
    }

    let mut values = BTreeMap::new();
    for field in &authorized {
        if let Some(value) = object.get(field) {
            values.insert(field.clone(), value.clone());
        }
    }
    let projection = json!({ "fields": authorized, "values": values });
    let projection_json = serde_json::to_string(&projection)
        .map_err(|error| DecisionError::new("DECISION_INVALID_RESULT", error.to_string()))?;
    Ok(DecisionContextPacket {
        collection: collection.into(),
        record_id: record_id.into(),
        record_version,
        reference: state_reference(application_id, collection, record_id),
        fields: projection["fields"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str().map(str::to_string))
            .collect(),
        values,
        withheld_fields: withheld.into_iter().collect(),
        relevant_state_version: digest(&[
            "semantic-decision-state-v1".into(),
            collection.into(),
            record_id.into(),
            projection_json.clone(),
        ]),
        context_hash: digest(&["semantic-decision-context-v1".into(), projection_json]),
        authorization_decisions: decisions,
    })
}

fn request_with_context(
    definition: &DecisionDefinition,
    context: &DecisionContextPacket,
) -> DecisionRequest {
    request_with_values(definition, &context.values)
}

fn request_with_values(
    definition: &DecisionDefinition,
    values: &BTreeMap<String, Value>,
) -> DecisionRequest {
    let context_value = Value::Object(
        values
            .iter()
            .map(|(field, value)| (field.clone(), value.clone()))
            .collect(),
    );
    match definition {
        DecisionDefinition::Binary { predicate } => DecisionRequest::Binary {
            predicate: predicate.clone(),
            context: context_value,
        },
        DecisionDefinition::Choice { question, options } => DecisionRequest::Choice {
            question: question.clone(),
            options: options.clone(),
            context: context_value,
        },
        DecisionDefinition::Score { question, levels } => DecisionRequest::Score {
            question: question.clone(),
            levels: levels.clone(),
            context: context_value,
        },
    }
}

fn require_runtime_capability(
    runtime: &dyn DecisionRuntime,
    kind: &DecisionKind,
) -> Result<(), DecisionError> {
    let capabilities = runtime.capabilities();
    let supported = match kind {
        DecisionKind::Binary => capabilities.binary,
        DecisionKind::Choice => capabilities.choice,
        DecisionKind::Score => capabilities.score,
    };
    if supported {
        Ok(())
    } else {
        Err(DecisionError::new(
            "DECISION_UNSUPPORTED",
            format!("runtime does not support {:?} decisions", kind),
        ))
    }
}

fn decision_collection<'a>(options: &'a DecisionOptions) -> &'a str {
    options
        .decision_collection
        .as_deref()
        .unwrap_or(DEFAULT_DECISION_COLLECTION)
}

fn schema_version(options: &DecisionOptions) -> &str {
    options
        .schema_version
        .as_deref()
        .unwrap_or("semantic-decision-v1")
}

fn lineage_key(
    collection: &str,
    record_id: &str,
    definition: &DecisionDefinition,
    runtime: &DecisionRuntimeMetadata,
    schema_version: &str,
) -> String {
    digest(&[
        "semantic-decision-lineage-v1".into(),
        format!("{:?}", definition.kind()),
        collection.into(),
        record_id.into(),
        canonical_text(definition.prompt()),
        serde_json::to_string(definition.schema_values()).unwrap_or_default(),
        runtime.runtime.clone(),
        runtime
            .model_revision
            .clone()
            .or(runtime.model.clone())
            .unwrap_or_default(),
        runtime.prompt_revision.clone().unwrap_or_default(),
        serde_json::to_string(&runtime.execution_method).unwrap_or_default(),
        runtime.decision_schema_revision.clone().unwrap_or_default(),
        schema_version.into(),
    ])
}

pub fn decision_cache_key(
    collection: &str,
    record_id: &str,
    definition: &DecisionDefinition,
    context: &DecisionContextPacket,
    runtime: &DecisionRuntimeMetadata,
    schema_version: &str,
) -> String {
    digest(&[
        "semantic-decision-cache-v1".into(),
        format!("{:?}", definition.kind()),
        collection.into(),
        record_id.into(),
        canonical_text(definition.prompt()),
        serde_json::to_string(definition.schema_values()).unwrap_or_default(),
        context.relevant_state_version.clone(),
        runtime.runtime.clone(),
        runtime
            .model_revision
            .clone()
            .or(runtime.model.clone())
            .unwrap_or_default(),
        runtime.prompt_revision.clone().unwrap_or_default(),
        serde_json::to_string(&runtime.execution_method).unwrap_or_default(),
        runtime.decision_schema_revision.clone().unwrap_or_default(),
        schema_version.into(),
    ])
}

fn valid_supporting_fields(
    context: &DecisionContextPacket,
    result: &DecisionResult,
) -> Vec<String> {
    result
        .supporting_fields()
        .iter()
        .filter(|field| context.fields.contains(field))
        .cloned()
        .collect()
}

fn supporting_context(
    context: &DecisionContextPacket,
    supporting_fields: &[String],
) -> Vec<DecisionEvidenceField> {
    let fields = if supporting_fields.is_empty() {
        &context.fields
    } else {
        supporting_fields
    };
    fields
        .iter()
        .filter_map(|field| {
            context
                .values
                .get(field)
                .map(|value| DecisionEvidenceField {
                    field: field.clone(),
                    value: value.clone(),
                })
        })
        .collect()
}

fn load_judgment(
    db: &FeltDb,
    collection: &str,
    key: &str,
) -> Result<Option<DecisionJudgmentRecord>, DecisionError> {
    let storage_key = format!("{collection}:{key}");
    db.capability(collection)
        .get(&storage_key)
        .map_err(|error| DecisionError::new("DECISION_STORAGE_ERROR", error.to_string()))
}

fn update_judgment(
    db: &FeltDb,
    collection: &str,
    key: &str,
    record: &DecisionJudgmentRecord,
) -> Result<(), DecisionError> {
    let storage_key = format!("{collection}:{key}");
    db.capability(collection)
        .update(&storage_key, record.clone())
        .map_err(|error| DecisionError::new("DECISION_STORAGE_ERROR", error.to_string()))
}

fn insert_judgment(
    db: &FeltDb,
    collection: &str,
    key: &str,
    record: &DecisionJudgmentRecord,
) -> Result<(), DecisionError> {
    let storage_key = format!("{collection}:{key}");
    db.capability(collection)
        .insert_with_key(&storage_key, record.clone())
        .map_err(|error| DecisionError::new("DECISION_STORAGE_ERROR", error.to_string()))
}

fn supersede_lineage(
    db: &FeltDb,
    decision_collection: &str,
    lineage_key: &str,
    keep: &str,
) -> Result<(), DecisionError> {
    let stale: Vec<DecisionJudgmentRecord> = db
        .capability(decision_collection)
        .find(|row: &DecisionJudgmentRecord| {
            row.lineage_key == lineage_key && row.status == DecisionStatus::Active && row.id != keep
        })
        .map_err(|error| DecisionError::new("DECISION_STORAGE_ERROR", error.to_string()))?;
    let superseded_at = semantic_now_ms();
    for mut row in stale {
        row.status = DecisionStatus::Superseded;
        row.superseded_at = Some(superseded_at);
        update_judgment(db, decision_collection, &row.id, &row)?;
    }
    Ok(())
}

fn stored_to_evaluation(
    stored: DecisionJudgmentRecord,
    context: &DecisionContextPacket,
    source: &str,
) -> DecisionEvaluation {
    DecisionEvaluation {
        result: stored.result.clone(),
        cached: source == "cache",
        evidence: vec![DecisionEvidence {
            decision_id: stored.id.clone(),
            kind: stored.kind.clone(),
            input: stored.input.clone(),
            result: stored.result.clone(),
            supporting_context: stored
                .supporting_fields
                .iter()
                .filter_map(|field| {
                    context
                        .values
                        .get(field)
                        .map(|value| DecisionEvidenceField {
                            field: field.clone(),
                            value: value.clone(),
                        })
                })
                .collect(),
            runtime: DecisionRuntimeMetadata {
                runtime: stored.runtime.clone(),
                model: stored.model.clone(),
                model_revision: stored.model_revision.clone(),
                prompt_revision: stored.prompt_revision.clone(),
                execution_method: stored.execution_method.clone(),
                decision_schema_revision: stored.decision_schema_revision.clone(),
            },
            state: stored.state.clone(),
        }],
        provenance: DecisionProvenance {
            runtime: stored.runtime,
            model: stored.model,
            model_revision: stored.model_revision,
            prompt_revision: stored.prompt_revision,
            execution_method: stored.execution_method,
            decision_schema_revision: stored.decision_schema_revision,
            context_hash: context.context_hash.clone(),
            relevant_state_version: context.relevant_state_version.clone(),
            cache_key: stored.id,
            source: source.into(),
            evaluated_at: stored.evaluated_at,
            served_at: semantic_now_ms(),
        },
    }
}

/// The stored judgment whose identity matches `cache_key`, if one exists.
///
/// A judgment superseded by an intermediate state change that has since been
/// reversed carries the same identity as the state it was derived from, so it
/// is reactivated rather than re-derived: the lineage's other judgments are
/// superseded and this one becomes active again.
fn reusable_judgment(
    db: &FeltDb,
    storage: &str,
    cache_key: &str,
    lineage: &str,
) -> Result<Option<DecisionJudgmentRecord>, DecisionError> {
    let Some(mut stored) = load_judgment(db, storage, cache_key)? else {
        return Ok(None);
    };
    if stored.status != DecisionStatus::Active {
        supersede_lineage(db, storage, lineage, &stored.id)?;
        stored.status = DecisionStatus::Active;
        stored.superseded_at = None;
        update_judgment(db, storage, &stored.id, &stored)?;
    }
    Ok(Some(stored))
}

/// Reuse a durable judgment for a record without invoking the runtime.
///
/// This is the `reuse-only` path of semantic query composition: the record is
/// read and authorized exactly as `decide_record` reads and authorizes it, the
/// judgment identity is derived from that authorized state and the runtime's
/// metadata, and the stored judgment is returned when one with that identity
/// exists. `Ok(None)` means no reusable judgment exists; nothing is executed
/// and nothing is written except the reactivation described on
/// `reusable_judgment`.
pub fn reuse_record_judgment(
    db: &FeltDb,
    target: &DecisionRecordRef,
    definition: &DecisionDefinition,
    runtime_metadata: &DecisionRuntimeMetadata,
    options: &DecisionOptions,
    authorization: Option<(&dyn DecisionAuthorizationGate, &DecisionAuthorizationConfig)>,
) -> Result<Option<DecisionEvaluation>, DecisionError> {
    let definition = validate_decision_definition(definition)?;
    let record: Value = db
        .capability(&target.collection)
        .get(&target.record_id)
        .map_err(|error| DecisionError::new("DECISION_STORAGE_ERROR", error.to_string()))?
        .ok_or_else(|| {
            DecisionError::new(
                "DECISION_RECORD_NOT_FOUND",
                "decision target record not found",
            )
        })?;
    let context = authorize_context(
        &target.collection,
        &target.record_id,
        &record,
        options,
        authorization,
    )?;
    if context.fields.is_empty() {
        return Err(DecisionError::new(
            "DECISION_CONTEXT_EMPTY",
            "decision refuses to evaluate an empty authorized context",
        ));
    }
    let cache_key = decision_cache_key(
        &target.collection,
        &target.record_id,
        &definition,
        &context,
        runtime_metadata,
        schema_version(options),
    );
    let lineage = lineage_key(
        &target.collection,
        &target.record_id,
        &definition,
        runtime_metadata,
        schema_version(options),
    );
    Ok(
        reusable_judgment(db, decision_collection(options), &cache_key, &lineage)?
            .map(|stored| stored_to_evaluation(stored, &context, "cache")),
    )
}

/// Build the runtime binding a FeltDB deployment configures for semantic
/// query composition.
///
/// A query never names its runtime; the deployment does, and the binding's
/// identity becomes part of every judgment the query stage produces. Only the
/// deterministic runtime exists in Rust today, so this is the only binding a
/// deployment can configure; the recording runtime is a transport test double
/// that answers every request identically and is refused here.
pub fn configured_decision_runtime(
    runtime: &DecisionTransportRuntime,
) -> Result<Arc<dyn DecisionRuntime + Send + Sync>, DecisionError> {
    match runtime {
        DecisionTransportRuntime::Deterministic {
            metadata,
            responses,
        } => Ok(Arc::new(DeterministicDecisionRuntime::new(
            metadata.clone(),
            responses.clone(),
        ))),
        DecisionTransportRuntime::Recording { .. } => Err(DecisionError::new(
            "DECISION_UNSUPPORTED",
            "the recording runtime is a transport test double, not a configurable runtime binding",
        )),
    }
}

pub fn decide_record(
    db: &FeltDb,
    target: &DecisionRecordRef,
    definition: &DecisionDefinition,
    runtime: &dyn DecisionRuntime,
    options: &DecisionOptions,
    authorization: Option<(&dyn DecisionAuthorizationGate, &DecisionAuthorizationConfig)>,
) -> Result<DecisionEvaluation, DecisionError> {
    let definition = validate_decision_definition(definition)?;
    require_runtime_capability(runtime, &definition.kind())?;
    let record: Value = db
        .capability(&target.collection)
        .get(&target.record_id)
        .map_err(|error| DecisionError::new("DECISION_STORAGE_ERROR", error.to_string()))?
        .ok_or_else(|| {
            DecisionError::new(
                "DECISION_RECORD_NOT_FOUND",
                "decision target record not found",
            )
        })?;
    let context = authorize_context(
        &target.collection,
        &target.record_id,
        &record,
        options,
        authorization,
    )?;
    if context.fields.is_empty() {
        return Err(DecisionError::new(
            "DECISION_CONTEXT_EMPTY",
            "decision refuses to evaluate an empty authorized context",
        ));
    }
    let runtime_metadata = runtime.metadata();
    let cache_key = decision_cache_key(
        &target.collection,
        &target.record_id,
        &definition,
        &context,
        &runtime_metadata,
        schema_version(options),
    );
    let lineage = lineage_key(
        &target.collection,
        &target.record_id,
        &definition,
        &runtime_metadata,
        schema_version(options),
    );
    let storage = decision_collection(options);
    if let Some(stored) = reusable_judgment(db, storage, &cache_key, &lineage)? {
        return Ok(stored_to_evaluation(stored, &context, "cache"));
    }

    let request = request_with_context(&definition, &context);
    let result = validate_decision_result(&request, &runtime.execute(&request)?)?;
    let evaluated_at = semantic_now_ms();
    let supporting_fields = valid_supporting_fields(&context, &result);
    let supporting_context = supporting_context(&context, &supporting_fields);
    let state = DecisionStateReference {
        collection: target.collection.clone(),
        record_id: target.record_id.clone(),
        record_version: context.record_version,
        reference: context.reference.clone(),
        relevant_state_version: context.relevant_state_version.clone(),
        withheld_fields: context.withheld_fields.clone(),
    };
    let evidence = DecisionEvidence {
        decision_id: cache_key.clone(),
        kind: definition.kind(),
        input: definition.clone(),
        result: result.clone(),
        supporting_context,
        runtime: runtime_metadata.clone(),
        state: state.clone(),
    };
    let record = DecisionJudgmentRecord {
        id: cache_key.clone(),
        lineage_key: lineage.clone(),
        kind: definition.kind(),
        collection: target.collection.clone(),
        record_id: target.record_id.clone(),
        input: definition.clone(),
        schema_version: schema_version(options).into(),
        relevant_state_version: context.relevant_state_version.clone(),
        runtime: runtime_metadata.runtime.clone(),
        model: runtime_metadata.model.clone(),
        model_revision: runtime_metadata.model_revision.clone(),
        prompt_revision: runtime_metadata.prompt_revision.clone(),
        execution_method: runtime_metadata.execution_method.clone(),
        decision_schema_revision: runtime_metadata.decision_schema_revision.clone(),
        option_mass: result.option_mass(),
        context_fields: context.fields.clone(),
        supporting_fields: if result.supporting_fields().is_empty() {
            context.fields.clone()
        } else {
            supporting_fields.clone()
        },
        status: DecisionStatus::Active,
        state,
        evidence: evidence.clone(),
        result: result.clone(),
        evaluated_at,
        superseded_at: None,
    };
    supersede_lineage(db, storage, &lineage, &cache_key)?;
    insert_judgment(db, storage, &cache_key, &record)?;
    Ok(DecisionEvaluation {
        result,
        cached: false,
        evidence: vec![evidence],
        provenance: DecisionProvenance {
            runtime: runtime_metadata.runtime,
            model: runtime_metadata.model,
            model_revision: runtime_metadata.model_revision,
            prompt_revision: runtime_metadata.prompt_revision,
            execution_method: runtime_metadata.execution_method,
            decision_schema_revision: runtime_metadata.decision_schema_revision,
            context_hash: context.context_hash,
            relevant_state_version: context.relevant_state_version,
            cache_key,
            source: "runtime".into(),
            evaluated_at,
            served_at: evaluated_at,
        },
    })
}

fn evaluate_context_packet(
    target: DecisionRecordRef,
    definition: &DecisionDefinition,
    context: &DecisionContextPacket,
    runtime: &dyn DecisionRuntime,
    options: &DecisionOptions,
) -> Result<DecisionEvaluation, DecisionError> {
    let definition = validate_decision_definition(definition)?;
    require_runtime_capability(runtime, &definition.kind())?;
    if context.fields.is_empty() {
        return Err(DecisionError::new(
            "DECISION_CONTEXT_EMPTY",
            "decision refuses to evaluate an empty authorized context",
        ));
    }
    let runtime_metadata = runtime.metadata();
    let cache_key = decision_cache_key(
        &target.collection,
        &target.record_id,
        &definition,
        context,
        &runtime_metadata,
        schema_version(options),
    );
    let request = request_with_context(&definition, context);
    let result = validate_decision_result(&request, &runtime.execute(&request)?)?;
    let evaluated_at = semantic_now_ms();
    let supporting_fields = valid_supporting_fields(context, &result);
    let state = DecisionStateReference {
        collection: target.collection,
        record_id: target.record_id,
        record_version: context.record_version,
        reference: context.reference.clone(),
        relevant_state_version: context.relevant_state_version.clone(),
        withheld_fields: context.withheld_fields.clone(),
    };
    let evidence = DecisionEvidence {
        decision_id: cache_key.clone(),
        kind: definition.kind(),
        input: definition,
        result: result.clone(),
        supporting_context: supporting_context(context, &supporting_fields),
        runtime: runtime_metadata.clone(),
        state,
    };
    Ok(DecisionEvaluation {
        result,
        cached: false,
        evidence: vec![evidence],
        provenance: DecisionProvenance {
            runtime: runtime_metadata.runtime,
            model: runtime_metadata.model,
            model_revision: runtime_metadata.model_revision,
            prompt_revision: runtime_metadata.prompt_revision,
            execution_method: runtime_metadata.execution_method,
            decision_schema_revision: runtime_metadata.decision_schema_revision,
            context_hash: context.context_hash.clone(),
            relevant_state_version: context.relevant_state_version.clone(),
            cache_key,
            source: "runtime".into(),
            evaluated_at,
            served_at: evaluated_at,
        },
    })
}

fn inline_context_packet(
    request: &DecisionTransportExecutionRequest,
    authorization: Option<(&dyn DecisionAuthorizationGate, &DecisionAuthorizationConfig)>,
) -> Result<DecisionContextPacket, DecisionError> {
    let context = request.context.as_ref().ok_or_else(|| {
        DecisionError::new(
            "DECISION_INVALID_RESULT",
            "inline decision transport requires a context object",
        )
    })?;
    authorize_context(
        "$context",
        "inline",
        context,
        &request.options,
        authorization,
    )
}

pub fn execute_inline_decision_transport(
    request: &DecisionTransportExecutionRequest,
) -> Result<DecisionTransportExecutionResponse, DecisionError> {
    let runtime = TransportRuntimeHandle::new(&request.runtime);
    let authorization_gate = request.authorization.as_ref().map(|authorization| {
        (
            TransportAuthorizationGate::new(&authorization.allowed_fields),
            DecisionAuthorizationConfig {
                principal: authorization.principal.clone(),
            },
        )
    });
    let authorization = authorization_gate
        .as_ref()
        .map(|(gate, config)| (gate as &dyn DecisionAuthorizationGate, config));
    if request.target.is_some() {
        return Err(DecisionError::new(
            "DECISION_INVALID_RESULT",
            "inline decision transport does not support durable target records",
        ));
    }
    let evaluation = evaluate_context_packet(
        DecisionRecordRef {
            collection: "$context".into(),
            record_id: "inline".into(),
        },
        &request.definition,
        &inline_context_packet(request, authorization)?,
        &runtime,
        &request.options,
    )?;
    Ok(DecisionTransportExecutionResponse {
        evaluation,
        runtime_requests: runtime.recorded_requests(),
    })
}

pub fn execute_decision_transport(
    db: &FeltDb,
    request: &DecisionTransportExecutionRequest,
) -> Result<DecisionTransportExecutionResponse, DecisionError> {
    let runtime = TransportRuntimeHandle::new(&request.runtime);
    let authorization_gate = request.authorization.as_ref().map(|authorization| {
        (
            TransportAuthorizationGate {
                allowed_fields: authorization.allowed_fields.iter().cloned().collect(),
            },
            DecisionAuthorizationConfig {
                principal: authorization.principal.clone(),
            },
        )
    });
    let authorization = authorization_gate
        .as_ref()
        .map(|(gate, config)| (gate as &dyn DecisionAuthorizationGate, config));
    let evaluation = match (&request.target, &request.context) {
        (Some(_), Some(_)) => return Err(DecisionError::new(
            "DECISION_INVALID_RESULT",
            "decision transport accepts either a target record or an inline context, but not both",
        )),
        (Some(target), None) => decide_record(
            db,
            target,
            &request.definition,
            &runtime,
            &request.options,
            authorization,
        )?,
        (None, Some(context)) => evaluate_context_packet(
            DecisionRecordRef {
                collection: "$context".into(),
                record_id: "inline".into(),
            },
            &request.definition,
            &authorize_context(
                "$context",
                "inline",
                context,
                &request.options,
                authorization,
            )?,
            &runtime,
            &request.options,
        )?,
        (None, None) => {
            return Err(DecisionError::new(
                "DECISION_INVALID_RESULT",
                "decision transport requires either a target record or an inline context",
            ))
        }
    };
    Ok(DecisionTransportExecutionResponse {
        evaluation,
        runtime_requests: runtime.recorded_requests(),
    })
}

pub fn execute_decision_batch(
    context: &DecisionContextPacket,
    batch: &DecisionBatch,
    runtime: &dyn DecisionRuntime,
) -> Result<Vec<DecisionBatchEvaluation>, DecisionError> {
    if batch.decisions.is_empty() {
        return Err(DecisionError::new(
            "DECISION_INVALID_RESULT",
            "decision batches require at least one decision",
        ));
    }
    if !runtime.capabilities().batch {
        return Err(DecisionError::new(
            "DECISION_UNSUPPORTED",
            "runtime does not support decision batches",
        ));
    }
    let mut ids = BTreeSet::new();
    let shared = require_object(
        &batch.context,
        "decision batch shared context must be object-shaped",
    )?;
    let mut base = context.values.clone();
    for (field, value) in shared {
        base.insert(field, value);
    }
    let mut requests = Vec::with_capacity(batch.decisions.len());
    let mut ids_in_order = Vec::with_capacity(batch.decisions.len());
    for decision in &batch.decisions {
        if canonical_text(&decision.id).is_empty() || !ids.insert(decision.id.clone()) {
            return Err(DecisionError::new(
                "DECISION_INVALID_RESULT",
                "decision batches require unique non-empty decision ids",
            ));
        }
        let local = require_object(
            &decision.context,
            "decision batch decision context must be object-shaped",
        )?;
        let definition = validate_decision_definition(&decision.definition)?;
        let mut merged = base.clone();
        for (field, value) in local {
            merged.insert(field, value);
        }
        let request = request_with_values(&definition, &merged);
        ids_in_order.push(decision.id.clone());
        requests.push(request);
    }
    let results = runtime.execute_batch(&requests)?;
    if results.len() != requests.len() {
        return Err(DecisionError::new(
            "DECISION_INVALID_RESULT",
            "runtime returned a batch result count different from the request count",
        ));
    }
    let mut evaluations = Vec::with_capacity(results.len());
    for ((request, result), id) in requests.into_iter().zip(results).zip(ids_in_order) {
        let result = validate_decision_result(&request, &result)?;
        evaluations.push(DecisionBatchEvaluation { id, result });
    }
    Ok(evaluations)
}

fn permutations(values: &[String]) -> Vec<Vec<String>> {
    if values.len() <= 1 {
        return vec![values.to_vec()];
    }
    let mut result = Vec::new();
    for index in 0..values.len() {
        let mut remaining = values.to_vec();
        let head = remaining.remove(index);
        for mut tail in permutations(&remaining) {
            let mut permutation = vec![head.clone()];
            permutation.append(&mut tail);
            result.push(permutation);
        }
    }
    result
}

fn observe_choice(
    context: &DecisionContextPacket,
    question: &str,
    options: Vec<String>,
    runtime: &dyn DecisionRuntime,
) -> Result<ChoicePermutationObservation, DecisionError> {
    let definition = validate_decision_definition(&DecisionDefinition::Choice {
        question: question.into(),
        options: options.clone(),
    })?;
    let request = request_with_context(&definition, context);
    let result = validate_decision_result(&request, &runtime.execute(&request)?)?;
    match result {
        DecisionResult::Choice {
            selected,
            options: distribution,
            ..
        } => {
            let rank = distribution
                .iter()
                .enumerate()
                .find_map(|(index, option)| (option.value == selected).then_some(index + 1))
                .unwrap_or(distribution.len());
            let probability = distribution
                .iter()
                .find_map(|option| (option.value == selected).then_some(option.probability))
                .unwrap_or(0.0);
            Ok(ChoicePermutationObservation {
                order: options,
                selected,
                distribution,
                rank,
                probability,
            })
        }
        _ => Err(DecisionError::new(
            "DECISION_INVALID_RESULT",
            "choice diagnostics require a choice decision",
        )),
    }
}

pub fn run_choice_diagnostics(
    context: &DecisionContextPacket,
    question: &str,
    options: &[String],
    expansion_option: Option<&str>,
    runtime: &dyn DecisionRuntime,
) -> Result<ChoiceDiagnosticsReport, DecisionError> {
    let canonical = validate_decision_definition(&DecisionDefinition::Choice {
        question: question.into(),
        options: options.to_vec(),
    })?;
    let base_options = canonical.schema_values().to_vec();
    let mut permutations_observed = Vec::new();
    for order in permutations(&base_options) {
        permutations_observed.push(observe_choice(context, question, order, runtime)?);
    }
    let expansion = match expansion_option {
        Some(option) => {
            let baseline = observe_choice(context, question, base_options.clone(), runtime)?;
            let mut expanded = base_options.clone();
            expanded.push(option.into());
            Some(ChoiceExpansionObservation {
                baseline,
                expanded: observe_choice(context, question, expanded, runtime)?,
            })
        }
        None => None,
    };
    Ok(ChoiceDiagnosticsReport {
        permutations: permutations_observed,
        expansion,
    })
}

pub fn jev(
    db: &FeltDb,
    target: &DecisionRecordRef,
    predicate: &str,
    runtime: &dyn DecisionRuntime,
    options: &DecisionOptions,
    authorization: Option<(&dyn DecisionAuthorizationGate, &DecisionAuthorizationConfig)>,
) -> Result<DecisionEvaluation, DecisionError> {
    decide_record(
        db,
        target,
        &DecisionDefinition::Binary {
            predicate: predicate.into(),
        },
        runtime,
        options,
        authorization,
    )
}

#[derive(Debug, Clone)]
pub struct DeterministicDecisionRuntime {
    metadata: DecisionRuntimeMetadata,
    responses: BTreeMap<String, DecisionResult>,
}

impl DeterministicDecisionRuntime {
    pub fn new(
        metadata: DecisionRuntimeMetadata,
        responses: BTreeMap<String, DecisionResult>,
    ) -> Self {
        Self {
            metadata,
            responses,
        }
    }
}

impl DecisionRuntime for DeterministicDecisionRuntime {
    fn metadata(&self) -> DecisionRuntimeMetadata {
        self.metadata.clone()
    }

    fn decide(&self, request: &DecisionRequest) -> Result<DecisionResult, DecisionError> {
        let key = serde_json::to_string(request)
            .map_err(|error| DecisionError::new("DECISION_INVALID_RESULT", error.to_string()))?;
        self.responses.get(&key).cloned().ok_or_else(|| {
            DecisionError::new(
                "DECISION_INVALID_RESULT",
                "deterministic runtime has no configured response for this request",
            )
        })
    }
}

pub fn deterministic_request_key(request: &DecisionRequest) -> Result<String, DecisionError> {
    serde_json::to_string(request)
        .map_err(|error| DecisionError::new("DECISION_INVALID_RESULT", error.to_string()))
}

pub fn run_decision_benchmark(
    db: &FeltDb,
    runtime: &dyn DecisionRuntime,
    options: &DecisionOptions,
) -> Result<DecisionBenchmarkReport, DecisionError> {
    let collection = db.capability("benchmark_people");
    for index in 0..20 {
        collection
            .insert_with_key(
                &format!("b-{index}"),
                json!({
                    "id": format!("b-{index}"),
                    "__version": 1,
                    "employmentType": if index % 2 == 0 { "remote" } else { "office" },
                    "publicRole": if index % 3 == 0 { "Staff Backend Engineer" } else { "Lab Technician" },
                    "urgency": if index % 3 == 0 { "high" } else if index % 3 == 1 { "medium" } else { "low" },
                    "notes": if index % 2 == 0 { "Remote-first and distributed systems ownership." } else { "On-site inventory and lab work." }
                }),
            )
            .map_err(|error| DecisionError::new("DECISION_STORAGE_ERROR", error.to_string()))?;
    }
    let mut first = 0usize;
    let mut second_cache_hits = 0usize;
    let mut second_runtime = 0usize;
    for index in 0..20 {
        let target = DecisionRecordRef {
            collection: "benchmark_people".into(),
            record_id: format!("b-{index}"),
        };
        for definition in [
            DecisionDefinition::Binary {
                predicate: "could work from home".into(),
            },
            DecisionDefinition::Choice {
                question: "Which reviewer should handle this?".into(),
                options: vec!["approve".into(), "reject".into(), "review".into()],
            },
            DecisionDefinition::Score {
                question: "What is the urgency?".into(),
                levels: vec!["low".into(), "medium".into(), "high".into()],
            },
        ] {
            let first_eval = decide_record(db, &target, &definition, runtime, options, None)?;
            if !first_eval.cached {
                first += 1;
            }
            let second_eval = decide_record(db, &target, &definition, runtime, options, None)?;
            if second_eval.cached {
                second_cache_hits += 1;
            } else {
                second_runtime += 1;
            }
        }
    }
    Ok(DecisionBenchmarkReport {
        binary: 20,
        choice: 20,
        score: 20,
        runtime_invocations_first_pass: first,
        runtime_invocations_second_pass: second_runtime,
        cache_hits_second_pass: second_cache_hits,
    })
}

impl From<DecisionError> for FlowError {
    fn from(error: DecisionError) -> Self {
        FlowError::CapabilityError(format!("{}: {}", error.code, error.message))
    }
}
