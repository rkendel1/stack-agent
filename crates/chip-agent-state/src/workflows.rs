//! Workflow candidates: repeatable procedures proposed from verified episodes (the
//! `chip.WorkflowCandidate` collection).
//!
//! An episode describes one attempt. A candidate describes the procedure one or more verified
//! attempts followed, in terms of the capabilities they executed, so that it can be inspected and
//! checked against the runtime as it is now. It is a proposal: it executes nothing, authorizes
//! nothing and is never trusted because it exists. Whether a procedure is safe and succeeds now is
//! established only by a work that performs it under the current validation and verification.
//!
//! # Extraction ([`extract`])
//!
//! Deterministic and conservative, from records FeltDB already holds:
//!
//! * Only `verified_success` episodes whose outcome is consistent with their verdict and whose
//!   cited records back them are considered; every other episode is rejected with a reason.
//! * Each step is read from the episode's procedure and the `chip.Observation` record it cites,
//!   which holds the validated request. Its inputs are named as the request named them; only a
//!   project-relative `path` is kept as a value. Every other input (file content, which a model
//!   wrote) is a **parameter**: a work performing the step supplies it, under the capability's own
//!   validation. No content is ever copied into a candidate.
//! * An episode whose steps cannot be reconstructed from its records (a step's request was not
//!   recorded, or the procedure was cut short) is rejected, not completed by guesswork.
//! * Episodes are grouped by project, kind of work, goal signature (the exact set of goal-term
//!   digests), conditions, and procedure (each step's capability, contract and target). Each group
//!   is one candidate. Different procedures for the same task are never merged: they are separate
//!   candidates, and validation reports each as conflicting with the others.
//! * What the records do not establish stays explicitly unresolved: a procedure with failed steps
//!   (which earlier steps the result depends on is unknown), or no verification after the last
//!   change.
//! * A candidate is written once. Its id names its procedure and the exact set of episodes that
//!   support it, so extracting again writes nothing new, and more support writes a new candidate
//!   for the same procedure rather than changing the old one. One supporting episode is one
//!   observation, and the candidate says so; more support strengthens provenance and changes no
//!   validation.
//!
//! # Validation ([`validate`])
//!
//! Advisory and read-only. It checks a candidate against the current project, conditions,
//! capability contracts and declared inputs, its supporting evidence, and the other candidates,
//! and answers with stable reason codes. Passing grants no authority and promises no success.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::episodes::{
    EpisodeOutcome, EpisodeRecord, EpisodeScope, EpisodeWrite, MIN_SIMILARITY_PCT,
    outcome_consistent,
};
use super::{AgentStore, EPISODE_COLLECTION, StateError, from_flow};

pub const WORKFLOW_COLLECTION: &str = "chip.WorkflowCandidate";
pub const WORKFLOW_SCHEMA: &str = "chip.workflow_candidate.v1";

/// The product's change and verification capabilities: what makes a step a change, and what
/// verifies one.
pub const CHANGE_CAPABILITY: &str = "project.write";
pub const VERIFY_CAPABILITY: &str = "pax.test";

/// Where a step's input comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputSource {
    /// The same value every supporting episode used: only a project-relative path.
    Fixed { value: String },
    /// Supplied by the work that performs the step, validated by the capability then.
    Parameter,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowInput {
    pub name: String,
    pub source: InputSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStep {
    pub capability: String,
    /// The digest of the contract the capability declared when the supporting episodes ran.
    pub contract: Option<String>,
    /// `change`, `verify` or `observe`.
    pub role: String,
    /// The inputs the recorded request carried, by name.
    pub inputs: Vec<WorkflowInput>,
    /// The status its observation had in the supporting episodes (all `success` for a procedure
    /// with nothing unresolved).
    pub expected_status: String,
    /// The observation record of this step in each supporting episode, in support order.
    pub evidence: Vec<String>,
}

/// What the supporting episodes do not establish.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unresolved {
    pub code: String,
    pub detail: String,
}

/// How a work following the procedure must be verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationRequirement {
    /// `test_run_after_last_change` (a passing `pax.test` after the last write) or
    /// `grounded_answer` (an inspection answer the host accepts as grounded).
    pub kind: String,
    /// The step that verifies, for a test run.
    pub step: Option<usize>,
    /// What the host must conclude, as it did for every supporting episode.
    pub host_verdict: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Support {
    pub episode: String,
    pub episode_record: String,
    pub result_record: String,
    pub outcome: EpisodeOutcome,
}

