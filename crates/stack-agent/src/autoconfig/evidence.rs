//! Evidence records: what was decided (selection) and what then happened (outcome), kept apart.
//!
//! Records are returned in the command's report, and each `work` run's outcome record is kept in
//! its learning documents (`crate::learning`, docs/product/learning.md), whose re-validated
//! behaviors feed later `--auto` decisions. An operator can also hand a snapshot back with
//! `--selection-evidence FILE`, which is read and never written. [`EvidenceSource`] is the seam.
//!
//! An outcome's class is computed by the host from the work's verified result, terminal state and
//! audit: never by the configuration that ran. Records carry no goal text, no file content, no path
//! and no credential: only coarse, privacy-safe comparison features.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::software_work::SoftwareWork;
use chip_core::WorkOutcome;

pub const RECORD_SCHEMA: &str = "stack-agent.selection-evidence.v1";

/// Who produced an outcome. The production scorer counts only `production` outcomes: experiment
/// results inform people, never the selection policy, unless a reviewed change says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Production,
    Experiment,
}

/// The host's classification of one outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeClass {
    /// An independent predicate verified the requested outcome.
    VerifiedSuccess,
    /// The host established that it did not: a test run that did not pass after the last change,
    /// or verification evidence that was refused (tampering, a project changed after the run).
    VerifiedFailure,
    /// The work ended without any verification evidence either way (completed, a limit, escalated).
    Unverified,
    /// The work stopped because something it required was unavailable or refused.
    Blocked,
    /// Stopped by its owner (a request or the deadline).
    Cancelled,
    /// A runtime failure (a provider outage, an invalid model decision, a safety violation).
    RuntimeFailure,
    /// The host could not judge the run (a run that did not happen, or evidence it cannot use).
    InsufficientEvidence,
}

impl OutcomeClass {
    pub fn name(self) -> &'static str {
        match self {
            Self::VerifiedSuccess => "verified_success",
            Self::VerifiedFailure => "verified_failure",
            Self::Unverified => "unverified",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
            Self::RuntimeFailure => "runtime_failure",
            Self::InsufficientEvidence => "insufficient_evidence",
        }
    }

    /// From the host's own view of a finished work.
    pub fn of(work: &SoftwareWork) -> Self {
        if !work.invariants_hold() {
            return Self::RuntimeFailure;
        }
        if work.verified {
            return Self::VerifiedSuccess;
        }
        if work.evidence_invalidated.is_some() {
            return Self::VerifiedFailure;
        }
        let tests_ran_and_did_not_pass = work.report.observations.iter().rev().find_map(|o| {
            let first = o.output.as_deref()?.lines().next()?;
            chip_pax::parse_execution_result(first.as_bytes()).ok()
        });
        match &work.report.outcome {
            WorkOutcome::Cancelled { .. } => Self::Cancelled,
            WorkOutcome::Failed { .. } => Self::RuntimeFailure,
            WorkOutcome::Blocked { .. } if tests_ran_and_did_not_pass.is_none() => Self::Blocked,
            _ if tests_ran_and_did_not_pass.is_some_and(|r| !r.is_passing_evidence()) => {
                Self::VerifiedFailure
            }
            _ => Self::Unverified,
        }
    }
}

/// Coarse comparison features: enough to tell whether two tasks are comparable, nothing more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Features {
    pub task_kind: String,
    /// The primary build system, or `null` when unknown or ambiguous.
    pub build_system: Option<String>,
}

/// One outcome, as recorded after a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeRecord {
    pub schema: String,
    pub record: String,
    pub id: String,
    pub selection_id: Option<String>,
    /// Which run this is (process id and clock), so two runs never share a record id even when
    /// everything else about them is identical.
    pub run: String,
    /// `id@version`, or `null` when the run used the baseline (no configuration was selected).
    pub configuration: Option<String>,
    pub origin: Origin,
    pub features: Features,
    /// Unix seconds when the outcome was recorded.
    pub recorded_at: u64,
    pub outcome: OutcomeClass,
    pub terminal_state: String,
    pub verified: bool,
    pub delivery: Option<String>,
    pub cancellation: Option<String>,
    pub capabilities_invoked: std::collections::BTreeMap<String, usize>,
    /// `sha256` digests of the observations that carried test results, as references.
    pub verification_refs: Vec<String>,
    pub duration_ms: Option<u64>,
    pub tokens: Option<u64>,
    /// No provider here reports cost; it is never estimated.
    pub cost: Option<String>,
    pub limitations: Vec<String>,
}

