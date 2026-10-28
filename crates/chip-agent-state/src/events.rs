//! Every event of a work, durable as it happens: the `chip.WorkEvent` collection.
//!
//! Intents, observations and the work result say what was executed and how the work ended. The
//! work's event trajectory says everything else: each decision and who made it, each model call
//! and what the provider reported, each capability request, each evaluation of the goal, each
//! limit and the terminal state. One record per event, written through the same guarded
//! "must not exist yet" transaction every Chip record uses, as the loop records the event, so a
//! process that is killed leaves every event it had recorded.
//!
//! # Provenance
//!
//! Every record says where its fact came from, because a trajectory mixes what reality showed
//! with what the model said:
//!
//! * [`Provenance::HostObserved`]: the host saw it happen. An execution, an observation, evidence,
//!   a goal evaluation, whether a model call returned and the usage the provider reported.
//! * [`Provenance::ModelInferred`]: the model said it. A decision the model made, the capability it
//!   asked for. It is a claim, never evidence.
//! * [`Provenance::RuntimeDecided`]: Chip decided it. The start of the work, a local decision, the
//!   choice to ask the model, a context limit, the terminal state.
//!
//! The record carries no execution output and no prompt: an execution's output is its
//! `chip.Observation` record, which the event names with the output's size and SHA-256.

use chip_core::{CapabilityEvent, ExecutionEvent, WorkEvent};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Where a recorded fact came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    HostObserved,
    ModelInferred,
    RuntimeDecided,
}

impl Provenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HostObserved => "host_observed",
            Self::ModelInferred => "model_inferred",
            Self::RuntimeDecided => "runtime_decided",
        }
    }
}

