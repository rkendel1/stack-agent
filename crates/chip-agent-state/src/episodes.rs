//! Episodes: what one finished attempt of a work established, kept in FeltDB so a later, different
//! work can be told of it (the `chip.Episode` collection).
//!
//! An episode is an index over records FeltDB already holds, not a second journal. It names the
//! attempt's `chip.WorkResult` and `chip.Observation` records instead of copying them, and adds
//! only what recall needs to decide whether it bears on a new work: the scope, the kind of work, a
//! digest of its goal's terms, the conditions it ran under, and the capabilities it executed (with
//! the project-relative path each acted on, if any) and the contract each declared.
//!
//! ```text
//!   finished attempt (FeltDB)                    episode (chip.episode.v1)
//!   ---------------------------------------      -----------------------------------------
//!   chip.WorkResult <task>~a<n> + verdict  --->  outcome, decided here from the host's verdict
//!   chip.Observation <task>~a<n>.<exec>    --->  procedure (capability, contract, status) + refs
//!   _feltdb.AgentTask status               --->  only `completed` can be a verified success
//! ```
//!
//! # The outcome is decided from evidence, never from the model
//!
//! * `verified_success`: FeltDB holds the task `completed` and the host accepted the attempt on
//!   evidence it observed (PAX verified it, or the answer was grounded). A successful execution, or
//!   a model saying it is done, is not enough.
//! * `verified_failure`: the host's evidence establishes the goal was not met when the work ended:
//!   its requirement evaluated unmet with an execution observed failing (a failing PAX run, a
//!   failed write), or the evidence it relied on was refused. A work that merely stopped with its
//!   goal unmet and nothing observed failing is `unknown`: giving up is not evidence of failure.
//! * `interrupted`: the work was stopped (cancelled, a deadline) before it finished.
//! * `unknown`: everything else, including an action whose outcome was never recorded.
//!
//! An attempt with no recorded result (its process ended mid-work) gets no episode at all: nothing
//! is fabricated for it. No model text is stored: an episode holds no summary, no explanation and
//! no lesson, and no goal text (its terms are kept only as digests), file content, command output,
//! absolute path or credential.
//!
//! # Recall
//!
//! [`select`] is deterministic and inspectable: a pure function of the stored episodes and the
//! query, which says why each episode it leaves out was left out. Only a verified outcome of the
//! same project and kind, whose goal shares enough terms, that ran under the same conditions, and
//! whose every capability is still offered under the contract it was executed with, is recalled;
//! at most [`chip_core::MAX_RECALLED_EPISODES`]. [`AgentStore::recall`] then checks each against
//! the records it cites: an episode FeltDB does not back is not recalled.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    AgentStore, History, OBSERVATION_COLLECTION, RESULT_COLLECTION, StateError, Verification,
    WorkResultRecord, event_kind, from_flow,
};

pub const EPISODE_COLLECTION: &str = "chip.Episode";
pub const EPISODE_SCHEMA: &str = "chip.episode.v1";
/// The only scope this build records and recalls. An episode of any other level is never recalled.
pub const PROJECT_SCOPE: &str = "project";
/// How long an episode can be recalled: the support window learning uses (180 days).
pub const EPISODE_VALID_MS: u64 = 180 * 24 * 60 * 60 * 1000;
pub const MAX_INTENT_TERMS: usize = 32;
pub const MAX_PROCEDURE_STEPS: usize = 16;
pub const MAX_EPISODE_OBSERVATIONS: usize = 16;
/// The least share of goal terms (Dice coefficient, in percent) a recalled episode has in common
/// with the work.
pub const MIN_SIMILARITY_PCT: u32 = 50;

/// What a finished attempt established, as decided from the host's evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeOutcome {
    VerifiedSuccess,
    VerifiedFailure,
    Interrupted,
    Unknown,
}

impl EpisodeOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VerifiedSuccess => "verified_success",
            Self::VerifiedFailure => "verified_failure",
            Self::Interrupted => "interrupted",
            Self::Unknown => "unknown",
        }
    }

    pub fn is_verified(self) -> bool {
        matches!(self, Self::VerifiedSuccess | Self::VerifiedFailure)
    }
}

/// Whose experience it is. Project episodes are recalled only into works of the same project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpisodeScope {
    pub level: String,
    pub project: String,
}

/// One execution of the attempt, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcedureStep {
    pub capability: String,
    /// The request's `path` input, when it had one that is project-relative (at most 128 bytes).
    /// No other input is kept: never a file's content, a command or an absolute path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The digest of the contract the capability declared when the episode was recorded; `None`
    /// when it was not offered then (such a step makes the episode stale).
    pub contract: Option<String>,
    /// The observed status (`success`, `failure`, `cancelled`, `error`).
    pub status: String,
}