/// One `chip.WorkflowCandidate` record. Written once, never changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowCandidate {
    pub schema: String,
    /// `<procedure key>.<support digest>`.
    pub id: String,
    /// The procedure's identity, the same for every candidate of this procedure.
    pub procedure_key: String,
    pub scope: EpisodeScope,
    pub kind: String,
    /// A digest of the goal-term digests the supporting episodes share. Not readable text.
    pub goal_signature: String,
    pub goal_terms: Vec<String>,
    /// The conditions every supporting episode ran under.
    pub preconditions: BTreeMap<String, String>,
    /// Each capability the procedure executes, to its contract digest.
    pub required_capabilities: BTreeMap<String, Option<String>>,
    pub steps: Vec<WorkflowStep>,
    pub verification: Option<VerificationRequirement>,
    pub support: Vec<Support>,
    /// `single_observation` or `repeated`.
    pub strength: String,
    pub unresolved: Vec<Unresolved>,
    /// The most recent supporting episode's time: a fact of the records, so extraction repeats.
    pub created_at_ms: u64,
    pub extracted_by: String,
}

impl WorkflowCandidate {
    /// The (kind, goal, project, conditions) a candidate's procedure answers: candidates that share
    /// it and differ in procedure conflict.
    fn task_key(&self) -> (String, String, String, &BTreeMap<String, String>) {
        (
            self.scope.project.clone(),
            self.kind.clone(),
            self.goal_signature.clone(),
            &self.preconditions,
        )
    }
}

/// Why an episode yields no candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub episode: String,
    pub code: &'static str,
    pub detail: String,
}

/// What [`extract`] proposes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Extraction {
    pub candidates: Vec<WorkflowCandidate>,
    pub rejected: Vec<Rejected>,
}

/// The request one step's observation recorded: its capability and inputs, as recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedStep {
    pub capability: String,
    pub inputs: BTreeMap<String, Value>,
}

fn hex(bytes: &[u8], n: usize) -> String {
    bytes[..n].iter().map(|b| format!("{b:02x}")).collect()
}

fn digest(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    hex(&h.finalize(), 8)
}

fn role(capability: &str) -> &'static str {
    match capability {
        CHANGE_CAPABILITY => "change",
        VERIFY_CAPABILITY => "verify",
        _ => "observe",
    }
}

fn safe_path(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|p| {
            !p.is_empty()
                && p.len() <= 128
                && !p.starts_with('/')
                && !p.split(['/', '\\']).any(|part| part == "..")
        })
        .map(str::to_string)
}

/// One eligible episode's procedure, reconstructed, or why it cannot be.
fn reconstruct(
    episode: &EpisodeRecord,
    recorded: &dyn Fn(&str) -> Option<RecordedStep>,
) -> Result<(Vec<WorkflowStep>, Vec<Unresolved>), (&'static str, String)> {
    if episode.procedure.is_empty() {
        return Err(("no_procedure", "the episode executed nothing".into()));
    }
    if episode.procedure.len() != episode.observation_records.len()
        || episode.executions > episode.procedure.len()
    {
        return Err((
            "procedure_incomplete",
            format!(
                "{} execution(s), {} recorded step(s): the procedure cannot be reconstructed whole",
                episode.executions,
                episode.procedure.len()
            ),
        ));
    }
    let mut steps = Vec::new();
    for (i, (step, record)) in episode
        .procedure
        .iter()
        .zip(&episode.observation_records)
        .enumerate()
    {
        let id = record
            .strip_prefix("chip.Observation:")
            .unwrap_or(record.as_str());
        let Some(request) = recorded(id) else {
            return Err((
                "inputs_unrecorded",
                format!("step {i}: its observation record holds no validated request"),
            ));
        };
        if request.capability != step.capability {
            return Err((
                "inputs_unrecorded",
                format!("step {i}: its observation record names another capability"),
            ));
        }
        let mut inputs = Vec::new();
        for (name, value) in &request.inputs {
            let source = match (name.as_str(), safe_path(value)) {
                ("path", Some(path)) => InputSource::Fixed { value: path },
                ("path", None) => {
                    return Err((
                        "inputs_unrecorded",
                        format!("step {i}: its path is not a project-relative path"),
                    ));
                }
                _ => InputSource::Parameter,
            };
            inputs.push(WorkflowInput {
                name: name.clone(),
                source,
            });
        }
        steps.push(WorkflowStep {
            capability: step.capability.clone(),
            contract: step.contract.clone(),
            role: role(&step.capability).into(),
            inputs,
            expected_status: step.status.clone(),
            evidence: vec![record.clone()],
        });
    }
    let mut unresolved = Vec::new();
    let failed: Vec<usize> = episode
        .procedure
        .iter()
        .enumerate()
        .filter(|(_, s)| s.status != "success")
        .map(|(i, _)| i)
        .collect();
    if !failed.is_empty() {
        unresolved.push(Unresolved {
            code: "failed_steps".into(),
            detail: format!(
                "step(s) {failed:?} failed before the verified result: which earlier steps that result depends on is not established"
            ),
        });
    }
    Ok((steps, unresolved))
}

