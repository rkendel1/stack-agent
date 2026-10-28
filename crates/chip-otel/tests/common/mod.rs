//! A FeltDB history built from the loop's own events, through the same `describe` the durable
//! record uses, without running a work: the export is tested on the record alone.

#![allow(dead_code)]

use chip_agent_state::{
    History, TaskValue, Verification, WorkEventRecord, WorkResultRecord, describe_event,
};
use chip_core::{
    CapabilityEvent, CapabilityId, ContextMetrics, ExecutionEvent, ExecutionId, LimitKind,
    ModelUsage, ObservationKind, WorkEvent, WorkId, WorkLimits,
};
use serde_json::{Value, json};

pub const TASK: &str = "work_00000000000000aa";

pub fn work() -> WorkId {
    WorkId::new(TASK)
}

pub fn exec(n: u32) -> ExecutionId {
    ExecutionId::new(format!("{TASK}-exec-{n}"))
}

pub fn cap(id: &str) -> CapabilityId {
    CapabilityId::new(id).unwrap()
}

pub fn metrics(bytes: usize) -> ContextMetrics {
    ContextMetrics {
        bytes,
        chars: bytes,
        observations: 0,
        decisions: 0,
        evidence_items: 0,
        ruled_out: 0,
        messages: 1,
        system_messages: 0,
        user_messages: 1,
        assistant_messages: 0,
        omitted_observations: 0,
        condensed_observations: 0,
    }
}

pub fn usage() -> ModelUsage {
    ModelUsage {
        prompt_tokens: Some(1200),
        completion_tokens: Some(80),
        reasoning_tokens: Some(30),
        cache_read_tokens: Some(1000),
        cache_write_tokens: Some(0),
        cost: Some("0.00421".into()),
        model: Some("claude-sonnet-4-5-20250929".into()),
    }
}

/// One attempt that asks the model, runs one capability and completes.
pub fn completed_attempt() -> Vec<WorkEvent> {
    let w = work();
    vec![
        WorkEvent::WorkStarted {
            work_id: w.clone(),
            goal: "make the tests pass".into(),
            limits: WorkLimits {
                max_turns: 4,
                max_executions: 3,
            },
        },
        WorkEvent::Capability(CapabilityEvent::CapabilitiesRequested),
        WorkEvent::Capability(CapabilityEvent::CapabilitiesAvailable { count: 5 }),
        WorkEvent::DecisionStarted {
            work_id: w.clone(),
            turn: 0,
        },
        WorkEvent::ModelEscalation {
            work_id: w.clone(),
            turn: 0,
            reason: "no local decision".into(),
            context_policy: "full".into(),
            context: metrics(2048),
            profile: None,
        },
        WorkEvent::ModelCalled {
            work_id: w.clone(),
            turn: 0,
            usage: Some(usage()),
            succeeded: true,
        },
        WorkEvent::DecisionMade {
            work_id: w.clone(),
            turn: 0,
            decision: "request project.test".into(),
        },
        WorkEvent::CapabilityRequested {
            work_id: w.clone(),
            turn: 0,
            capability: cap("project.test"),
        },
        WorkEvent::Execution(ExecutionEvent::ExecutionRequested {
            id: exec(1),
            intent: "project.test".into(),
        }),
        WorkEvent::Execution(ExecutionEvent::ExecutionStarted { id: exec(1) }),
        WorkEvent::Execution(ExecutionEvent::ExecutionCompleted {
            id: exec(1),
            output: "test result: ok".into(),
        }),
        WorkEvent::ObservationRecorded {
            work_id: w.clone(),
            turn: 0,
            execution_id: exec(1),
            kind: ObservationKind::ExecutionCompleted,
            receipt_id: None,
        },
        WorkEvent::EvidenceRecorded {
            work_id: w.clone(),
            turn: 0,
            capability: cap("project.test"),
        },
        WorkEvent::GoalEvaluated {
            work_id: w.clone(),
            turn: 0,
            satisfied: true,
            remaining: 0,
        },
        WorkEvent::WorkCompleted { work_id: w },
    ]
}