/// One `chip.Episode` record. Written once, never changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpisodeRecord {
    pub schema: String,
    /// `<task>~a<attempt>`: one episode per finished attempt.
    pub id: String,
    pub scope: EpisodeScope,
    pub task_id: String,
    pub attempt: u64,
    pub work_id: String,
    /// The kind of work (`change`, `verify`, `inspect`).
    pub kind: String,
    /// Salted digests of the goal's terms ([`intent_terms`]), sorted. Never the terms themselves.
    pub intent_terms: Vec<String>,
    /// The conditions the work ran under, as the host stated them (build systems, tests present,
    /// environment, runtime line).
    pub conditions: BTreeMap<String, String>,
    pub procedure: Vec<ProcedureStep>,
    /// The attempt's `chip.WorkResult` record.
    pub result_record: String,
    /// The attempt's `chip.Observation` records, in execution order.
    pub observation_records: Vec<String>,
    pub turns: usize,
    pub executions: usize,
    pub model_calls: u64,
    pub outcome: EpisodeOutcome,
    /// Why the outcome is what it is, in Chip's words, from the verdict and the task status.
    pub outcome_basis: String,
    /// The host's verdict, as the result record holds it.
    pub verification: Option<Verification>,
    /// Observation records of executions observed failing (status `failure`).
    pub failure_evidence: Vec<String>,
    /// Episodes of earlier attempts of the same task.
    pub related: Vec<String>,
    /// Episodes this attempt was told of when it started. They decided nothing about its outcome.
    pub recalled: Vec<String>,
    /// When the attempt's terminal checkpoint was committed.
    pub created_at_ms: u64,
    pub recorded_by: String,
}

/// What the host knows about a work that FeltDB's history does not hold. The same value describes
/// a finished work (to record its episode) and a new one (to recall for it).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EpisodeContext {
    /// The project's identity. Never a path.
    pub project: String,
    pub kind: String,
    /// [`intent_terms`] of the goal.
    pub intent_terms: Vec<String>,
    pub conditions: BTreeMap<String, String>,
    /// Each capability the work offers, to the digest of the contract it declares.
    pub contracts: BTreeMap<String, String>,
    pub recorded_by: String,
}

/// Whether recording wrote the episode or found the same one already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpisodeWrite {
    Recorded,
    AlreadyRecorded,
}

const STOP_WORDS: &[&str] = &[
    "and", "are", "but", "can", "does", "for", "from", "has", "have", "how", "into", "its", "not",
    "that", "the", "then", "this", "was", "what", "when", "which", "with", "you", "your",
];

/// The goal's terms, each as a salted digest: lowercased words of 3 to 32 letters, digits or
/// underscores, without a few function words, distinct and sorted, at most [`MAX_INTENT_TERMS`].
/// Two goals that share a word share its digest; the word itself is not kept.
pub fn intent_terms(goal: &str) -> Vec<String> {
    let mut terms: Vec<String> = goal
        .to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|w| (3..=32).contains(&w.chars().count()) && !STOP_WORDS.contains(w))
        .map(|w| {
            let mut hasher = Sha256::new();
            hasher.update(b"chip.episode.term.v1\0");
            hasher.update(w.as_bytes());
            let hex: String = hasher.finalize()[..6]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("t{hex}")
        })
        .collect();
    terms.sort();
    terms.dedup();
    terms.truncate(MAX_INTENT_TERMS);
    terms
}

/// The episode outcome of an attempt, from the host's verdict, the task status FeltDB holds and
/// how many of the attempt's executions were observed failing, with the reason.
pub fn classify(
    task_status: &str,
    unresolved: u64,
    result: &WorkResultRecord,
    observed_failures: usize,
) -> (EpisodeOutcome, String) {
    use EpisodeOutcome::*;
    if result.outcome == "cancelled" || task_status == "cancelled" {
        return (
            Interrupted,
            format!("the work was stopped before it finished: {}", result.detail),
        );
    }
    if unresolved > 0 {
        return (
            Unknown,
            format!("{unresolved} action(s) of the task have no recorded outcome"),
        );
    }
    if !result.persistence_failures.is_empty() {
        return (
            Unknown,
            "the attempt was not recorded whole in FeltDB".to_string(),
        );
    }
    let Some(v) = &result.verification else {
        return (Unknown, "no host verdict was recorded".to_string());
    };
    if task_status == "completed" && v.accepted && (v.verified || v.grounded) {
        let how = if v.verified {
            "the host verified it on observed test evidence"
        } else {
            "the host accepted an answer grounded in observations"
        };
        return (
            VerifiedSuccess,
            format!("FeltDB holds the task completed and {how}"),
        );
    }
    if let Some(why) = &v.evidence_invalidated {
        return (
            VerifiedFailure,
            format!("the host refused the evidence the work relied on: {why}"),
        );
    }
    if v.goal_satisfied == Some(false) && observed_failures > 0 {
        return (
            VerifiedFailure,
            format!(
                "the goal's requirement was unmet when the work ended ({}), with {observed_failures} execution(s) observed failing",
                result.outcome
            ),
        );
    }
    (
        Unknown,
        format!(
            "the work ended {} and nothing the host observed established either outcome",
            result.outcome
        ),
    )
}

