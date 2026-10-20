//! Semantic query composition over a bounded candidate page.
//!
//! This module implements `docs/reference/semantic-query.md`: a semantic
//! decision participates in a FeltDB query only as a bounded transformation of
//! an already-authorized deterministic candidate page. The deterministic query
//! decides which records exist in the page and in what order; this module may
//! annotate, filter, or rank those records with the existing semantic decision
//! primitive and may never enlarge the page, refill it, touch an index, or
//! reach a record outside it.
//!
//! It is deliberately not a query engine. Every judgment is produced by
//! `semantic_decision::decide_record` or reused through
//! `semantic_decision::reuse_record_judgment`, so authorization, judgment
//! identity, durable supersession, evidence, and provenance are exactly what
//! the record-scoped primitive already provides.

use crate::semantic_decision::{
    decide_record, reuse_record_judgment, validate_decision_definition,
    DecisionAuthorizationConfig, DecisionAuthorizationGate, DecisionDefinition, DecisionError,
    DecisionKind, DecisionOptions, DecisionPrincipal, DecisionProvenance, DecisionRecordRef,
    DecisionRequest, DecisionResult, DecisionRuntime, DecisionRuntimeCapabilities,
    DecisionRuntimeMetadata, DecisionTransportAuthorization, TransportAuthorizationGate,
};
use crate::FeltDb;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cell::Cell;

/// The largest candidate page one semantic query request may transform; the
/// bounded authority query's own limit.
pub const MAX_SEMANTIC_CANDIDATES: usize = 1_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SemanticQueryMode {
    /// Reuse a durable judgment where one exists; execute the runtime for every
    /// other candidate with a non-empty authorized context.
    #[default]
    Evaluate,
    /// Never reach the runtime. Candidates without a reusable judgment are
    /// reported as unevaluated.
    ReuseOnly,
}

/// The typed filter a semantic clause applies to each candidate's result.
///
/// Which keys are valid depends on the decision kind: `binary` takes `is`,
/// `choice` takes `in`, and `score` takes `in`, `atLeast`, and `atMost`, all
/// compared by the ordinal position of the selected level. `option_mass` is
/// never a filter operand.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct SemanticQueryFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is: Option<bool>,
    #[serde(default, rename = "in", skip_serializing_if = "Option::is_none")]
    pub within: Option<Vec<String>>,
    #[serde(default, rename = "atLeast", skip_serializing_if = "Option::is_none")]
    pub at_least: Option<String>,
    #[serde(default, rename = "atMost", skip_serializing_if = "Option::is_none")]
    pub at_most: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SemanticOrderDirection {
    Asc,
    Desc,
}

/// Ordering within the candidate page by the ordinal of a `score` level.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticQueryOrder {
    pub direction: SemanticOrderDirection,
}

/// The semantic clause of a bounded authority query.
///
/// Unknown keys are refused rather than ignored: a clause a runtime does not
/// understand must fail the request, never execute the deterministic part and
/// return it as though the clause had been honored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SemanticQueryClause {
    pub decision: DecisionDefinition,
    #[serde(default)]
    pub mode: SemanticQueryMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<SemanticQueryFilter>,
    #[serde(default, rename = "orderBy", skip_serializing_if = "Option::is_none")]
    pub order_by: Option<SemanticQueryOrder>,
    /// Explicit field selection, narrowed further by authorization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<String>>,
    #[serde(
        default,
        rename = "schemaVersion",
        skip_serializing_if = "Option::is_none"
    )]
    pub schema_version: Option<String>,
    /// Field-level authorization narrowing, in the same shape the
    /// semantic-decision transport accepts. The principal is bound by the
    /// authority to the authenticated principal before evaluation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization: Option<DecisionTransportAuthorization>,
}

/// One record of the deterministic page, as the deterministic stage returned it.
#[derive(Debug, Clone, PartialEq)]
pub struct SemanticCandidate {
    pub record_id: String,
    pub record: Value,
}

/// The semantic annotation of one page record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticAnnotation {
    pub record_id: String,
    pub result: DecisionResult,
    pub cached: bool,
    pub provenance: DecisionProvenance,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SemanticUnevaluatedReason {
    /// The authorized context was empty, so the primitive refused to evaluate.
    Unauthorized,
    /// `reuse-only` mode found no reusable judgment.
    NoJudgment,
    /// The record left authoritative state between the deterministic page
    /// and the semantic stage.
    Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticUnevaluated {
    pub record_id: String,
    pub reason: SemanticUnevaluatedReason,
}

/// Stage-level provenance and the measured execution counts of one request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticStageSummary {
    pub definition_hash: String,
    pub schema_version: String,
    pub runtime: DecisionRuntimeMetadata,
    pub mode: SemanticQueryMode,
    pub candidates: usize,
    pub reused: usize,
    pub evaluated: usize,
    pub unevaluated: usize,
    pub filtered: usize,
}

