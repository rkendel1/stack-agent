//! The evidence and provenance report (`chip.experiment-report.v1`) and its human summary.
//!
//! The report is an evidence artifact: it records what the host observed and references the
//! observations by digest. It is not an authoritative record of external reality and it is not
//! stored anywhere by this crate; whoever writes it out owns its durability. Before it is returned,
//! every secret value the host passed in is replaced, in the JSON and the summary alike.

use serde_json::{Value, json};

use crate::criteria::CriterionResult;
use crate::definition::{EvidenceField, Mode};
use crate::plugin::Confinement;
use crate::registry::ResolvedExperiment;
use crate::runner::{Cleanup, Conclusion, ExperimentResult, RunRecord, Termination, Verdict};

pub const REPORT_SCHEMA: &str = "chip.experiment-report.v1";

/// Who produced the report, from the host. Never derived from the environment by this crate.
#[derive(Debug, Clone, Default)]
pub struct Provenance {
    /// The host program and its version, for example `stack-agent 0.1.0`.
    pub host: String,
    /// The source revision the host was built from, when it knows it.
    pub source_revision: Option<String>,
}

/// Replaces exact secret values. The same rule the native provider applies to its own messages
/// (exact-value scrubbing); its scrubber is private to that crate, so the rule is applied here to
/// the values the host names. Values shorter than 4 bytes are not treated as secrets (they would
/// erase ordinary text and protect nothing).
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    secrets: Vec<String>,
}

pub const REDACTED: &str = "[redacted]";

impl Redactor {
    pub fn new(secrets: impl IntoIterator<Item = String>) -> Self {
        let mut secrets: Vec<String> = secrets.into_iter().filter(|s| s.len() >= 4).collect();
        // Longest first, so a secret containing another is removed whole.
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Self { secrets }
    }

    pub fn scrub(&self, text: &str) -> String {
        let mut out = text.to_string();
        for secret in &self.secrets {
            out = out.replace(secret.as_str(), REDACTED);
        }
        out
    }

    fn scrub_value(&self, value: Value) -> Value {
        match value {
            Value::String(s) => Value::String(self.scrub(&s)),
            Value::Array(a) => Value::Array(a.into_iter().map(|v| self.scrub_value(v)).collect()),
            Value::Object(o) => Value::Object(
                o.into_iter()
                    .map(|(k, v)| (self.scrub(&k), self.scrub_value(v)))
                    .collect(),
            ),
            other => other,
        }
    }
}

fn mode_guarantees(mode: Mode) -> (Vec<&'static str>, Vec<&'static str>) {
    match mode {
        Mode::Observe => (
            vec![
                "only the baseline ran, and it is a strategy the host registered as its production behavior",
                "the run used a disposable project directory, removed afterwards",
                "only capabilities whose effects their executor confines to that directory were offered",
            ],
            vec![
                "the run used the experiment's criteria, not the production goal kind's completion predicate",
                "a recorded model, when used, is not a live model",
            ],
        ),
        Mode::Replay => (
            vec![
                "nothing executed: every capability result came from the task's recording",
                "a request not in the recording failed; nothing was substituted",
            ],
            vec![
                "replay reproduces recorded inputs only: not external effects, timing or a live model",
                "criteria were decided from recorded observations",
            ],
        ),
        Mode::Isolated => (
            vec![
                "each run had its own disposable project directory, removed afterwards",
                "only capabilities whose effects their executor confines to that directory were offered",
                "the project path rules refused any path outside the directory, and the audit checked every observation against it",
            ],
            vec![
                "the directory is not an operating-system sandbox: the process runs with the operator's permissions, and isolation rests on the offered capabilities' own confinement",
            ],
        ),
    }
}

fn criterion_json(id: &str, result: &CriterionResult) -> Value {
    json!({"id": id, "result": result.name(), "detail": result.detail()})
}

fn verdict_json(v: &Verdict) -> Value {
    let mut out = json!({"class": v.class()});
    let detail = match v {
        Verdict::Verified => Value::Null,
        Verdict::CompletedUnverified { failed } => json!({"failed_criteria": failed}),
        Verdict::Incomplete { state, reason } => json!({"state": state, "reason": reason}),
        Verdict::RuntimeFailure { reason, delivery } => {
            json!({"reason": reason, "delivery": delivery})
        }
        Verdict::Cancelled {
            cause,
            reason,
            termination,
        } => json!({
            "cause": cause,
            "reason": reason,
            "termination": match termination {
                Termination::Known => json!({"known": true}),
                Termination::Uncertain(why) => json!({"known": false, "why": why}),
            },
        }),
        Verdict::EvidenceInvalid { reason } | Verdict::NotRun { reason } => {
            json!({"reason": reason})
        }
    };
    out["detail"] = detail;
    out
}