/// Why `episode` could never be recalled at `now_ms`, whatever work asked, from the record alone:
/// its scope level is not `project`, its outcome is not verified (or its record does not carry the
/// verdict its outcome needs), or it expired. `None` when the record itself allows recall; the
/// work's project, kind, goal, conditions and capability contracts, and the records the episode
/// cites ([`AgentStore::backing`]), decide the rest.
pub fn unrecallable(episode: &EpisodeRecord, now_ms: u64) -> Option<Exclusion> {
    if episode.scope.level != PROJECT_SCOPE {
        Some(Exclusion::OtherScope)
    } else if !episode.outcome.is_verified() || !outcome_consistent(episode) {
        Some(Exclusion::Unverified {
            outcome: episode.outcome,
        })
    } else if now_ms.saturating_sub(episode.created_at_ms) > EPISODE_VALID_MS {
        Some(Exclusion::Expired)
    } else {
        None
    }
}

/// Which stored condition differs from `conditions`, if any: the check recall makes.
pub fn differing_condition(
    episode: &EpisodeRecord,
    conditions: &BTreeMap<String, String>,
) -> Option<String> {
    episode
        .conditions
        .keys()
        .chain(conditions.keys())
        .find(|k| episode.conditions.get(*k) != conditions.get(*k))
        .cloned()
}

fn scope_ok(record: &EpisodeRecord, project: &str) -> bool {
    record.scope.level == PROJECT_SCOPE && record.scope.project == project && !project.is_empty()
}

/// Whether a stored record carries what its outcome claims: a verified success must carry an
/// accepted verdict, a verified failure one that establishes failure.
fn outcome_consistent(record: &EpisodeRecord) -> bool {
    match (record.outcome, &record.verification) {
        (EpisodeOutcome::VerifiedSuccess, Some(v)) => v.accepted && (v.verified || v.grounded),
        (EpisodeOutcome::VerifiedFailure, Some(v)) => {
            (v.goal_satisfied == Some(false) && !record.failure_evidence.is_empty())
                || v.evidence_invalidated.is_some()
        }
        (EpisodeOutcome::VerifiedSuccess | EpisodeOutcome::VerifiedFailure, None) => false,
        _ => true,
    }
}

/// Why an episode was not recalled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exclusion {
    /// The record does not parse, or is of another schema.
    Unreadable(String),
    /// Another project's, or another scope's. Its id is not reported.
    OtherScope,
    /// An earlier attempt of this same task (its own trajectory is restored, not recalled).
    SameTask,
    OtherKind,
    /// Too few goal terms in common.
    Dissimilar {
        similarity_pct: u32,
    },
    /// A condition it ran under is not this work's.
    PreconditionsDiffer {
        condition: String,
    },
    /// A capability it executed is not offered now, or is offered under another contract.
    StaleProcedure {
        capability: String,
    },
    /// Its outcome is not verified, or its record does not carry the verdict its outcome needs.
    Unverified {
        outcome: EpisodeOutcome,
    },
    /// Older than [`EPISODE_VALID_MS`].
    Expired,
    /// The records it cites do not back it in FeltDB.
    Unbacked(String),
    /// Relevant, but beyond the recall limit.
    OverLimit,
}

/// The four questions recall asks of an episode, in the order it asks them. The first one an
/// episode fails decides why it is not recalled.
///
/// * **Hard exclusion**: it must never be recalled into this work, whatever else holds: its record
///   is unreadable, it is another project's or scope's, it is this task's own earlier attempt, it
///   is another kind of work, or its outcome does not establish a verified result.
/// * **Evidence quality**: the records it cites no longer back it, or it has expired.
/// * **Relevance**: whether it addresses this goal (shared goal terms).
/// * **Context compatibility**: whether what it did is likely to apply here: the conditions it ran
///   under (build systems, tests present, the project's test definitions, verifier, runtime line)
///   and the contracts of the capabilities it executed.
///
/// Passing all four does not make an episode's procedure correct now. A verified episode proves
/// that one attempt passed the verification it had then; it is recalled as evidence of that, and
/// this work still has to earn its own verified result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dimension {
    HardExclusion,
    EvidenceQuality,
    Relevance,
    ContextCompatibility,
    /// Eligible, but beyond the bounded recall limit.
    RecallLimit,
}

impl Dimension {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HardExclusion => "hard_exclusion",
            Self::EvidenceQuality => "evidence_quality",
            Self::Relevance => "relevance",
            Self::ContextCompatibility => "context_compatibility",
            Self::RecallLimit => "recall_limit",
        }
    }
}

/// What one condition differing means, in words that name no goal content and no path.
fn condition_reason(condition: &str) -> String {
    match condition {
        "build_systems" => "the project's build systems differ from when it was verified".into(),
        "tests" => "whether the project has tests differs from when it was verified".into(),
        "test_definitions" => "the project's test definitions changed since it was verified: it passed different tests".into(),
        "verifier" => "the verifier binary differs from the one that verified it".into(),
        "runtime_line" => "it was recorded by another stack-agent release line".into(),
        other => format!("it ran under another {other}"),
    }
}

impl Exclusion {
    pub fn dimension(&self) -> Dimension {
        match self {
            Self::Unreadable(_)
            | Self::OtherScope
            | Self::SameTask
            | Self::OtherKind
            | Self::Unverified { .. } => Dimension::HardExclusion,
            Self::Expired | Self::Unbacked(_) => Dimension::EvidenceQuality,
            Self::Dissimilar { .. } => Dimension::Relevance,
            Self::PreconditionsDiffer { .. } | Self::StaleProcedure { .. } => {
                Dimension::ContextCompatibility
            }
            Self::OverLimit => Dimension::RecallLimit,
        }
    }