/// The candidate page after the semantic stage.
///
/// `records` and `annotations` are aligned: `annotations[i]` describes
/// `records[i]`, and is `None` for a record that could not be evaluated.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticPage {
    pub records: Vec<Value>,
    pub annotations: Vec<Option<SemanticAnnotation>>,
    pub unevaluated: Vec<SemanticUnevaluated>,
    pub complete: bool,
    pub stage: SemanticStageSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticQueryError {
    pub code: String,
    pub message: String,
}

impl SemanticQueryError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new("INVALID_QUERY", message)
    }
}

impl std::fmt::Display for SemanticQueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SemanticQueryError {}

/// Validate a semantic clause before any deterministic work runs.
///
/// Returns the validated (whitespace-canonical) decision definition, which is
/// the definition every candidate is evaluated with.
pub fn validate_semantic_clause(
    clause: &SemanticQueryClause,
) -> Result<DecisionDefinition, SemanticQueryError> {
    let definition = validate_decision_definition(&clause.decision)
        .map_err(|error| SemanticQueryError::invalid(error.message))?;
    let kind = definition.kind();
    let declared = definition.schema_values();
    if let Some(filter) = &clause.filter {
        let keys = [
            filter.is.is_some(),
            filter.within.is_some(),
            filter.at_least.is_some(),
            filter.at_most.is_some(),
        ];
        if !keys.iter().any(|set| *set) {
            return Err(SemanticQueryError::invalid(
                "semantic filter requires at least one operator",
            ));
        }
        match kind {
            DecisionKind::Binary => {
                if filter.is.is_none() || keys[1] || keys[2] || keys[3] {
                    return Err(SemanticQueryError::invalid(
                        "binary semantic filters take exactly `is`",
                    ));
                }
            }
            DecisionKind::Choice => {
                if filter.within.is_none() || keys[0] || keys[2] || keys[3] {
                    return Err(SemanticQueryError::invalid(
                        "choice semantic filters take exactly `in`",
                    ));
                }
            }
            DecisionKind::Score => {
                if keys[0] {
                    return Err(SemanticQueryError::invalid(
                        "score semantic filters take `in`, `atLeast`, and `atMost`",
                    ));
                }
            }
        }
        for value in filter
            .within
            .iter()
            .flatten()
            .chain(filter.at_least.iter())
            .chain(filter.at_most.iter())
        {
            if !declared.contains(value) {
                return Err(SemanticQueryError::invalid(format!(
                    "semantic filter names \"{value}\", which the decision does not declare"
                )));
            }
        }
        if let Some(values) = &filter.within {
            if values.is_empty() {
                return Err(SemanticQueryError::invalid(
                    "semantic `in` filter requires at least one declared value",
                ));
            }
        }
    }
    if clause.order_by.is_some() && kind != DecisionKind::Score {
        return Err(SemanticQueryError::invalid(
            "only score decisions order a semantic page",
        ));
    }
    if let Some(fields) = &clause.fields {
        if fields.is_empty() || fields.iter().any(|field| field.trim().is_empty()) {
            return Err(SemanticQueryError::invalid(
                "semantic field selection must name at least one non-empty field",
            ));
        }
    }
    Ok(definition)
}

/// Stable identity of a decision definition for stage provenance and cursor
/// binding.
pub fn semantic_definition_hash(definition: &DecisionDefinition) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"semantic-query-definition-v1");
    hasher.update([0x1f]);
    hasher.update(serde_json::to_vec(definition).unwrap_or_default());
    format!("sha256:{:x}", hasher.finalize())
}

/// Stable identity of a whole semantic clause, for binding a continuation
/// cursor to the clause it was issued under.
pub fn semantic_clause_hash(clause: &SemanticQueryClause) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"semantic-query-clause-v1");
    hasher.update([0x1f]);
    hasher.update(serde_json::to_vec(clause).unwrap_or_default());
    format!("sha256:{:x}", hasher.finalize())
}

/// Wraps the configured runtime so a failure inside the provider can be told
/// apart from a result the provider returned but FeltDB rejected.
struct ProviderBoundary<'a> {
    inner: &'a dyn DecisionRuntime,
    failed: Cell<Option<DecisionError>>,
}

impl DecisionRuntime for ProviderBoundary<'_> {
    fn metadata(&self) -> DecisionRuntimeMetadata {
        self.inner.metadata()
    }

    fn capabilities(&self) -> DecisionRuntimeCapabilities {
        self.inner.capabilities()
    }

    fn decide(&self, request: &DecisionRequest) -> Result<DecisionResult, DecisionError> {
        self.inner
            .execute(request)
            .inspect_err(|error| self.failed.set(Some(error.clone())))
    }
}