fn source_json(a: &crate::runner::SourceActivation) -> Value {
    let (evidence, reason) = match &a.detection {
        crate::plugin::Detection::Confirmed { evidence }
        | crate::plugin::Detection::NotDetected { evidence } => (evidence.clone(), None),
        crate::plugin::Detection::Inconclusive { reason } => (Vec::new(), Some(reason.clone())),
    };
    json!({
        "source": a.source,
        "detection": {"result": a.detection.name(), "evidence": evidence, "reason": reason},
        "active": a.active,
        "inactive_reason": a.inactive_reason,
        "guidance": a.guidance.as_ref().map(|(path, sha256, bytes)| json!({"path": path, "sha256": sha256, "bytes": bytes})),
    })
}

fn run_json(run: &RunRecord, evidence: &[EvidenceField]) -> Value {
    let wants = |f: EvidenceField| evidence.contains(&f);
    let mut out = json!({
        "side": run.side.name(),
        "strategy": {"id": run.strategy, "version": run.strategy_version},
        "task": {"id": run.task, "version": run.task_version, "fixture_digest": run.task_digest},
        "work_id": run.work_id,
        "verdict": verdict_json(&run.verdict),
        "terminal_state": run.terminal_state,
        "outcome_reason": run.outcome_reason,
        "delivery": run.delivery,
        "criteria": run.criteria.iter().map(|(id, r)| criterion_json(id, r)).collect::<Vec<_>>(),
        "audit": {
            "clean": run.audit_clean,
            "trajectory_violations": run.trajectory_violations,
            "details": run.audit_details,
        },
        "evidence": run.evidence.iter().map(|e| json!({
            "execution_id": e.execution_id,
            "capability": e.capability,
            "status": e.status,
            "output_sha256": e.output_sha256,
        })).collect::<Vec<_>>(),
        "isolation_root_named_in_observations": run.isolation_root_named,
        "cleanup": match &run.cleanup {
            Cleanup::NotNeeded => json!({"needed": false}),
            Cleanup::Removed => json!({"needed": true, "ok": true}),
            Cleanup::Failed(why) => json!({"needed": true, "ok": false, "error": why}),
        },
    });
    if !run.sources.is_empty() {
        out["sources"] = json!(run.sources.iter().map(source_json).collect::<Vec<_>>());
    }
    if wants(EvidenceField::Decisions) {
        out["decisions"] = json!(run.decisions);
    }
    if wants(EvidenceField::Executions) {
        out["executions"] = json!({
            "total": run.executions,
            "by_capability": run.executions_by_capability,
            "failed_observations": run.failed_observations,
        });
    }
    if wants(EvidenceField::ModelCalls) {
        out["model_calls"] = json!(run.model_calls);
    }
    if wants(EvidenceField::Duration) {
        out["duration"] = json!({
            "ms": run.duration.map(|d| d.as_millis() as u64),
            "note": "observed wall clock; never compared",
        });
    }
    if wants(EvidenceField::Tokens) {
        out["tokens"] = match run.tokens {
            Some((p, c)) => json!({"available": true, "prompt": p, "completion": c}),
            None if run.model_calls == 0 => {
                json!({"available": false, "reason": "no model call was made"})
            }
            None => json!({"available": false, "reason": "the provider reported no usage"}),
        };
    }
    out["cost"] = json!({"available": false, "reason": "no provider in this workspace reports cost; it is never estimated"});
    out
}

fn conclusion_json(c: &Conclusion) -> Value {
    match c {
        Conclusion::NotApplicable(why) => json!({"name": c.name(), "why": why}),
        Conclusion::Incomplete(reasons) => json!({"name": c.name(), "reasons": reasons}),
        _ => json!({"name": c.name()}),
    }
}