    /// A stable code for reports and tests.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unreadable(_) => "unreadable",
            Self::OtherScope => "other_scope",
            Self::SameTask => "same_task",
            Self::OtherKind => "other_kind",
            Self::Dissimilar { .. } => "dissimilar",
            Self::PreconditionsDiffer { .. } => "preconditions_differ",
            Self::StaleProcedure { .. } => "stale_procedure",
            Self::Unverified { .. } => "unverified",
            Self::Expired => "expired",
            Self::Unbacked(_) => "unbacked",
            Self::OverLimit => "over_limit",
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Unreadable(why) => format!("unreadable: {why}"),
            Self::OtherScope => "another scope".into(),
            Self::SameTask => "an earlier attempt of this task".into(),
            Self::OtherKind => "another kind of work".into(),
            Self::Dissimilar { similarity_pct } => {
                format!("dissimilar goal ({similarity_pct}% of terms shared)")
            }
            Self::PreconditionsDiffer { condition } => condition_reason(condition),
            Self::StaleProcedure { capability } => {
                format!("stale procedure: {capability} is not offered under the same contract")
            }
            Self::Unverified { outcome } => format!("unverified outcome ({})", outcome.as_str()),
            Self::Expired => "expired".into(),
            Self::Unbacked(why) => format!("not backed by FeltDB: {why}"),
            Self::OverLimit => "beyond the recall limit".into(),
        }
    }
}

/// One episode recalled for a work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recalled {
    pub episode: EpisodeRecord,
    pub shared_terms: usize,
    pub similarity_pct: u32,
    /// Other recalled episodes that reached a verified success by a different final write.
    pub conflicts_with: Vec<String>,
}

impl Recalled {
    /// What Chip states of it to the model, from its record alone.
    pub fn statement(&self) -> String {
        let e = &self.episode;
        let steps: Vec<String> = e
            .procedure
            .iter()
            .take(8)
            .map(|s| match &s.target {
                Some(target) => format!("{} {target} ({})", s.capability, s.status),
                None => format!("{} ({})", s.capability, s.status),
            })
            .collect();
        let steps = if steps.is_empty() {
            "nothing".to_string()
        } else {
            let more = e.procedure.len().saturating_sub(8);
            let mut text = steps.join(" -> ");
            if more > 0 {
                text.push_str(&format!(" and {more} more"));
            }
            text
        };
        let head = match e.outcome {
            EpisodeOutcome::VerifiedSuccess => "verified success",
            EpisodeOutcome::VerifiedFailure => "verified failure",
            other => other.as_str(),
        };
        // What the record cannot show, and any conflict, come first: a statement is clipped at
        // its end.
        let conflicts: String = self
            .conflicts_with
            .iter()
            .map(|other| {
                format!("; it conflicts with recalled episode {EPISODE_COLLECTION}:{other}, which ended with a different write")
            })
            .collect();
        format!(
            "{head} (it passed the verification it had then; that is not proof the procedure applies now{conflicts}): an earlier {} work in this project whose goal shares {} of its terms ({}%) executed, in order, {steps}; {} turn(s), {} execution(s). {}.",
            e.kind, self.shared_terms, self.similarity_pct, e.turns, e.executions, e.outcome_basis
        )
    }

    pub fn to_recalled_episode(&self) -> chip_core::RecalledEpisode {
        chip_core::RecalledEpisode {
            record: format!("{EPISODE_COLLECTION}:{}", self.episode.id),
            statement: self.statement(),
        }
    }
}

/// How one dimension of an [`Eligibility`] came out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Passed(String),
    Failed(Exclusion),
    /// Not asked: an earlier dimension already excluded the episode, or (when inspecting outside a
    /// work) it depends on a work that has not started.
    NotEvaluated(&'static str),
}

impl Check {
    fn to_json(&self) -> Value {
        match self {
            Self::Passed(why) => serde_json::json!({"status": "passed", "reason": why}),
            Self::Failed(e) => serde_json::json!({
                "status": "failed", "code": e.code(), "reason": e.describe(),
            }),
            Self::NotEvaluated(why) => {
                serde_json::json!({"status": "not_evaluated", "reason": why})
            }
        }
    }
}

/// Recall's decision about one episode, dimension by dimension (see [`Dimension`]). The id is
/// `None` for another project's episode, which is never named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Eligibility {
    pub episode: Option<String>,
    pub hard_exclusion: Check,
    pub evidence_quality: Check,
    pub relevance: Check,
    pub context_compatibility: Check,
    /// `None`: eligible. Otherwise the first failing dimension's exclusion.
    pub excluded: Option<Exclusion>,
}