fn ordinal(definition: &DecisionDefinition, value: &str) -> Option<usize> {
    definition
        .schema_values()
        .iter()
        .position(|declared| declared == value)
}

fn selected_value(result: &DecisionResult) -> Option<&str> {
    match result {
        DecisionResult::Binary { .. } => None,
        DecisionResult::Choice { selected, .. } | DecisionResult::Score { selected, .. } => {
            Some(selected.as_str())
        }
    }
}

fn filter_keeps(
    definition: &DecisionDefinition,
    filter: &SemanticQueryFilter,
    result: &DecisionResult,
) -> bool {
    match result {
        DecisionResult::Binary { decision, .. } => filter.is == Some(*decision),
        DecisionResult::Choice { selected, .. } => filter
            .within
            .as_ref()
            .is_some_and(|values| values.iter().any(|value| value == selected)),
        DecisionResult::Score { selected, .. } => {
            let Some(position) = ordinal(definition, selected) else {
                return false;
            };
            let within = filter
                .within
                .as_ref()
                .is_none_or(|values| values.iter().any(|value| value == selected));
            let at_least = filter
                .at_least
                .as_ref()
                .and_then(|level| ordinal(definition, level))
                .is_none_or(|floor| position >= floor);
            let at_most = filter
                .at_most
                .as_ref()
                .and_then(|level| ordinal(definition, level))
                .is_none_or(|ceiling| position <= ceiling);
            within && at_least && at_most
        }
    }
}

fn map_stage_error(error: DecisionError, provider_failed: bool) -> SemanticQueryError {
    match error.code.as_str() {
        "DECISION_UNSUPPORTED" => SemanticQueryError::new("SEMANTIC_UNSUPPORTED", error.message),
        "DECISION_STORAGE_ERROR" => SemanticQueryError::new(&error.code, error.message),
        _ if provider_failed => SemanticQueryError::new(
            "SEMANTIC_PROVIDER_UNAVAILABLE",
            format!("semantic runtime could not evaluate a candidate: {error}"),
        ),
        _ => SemanticQueryError::new(
            "SEMANTIC_INVALID_RESULT",
            format!("semantic runtime returned a result FeltDB rejected: {error}"),
        ),
    }
}

