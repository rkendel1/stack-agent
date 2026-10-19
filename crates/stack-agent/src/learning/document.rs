//! The learning document formats: run documents (historical, immutable, content-addressed) and
//! behaviors (actionable, evidence-backed, a mutable index whose status is always recomputed).
//!
//! Both scopes share these schemas. A document never carries the goal's text, file contents,
//! command output, a host path or a credential: only host-derived classifications, counts,
//! digests and versions.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::autoconfig::evidence::{OutcomeClass, OutcomeRecord};

pub const RUN_SCHEMA: &str = "stack-agent.learning.run.v1";
pub const BEHAVIOR_SCHEMA: &str = "stack-agent.learning.behavior.v1";
/// Larger documents are refused on write and on load.
pub const MAX_DOCUMENT_BYTES: usize = 16 * 1024;
/// A behavior keeps at most this many evidence references (the most recent).
pub const MAX_REFERENCES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Project,
    Runtime,
}

impl Scope {
    pub fn name(self) -> &'static str {
        match self {
            Scope::Project => "project",
            Scope::Runtime => "runtime",
        }
    }
}

/// Whether a lesson states something the host observed or something it supposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LessonKind {
    Fact,
    Hypothesis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lesson {
    pub kind: LessonKind,
    pub statement: String,
    /// Ids of the evidence the lesson rests on (an outcome record, observation digests).
    pub evidence: Vec<String>,
}

/// What the run started from: bounded metadata, enough to decide applicability later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartingContext {
    pub build_system: Option<String>,
    pub has_tests: Option<bool>,
    pub git_repository: Option<bool>,
    pub verification: Option<String>,
    pub profile_version: String,
    /// The profile's digest (project scope only: it is about this project).
    pub profile_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Actions {
    pub capabilities_invoked: std::collections::BTreeMap<String, usize>,
    pub model_calls: usize,
    pub decisions: usize,
    pub failed_executions: usize,
}

/// When a document stops supporting conclusions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Applicability {
    pub task_kind: String,
    pub build_system: Option<String>,
    /// The configuration that ran (`id@version`), or `baseline` without one.
    pub configuration: String,
    pub catalog_version: String,
    /// The `major.minor` of the runtime that recorded it.
    pub runtime_line: String,
    /// Unix seconds.
    pub recorded_at: u64,
    pub valid_days: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunDocument {
    pub schema: String,
    /// `sha256:` of the document with this field empty. The file is named after it.
    pub id: String,
    pub scope: Scope,
    /// Unique per run; shared by the project and runtime documents of the same run.
    pub run: String,
    pub runtime_version: String,
    pub task_kind: String,
    /// Project scope: the project's stable id. Runtime scope: `null`.
    pub project: Option<String>,
    /// Runtime scope: a salted, installation-local token for the project, only so distinct projects
    /// can be counted. Project scope: `null`.
    pub project_token: Option<String>,
    pub starting_context: StartingContext,
    pub actions: Actions,
    /// Host-classified, never model-claimed.
    pub outcome: OutcomeClass,
    pub terminal_state: String,
    /// The autoconfig outcome record of this run (project scope with verification references,
    /// runtime scope without them). `null` for a run that never started.
    pub outcome_record: Option<OutcomeRecord>,
    pub lessons: Vec<Lesson>,
    pub evidence: Vec<String>,
    pub applicability: Applicability,
    pub limitations: Vec<String>,
    /// The behaviors this run created or updated (ids).
    pub behaviors: Vec<String>,
}