impl Eligibility {
    pub fn eligible(&self) -> bool {
        self.excluded.is_none()
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "episode": self.episode,
            "decision": match &self.excluded {
                None => "eligible",
                Some(e) if e.dimension() == Dimension::RecallLimit => "eligible_beyond_limit",
                Some(_) => "excluded",
            },
            "dimension": self.excluded.as_ref().map(|e| e.dimension().as_str()),
            "code": self.excluded.as_ref().map(Exclusion::code),
            "reason": match &self.excluded {
                None => "every check passed; recalled as evidence of a past verified result, not as an instruction".to_string(),
                Some(e) => e.describe(),
            },
            "checks": {
                "hard_exclusion": self.hard_exclusion.to_json(),
                "evidence_quality": self.evidence_quality.to_json(),
                "relevance": self.relevance.to_json(),
                "context_compatibility": self.context_compatibility.to_json(),
            },
        })
    }
}

/// What the assessment knows about the work asking. Outside a work (inspection), the kind, goal
/// and capability contracts are unknown and those checks are not evaluated.
#[derive(Debug, Clone, Copy)]
pub struct Asking<'a> {
    pub context: &'a EpisodeContext,
    /// The task asking, whose own episodes are never recalled into it.
    pub task_id: Option<&'a str>,
    /// Whether `context.kind`, `context.intent_terms` and `context.contracts` describe a work.
    pub work_known: bool,
}

const NOT_YET: &str = "decided when a work starts";
const AFTER_EXCLUSION: &str = "not asked: an earlier check excluded it";

/// The goal-term overlap of `episode` with `terms`: (shared, Dice coefficient in percent).
fn similarity(episode: &EpisodeRecord, terms: &[String]) -> (usize, u32) {
    let shared = episode
        .intent_terms
        .iter()
        .filter(|t| terms.contains(t))
        .count();
    let total = episode.intent_terms.len() + terms.len();
    let pct = if total == 0 {
        0
    } else {
        (200 * shared / total) as u32
    };
    (shared, pct)
}

/// Recall's four checks of `episode` for `asking` at `now_ms`, with `backing` deciding whether the
/// records it cites back it. Deterministic, and it reads nothing but what `backing` reads.
pub fn assess(
    episode: &EpisodeRecord,
    asking: &Asking<'_>,
    now_ms: u64,
    backing: &dyn Fn(&EpisodeRecord) -> Result<(), String>,
) -> Eligibility {
    let query = asking.context;
    let mut out = Eligibility {
        episode: Some(episode.id.clone()),
        hard_exclusion: Check::NotEvaluated(AFTER_EXCLUSION),
        evidence_quality: Check::NotEvaluated(AFTER_EXCLUSION),
        relevance: Check::NotEvaluated(AFTER_EXCLUSION),
        context_compatibility: Check::NotEvaluated(AFTER_EXCLUSION),
        excluded: None,
    };
    // Hard exclusions.
    let hard = if !scope_ok(episode, &query.project) {
        out.episode = None;
        Some(Exclusion::OtherScope)
    } else if asking.task_id == Some(episode.task_id.as_str()) {
        Some(Exclusion::SameTask)
    } else if asking.work_known && episode.kind != query.kind {
        Some(Exclusion::OtherKind)
    } else if !episode.outcome.is_verified() || !outcome_consistent(episode) {
        Some(Exclusion::Unverified {
            outcome: episode.outcome,
        })
    } else {
        None
    };
    if let Some(e) = hard {
        out.hard_exclusion = Check::Failed(e.clone());
        out.excluded = Some(e);
        return out;
    }
    out.hard_exclusion = Check::Passed(format!(
        "this project's {} episode with a {} outcome{}",
        episode.kind,
        episode.outcome.as_str(),
        if asking.work_known {
            ", of the same kind of work"
        } else {
            ""
        }
    ));
    // Evidence quality.
    let evidence = if now_ms.saturating_sub(episode.created_at_ms) > EPISODE_VALID_MS {
        Some(Exclusion::Expired)
    } else {
        backing(episode).err().map(Exclusion::Unbacked)
    };
    if let Some(e) = evidence {
        out.evidence_quality = Check::Failed(e.clone());
        out.excluded = Some(e);
        return out;
    }
    out.evidence_quality = Check::Passed(
        "its result record exists, is its attempt's and carries its verdict; not expired".into(),
    );
    // Relevance.
    if asking.work_known {
        let (shared, pct) = similarity(episode, &query.intent_terms);
        if shared == 0 || pct < MIN_SIMILARITY_PCT {
            let e = Exclusion::Dissimilar {
                similarity_pct: pct,
            };
            out.relevance = Check::Failed(e.clone());
            out.excluded = Some(e);
            return out;
        }
        out.relevance = Check::Passed(format!("{shared} goal terms shared ({pct}%)"));
    } else {
        out.relevance = Check::NotEvaluated(NOT_YET);
    }
    // Context compatibility.
    if let Some(condition) = differing_condition(episode, &query.conditions) {
        let e = Exclusion::PreconditionsDiffer { condition };
        out.context_compatibility = Check::Failed(e.clone());
        out.excluded = Some(e);
        return out;
    }
    if asking.work_known {
        let stale = episode.procedure.iter().find(|step| {
            step.contract.is_none()
                || query.contracts.get(&step.capability) != step.contract.as_ref()
        });
        if let Some(step) = stale {
            let e = Exclusion::StaleProcedure {
                capability: step.capability.clone(),
            };
            out.context_compatibility = Check::Failed(e.clone());
            out.excluded = Some(e);
            return out;
        }
        out.context_compatibility = Check::Passed(
            "the same conditions, and every capability it executed is offered under the same contract"
                .into(),
        );
    } else {
        out.context_compatibility = Check::Passed(
            "the same conditions as this project now; capability contracts are compared when a work starts"
                .into(),
        );
    }
    out
}