fn sha(text: &str) -> String {
    format!(
        "sha256:{}",
        Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

impl OutcomeRecord {
    /// The record of one finished work. `id` is the digest of everything else in it.
    #[allow(clippy::too_many_arguments)]
    pub fn from_work(
        work: &SoftwareWork,
        run: String,
        selection_id: Option<String>,
        configuration: Option<String>,
        origin: Origin,
        features: Features,
        recorded_at: u64,
    ) -> Self {
        let outcome = OutcomeClass::of(work);
        let verification_refs = work
            .report
            .observations
            .iter()
            .filter(|o| {
                o.output
                    .as_deref()
                    .and_then(|t| t.lines().next())
                    .is_some_and(|l| chip_pax::parse_execution_result(l.as_bytes()).is_ok())
            })
            .map(|o| sha(o.output.as_deref().unwrap_or_default()))
            .collect();
        let (delivery, cancellation) = match &work.report.outcome {
            WorkOutcome::Failed { delivery, .. } => {
                (delivery.map(|d| d.as_str().to_string()), None)
            }
            WorkOutcome::Cancelled {
                cause,
                delivery,
                remote,
                ..
            } => (
                delivery.map(|d| d.as_str().to_string()),
                Some(format!(
                    "{}{}",
                    cause.name(),
                    remote
                        .map(|r| format!(" (remote stop {})", r.as_str()))
                        .unwrap_or_default()
                )),
            ),
            _ => (None, None),
        };
        let mut limitations = vec![
            "stack-agent does not persist this record; keep it to use it as evidence".to_string(),
        ];
        if work.utility.total_tokens.is_none() {
            limitations.push("token usage not reported by the provider".into());
        }
        let record = OutcomeRecord {
            schema: RECORD_SCHEMA.into(),
            record: "outcome".into(),
            id: String::new(),
            selection_id,
            run,
            configuration,
            origin,
            features,
            recorded_at,
            outcome,
            terminal_state: work.report.outcome.terminal_state().name().into(),
            verified: work.verified,
            delivery,
            cancellation,
            capabilities_invoked: work.utility.executions_by_capability.clone(),
            verification_refs,
            duration_ms: Some(work.utility.total_latency_ms),
            tokens: work.utility.total_tokens,
            cost: None,
            limitations,
        };
        record.seal()
    }

    /// Sets `id` to the digest of everything else in the record.
    pub fn seal(mut self) -> Self {
        self.id = String::new();
        self.id = sha(&serde_json::to_string(&self).expect("a record serializes"));
        self
    }
}

/// Where selection-time evidence comes from. A durable authority would implement this; today only
/// an operator-supplied snapshot does, and selection never writes back through it.
pub trait EvidenceSource {
    fn outcomes(&self) -> &[OutcomeRecord];
}

/// No evidence: every decision is made from the catalog's documented defaults.
pub struct NoEvidence;

impl EvidenceSource for NoEvidence {
    fn outcomes(&self) -> &[OutcomeRecord] {
        &[]
    }
}

/// Outcome records an operator kept and handed back. Read-only; never written by stack-agent.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    records: Vec<OutcomeRecord>,
}

impl EvidenceSource for Snapshot {
    fn outcomes(&self) -> &[OutcomeRecord] {
        &self.records
    }
}

impl Snapshot {
    pub fn new(records: Vec<OutcomeRecord>) -> Self {
        Self { records }
    }

    pub fn records(&self) -> &[OutcomeRecord] {
        &self.records
    }

    pub fn records_len(&self) -> usize {
        self.records.len()
    }

    /// A JSON array of records, or one record per line. Selection records and anything that is not
    /// an outcome of this schema are refused, not skipped: a snapshot is evidence and must be exact.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let values: Vec<serde_json::Value> = match serde_json::from_str::<serde_json::Value>(text) {
            Ok(serde_json::Value::Array(a)) => a,
            Ok(single @ serde_json::Value::Object(_)) => vec![single],
            _ => text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .enumerate()
                .map(|(i, l)| serde_json::from_str(l).map_err(|e| format!("line {}: {e}", i + 1)))
                .collect::<Result<_, _>>()?,
        };
        let mut records = Vec::new();
        for (i, v) in values.into_iter().enumerate() {
            if v["schema"] != RECORD_SCHEMA {
                return Err(format!("record {}: schema is not {RECORD_SCHEMA}", i + 1));
            }
            if v["record"] != "outcome" {
                return Err(format!(
                    "record {}: only outcome records are evidence",
                    i + 1
                ));
            }
            let r: OutcomeRecord =
                serde_json::from_value(v).map_err(|e| format!("record {}: {e}", i + 1))?;
            let mut check = r.clone();
            check.id = String::new();
            if sha(&serde_json::to_string(&check).expect("a record serializes")) != r.id {
                return Err(format!(
                    "record {}: its id does not match its content",
                    i + 1
                ));
            }
            records.push(r);
        }
        Ok(Self { records })
    }
}
