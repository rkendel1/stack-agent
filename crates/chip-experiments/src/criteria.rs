//! Host-defined acceptance criteria.
//!
//! A criterion reads what the host observed — the work's recorded observations, and in modes that
//! have one, the disposable project as the run left it — and nothing a strategy or a model said
//! about itself. A criterion that can be decided from observations is also handed to Chip as a
//! required observation, so the loop itself refuses a completion before it is met; the evaluator
//! then asks every criterion again of the finished run.

use std::path::Path;
use std::sync::Arc;

use chip_core::{ExecutionStatus, Observation, ObservationKind, ObservationPredicate, WorkReport};
use sha2::{Digest, Sha256};

use crate::definition::Mode;
use crate::plugin::TaskFixture;

/// What one criterion concluded about one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CriterionResult {
    Met,
    NotMet(String),
    /// The host does not have what it would need to decide. Never read as met.
    Unavailable(String),
}

impl CriterionResult {
    pub fn name(&self) -> &'static str {
        match self {
            CriterionResult::Met => "met",
            CriterionResult::NotMet(_) => "not_met",
            CriterionResult::Unavailable(_) => "unavailable",
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            CriterionResult::Met => None,
            CriterionResult::NotMet(d) | CriterionResult::Unavailable(d) => Some(d),
        }
    }
}

/// Everything the host observed about one run.
pub struct HostObservation<'a> {
    pub mode: Mode,
    pub task: &'a TaskFixture,
    pub report: &'a WorkReport,
    /// The disposable project after the run, before cleanup. `None` in replay, which has none.
    pub final_tree: Option<&'a Path>,
}