/// The target of the last successful write of a verified success's procedure: what a work
/// following it would write. `None` for anything else.
pub fn final_write(episode: &EpisodeRecord) -> Option<&str> {
    if episode.outcome != EpisodeOutcome::VerifiedSuccess {
        return None;
    }
    episode
        .procedure
        .iter()
        .rev()
        .find(|s| s.capability == "project.write" && s.status == "success")
        .and_then(|s| s.target.as_deref())
}

/// What recall found, and why each episode it considered was or was not recalled.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Recall {
    pub considered: usize,
    pub recalled: Vec<Recalled>,
    /// `(episode id, why)`. An episode of another scope is listed without its id.
    pub excluded: Vec<(Option<String>, Exclusion)>,
    /// Every considered episode's decision, dimension by dimension, in the order considered.
    pub eligibility: Vec<Eligibility>,
    /// Recall could not be made; nothing was recalled and the work runs as without it.
    pub failed: Option<String>,
}

impl Recall {
    pub fn failed(why: String) -> Self {
        Self {
            failed: Some(why),
            ..Self::default()
        }
    }

    pub fn to_json(&self) -> Value {
        let mut excluded: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_dimension: BTreeMap<String, usize> = BTreeMap::new();
        for (_, why) in &self.excluded {
            *excluded.entry(why.code().to_string()).or_default() += 1;
            *by_dimension
                .entry(why.dimension().as_str().to_string())
                .or_default() += 1;
        }
        // Bounded: the first decisions in full; every one is counted above.
        const SHOWN: usize = 16;
        serde_json::json!({
            "considered": self.considered,
            "recalled": self.recalled.iter().map(|r| serde_json::json!({
                "episode": r.episode.id,
                "outcome": r.episode.outcome.as_str(),
                "similarity_pct": r.similarity_pct,
                "record": format!("{EPISODE_COLLECTION}:{}", r.episode.id),
                "conflicts_with": r.conflicts_with,
            })).collect::<Vec<_>>(),
            "excluded": excluded,
            "excluded_by_dimension": by_dimension,
            "eligibility": self.eligibility.iter().take(SHOWN).map(Eligibility::to_json).collect::<Vec<_>>(),
            "eligibility_shown": self.eligibility.len().min(SHOWN),
            "failed": self.failed,
        })
    }
}

/// Recall over `episodes` (with the ids of records that did not parse) for a work of `task_id`
/// described by `query`, at `now_ms`. Deterministic: the same records and query give the same
/// answer. Ranked by share of goal terms, then the most recent, then the id.
pub fn select(
    episodes: Vec<EpisodeRecord>,
    unreadable: Vec<(String, String)>,
    task_id: &str,
    query: &EpisodeContext,
    now_ms: u64,
    limit: usize,
    backing: &dyn Fn(&EpisodeRecord) -> Result<(), String>,
) -> Recall {
    // Considered in id order, so the answer does not depend on the order the records were read.
    let mut episodes = episodes;
    episodes.sort_by(|a, b| a.id.cmp(&b.id));
    let mut unreadable = unreadable;
    unreadable.sort();
    let mut recall = Recall {
        considered: episodes.len() + unreadable.len(),
        ..Recall::default()
    };
    for (id, why) in unreadable {
        let e = Exclusion::Unreadable(why);
        recall.eligibility.push(Eligibility {
            episode: Some(id.clone()),
            hard_exclusion: Check::Failed(e.clone()),
            evidence_quality: Check::NotEvaluated(AFTER_EXCLUSION),
            relevance: Check::NotEvaluated(AFTER_EXCLUSION),
            context_compatibility: Check::NotEvaluated(AFTER_EXCLUSION),
            excluded: Some(e.clone()),
        });
        recall.excluded.push((Some(id), e));
    }
    let asking = Asking {
        context: query,
        task_id: Some(task_id),
        work_known: true,
    };
    let mut candidates = Vec::new();
    for episode in episodes {
        let decision = assess(&episode, &asking, now_ms, backing);
        if let Some(e) = &decision.excluded {
            recall.excluded.push((decision.episode.clone(), e.clone()));
            recall.eligibility.push(decision);
            continue;
        }
        let (shared, similarity_pct) = similarity(&episode, &query.intent_terms);
        candidates.push((
            Recalled {
                episode,
                shared_terms: shared,
                similarity_pct,
                conflicts_with: Vec::new(),
            },
            decision,
        ));
    }
    candidates.sort_by(|(a, _), (b, _)| {
        b.similarity_pct
            .cmp(&a.similarity_pct)
            .then(b.episode.created_at_ms.cmp(&a.episode.created_at_ms))
            .then(a.episode.id.cmp(&b.episode.id))
    });
    for (n, (candidate, mut decision)) in candidates.into_iter().enumerate() {
        if n < limit {
            recall.recalled.push(candidate);
        } else {
            decision.excluded = Some(Exclusion::OverLimit);
            recall
                .excluded
                .push((Some(candidate.episode.id.clone()), Exclusion::OverLimit));
        }
        recall.eligibility.push(decision);
    }
    // Two recalled successes that ended with different writes cannot both apply: each says so,
    // naming the other. Deterministic: the recalled order is.
    let finals: Vec<Option<String>> = recall
        .recalled
        .iter()
        .map(|r| final_write(&r.episode).map(str::to_string))
        .collect();
    for i in 0..recall.recalled.len() {
        for j in 0..recall.recalled.len() {
            if i != j
                && let (Some(a), Some(b)) = (&finals[i], &finals[j])
                && a != b
            {
                let other = recall.recalled[j].episode.id.clone();
                recall.recalled[i].conflicts_with.push(other);
            }
        }
    }
    recall
}