pub fn limit_attempt() -> Vec<WorkEvent> {
    let w = work();
    vec![
        WorkEvent::WorkStarted {
            work_id: w.clone(),
            goal: "g".into(),
            limits: WorkLimits::default(),
        },
        WorkEvent::WorkLimitReached {
            work_id: w,
            limit: LimitKind::Turns,
        },
    ]
}

/// The records of one attempt, one millisecond apart from `start_ms`.
pub fn records(attempt: u64, events: &[WorkEvent], start_ms: u64) -> Vec<WorkEventRecord> {
    let observation = |id: &str| format!("{TASK}~a{attempt}.{id}");
    events
        .iter()
        .enumerate()
        .map(|(i, event)| {
            let (provenance, event) = describe_event(event, &observation);
            WorkEventRecord {
                schema: "chip.work_event.v1".into(),
                task_id: TASK.into(),
                work_id: TASK.into(),
                attempt,
                fence: attempt,
                index: i as u64,
                recorded_at_ms: start_ms + i as u64,
                provenance,
                event,
            }
        })
        .collect()
}

pub fn task(status: &str) -> TaskValue {
    serde_json::from_value(json!({
        "checkpoint_version": 1,
        "created_at_ms": 1_700_000_000_000u64,
        "fence": 1,
        "lease": null,
        "objective": "make the tests pass",
        "owner": "stack-agent",
        "status": status,
        "task_id": TASK,
        "unresolved_intents": 0,
        "updated_at_ms": 1_700_000_000_000u64,
    }))
    .unwrap()
}

pub fn result(
    attempt: u64,
    outcome: &str,
    detail: &str,
    verification: Option<Verification>,
) -> WorkResultRecord {
    WorkResultRecord {
        schema: "chip.work_result.v2".into(),
        task_id: TASK.into(),
        work_id: TASK.into(),
        attempt,
        fence: attempt,
        outcome: outcome.into(),
        detail: detail.into(),
        requested_status: "completed".into(),
        turns: 1,
        executions: Vec::new(),
        decisions: Vec::new(),
        persistence_failures: Vec::new(),
        verification,
    }
}

pub fn verified() -> Verification {
    Verification {
        accepted: true,
        verified: true,
        goal_satisfied: Some(true),
        grounded: false,
        evidence_invalidated: None,
        exit_status: 0,
    }
}

pub fn history(events: Vec<WorkEventRecord>, results: Vec<WorkResultRecord>) -> History {
    History {
        task: task("completed"),
        task_version: 1,
        checkpoints: Vec::new(),
        intents: Vec::new(),
        results,
        events,
        model_responses: Vec::new(),
        reconciliations: Vec::new(),
    }
}

pub fn context() -> chip_otel::Context {
    chip_otel::Context {
        service_name: "stack-agent".into(),
        service_version: "0.1.0".into(),
        provider: Some("anthropic".into()),
        request_model: Some("claude-sonnet-4-5".into()),
    }
}

pub fn spans(request: &Value) -> Vec<Value> {
    request["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .unwrap()
        .clone()
}

pub fn attr<'a>(item: &'a Value, key: &str) -> Option<&'a Value> {
    item["attributes"]
        .as_array()?
        .iter()
        .find(|a| a["key"] == key)
        .map(|a| &a["value"])
}

pub fn str_attr<'a>(item: &'a Value, key: &str) -> Option<&'a str> {
    attr(item, key)?["stringValue"].as_str()
}

pub fn int_attr(item: &Value, key: &str) -> Option<u64> {
    attr(item, key)?["intValue"].as_str()?.parse().ok()
}

pub fn bool_attr(item: &Value, key: &str) -> Option<bool> {
    attr(item, key)?["boolValue"].as_bool()
}

pub fn named<'a>(spans: &'a [Value], prefix: &str) -> Vec<&'a Value> {
    spans
        .iter()
        .filter(|s| s["name"].as_str().unwrap().starts_with(prefix))
        .collect()
}
