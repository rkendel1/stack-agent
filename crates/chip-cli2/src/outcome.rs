//! Turning cli2's recorded result into a Chip observation that keeps the distinctions apart, and
//! reading such an observation back.
//!
//! The first line of a `cli2.run` observation's output is one canonical JSON object; what
//! follows is bounded text from the task's own output, which is data and never read for meaning.

use chip_core::{
    ExecutionError, ExecutionId, ExecutionResult, ExecutionStatus, Observation, ObservationKind,
};
use serde_json::{Value, json};

use crate::{CLI2_RUN, Task};

/// How a `cli2.run` ended, from the cli2 record. Exactly one applies. None of them is "the goal
/// is satisfied".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// Ran, exited 0, and the checks the host declared for this task passed (cli2 evaluated them).
    Verified,
    /// Ran and exited 0; nothing task-level was established.
    SucceededUnverified,
    /// Ran and failed: exit status, signal or timeout.
    ProcessFailed,
    /// Ran to completion, but a declared check did not hold.
    VerificationFailed,
    /// Nothing ran: approval declined, policy forbids, or the plan went stale.
    Rejected,
    /// The run was cancelled.
    Cancelled,
}

impl RunOutcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::SucceededUnverified => "succeeded_unverified",
            Self::ProcessFailed => "process_failed",
            Self::VerificationFailed => "verification_failed",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "verified" => Self::Verified,
            "succeeded_unverified" => Self::SucceededUnverified,
            "process_failed" => Self::ProcessFailed,
            "verification_failed" => Self::VerificationFailed,
            "rejected" => Self::Rejected,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }
}

/// What a `cli2.run` observation says, for callers that need more than the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub task: String,
    pub outcome: RunOutcome,
    /// A process was actually started.
    pub ran: bool,
    /// cli2 does not have the task declared read-only.
    pub may_change: bool,
    pub execution_id: Option<String>,
    pub plan_id: Option<String>,
    pub plan_digest: Option<String>,
    pub exit_code: Option<i64>,
}

/// Parse the canonical first line of a `cli2.run` observation. `None` for anything else, and for
/// a line that does not say exactly what this crate writes.
pub fn run_summary(observation: &Observation) -> Option<RunSummary> {
    let first = observation.output.as_deref()?.lines().next()?;
    let v: Value = serde_json::from_str(first).ok()?;
    if v.get("capability")?.as_str()? != CLI2_RUN {
        return None;
    }
    Some(RunSummary {
        task: v.get("task")?.as_str()?.to_string(),
        outcome: RunOutcome::parse(v.get("outcome")?.as_str()?)?,
        ran: v.get("ran")?.as_bool()?,
        may_change: v.get("may_change")?.as_bool()?,
        execution_id: v
            .get("execution_id")
            .and_then(Value::as_str)
            .map(String::from),
        plan_id: v.get("plan_id").and_then(Value::as_str).map(String::from),
        plan_digest: v
            .get("plan_digest")
            .and_then(Value::as_str)
            .map(String::from),
        exit_code: v.get("exit_code").and_then(Value::as_i64),
    })
}

/// Whether this observation records a task that ran and that cli2 does not have declared
/// read-only: the project may differ from before it, so a test pass that predates it does not
/// describe the project now. A refusal (nothing ran) is not a change.
pub fn may_have_changed(observation: &Observation) -> bool {
    matches!(
        observation.kind,
        ObservationKind::ExecutionCompleted
            | ObservationKind::ExecutionFailed
            | ObservationKind::ExecutionCancelled
    ) && run_summary(observation).is_some_and(|s| s.ran && s.may_change)
}

const TAIL: usize = 2048;

fn tail(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// The observation for a finished execution, from cli2's execution snapshot.
pub(crate) fn result(
    id: ExecutionId,
    task: &Task,
    plan_id: &str,
    digest: &str,
    snap: &Value,
    mut fetch: impl FnMut(&str) -> Option<String>,
) -> Result<ExecutionResult, ExecutionError> {
    let status = snap["task_status"].as_str().unwrap_or("");
    let verification = snap["verification"].as_str().unwrap_or("not_run");
    let step = &snap["steps"][0];
    let termination = step["termination"].as_str().unwrap_or("not_started");
    let ran = matches!(
        termination,
        "exited" | "signaled" | "timed_out" | "cancelled"
    );
    let outcome = match status {
        "succeeded" if verification == "passed" => RunOutcome::Verified,
        "succeeded" => RunOutcome::SucceededUnverified,
        "verification_failed" => RunOutcome::VerificationFailed,
        "failed" | "partially_succeeded" => RunOutcome::ProcessFailed,
        "cancelled" => RunOutcome::Cancelled,
        "rejected" | "awaiting_approval" | "needs_clarification" | "planned" => {
            RunOutcome::Rejected
        }
        other => {
            // cli2 finished it and said something this adapter does not know. Fail closed.
            return Err(ExecutionError::OutcomeUnknown(format!(
                "cli2 reported the finished execution {} with an unrecognized status {other:?}",
                snap["execution_id"]
            )));
        }
    };
    let execution_id = snap["execution_id"].as_str().unwrap_or_default();
    let mut head = json!({
        "capability": CLI2_RUN,
        "task": task.id,
        "outcome": outcome.name(),
        "ran": ran && outcome != RunOutcome::Rejected,
        "may_change": task.may_change,
        "cli2_status": status,
        "cli2_verification": verification,
        "execution_id": execution_id,
        "plan_id": plan_id,
        "plan_digest": digest,
    });
    if let Some(code) = step["exit_code"].as_i64() {
        head["exit_code"] = json!(code);
    }
    head["termination"] = json!(termination);
    let mut out = String::new();
    for (label, key) in [("stdout", "stdout_ref"), ("stderr", "stderr_ref")] {
        if let Some(r) = step[key].as_str() {
            head[key] = json!(r);
            if let Some(text) = fetch(r)
                && !text.trim().is_empty()
            {
                out.push_str(&format!(
                    "\n--- {label} (last {TAIL} bytes at most; full output: {r}) ---\n"
                ));
                out.push_str(tail(&text, TAIL));
            }
        }
    }
    let text = format!("{head}{out}");
    Ok(match outcome {
        RunOutcome::Verified | RunOutcome::SucceededUnverified => {
            ExecutionResult::success(id, text).with_receipt_id(execution_id)
        }
        RunOutcome::Cancelled => {
            let mut r = ExecutionResult::failure(id, text).with_receipt_id(execution_id);
            r.status = ExecutionStatus::Cancelled;
            r
        }
        _ => ExecutionResult::failure(id, text).with_receipt_id(execution_id),
    })
}