impl AgentStore {
    /// Every episode in the database, and `(id, why)` for each record that is not a readable
    /// `chip.episode.v1` episode.
    pub fn episodes(&self) -> Result<(Vec<EpisodeRecord>, Vec<(String, String)>), StateError> {
        let rows = self
            .db
            .list_collection(EPISODE_COLLECTION)
            .map_err(|e| from_flow("list", e))?;
        let (mut episodes, mut unreadable) = (Vec::new(), Vec::new());
        for row in rows {
            let Some(id) = row.key.strip_prefix(&format!("{EPISODE_COLLECTION}:")) else {
                continue;
            };
            match self.episode(id) {
                Ok(Some(e)) => episodes.push(e),
                Ok(None) => {}
                Err(e) => unreadable.push((id.to_string(), e.to_string())),
            }
        }
        Ok((episodes, unreadable))
    }

    pub fn episode(&self, id: &str) -> Result<Option<EpisodeRecord>, StateError> {
        let Some((_, value)) = self.read(EPISODE_COLLECTION, id)? else {
            return Ok(None);
        };
        super::schema_of(EPISODE_COLLECTION, id, &value, EPISODE_SCHEMA)?;
        let record: EpisodeRecord =
            serde_json::from_value(value).map_err(|e| StateError::InvalidHistory {
                task_id: id.split('~').next().unwrap_or_default().to_string(),
                detail: format!("episode {id} does not parse: {e}"),
            })?;
        if record.id != id {
            return Err(StateError::InvalidHistory {
                task_id: record.task_id,
                detail: format!("episode {id} names {}", record.id),
            });
        }
        Ok(Some(record))
    }

    /// The episode of `attempt` of the task whose durable `history` this is, as `context`
    /// describes the work, having been told of `recalled`. An attempt with no recorded result has
    /// not finished and has no episode: that is an error, never an invented outcome.
    pub fn episode_of(
        &self,
        history: &History,
        attempt: u64,
        context: &EpisodeContext,
        recalled: &[String],
    ) -> Result<EpisodeRecord, StateError> {
        let task_id = history.task.task_id.clone();
        let result = history
            .results
            .iter()
            .find(|r| r.attempt == attempt)
            .ok_or_else(|| {
                StateError::InvalidInput(format!(
                    "attempt {attempt} of task {task_id} has no recorded result: it has not finished, and no episode is made for it"
                ))
            })?;
        let id = format!("{task_id}~a{attempt}");
        let result_id = id.clone();
        let (mut procedure, mut observation_records, mut failure_evidence) =
            (Vec::new(), Vec::new(), Vec::new());
        let mut observed_failures = 0;
        for entry in &result.executions {
            let observation_id = format!("{task_id}~{}", entry.intent_key);
            let Some((_, observation)) = self.observation(&observation_id)? else {
                continue;
            };
            let capability = observation
                .request
                .as_ref()
                .map_or_else(|| "unrecorded".to_string(), |r| r.capability.clone());
            let target = observation
                .request
                .as_ref()
                .and_then(|r| r.inputs.get("path"))
                .and_then(Value::as_str)
                .filter(|p| {
                    !p.is_empty()
                        && p.len() <= 128
                        && !p.starts_with('/')
                        && !p.split(['/', '\\']).any(|part| part == "..")
                })
                .map(str::to_string);
            if observation.status == "failure" {
                observed_failures += 1;
                if failure_evidence.len() < MAX_EPISODE_OBSERVATIONS {
                    failure_evidence.push(format!("{OBSERVATION_COLLECTION}:{observation_id}"));
                }
            }
            if procedure.len() < MAX_PROCEDURE_STEPS {
                procedure.push(ProcedureStep {
                    contract: context.contracts.get(&capability).cloned(),
                    capability,
                    target,
                    status: observation.status.clone(),
                });
            }
            if observation_records.len() < MAX_EPISODE_OBSERVATIONS {
                observation_records.push(format!("{OBSERVATION_COLLECTION}:{observation_id}"));
            }
        }
        let (outcome, outcome_basis) = classify(
            &history.task.status,
            history.task.unresolved_intents,
            result,
            observed_failures,
        );
        let model_calls = history
            .events
            .iter()
            .filter(|e| e.attempt == attempt && event_kind(e) == "ModelCalled")
            .count() as u64;
        // When the attempt's terminal checkpoint, the one citing its result, was committed: a
        // fact of the record, so recording the same attempt again writes the same episode.
        let created_at_ms = history
            .checkpoints
            .iter()
            .find(|c| {
                c.evidence.iter().any(|cite| {
                    cite.collection.as_deref() == Some(RESULT_COLLECTION)
                        && cite.id.as_deref() == Some(result_id.as_str())
                })
            })
            .map_or(history.task.updated_at_ms, |c| c.created_at_ms);
        let mut related: Vec<String> = self
            .ids(EPISODE_COLLECTION, &task_id)?
            .into_iter()
            .filter(|other| *other != id)
            .collect();
        related.sort();
        let mut recalled = recalled.to_vec();
        recalled.sort();
        recalled.dedup();
        Ok(EpisodeRecord {
            schema: EPISODE_SCHEMA.into(),
            id,
            scope: EpisodeScope {
                level: PROJECT_SCOPE.into(),
                project: context.project.clone(),
            },
            task_id,
            attempt,
            work_id: result.work_id.clone(),
            kind: context.kind.clone(),
            intent_terms: context.intent_terms.clone(),
            conditions: context.conditions.clone(),
            procedure,
            result_record: format!("{RESULT_COLLECTION}:{result_id}"),
            observation_records,
            turns: result.turns,
            executions: result.executions.len(),
            model_calls,
            outcome,
            outcome_basis,
            verification: result.verification.clone(),
            failure_evidence,
            related,
            recalled,
            created_at_ms,
            recorded_by: context.recorded_by.clone(),
        })
    }