/// The verification a procedure carries: for a change, a passing test run after its last change;
/// for an inspection, the host's grounded answer.
fn verification_of(
    kind: &str,
    steps: &[WorkflowStep],
    support: &[&EpisodeRecord],
) -> Option<VerificationRequirement> {
    if kind == "inspect" {
        return support
            .iter()
            .all(|e| e.verification.as_ref().is_some_and(|v| v.grounded))
            .then(|| VerificationRequirement {
                kind: "grounded_answer".into(),
                step: None,
                host_verdict: "grounded".into(),
            });
    }
    let last_change = steps.iter().rposition(|s| s.role == "change");
    let verify = steps
        .iter()
        .enumerate()
        .rev()
        .find(|(i, s)| s.role == "verify" && last_change.is_none_or(|c| *i > c))
        .map(|(i, _)| i)?;
    support
        .iter()
        .all(|e| e.verification.as_ref().is_some_and(|v| v.verified))
        .then(|| VerificationRequirement {
            kind: "test_run_after_last_change".into(),
            step: Some(verify),
            host_verdict: "verified".into(),
        })
}

/// Proposes candidates from `episodes`. `recorded` reads one observation record's validated
/// request; `backing` says whether an episode's cited records back it. Deterministic: the same
/// records give the same candidates, in the same order.
pub fn extract(
    episodes: &[EpisodeRecord],
    recorded: &dyn Fn(&str) -> Option<RecordedStep>,
    backing: &dyn Fn(&EpisodeRecord) -> Result<(), String>,
    extracted_by: &str,
) -> Extraction {
    let mut out = Extraction::default();
    let mut sorted: Vec<&EpisodeRecord> = episodes.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    type Group<'a> = (Vec<WorkflowStep>, Vec<Unresolved>, Vec<&'a EpisodeRecord>);
    let mut groups: BTreeMap<String, Group<'_>> = BTreeMap::new();
    for e in sorted {
        let reject = |code: &'static str, detail: String| Rejected {
            episode: e.id.clone(),
            code,
            detail,
        };
        if e.outcome != EpisodeOutcome::VerifiedSuccess {
            out.rejected.push(reject(
                "not_verified_success",
                format!("its outcome is {}", e.outcome.as_str()),
            ));
            continue;
        }
        if !outcome_consistent(e) || e.scope.level != super::PROJECT_SCOPE {
            out.rejected.push(reject(
                "unsupported",
                "its record does not carry the verdict its outcome claims, or is not a project episode".into(),
            ));
            continue;
        }
        if let Err(why) = backing(e) {
            out.rejected.push(reject("evidence_missing", why));
            continue;
        }
        let (steps, unresolved) = match reconstruct(e, recorded) {
            Ok(r) => r,
            Err((code, detail)) => {
                out.rejected.push(reject(code, detail));
                continue;
            }
        };
        let signature: Vec<String> = steps
            .iter()
            .map(|s| {
                let fixed: Vec<String> = s
                    .inputs
                    .iter()
                    .map(|i| match &i.source {
                        InputSource::Fixed { value } => format!("{}={value}", i.name),
                        InputSource::Parameter => format!("{}=?", i.name),
                    })
                    .collect();
                format!(
                    "{}@{}({})={}",
                    s.capability,
                    s.contract.as_deref().unwrap_or("-"),
                    fixed.join(","),
                    s.expected_status
                )
            })
            .collect();
        let conditions: Vec<String> = e
            .conditions
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let key = digest(&[
            WORKFLOW_SCHEMA,
            &e.scope.project,
            &e.kind,
            &e.intent_terms.join(","),
            &conditions.join(";"),
            &signature.join(" > "),
        ]);
        let group = groups
            .entry(format!("wfp1.{key}"))
            .or_insert_with(|| (steps.clone(), Vec::new(), Vec::new()));
        for u in unresolved {
            if !group.1.contains(&u) {
                group.1.push(u);
            }
        }
        if !group.2.is_empty() {
            for (step, theirs) in group.0.iter_mut().zip(&steps) {
                step.evidence.extend(theirs.evidence.iter().cloned());
            }
        }
        group.2.push(e);
    }
    for (procedure_key, (steps, mut unresolved, support)) in groups {
        let first = support[0];
        let verification = verification_of(&first.kind, &steps, &support);
        if verification.is_none() {
            unresolved.push(Unresolved {
                code: "verification_undefined".into(),
                detail: if first.kind == "inspect" {
                    "not every supporting episode's answer was accepted as grounded".into()
                } else {
                    "no passing test run follows the procedure's last change in its supporting record".into()
                },
            });
        }
        unresolved.sort();
        let ids: Vec<&str> = support.iter().map(|e| e.id.as_str()).collect();
        let required_capabilities = steps
            .iter()
            .map(|s| (s.capability.clone(), s.contract.clone()))
            .collect();
        out.candidates.push(WorkflowCandidate {
            schema: WORKFLOW_SCHEMA.into(),
            id: format!("{procedure_key}.{}", digest(&ids)),
            procedure_key,
            scope: first.scope.clone(),
            kind: first.kind.clone(),
            goal_signature: format!("g1.{}", digest(&[&first.intent_terms.join(",")])),
            goal_terms: first.intent_terms.clone(),
            preconditions: first.conditions.clone(),
            required_capabilities,
            steps,
            verification,
            support: support
                .iter()
                .map(|e| Support {
                    episode: e.id.clone(),
                    episode_record: format!("{EPISODE_COLLECTION}:{}", e.id),
                    result_record: e.result_record.clone(),
                    outcome: e.outcome,
                })
                .collect(),
            strength: if support.len() == 1 {
                "single_observation"
            } else {
                "repeated"
            }
            .into(),
            unresolved,
            created_at_ms: support.iter().map(|e| e.created_at_ms).max().unwrap_or(0),
            extracted_by: extracted_by.into(),
        });
    }
    out
}