pub trait Criterion: Send + Sync {
    fn id(&self) -> &'static str;
    fn describe(&self) -> String;
    /// Whether the criterion can be decided in `mode`. A definition that uses it in another mode is
    /// refused rather than reported "unavailable" on every run.
    fn supports(&self, mode: Mode) -> bool;
    /// The same requirement as a predicate over observations, for Chip to enforce during the work.
    fn in_loop(&self, _task: &TaskFixture) -> Option<Arc<dyn ObservationPredicate>> {
        None
    }
    fn evaluate(&self, host: &HostObservation<'_>) -> CriterionResult;
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The JSON first line of a successful `project.write` observation: (path, sha256).
fn write_of(observation: &Observation) -> Option<(String, String)> {
    if observation.kind != ObservationKind::ExecutionCompleted
        || observation.status != ExecutionStatus::Success
    {
        return None;
    }
    let first = observation.output.as_deref()?.lines().next()?;
    let v: serde_json::Value = serde_json::from_str(first).ok()?;
    (v["capability"] == chip_project::PROJECT_WRITE).then(|| {
        (
            v["path"].as_str().unwrap_or_default().to_string(),
            v["sha256"].as_str().unwrap_or_default().to_string(),
        )
    })
}

/// "For every expected file, the last successful `project.write` of that path wrote exactly the
/// expected content." Decided from observations, so it holds in every mode (in replay the
/// observations are recorded ones, and the report says so).
#[derive(Debug, Clone, Copy, Default)]
pub struct ExpectedWritesObserved;

#[derive(Debug)]
struct ExpectedWrites(Vec<(String, String)>);

impl ObservationPredicate for ExpectedWrites {
    fn describe(&self) -> String {
        format!(
            "the last write of each of {} observed with the expected content",
            self.0
                .iter()
                .map(|(p, _)| p.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    fn satisfied_by(&self, _observation: &Observation) -> bool {
        false
    }

    fn satisfied_by_trajectory(&self, observations: &[Observation]) -> bool {
        !self.0.is_empty()
            && self.0.iter().all(|(path, sha)| {
                observations
                    .iter()
                    .filter_map(write_of)
                    .filter(|(p, _)| p == path)
                    .last()
                    .is_some_and(|(_, s)| &s == sha)
            })
    }
}

impl ExpectedWritesObserved {
    fn predicate(task: &TaskFixture) -> ExpectedWrites {
        ExpectedWrites(
            task.expected_files
                .iter()
                .map(|(p, c)| (p.clone(), sha256_hex(c)))
                .collect(),
        )
    }
}

impl Criterion for ExpectedWritesObserved {
    fn id(&self) -> &'static str {
        "observed.expected-writes"
    }

    fn describe(&self) -> String {
        "the last observed write of each expected file has exactly the expected content".into()
    }

    fn supports(&self, _mode: Mode) -> bool {
        true
    }

    fn in_loop(&self, task: &TaskFixture) -> Option<Arc<dyn ObservationPredicate>> {
        Some(Arc::new(Self::predicate(task)))
    }

    fn evaluate(&self, host: &HostObservation<'_>) -> CriterionResult {
        if host.task.expected_files.is_empty() {
            return CriterionResult::Unavailable("the task expects no file".into());
        }
        if Self::predicate(host.task).satisfied_by_trajectory(&host.report.observations) {
            CriterionResult::Met
        } else {
            CriterionResult::NotMet(
                "no observed write leaves every expected file with the expected content".into(),
            )
        }
    }
}

/// "Every expected file, read from the disposable project after the run, has the expected
/// content." Read by the host directly, so it does not depend on what any observation said.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExpectedTree;

impl Criterion for ExpectedTree {
    fn id(&self) -> &'static str {
        "tree.expected-files"
    }

    fn describe(&self) -> String {
        "after the run, every expected file in the project has exactly the expected content".into()
    }

    fn supports(&self, mode: Mode) -> bool {
        mode != Mode::Replay
    }

    fn evaluate(&self, host: &HostObservation<'_>) -> CriterionResult {
        let Some(root) = host.final_tree else {
            return CriterionResult::Unavailable("this run has no project to read".into());
        };
        if host.task.expected_files.is_empty() {
            return CriterionResult::Unavailable("the task expects no file".into());
        }
        for (path, expected) in &host.task.expected_files {
            match std::fs::read_to_string(root.join(path)) {
                Ok(actual) if &actual == expected => {}
                Ok(_) => {
                    return CriterionResult::NotMet(format!(
                        "{path} differs from the expected content"
                    ));
                }
                Err(e) => {
                    return CriterionResult::NotMet(format!(
                        "{path} cannot be read ({})",
                        e.kind()
                    ));
                }
            }
        }
        CriterionResult::Met
    }
}

/// "Nothing in the project changed except the expected files": no other file edited, added or
/// removed. Read by the host from the disposable project.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoUnexpectedChanges;

fn tree(root: &Path) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                walk(root, &entry.path(), out)?;
            } else {
                let rel = entry
                    .path()
                    .strip_prefix(root)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                // A symbolic link is reported as content-less so any appearance of one differs.
                let content = if kind.is_file() {
                    std::fs::read(entry.path())?
                } else {
                    b"<not a regular file>".to_vec()
                };
                out.push((rel, content));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort();
    Ok(out)
}

impl Criterion for NoUnexpectedChanges {
    fn id(&self) -> &'static str {
        "tree.no-unexpected-changes"
    }

    fn describe(&self) -> String {
        "after the run, no project file other than the expected ones was changed, added or removed"
            .into()
    }

    fn supports(&self, mode: Mode) -> bool {
        mode != Mode::Replay
    }

    fn evaluate(&self, host: &HostObservation<'_>) -> CriterionResult {
        let Some(root) = host.final_tree else {
            return CriterionResult::Unavailable("this run has no project to read".into());
        };
        let actual = match tree(root) {
            Ok(t) => t,
            Err(e) => {
                return CriterionResult::Unavailable(format!(
                    "the project cannot be read ({})",
                    e.kind()
                ));
            }
        };
        let expected_paths: Vec<&str> = host
            .task
            .expected_files
            .iter()
            .map(|(p, _)| p.as_str())
            .collect();
        let mut problems = Vec::new();
        for (path, initial) in &host.task.files {
            if expected_paths.contains(&path.as_str()) {
                continue;
            }
            match actual.iter().find(|(p, _)| p == path) {
                Some((_, c)) if c == initial.as_bytes() => {}
                Some(_) => problems.push(format!("{path} changed")),
                None => problems.push(format!("{path} was removed")),
            }
        }
        for (path, _) in &actual {
            let known = host.task.files.iter().any(|(p, _)| p == path)
                || expected_paths.contains(&path.as_str());
            if !known {
                problems.push(format!("{path} was added"));
            }
        }
        if problems.is_empty() {
            CriterionResult::Met
        } else {
            CriterionResult::NotMet(problems.join("; "))
        }
    }
}