    /// Writes `episode` once. The same episode again is [`EpisodeWrite::AlreadyRecorded`]; a
    /// different one under the same id is refused and the stored one is kept.
    pub fn record_episode(&self, episode: &EpisodeRecord) -> Result<EpisodeWrite, StateError> {
        if episode.schema != EPISODE_SCHEMA || episode.scope.project.is_empty() {
            return Err(StateError::InvalidInput(format!(
                "episode {} is not a {EPISODE_SCHEMA} episode with a project scope",
                episode.id
            )));
        }
        if let Some(stored) = self.episode(&episode.id)? {
            return same(&stored, episode);
        }
        let value = serde_json::to_value(episode).expect("an episode serializes");
        match self.create(EPISODE_COLLECTION, &episode.id, value) {
            Ok(()) => Ok(EpisodeWrite::Recorded),
            // Another writer got there first: it is the same episode, or ours is refused.
            Err(e) => match self.episode(&episode.id)? {
                Some(stored) => same(&stored, episode),
                None => Err(e),
            },
        }
    }

    /// [`select`] over this database's episodes, with each one's cited records checked by
    /// [`backing`](Self::backing): one they do not back is excluded as [`Exclusion::Unbacked`].
    pub fn recall(
        &self,
        task_id: &str,
        query: &EpisodeContext,
        now_ms: u64,
        limit: usize,
    ) -> Result<Recall, StateError> {
        let (episodes, unreadable) = self.episodes()?;
        Ok(select(
            episodes,
            unreadable,
            task_id,
            query,
            now_ms,
            limit,
            &|e| self.backing(e),
        ))
    }

    /// Whether the records `episode` cites back it: its result record exists, is its attempt's
    /// and carries its verdict, and a verified success's task is `completed`. Reads only.
    pub fn backing(&self, episode: &EpisodeRecord) -> Result<(), String> {
        let result_id = episode
            .result_record
            .strip_prefix(&format!("{RESULT_COLLECTION}:"))
            .ok_or("its result record is not a work result")?;
        let (_, value) = self
            .read(RESULT_COLLECTION, result_id)
            .map_err(|e| e.to_string())?
            .ok_or("its result record does not exist")?;
        let result: WorkResultRecord =
            serde_json::from_value(value).map_err(|e| format!("its result does not parse: {e}"))?;
        if result.task_id != episode.task_id || result.attempt != episode.attempt {
            return Err("its result record is another attempt's".into());
        }
        if result.verification != episode.verification {
            return Err("its verdict is not the one its result record holds".into());
        }
        if episode.outcome == EpisodeOutcome::VerifiedSuccess {
            let (_, task) = self
                .task(&episode.task_id)
                .map_err(|e| e.to_string())?
                .ok_or("its task does not exist")?;
            if task.status != "completed" {
                return Err(format!("its task is {}, not completed", task.status));
            }
        }
        Ok(())
    }
}

fn same(stored: &EpisodeRecord, episode: &EpisodeRecord) -> Result<EpisodeWrite, StateError> {
    if stored == episode {
        Ok(EpisodeWrite::AlreadyRecorded)
    } else {
        Err(StateError::Conflict {
            operation: "episode",
            detail: format!(
                "a different episode is already recorded as {}; it is kept, not replaced",
                episode.id
            ),
        })
    }
}