// ---- validation -------------------------------------------------------------------------------

/// Stable reason codes. A check that fails names one of these.
pub mod codes {
    pub const EVIDENCE_MISSING: &str = "evidence_missing";
    pub const OTHER_PROJECT: &str = "other_project";
    pub const PRECONDITION_UNMET: &str = "precondition_unmet";
    pub const CAPABILITY_UNAVAILABLE: &str = "capability_unavailable";
    pub const CONTRACT_CHANGED: &str = "contract_changed";
    pub const INPUT_UNSATISFIABLE: &str = "input_unsatisfiable";
    pub const VERIFICATION_UNDEFINED: &str = "verification_undefined";
    pub const UNRESOLVED_STEP: &str = "unresolved_step";
    pub const CONFLICTING_PROCEDURE: &str = "conflicting_procedure";
    pub const SUPERSEDED: &str = "superseded";
    pub const TASK_MISMATCH: &str = "task_mismatch";
}

/// What a capability declares now, as validation compares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub contract: String,
    /// Each declared input's name and whether it is required.
    pub inputs: Vec<(String, bool)>,
}

/// The runtime a candidate is validated against.
pub struct Current<'a> {
    pub project: &'a str,
    pub conditions: &'a BTreeMap<String, String>,
    /// The capabilities a work of the candidate's kind is offered now.
    pub capabilities: &'a BTreeMap<String, Declared>,
    /// The task, when one is being considered: its kind and goal-term digests.
    pub task: Option<(&'a str, &'a [String])>,
    pub episode: &'a dyn Fn(&str) -> Option<EpisodeRecord>,
    pub backing: &'a dyn Fn(&EpisodeRecord) -> Result<(), String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationCheck {
    pub check: &'static str,
    /// `passed`, `failed` or `not_evaluated`.
    pub status: &'static str,
    pub code: Option<&'static str>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validation {
    pub candidate: String,
    pub eligible: bool,
    pub checks: Vec<ValidationCheck>,
}

