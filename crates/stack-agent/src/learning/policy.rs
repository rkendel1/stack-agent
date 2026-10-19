//! Validation and promotion. A behavior's status is recomputed every time from the run documents
//! its references resolve to; the status a behavior file records is never trusted.
//!
//! Project scope: validated with at least [`MIN_SUPPORT`] verified successes, no contradicting
//! outcome, all references valid, and fresh support. Runtime scope: the same, and the successes
//! must come from at least [`MIN_PROJECTS`] distinct projects of the same runtime line; one
//! project's success is never enough to generalize. Conflicting outcomes make a behavior
//! `contested`, never silently overwritten.

use std::collections::BTreeSet;

use super::document::{Behavior, RunDocument, Scope, Status};
use super::store::Store;
use crate::autoconfig::evidence::{OutcomeClass, OutcomeRecord};

pub const MIN_SUPPORT: usize = 2;
pub const MIN_PROJECTS: usize = 2;
/// Support older than this makes a behavior stale.
pub const STALE_DAYS: u64 = 180;
/// Contradictions with no support at all reject a behavior at this count.
pub const REJECT_AT: usize = 3;
/// At most this many applicable behaviors are retrieved per scope.
pub const MAX_RETRIEVED: usize = 16;

/// One behavior as a reader established it.
#[derive(Debug, Clone)]
pub struct Assessed {
    pub behavior: Behavior,
    pub status: Status,
    pub support: usize,
    pub contradiction: usize,
    pub distinct_projects: usize,
    /// The documents its references resolved to, validated.
    pub documents: Vec<RunDocument>,
    /// References that did not resolve, and why.
    pub problems: Vec<String>,
}

/// Whether an outcome bears on whether a configuration works: only these update behaviors.
pub fn bears_on_success(outcome: OutcomeClass) -> bool {
    matches!(
        outcome,
        OutcomeClass::VerifiedSuccess | OutcomeClass::VerifiedFailure | OutcomeClass::Unverified
    )
}

pub fn assess(
    store: &Store,
    behavior: Behavior,
    now: u64,
    catalog_version: &str,
    runtime_line: &str,
) -> Assessed {
    let mut documents = Vec::new();
    let mut problems = Vec::new();
    let mut seen_runs = BTreeSet::new();
    for r in &behavior.references {
        match store.load_run(&r.document) {
            Ok(d) => {
                let applies = d.task_kind == behavior.key.task_kind
                    && d.applicability.build_system == behavior.key.build_system
                    && d.applicability.configuration == behavior.key.configuration;
                if !applies {
                    problems.push(format!(
                        "{}: the document is about a different task or configuration",
                        r.path
                    ));
                } else if !seen_runs.insert(d.run.clone()) {
                    problems.push(format!(
                        "{}: a second document for the same run (counted once)",
                        r.path
                    ));
                } else {
                    documents.push(d);
                }
            }
            Err(e) => problems.push(format!("{}: {e}", r.path)),
        }
    }
    let support_docs: Vec<&RunDocument> = documents
        .iter()
        .filter(|d| d.outcome == OutcomeClass::VerifiedSuccess)
        .collect();
    let support = support_docs.len();
    let contradiction = documents
        .iter()
        .filter(|d| {
            matches!(
                d.outcome,
                OutcomeClass::VerifiedFailure | OutcomeClass::Unverified
            )
        })
        .count();
    let distinct_projects = support_docs
        .iter()
        .filter_map(|d| d.project_token.as_ref().or(d.project.as_ref()))
        .collect::<BTreeSet<_>>()
        .len();
    let newest_support = support_docs
        .iter()
        .map(|d| d.applicability.recorded_at)
        .max();
    let day = |t: u64| t / 86_400;
    let stale = behavior.catalog_version != catalog_version
        || (behavior.scope == Scope::Runtime && behavior.runtime_line != runtime_line)
        || newest_support.is_some_and(|t| day(now).saturating_sub(day(t)) > STALE_DAYS);
    let status = if stale {
        Status::Stale
    } else if support >= 1 && contradiction >= 1 {
        Status::Contested
    } else if support == 0 && contradiction >= REJECT_AT {
        Status::Rejected
    } else if support >= MIN_SUPPORT
        && problems.is_empty()
        && (behavior.scope == Scope::Project || distinct_projects >= MIN_PROJECTS)
    {
        Status::Validated
    } else {
        Status::Candidate
    };
    Assessed {
        behavior,
        status,
        support,
        contradiction,
        distinct_projects,
        documents,
        problems,
    }
}

/// Whether a behavior applies to the current task.
pub fn applies(b: &Behavior, task_kind: &str, build_system: Option<&str>) -> bool {
    b.key.task_kind == task_kind && b.key.build_system.as_deref() == build_system
}

/// The outcome records a scope contributes to `--auto` ranking. Project scope: behaviors that are
/// validated, contested or rejected (their failures matter as much as their successes); candidates
/// and stale ones do not. Runtime scope: validated behaviors only. Only configured runs carry a
/// configuration the ranking can use.
pub fn evidence(scope: Scope, assessed: &[Assessed]) -> Vec<OutcomeRecord> {
    let admitted = |s: Status| match scope {
        Scope::Project => matches!(s, Status::Validated | Status::Contested | Status::Rejected),
        Scope::Runtime => s == Status::Validated,
    };
    let mut out: Vec<OutcomeRecord> = assessed
        .iter()
        .filter(|a| admitted(a.status))
        .flat_map(|a| a.documents.iter().filter_map(|d| d.outcome_record.clone()))
        .filter(|r| r.configuration.is_some())
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

/// Retrieval order: validated first, then contested, candidate, stale, rejected; then by id.
pub fn order(mut assessed: Vec<Assessed>) -> Vec<Assessed> {
    assessed.sort_by(|a, b| {
        a.status
            .cmp(&b.status)
            .then(a.behavior.id.cmp(&b.behavior.id))
    });
    assessed.truncate(MAX_RETRIEVED);
    assessed
}