/// Transform one deterministic candidate page with a semantic clause.
///
/// `candidates` is the page exactly as the deterministic stage produced it,
/// in its total order. `runtime` is the deployment's configured binding; with
/// none configured the request fails with `SEMANTIC_PROVIDER_UNAVAILABLE` in
/// either mode, because even `reuse-only` needs the binding's identity to
/// name the judgments it may reuse. `principal` is the authenticated principal
/// the authority binds field-level authorization to.
pub fn compose_semantic_page(
    db: &FeltDb,
    collection: &str,
    candidates: Vec<SemanticCandidate>,
    clause: &SemanticQueryClause,
    runtime: Option<&dyn DecisionRuntime>,
    principal: Option<DecisionPrincipal>,
) -> Result<SemanticPage, SemanticQueryError> {
    let definition = validate_semantic_clause(clause)?;
    if candidates.len() > MAX_SEMANTIC_CANDIDATES {
        return Err(SemanticQueryError::invalid(format!(
            "semantic composition is bounded to {MAX_SEMANTIC_CANDIDATES} candidates per page"
        )));
    }
    let runtime = runtime.ok_or_else(|| {
        SemanticQueryError::new(
            "SEMANTIC_PROVIDER_UNAVAILABLE",
            "no semantic runtime binding is configured for this authority",
        )
    })?;
    let capabilities = runtime.capabilities();
    let supported = match definition.kind() {
        DecisionKind::Binary => capabilities.binary,
        DecisionKind::Choice => capabilities.choice,
        DecisionKind::Score => capabilities.score,
    };
    if !supported {
        return Err(SemanticQueryError::new(
            "SEMANTIC_UNSUPPORTED",
            format!(
                "the configured semantic runtime does not support {:?} decisions",
                definition.kind()
            ),
        ));
    }
    let boundary = ProviderBoundary {
        inner: runtime,
        failed: Cell::new(None),
    };
    let runtime_metadata = boundary.metadata();
    let options = DecisionOptions {
        fields: clause.fields.clone(),
        application_id: principal
            .as_ref()
            .map(|principal| principal.application_id.clone()),
        decision_collection: None,
        schema_version: clause
            .schema_version
            .clone()
            .or_else(|| DecisionOptions::default().schema_version),
    };
    let gate = clause.authorization.as_ref().map(|authorization| {
        (
            TransportAuthorizationGate::new(&authorization.allowed_fields),
            DecisionAuthorizationConfig {
                principal: principal.clone(),
            },
        )
    });
    let authorization = gate
        .as_ref()
        .map(|(gate, config)| (gate as &dyn DecisionAuthorizationGate, config));

    let mut stage = SemanticStageSummary {
        definition_hash: semantic_definition_hash(&definition),
        schema_version: options.schema_version.clone().unwrap_or_default(),
        runtime: runtime_metadata.clone(),
        mode: clause.mode,
        candidates: candidates.len(),
        reused: 0,
        evaluated: 0,
        unevaluated: 0,
        filtered: 0,
    };

    // Evaluate every candidate of the page and nothing else. Each entry keeps
    // its deterministic position so ordering can break ties with it.
    let mut evaluated: Vec<(usize, SemanticCandidate, Option<SemanticAnnotation>)> =
        Vec::with_capacity(candidates.len());
    let mut unevaluated = Vec::new();
    for (position, candidate) in candidates.into_iter().enumerate() {
        let target = DecisionRecordRef {
            collection: collection.into(),
            record_id: candidate.record_id.clone(),
        };
        let reused = reuse_record_judgment(
            db,
            &target,
            &definition,
            &runtime_metadata,
            &options,
            authorization,
        );
        let evaluation = match reused {
            Ok(Some(evaluation)) => {
                stage.reused += 1;
                Some(evaluation)
            }
            Ok(None) => match clause.mode {
                SemanticQueryMode::ReuseOnly => {
                    unevaluated.push(SemanticUnevaluated {
                        record_id: candidate.record_id.clone(),
                        reason: SemanticUnevaluatedReason::NoJudgment,
                    });
                    None
                }
                SemanticQueryMode::Evaluate => {
                    let evaluation =
                        decide_record(db, &target, &definition, &boundary, &options, authorization)
                            .map_err(|error| {
                                let failed = boundary.failed.take().is_some();
                                map_stage_error(error, failed)
                            })?;
                    if evaluation.cached {
                        // A judgment written between the lookup and the
                        // execution is still a reuse, not an execution.
                        stage.reused += 1;
                    } else {
                        stage.evaluated += 1;
                    }
                    Some(evaluation)
                }
            },
            Err(error) => match error.code.as_str() {
                "DECISION_CONTEXT_EMPTY" => {
                    unevaluated.push(SemanticUnevaluated {
                        record_id: candidate.record_id.clone(),
                        reason: SemanticUnevaluatedReason::Unauthorized,
                    });
                    None
                }
                "DECISION_RECORD_NOT_FOUND" => {
                    unevaluated.push(SemanticUnevaluated {
                        record_id: candidate.record_id.clone(),
                        reason: SemanticUnevaluatedReason::Missing,
                    });
                    None
                }
                _ => return Err(map_stage_error(error, false)),
            },
        };
        let annotation = evaluation.map(|evaluation| SemanticAnnotation {
            record_id: candidate.record_id.clone(),
            result: evaluation.result,
            cached: evaluation.cached,
            provenance: evaluation.provenance,
        });
        evaluated.push((position, candidate, annotation));
    }
    stage.unevaluated = unevaluated.len();

    // Filter within the page. An unevaluated candidate cannot satisfy a
    // filter, and its absence is reported, never silently assumed.
    if let Some(filter) = &clause.filter {
        let before = evaluated.len();
        evaluated.retain(|(_, _, annotation)| {
            annotation
                .as_ref()
                .is_some_and(|annotation| filter_keeps(&definition, filter, &annotation.result))
        });
        stage.filtered = before - evaluated.len();
    }

    // Order within the page: semantic ordinal, then the deterministic order
    // the page arrived in. Unevaluated candidates sort after every evaluated
    // one, in deterministic order.
    if let Some(order) = &clause.order_by {
        evaluated.sort_by(|left, right| {
            let key = |entry: &(usize, SemanticCandidate, Option<SemanticAnnotation>)| {
                entry
                    .2
                    .as_ref()
                    .and_then(|annotation| selected_value(&annotation.result))
                    .and_then(|selected| ordinal(&definition, selected))
            };
            match (key(left), key(right)) {
                (Some(l), Some(r)) => {
                    let ordered = l.cmp(&r);
                    let ordered = match order.direction {
                        SemanticOrderDirection::Asc => ordered,
                        SemanticOrderDirection::Desc => ordered.reverse(),
                    };
                    ordered.then(left.0.cmp(&right.0))
                }
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => left.0.cmp(&right.0),
            }
        });
    }

    let complete = unevaluated.is_empty();
    let (records, annotations) = evaluated
        .into_iter()
        .map(|(_, candidate, annotation)| (candidate.record, annotation))
        .unzip();
    Ok(SemanticPage {
        records,
        annotations,
        unevaluated,
        complete,
        stage,
    })
}