impl Validation {
    pub fn codes(&self) -> Vec<&'static str> {
        self.checks.iter().filter_map(|c| c.code).collect()
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "candidate": self.candidate,
            "eligible_for_consideration": self.eligible,
            "codes": self.codes(),
            "checks": self.checks.iter().map(|c| serde_json::json!({
                "check": c.check, "status": c.status, "code": c.code, "detail": c.detail,
            })).collect::<Vec<_>>(),
            "advisory": "validation checks records and declarations only: it executes nothing, grants no authority and does not promise success; a work that follows the procedure is validated and verified as any other",
        })
    }
}

/// Checks `candidate` against `current`, with `all` the candidates it is judged beside. Reads
/// only; never executes a step.
pub fn validate(
    candidate: &WorkflowCandidate,
    all: &[WorkflowCandidate],
    current: &Current<'_>,
) -> Validation {
    use codes::*;
    let mut checks = Vec::new();
    let mut push = |check: &'static str, failure: Option<(&'static str, String)>, ok: String| {
        checks.push(match failure {
            Some((code, detail)) => ValidationCheck {
                check,
                status: "failed",
                code: Some(code),
                detail,
            },
            None => ValidationCheck {
                check,
                status: "passed",
                code: None,
                detail: ok,
            },
        });
    };
    // Supporting evidence.
    let missing = candidate
        .support
        .iter()
        .find_map(|s| match (current.episode)(&s.episode) {
            None => Some(format!("episode {} is gone or unreadable", s.episode)),
            Some(e) if e.outcome != EpisodeOutcome::VerifiedSuccess || !outcome_consistent(&e) => {
                Some(format!("episode {} is not a verified success", s.episode))
            }
            Some(e) => (current.backing)(&e)
                .err()
                .map(|why| format!("episode {}: {why}", s.episode)),
        });
    push(
        "supporting_evidence",
        missing.map(|d| (EVIDENCE_MISSING, d)),
        format!(
            "{} supporting episode(s), each a verified success its records still back ({})",
            candidate.support.len(),
            candidate.strength
        ),
    );
    // Project and preconditions.
    push(
        "project",
        (candidate.scope.project != current.project || current.project.is_empty())
            .then(|| (OTHER_PROJECT, "another project's procedure".to_string())),
        "this project's procedure".into(),
    );
    let differing = candidate
        .preconditions
        .keys()
        .chain(current.conditions.keys())
        .find(|k| candidate.preconditions.get(*k) != current.conditions.get(*k));
    push(
        "preconditions",
        differing.map(|k| {
            (
                PRECONDITION_UNMET,
                format!("{k} differs from what the supporting episodes ran under"),
            )
        }),
        "the project's conditions are those the supporting episodes ran under".into(),
    );
    // Capabilities and their contracts.
    let mut capability_failure = None;
    for (capability, contract) in &candidate.required_capabilities {
        match current.capabilities.get(capability) {
            None => {
                capability_failure = Some((
                    CAPABILITY_UNAVAILABLE,
                    format!("{capability} is not offered to this kind of work now"),
                ));
                break;
            }
            Some(declared) if contract.as_deref() != Some(declared.contract.as_str()) => {
                capability_failure = Some((
                    CONTRACT_CHANGED,
                    format!("{capability} declares another contract than when the procedure ran"),
                ));
                break;
            }
            Some(_) => {}
        }
    }
    push(
        "capabilities",
        capability_failure,
        "every capability is offered under the contract the procedure ran with".into(),
    );
    // Inputs: the current declarations must accept each step's inputs and require nothing the
    // step does not carry.
    let mut input_failure = None;
    'steps: for (i, step) in candidate.steps.iter().enumerate() {
        let Some(declared) = current.capabilities.get(&step.capability) else {
            continue;
        };
        for input in &step.inputs {
            if !declared.inputs.iter().any(|(n, _)| *n == input.name) {
                input_failure = Some((
                    INPUT_UNSATISFIABLE,
                    format!(
                        "step {i}: {} no longer accepts input {}",
                        step.capability, input.name
                    ),
                ));
                break 'steps;
            }
        }
        for (name, required) in &declared.inputs {
            if *required && !step.inputs.iter().any(|x| &x.name == name) {
                input_failure = Some((
                    INPUT_UNSATISFIABLE,
                    format!(
                        "step {i}: {} requires input {name}, which the procedure does not carry",
                        step.capability
                    ),
                ));
                break 'steps;
            }
        }
    }
    push(
        "inputs",
        input_failure,
        "every step's inputs are accepted by its capability's current declaration; parameters are supplied and validated when a work performs the step".into(),
    );
    // Verification and resolution.
    push(
        "verification",
        candidate.verification.is_none().then(|| {
            (
                VERIFICATION_UNDEFINED,
                "the procedure carries no verification its supporting record establishes"
                    .to_string(),
            )
        }),
        candidate.verification.as_ref().map_or(String::new(), |v| {
            format!("{} (the host must conclude {})", v.kind, v.host_verdict)
        }),
    );
    push(
        "resolution",
        candidate
            .unresolved
            .first()
            .map(|u| (UNRESOLVED_STEP, format!("{}: {}", u.code, u.detail))),
        "nothing about the procedure is unresolved".into(),
    );
    // Other candidates: a different procedure for the same task conflicts; a later candidate for
    // the same procedure with more support supersedes this one.
    let conflicts: BTreeSet<&str> = all
        .iter()
        .filter(|c| {
            c.task_key() == candidate.task_key() && c.procedure_key != candidate.procedure_key
        })
        .map(|c| c.procedure_key.as_str())
        .collect();
    push(
        "conflicts",
        (!conflicts.is_empty()).then(|| {
            (
                CONFLICTING_PROCEDURE,
                format!(
                    "{} other procedure(s) were verified for the same task under the same conditions: {}",
                    conflicts.len(),
                    conflicts.into_iter().collect::<Vec<_>>().join(", ")
                ),
            )
        }),
        "no other procedure was verified for this task under these conditions".into(),
    );
    let newer = all.iter().find(|c| {
        c.procedure_key == candidate.procedure_key
            && c.id != candidate.id
            && (c.support.len(), c.created_at_ms, &c.id)
                > (
                    candidate.support.len(),
                    candidate.created_at_ms,
                    &candidate.id,
                )
    });
    push(
        "current",
        newer.map(|c| {
            (
                SUPERSEDED,
                format!(
                    "candidate {} supports the same procedure with {} episode(s)",
                    c.id,
                    c.support.len()
                ),
            )
        }),
        "the latest candidate for its procedure".into(),
    );
    // The task, when one is given.
    match current.task {
        None => checks.push(ValidationCheck {
            check: "task",
            status: "not_evaluated",
            code: None,
            detail: "no task was given".into(),
        }),
        Some((kind, terms)) => {
            let shared = candidate
                .goal_terms
                .iter()
                .filter(|t| terms.contains(t))
                .count();
            let total = candidate.goal_terms.len() + terms.len();
            let pct = if total == 0 {
                0
            } else {
                (200 * shared / total) as u32
            };
            let mismatch = if kind != candidate.kind {
                Some(format!(
                    "the task is {kind} work, the procedure {}",
                    candidate.kind
                ))
            } else if shared == 0 || pct < MIN_SIMILARITY_PCT {
                Some(format!("the task's goal shares {pct}% of its terms"))
            } else {
                None
            };
            checks.push(match mismatch {
                Some(detail) => ValidationCheck {
                    check: "task",
                    status: "failed",
                    code: Some(TASK_MISMATCH),
                    detail,
                },
                None => ValidationCheck {
                    check: "task",
                    status: "passed",
                    code: None,
                    detail: format!("the same kind of work; {pct}% of goal terms shared"),
                },
            });
        }
    }
    Validation {
        candidate: candidate.id.clone(),
        eligible: checks.iter().all(|c| c.status != "failed"),
        checks,
    }
}