/// The machine-readable report, redacted.
pub fn report_json(
    resolved: &ResolvedExperiment,
    result: &ExperimentResult,
    provenance: &Provenance,
    redactor: &Redactor,
) -> Value {
    let d = &resolved.definition;
    let (guarantees, limitations) = mode_guarantees(d.mode);
    let strategy = |s: &crate::registry::ResolvedStrategy| {
        json!({
            "id": s.strategy.id(),
            "version": s.strategy.version(),
            "description": s.strategy.description(),
            "production": s.production,
            "extension_points": s.strategy.extension_points().iter().map(|p| p.name()).collect::<Vec<_>>(),
        })
    };
    let c = &result.comparison;
    let report = json!({
        "schema": REPORT_SCHEMA,
        "experiment": {
            "id": d.id,
            "version": d.version,
            "definition_digest": d.digest(),
            "definition": d.to_json(),
        },
        "provenance": {
            "host": provenance.host,
            "source_revision": provenance.source_revision,
            "chip_experiments_version": env!("CARGO_PKG_VERSION"),
        },
        "mode": {
            "name": d.mode.name(),
            "guarantees": guarantees,
            "limitations": limitations,
            "capabilities": resolved.confinement.iter().map(|(c, conf): &(String, Confinement)| json!({"id": c, "confinement": conf.name()})).collect::<Vec<_>>(),
        },
        "model": {
            "kind": d.model.name(),
            "credentials": "none used",
        },
        "strategies": {
            "baseline": strategy(&resolved.baseline),
            "candidate": resolved.candidate.as_ref().map(strategy),
        },
        "tasks": resolved.tasks.iter().map(|t| json!({"id": t.id, "version": t.version, "fixture_digest": t.digest()})).collect::<Vec<_>>(),
        "criteria": resolved.criteria.iter().map(|c| json!({"id": c.id(), "describe": c.describe()})).collect::<Vec<_>>(),
        "runs": result.runs.iter().map(|r| run_json(r, &d.evidence)).collect::<Vec<_>>(),
        "comparison": {
            "conclusion": conclusion_json(&c.conclusion),
            "winner": c.conclusion.winner().map(|s| s.name()),
            "verified": {"baseline": c.verified_baseline, "candidate": c.verified_candidate, "tasks": c.tasks},
            "differences": c.differences,
            "basis": "verified task outcomes only; decisions, executions and durations are reported, not ranked",
        },
        "cleanup": {
            "ok": result.cleanup_failures.is_empty(),
            "failures": result.cleanup_failures,
        },
        "limitations": [
            "the report references host-observed evidence by digest; it is not an authoritative record of external reality",
            "this crate does not store the report; its durability is whatever the caller writes it to",
            "a promotion decision is a human decision; nothing here changes a production default",
        ],
    });
    redactor.scrub_value(report)
}

/// The human-readable summary, redacted.
pub fn summary(
    resolved: &ResolvedExperiment,
    result: &ExperimentResult,
    redactor: &Redactor,
) -> String {
    let d = &resolved.definition;
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };
    line(format!(
        "Experiment {}@{} ({} mode)",
        d.id,
        d.version,
        d.mode.name()
    ));
    line(format!(
        "Baseline: {}@{}{}",
        resolved.baseline.strategy.id(),
        resolved.baseline.strategy.version(),
        if resolved.baseline.production {
            " (production)"
        } else {
            ""
        }
    ));
    if let Some(c) = &resolved.candidate {
        line(format!(
            "Candidate: {}@{}",
            c.strategy.id(),
            c.strategy.version()
        ));
    }
    line(format!("Criteria: {}", d.criteria.join(", ")));
    for run in &result.runs {
        let detail = match &run.verdict {
            Verdict::Verified => String::new(),
            Verdict::CompletedUnverified { failed } => {
                format!(" (claimed complete; failed: {})", failed.join(", "))
            }
            Verdict::Incomplete { state, reason } => format!(" ({state}: {reason})"),
            Verdict::RuntimeFailure { reason, .. } => format!(" ({reason})"),
            Verdict::Cancelled {
                cause, termination, ..
            } => format!(
                " ({cause}; termination {})",
                match termination {
                    Termination::Known => "known".to_string(),
                    Termination::Uncertain(why) => format!("uncertain: {why}"),
                }
            ),
            Verdict::EvidenceInvalid { reason } | Verdict::NotRun { reason } => {
                format!(" ({reason})")
            }
        };
        line(format!(
            "  {} {}: {}{} — {} execution(s), {} model call(s)",
            run.task,
            run.side.name(),
            run.verdict.class(),
            detail,
            run.executions,
            run.model_calls
        ));
    }
    let c = &result.comparison;
    line(format!(
        "Verified: baseline {}/{}, candidate {}/{}",
        c.verified_baseline,
        c.tasks,
        c.verified_candidate,
        if resolved.candidate.is_some() {
            c.tasks
        } else {
            0
        }
    ));
    match &c.conclusion {
        Conclusion::Incomplete(reasons) => line(format!(
            "Conclusion: incomplete, no winner ({})",
            reasons.join("; ")
        )),
        Conclusion::NotApplicable(why) => line(format!("Conclusion: not applicable ({why})")),
        other => line(format!("Conclusion: {}", other.name().replace('_', " "))),
    }
    for difference in &c.differences {
        line(format!("  difference: {difference}"));
    }
    if result.cleanup_failures.is_empty() {
        line("Cleanup: ok".into());
    } else {
        line(format!(
            "Cleanup FAILED: {}",
            result.cleanup_failures.join("; ")
        ));
    }
    line(
        "Nothing here changes a production default; promotion is a separate, human decision."
            .into(),
    );
    redactor.scrub(&out)
}