/// One `chip.WorkEvent` record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkEventRecord {
    pub schema: String,
    pub task_id: String,
    pub work_id: String,
    pub attempt: u64,
    pub fence: u64,
    /// The event's position in this attempt's trajectory, from 0, without gaps.
    pub index: u64,
    pub recorded_at_ms: u64,
    pub provenance: Provenance,
    /// The event: `kind` and its fields (turns are 1-based, as every report shows them).
    pub event: Value,
}

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The event as it is recorded, and where its fact came from. `observation` names the
/// `chip.Observation` record of an execution's id, for the events that have one.
pub fn describe(event: &WorkEvent, observation: &dyn Fn(&str) -> String) -> (Provenance, Value) {
    use Provenance::{HostObserved, ModelInferred, RuntimeDecided};
    match event {
        WorkEvent::WorkStarted { goal, limits, .. } => (
            RuntimeDecided,
            json!({
                "kind": "WorkStarted",
                "goal_sha256": sha256(goal),
                "goal_bytes": goal.len(),
                "max_turns": limits.max_turns,
                "max_executions": limits.max_executions,
            }),
        ),
        WorkEvent::Capability(e) => match e {
            CapabilityEvent::CapabilitiesRequested => {
                (HostObserved, json!({"kind": "CapabilitiesRequested"}))
            }
            CapabilityEvent::CapabilitiesAvailable { count } => (
                HostObserved,
                json!({"kind": "CapabilitiesAvailable", "count": count}),
            ),
            CapabilityEvent::CapabilitiesUnavailable { reason } => (
                HostObserved,
                json!({"kind": "CapabilitiesUnavailable", "reason": reason}),
            ),
            // The operator's authorization withheld it: a decision of the runtime, not something a
            // host observed and not something a model said.
            CapabilityEvent::CapabilityWithheld { capability, reason } => (
                RuntimeDecided,
                json!({"kind": "CapabilityWithheld", "capability": capability, "reason": reason}),
            ),
        },
        WorkEvent::DecisionStarted { turn, .. } => (
            RuntimeDecided,
            json!({"kind": "DecisionStarted", "turn": turn + 1}),
        ),
        WorkEvent::LocalDecision { turn, decision, .. } => (
            RuntimeDecided,
            json!({"kind": "LocalDecision", "turn": turn + 1, "decision": decision}),
        ),
        WorkEvent::ModelEscalation {
            turn,
            reason,
            context_policy,
            context,
            profile,
            ..
        } => (RuntimeDecided, {
            let mut payload = json!({
                "kind": "ModelEscalation",
                "turn": turn + 1,
                "reason": reason,
                "context_policy": context_policy,
                "context_bytes": context.bytes,
            });
            // Which context profile the request was assembled under, and the rule that chose
            // it: present only for a policy with profiles, so other records read as before.
            if let Some(p) = profile {
                payload["context_profile"] = json!({
                    "profile": p.profile.name(),
                    "reason": p.reason,
                    "previous": p.previous.map(|q| q.name()),
                    "sections": p.sections,
                    "condensed_observations": context.condensed_observations,
                    "omitted_observations": context.omitted_observations,
                });
            }
            payload
        }),
        // That the call returned is observed; the token counts are the provider's report of it.
        WorkEvent::ModelCalled {
            turn,
            usage,
            succeeded,
            ..
        } => (
            HostObserved,
            json!({
                "kind": "ModelCalled",
                "turn": turn + 1,
                "succeeded": succeeded,
                "usage_reported": usage.is_some(),
                "prompt_tokens": usage.as_ref().and_then(|u| u.prompt_tokens),
                "completion_tokens": usage.as_ref().and_then(|u| u.completion_tokens),
                "reasoning_tokens": usage.as_ref().and_then(|u| u.reasoning_tokens),
                "cache_read_tokens": usage.as_ref().and_then(|u| u.cache_read_tokens),
                "cache_write_tokens": usage.as_ref().and_then(|u| u.cache_write_tokens),
                "cost": usage.as_ref().and_then(|u| u.cost.clone()),
                "model_reported": usage.as_ref().and_then(|u| u.model.clone()),
            }),
        ),
        WorkEvent::ContextLimit {
            turn,
            request_bytes,
            budget_bytes,
            ..
        } => (
            RuntimeDecided,
            json!({
                "kind": "ContextLimit",
                "turn": turn + 1,
                "request_bytes": request_bytes,
                "budget_bytes": budget_bytes,
            }),
        ),
        // Chip chose to answer the turn from a recorded response instead of a model call; what the
        // response says is the model's, and arrives as the DecisionMade that follows.
        WorkEvent::ModelResponseRestored { turn, record, .. } => (
            RuntimeDecided,
            json!({"kind": "ModelResponseRestored", "turn": turn + 1, "record": record}),
        ),
        WorkEvent::ModelResponseDiscarded {
            turn,
            record,
            reason,
            ..
        } => (
            RuntimeDecided,
            json!({"kind": "ModelResponseDiscarded", "turn": turn + 1, "record": record, "reason": reason}),
        ),
        WorkEvent::DecisionMade { turn, decision, .. } => (
            ModelInferred,
            json!({"kind": "DecisionMade", "turn": turn + 1, "decision": decision}),
        ),
        // The runtime's verdict on the reply's form; the reply's own text is not recorded here.
        WorkEvent::DecisionRejected {
            turn,
            reason,
            consecutive,
            limit,
            recovering,
            ..
        } => (
            RuntimeDecided,
            json!({
                "kind": "DecisionRejected",
                "turn": turn + 1,
                "reason": reason,
                "consecutive": consecutive,
                "limit": limit,
                "recovering": recovering,
            }),
        ),
        WorkEvent::CapabilityRequested {
            turn, capability, ..
        } => (
            ModelInferred,
            json!({"kind": "CapabilityRequested", "turn": turn + 1, "capability": capability.to_string()}),
        ),
        WorkEvent::Execution(e) => (
            HostObserved,
            match e {
                ExecutionEvent::ExecutionRequested { id, intent } => json!({
                    "kind": "ExecutionRequested",
                    "execution_id": id.to_string(),
                    "capability": intent,
                }),
                ExecutionEvent::ExecutionStarted { id } => {
                    json!({"kind": "ExecutionStarted", "execution_id": id.to_string()})
                }
                ExecutionEvent::ExecutionCompleted { id, output } => json!({
                    "kind": "ExecutionCompleted",
                    "execution_id": id.to_string(),
                    "output_bytes": output.len(),
                    "output_sha256": sha256(output),
                    "observation_record": observation(&id.0),
                }),
                ExecutionEvent::ExecutionFailed { id, reason } => json!({
                    "kind": "ExecutionFailed",
                    "execution_id": id.to_string(),
                    "reason": reason,
                    "observation_record": observation(&id.0),
                }),
            },
        ),
        WorkEvent::ObservationRecorded {
            turn,
            execution_id,
            kind,
            receipt_id,
            ..
        } => (
            HostObserved,
            json!({
                "kind": "ObservationRecorded",
                "turn": turn + 1,
                "execution_id": execution_id.to_string(),
                "observation": kind.as_str(),
                "receipt_id": receipt_id,
            }),
        ),
        WorkEvent::ObservationRestored {
            execution_id,
            capability,
            kind,
            receipt_id,
            remaining,
            reobserved,
            ..
        } => (
            HostObserved,
            json!({
                "kind": "ObservationRestored",
                "execution_id": execution_id.to_string(),
                "capability": capability.to_string(),
                "observation": kind.as_str(),
                "receipt_id": receipt_id,
                "remaining": remaining,
                "reobserved": reobserved,
            }),
        ),
        WorkEvent::EvidenceRecorded {
            turn, capability, ..
        } => (
            HostObserved,
            json!({"kind": "EvidenceRecorded", "turn": turn + 1, "capability": capability.to_string()}),
        ),
        WorkEvent::EvidenceReused {
            turn,
            capability,
            receipt_id,
            ..
        } => (
            HostObserved,
            json!({
                "kind": "EvidenceReused",
                "turn": turn + 1,
                "capability": capability.to_string(),
                "receipt_id": receipt_id,
            }),
        ),
        WorkEvent::GoalEvaluated {
            turn,
            satisfied,
            remaining,
            ..
        } => (
            HostObserved,
            json!({
                "kind": "GoalEvaluated",
                "turn": turn + 1,
                "satisfied": satisfied,
                "remaining": remaining,
            }),
        ),
        // The terminal state is Chip's; a reason may quote the model, and says so only as text.
        WorkEvent::WorkCompleted { .. } => (RuntimeDecided, json!({"kind": "WorkCompleted"})),
        WorkEvent::WorkEscalated { reason, .. } => (
            RuntimeDecided,
            json!({"kind": "WorkEscalated", "reason": reason}),
        ),
        WorkEvent::WorkBlocked { reason, .. } => (
            RuntimeDecided,
            json!({"kind": "WorkBlocked", "reason": reason}),
        ),
        WorkEvent::WorkLimitReached { limit, .. } => (
            RuntimeDecided,
            json!({"kind": "WorkLimitReached", "limit": limit.name()}),
        ),
        WorkEvent::WorkFailed {
            reason, delivery, ..
        } => (
            RuntimeDecided,
            json!({
                "kind": "WorkFailed",
                "reason": reason,
                "delivery": delivery.map(|d| d.as_str()),
            }),
        ),
        WorkEvent::WorkCancelled {
            cause,
            reason,
            delivery,
            remote,
            ..
        } => (
            RuntimeDecided,
            json!({
                "kind": "WorkCancelled",
                "cause": cause.name(),
                "reason": reason,
                "delivery": delivery.map(|d| d.as_str()),
                "remote_cancellation": remote.map(|r| r.as_str()),
            }),
        ),
    }
}