// ---- persistence ------------------------------------------------------------------------------

/// What [`AgentStore::extract_workflows`] did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Extracted {
    pub written: Vec<String>,
    pub already_recorded: Vec<String>,
    pub rejected: Vec<Rejected>,
}

impl AgentStore {
    /// The validated request one observation recorded, if it recorded one. Reads only.
    pub fn recorded_step(&self, id: &str) -> Option<RecordedStep> {
        let (_, observation) = self.observation(id).ok()??;
        let request = observation.request?;
        Some(RecordedStep {
            capability: request.capability,
            inputs: request.inputs,
        })
    }

    /// Extracts candidates from this database's episodes of `project` (see [`extract`]) and
    /// writes each once. Reads episodes and observations; writes only candidates.
    pub fn extract_workflows(
        &self,
        project: &str,
        extracted_by: &str,
    ) -> Result<Extracted, StateError> {
        let (episodes, _) = self.episodes()?;
        let mine: Vec<EpisodeRecord> = episodes
            .into_iter()
            .filter(|e| e.scope.project == project && !project.is_empty())
            .collect();
        let extraction = extract(
            &mine,
            &|id| self.recorded_step(id),
            &|e| self.backing(e),
            extracted_by,
        );
        let mut done = Extracted {
            rejected: extraction.rejected,
            ..Extracted::default()
        };
        for candidate in &extraction.candidates {
            match self.record_workflow(candidate)? {
                EpisodeWrite::Recorded => done.written.push(candidate.id.clone()),
                EpisodeWrite::AlreadyRecorded => done.already_recorded.push(candidate.id.clone()),
            }
        }
        Ok(done)
    }