pub fn sha(text: &str) -> String {
    format!(
        "sha256:{}",
        Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

/// The file name a document id maps to: its hex digest. The only names references may use.
pub fn file_name(id: &str) -> Option<String> {
    let hex = id.strip_prefix("sha256:")?;
    (hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
    .then(|| format!("{hex}.json"))
}

/// Why a document was not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalid(pub String);

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl RunDocument {
    pub fn seal(mut self) -> Self {
        self.id = String::new();
        self.id = sha(&serde_json::to_string(&self).expect("a document serializes"));
        self
    }

    pub fn to_text(&self) -> String {
        serde_json::to_string_pretty(self).expect("a document serializes") + "\n"
    }

    /// Parses and validates a run document read from `scope`'s store.
    pub fn from_text(text: &str, scope: Scope) -> Result<Self, Invalid> {
        if text.len() > MAX_DOCUMENT_BYTES {
            return Err(Invalid(format!("larger than {MAX_DOCUMENT_BYTES} bytes")));
        }
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|e| Invalid(format!("not JSON: {e}")))?;
        if v["schema"] != RUN_SCHEMA {
            return Err(Invalid(format!("unsupported schema {}", v["schema"])));
        }
        let d: RunDocument =
            serde_json::from_value(v).map_err(|e| Invalid(format!("malformed: {e}")))?;
        d.validate(scope)?;
        Ok(d)
    }

    pub fn validate(&self, scope: Scope) -> Result<(), Invalid> {
        if self.scope != scope {
            return Err(Invalid(format!(
                "a {} document in the {} store",
                self.scope.name(),
                scope.name()
            )));
        }
        if self.clone().seal().id != self.id {
            return Err(Invalid(
                "its id does not match its content (altered or forged)".into(),
            ));
        }
        match scope {
            Scope::Project if self.project.is_none() || self.project_token.is_some() => {
                return Err(Invalid(
                    "a project document names its project and no token".into(),
                ));
            }
            Scope::Runtime
                if self.project.is_some() || self.starting_context.profile_digest.is_some() =>
            {
                return Err(Invalid(
                    "a runtime document carries project-specific identity".into(),
                ));
            }
            _ => {}
        }
        if let Some(r) = &self.outcome_record {
            if r.outcome != self.outcome || r.clone().seal().id != r.id {
                return Err(Invalid("its outcome record does not match".into()));
            }
            if scope == Scope::Runtime && !r.verification_refs.is_empty() {
                return Err(Invalid(
                    "a runtime document carries project-specific evidence".into(),
                ));
            }
        }
        let len = serde_json::to_string(self)
            .map(|t| t.len())
            .unwrap_or(usize::MAX);
        if len > MAX_DOCUMENT_BYTES {
            return Err(Invalid(format!("larger than {MAX_DOCUMENT_BYTES} bytes")));
        }
        Ok(())
    }
}

/// A behavior's status. Always recomputed from validated references; a stored value is never
/// trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Validated,
    Contested,
    Candidate,
    Stale,
    Rejected,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Validated => "validated",
            Status::Contested => "contested",
            Status::Candidate => "candidate",
            Status::Stale => "stale",
            Status::Rejected => "rejected",
        }
    }
}

/// What a behavior is about: one configuration (or the baseline) for one kind of task on one kind
/// of project.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviorKey {
    pub task_kind: String,
    pub build_system: Option<String>,
    pub configuration: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    /// The run document's id; it is also its file name under `runs/`.
    pub document: String,
    /// The relative reference, for people: `runs/<hex>.json`.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Behavior {
    pub schema: String,
    /// Stable: `sha256:` of the scope and key.
    pub id: String,
    pub scope: Scope,
    pub key: BehaviorKey,
    /// What it proposes, in words. Advisory: it is evidence for ranking, never an instruction.
    pub proposal: String,
    pub catalog_version: String,
    pub runtime_line: String,
    pub references: Vec<Reference>,
    /// As last written. Informational only: readers recompute from the references.
    pub recorded_status: Status,
    pub recorded_support: usize,
    pub recorded_contradiction: usize,
    pub updated_at: u64,
    pub limitations: Vec<String>,
}

impl Behavior {
    pub fn id_for(scope: Scope, key: &BehaviorKey) -> String {
        sha(&format!(
            "{}|{}|{}|{}",
            scope.name(),
            key.task_kind,
            key.build_system.as_deref().unwrap_or("-"),
            key.configuration
        ))
    }

    pub fn to_text(&self) -> String {
        serde_json::to_string_pretty(self).expect("a behavior serializes") + "\n"
    }

    pub fn from_text(text: &str, scope: Scope) -> Result<Self, Invalid> {
        if text.len() > MAX_DOCUMENT_BYTES {
            return Err(Invalid(format!("larger than {MAX_DOCUMENT_BYTES} bytes")));
        }
        let v: serde_json::Value =
            serde_json::from_str(text).map_err(|e| Invalid(format!("not JSON: {e}")))?;
        if v["schema"] != BEHAVIOR_SCHEMA {
            return Err(Invalid(format!("unsupported schema {}", v["schema"])));
        }
        let b: Behavior =
            serde_json::from_value(v).map_err(|e| Invalid(format!("malformed: {e}")))?;
        if b.scope != scope {
            return Err(Invalid(format!(
                "a {} behavior in the {} store",
                b.scope.name(),
                scope.name()
            )));
        }
        if Behavior::id_for(scope, &b.key) != b.id {
            return Err(Invalid("its id does not match its key".into()));
        }
        if b.references.len() > MAX_REFERENCES {
            return Err(Invalid(format!("more than {MAX_REFERENCES} references")));
        }
        for r in &b.references {
            match file_name(&r.document) {
                Some(name) if r.path == format!("runs/{name}") => {}
                _ => {
                    return Err(Invalid(format!(
                        "reference {:?} is not a run document in this store",
                        r.path
                    )));
                }
            }
        }
        Ok(b)
    }
}