    /// Writes `candidate` once. The same candidate again is [`EpisodeWrite::AlreadyRecorded`]; a
    /// different one under the same id is refused and the stored one is kept.
    pub fn record_workflow(
        &self,
        candidate: &WorkflowCandidate,
    ) -> Result<EpisodeWrite, StateError> {
        if candidate.schema != WORKFLOW_SCHEMA {
            return Err(StateError::InvalidInput(format!(
                "candidate {} is not a {WORKFLOW_SCHEMA} record",
                candidate.id
            )));
        }
        let same = |stored: WorkflowCandidate| {
            if &stored == candidate {
                Ok(EpisodeWrite::AlreadyRecorded)
            } else {
                Err(StateError::Conflict {
                    operation: "workflow candidate",
                    detail: format!(
                        "a different candidate is already recorded as {}; it is kept, not replaced",
                        candidate.id
                    ),
                })
            }
        };
        if let Some(stored) = self.workflow(&candidate.id)? {
            return same(stored);
        }
        let value = serde_json::to_value(candidate).expect("a candidate serializes");
        match self.create(WORKFLOW_COLLECTION, &candidate.id, value) {
            Ok(()) => Ok(EpisodeWrite::Recorded),
            Err(e) => match self.workflow(&candidate.id)? {
                Some(stored) => same(stored),
                None => Err(e),
            },
        }
    }

    pub fn workflow(&self, id: &str) -> Result<Option<WorkflowCandidate>, StateError> {
        let Some((_, value)) = self.read(WORKFLOW_COLLECTION, id)? else {
            return Ok(None);
        };
        super::schema_of(WORKFLOW_COLLECTION, id, &value, WORKFLOW_SCHEMA)?;
        serde_json::from_value(value)
            .map(Some)
            .map_err(|e| StateError::InvalidHistory {
                task_id: id.to_string(),
                detail: format!("workflow candidate {id} does not parse: {e}"),
            })
    }

    /// Every candidate, by id, and `(id, why)` for each that is not readable.
    pub fn workflows(&self) -> Result<(Vec<WorkflowCandidate>, Vec<(String, String)>), StateError> {
        let rows = self
            .db
            .list_collection(WORKFLOW_COLLECTION)
            .map_err(|e| from_flow("list", e))?;
        let (mut found, mut unreadable) = (Vec::new(), Vec::new());
        for row in rows {
            let Some(id) = row.key.strip_prefix(&format!("{WORKFLOW_COLLECTION}:")) else {
                continue;
            };
            match self.workflow(id) {
                Ok(Some(c)) => found.push(c),
                Ok(None) => {}
                Err(e) => unreadable.push((id.to_string(), e.to_string())),
            }
        }
        found.sort_by(|a, b| a.id.cmp(&b.id));
        Ok((found, unreadable))
    }
}
