//! Nothing but the allowlist reaches telemetry: no goal, prompt, reply, reason, output, path or
//! credential, whether it comes through the loop's own events or is planted in a hostile record.

mod common;

use chip_agent_state::Provenance;
use chip_core::{CapabilityEvent, Delivery, ExecutionEvent, ModelUsage, StopCause, WorkEvent};
use chip_inference::RemoteCancellation;
use chip_otel::{Context, trace};
use common::*;
use serde_json::{Value, json};

const SENTINEL: &str = "LEAKSENTINEL";

/// Credential-shaped strings, assembled at run time so no scanner mistakes this file for a leak.
fn credentials() -> Vec<String> {
    let k = |parts: &[&str]| parts.concat();
    vec![
        k(&[
            "sk-ant-api03-",
            "Zq8vR2mXn4LpW7tYc3HbK9sFj6Dg1Ae5Uo0IyQwErTx",
        ]),
        k(&["sk-proj-", "a1B2c3D4e5F6g7H8i9J0kLmNoPqRsTuVwXyZ0123"]),
        k(&["ghp_", "abcdefghijklmnopqrstuvwxyz0123456789"]),
        k(&["AKIA", "IOSFODNN7EXAMPLE"]),
        k(&[
            "xoxb-",
            "123456789012-1234567890123-AbCdEfGhIjKlMnOpQrStUvWx",
        ]),
        k(&[
            "eyJhbGciOiJIUzI1NiJ9",
            ".eyJzdWIiOiIxMjM0NTY3ODkwIn0",
            ".dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
        ]),
    ]
}

fn assert_clean(request: &Value, secrets: &[String]) {
    let text = request.to_string();
    assert!(
        !text.contains(SENTINEL),
        "a sentinel reached telemetry: {text}"
    );
    for s in secrets {
        assert!(
            !text.contains(s.as_str()),
            "a credential reached telemetry: {s} in {text}"
        );
        // Not even a long piece of one.
        let tail = &s[s.len() - 12..];
        assert!(
            !text.contains(tail),
            "part of a credential reached telemetry: {tail}"
        );
    }
    for marker in [
        "/home/",
        "/Users/",
        "\\Users\\",
        "BEGIN PRIVATE KEY",
        "Authorization",
        "Bearer",
    ] {
        assert!(!text.contains(marker), "{marker} reached telemetry");
    }
}

#[test]
fn free_text_in_the_loops_own_events_never_reaches_telemetry() {
    let secrets = credentials();
    let leak = |what: &str| {
        format!(
            "{SENTINEL} {what} /home/alice/project {} Bearer {}",
            secrets[0], secrets[2]
        )
    };
    let w = work();
    let events = vec![
        WorkEvent::WorkStarted {
            work_id: w.clone(),
            goal: leak("goal"),
            limits: Default::default(),
        },
        WorkEvent::Capability(CapabilityEvent::CapabilitiesUnavailable {
            reason: leak("capabilities"),
        }),
        WorkEvent::LocalDecision {
            work_id: w.clone(),
            turn: 0,
            decision: leak("local decision"),
        },
        WorkEvent::ModelEscalation {
            work_id: w.clone(),
            turn: 0,
            reason: leak("escalation"),
            context_policy: leak("policy"),
            context: metrics(10),
            profile: None,
        },
        WorkEvent::ModelCalled {
            work_id: w.clone(),
            turn: 0,
            usage: Some(ModelUsage {
                prompt_tokens: Some(1),
                cost: Some(leak("cost")),
                model: Some(secrets[1].clone()),
                ..Default::default()
            }),
            succeeded: true,
        },
        WorkEvent::DecisionMade {
            work_id: w.clone(),
            turn: 0,
            decision: leak("model decision"),
        },
        WorkEvent::Execution(ExecutionEvent::ExecutionRequested {
            id: exec(1),
            intent: leak("intent"),
        }),
        WorkEvent::Execution(ExecutionEvent::ExecutionCompleted {
            id: exec(1),
            output: format!("{}\n-----BEGIN PRIVATE KEY-----\nMIIE\n", leak("output")),
        }),
        WorkEvent::Execution(ExecutionEvent::ExecutionRequested {
            id: exec(2),
            intent: "project.test".into(),
        }),
        WorkEvent::Execution(ExecutionEvent::ExecutionFailed {
            id: exec(2),
            reason: leak("execution failure"),
        }),
        WorkEvent::ObservationRecorded {
            work_id: w.clone(),
            turn: 0,
            execution_id: exec(1),
            kind: chip_core::ObservationKind::ExecutionCompleted,
            receipt_id: Some(leak("receipt")),
        },
        WorkEvent::EvidenceReused {
            work_id: w.clone(),
            turn: 0,
            capability: cap("project.test"),
            receipt_id: Some(secrets[3].clone()),
        },
        WorkEvent::WorkEscalated {
            work_id: w.clone(),
            reason: leak("escalated"),
        },
        WorkEvent::WorkBlocked {
            work_id: w.clone(),
            reason: leak("blocked"),
        },
        WorkEvent::WorkFailed {
            work_id: w.clone(),
            reason: leak("failed"),
            delivery: Some(Delivery::ConfirmedResponse),
        },
        WorkEvent::WorkCancelled {
            work_id: w,
            cause: StopCause::Deadline,
            reason: leak("cancelled"),
            delivery: None,
            remote: Some(RemoteCancellation::Unconfirmed),
        },
    ];
    let mut verification = verified();
    verification.evidence_invalidated = Some(leak("invalidated"));
    let mut h = history(
        records(1, &events, 1),
        vec![result(
            1,
            &leak("outcome"),
            &leak("detail"),
            Some(verification),
        )],
    );
    h.task.objective = leak("objective");
    let ctx = Context {
        service_name: leak("service"),
        service_version: secrets[4].clone(),
        provider: Some(secrets[5].clone()),
        request_model: Some(secrets[0].clone()),
    };
    let request = trace(&h, &ctx);
    assert_clean(&request, &secrets);
    // What is safe is still there: the export was not simply emptied.
    let spans = spans(&request);
    assert_eq!(named(&spans, "invoke_agent").len(), 1);
    assert_eq!(named(&spans, "execute_tool").len(), 2);
    assert_eq!(
        int_attr(named(&spans, "chat")[0], "gen_ai.usage.input_tokens"),
        Some(1)
    );
    let events = spans[0]["events"].as_array().unwrap();
    let cancelled = events
        .iter()
        .find(|e| e["name"] == "WorkCancelled")
        .unwrap();
    assert_eq!(
        str_attr(cancelled, "stack_agent.stop_cause"),
        Some("deadline")
    );
    assert_eq!(
        str_attr(cancelled, "stack_agent.remote_cancellation"),
        Some("unconfirmed")
    );
}

/// Every key any event kind has, and a few it never has.
const KEYS: &[&str] = &[
    "goal",
    "goal_sha256",
    "goal_bytes",
    "max_turns",
    "max_executions",
    "count",
    "reason",
    "turn",
    "decision",
    "context_policy",
    "context_bytes",
    "succeeded",
    "usage_reported",
    "prompt_tokens",
    "completion_tokens",
    "reasoning_tokens",
    "cache_read_tokens",
    "cache_write_tokens",
    "cost",
    "model_reported",
    "request_bytes",
    "budget_bytes",
    "capability",
    "execution_id",
    "output",
    "output_bytes",
    "output_sha256",
    "observation_record",
    "observation",
    "receipt_id",
    "remaining",
    "reobserved",
    "satisfied",
    "limit",
    "delivery",
    "cause",
    "remote_cancellation",
    "prompt",
    "messages",
    "content",
    "text",
    "path",
    "api_key",
    "authorization",
    "headers",
    "env",
];

const KINDS: &[&str] = &[
    "WorkStarted",
    "CapabilitiesRequested",
    "CapabilitiesAvailable",
    "CapabilitiesUnavailable",
    "DecisionStarted",
    "LocalDecision",
    "ModelEscalation",
    "ModelCalled",
    "ContextLimit",
    "DecisionMade",
    "CapabilityRequested",
    "ExecutionRequested",
    "ExecutionStarted",
    "ExecutionCompleted",
    "ExecutionFailed",
    "ObservationRecorded",
    "ObservationRestored",
    "EvidenceRecorded",
    "EvidenceReused",
    "GoalEvaluated",
    "WorkCompleted",
    "WorkEscalated",
    "WorkBlocked",
    "WorkLimitReached",
    "WorkFailed",
    "WorkCancelled",
    "SomethingNew",
];

#[test]
fn a_hostile_record_cannot_put_anything_outside_the_allowlist_into_telemetry() {
    // A record is data on disk; one edited by hand, or written by a future version, can hold
    // anything. Every key of every kind holds a sentinel or a credential, as text and nested.
    let secrets = credentials();
    for (k, kind) in KINDS.iter().enumerate() {
        for (v, value) in [
            json!(format!("{SENTINEL} {}", secrets[0])),
            json!(secrets[k % secrets.len()]),
            json!(format!("request {}", secrets[0].to_ascii_lowercase())),
            json!({"nested": SENTINEL}),
            json!([SENTINEL]),
        ]
        .into_iter()
        .enumerate()
        {
            let mut event = serde_json::Map::new();
            for key in KEYS {
                event.insert((*key).into(), value.clone());
            }
            event.insert("kind".into(), json!(kind));
            let mut rs = records(1, &[], 0);
            for i in 0..2u64 {
                rs.push(chip_agent_state::WorkEventRecord {
                    schema: "chip.work_event.v1".into(),
                    task_id: TASK.into(),
                    work_id: format!("{SENTINEL}{v}"),
                    attempt: 1,
                    fence: 1,
                    index: i,
                    recorded_at_ms: i,
                    provenance: Provenance::HostObserved,
                    event: Value::Object(event.clone()),
                });
            }
            let request = trace(&history(rs, Vec::new()), &context());
            assert_clean(&request, &secrets);
        }
    }
}

#[test]
fn an_attribute_value_is_always_a_count_a_flag_or_a_short_restricted_string() {
    let h = history(
        records(1, &completed_attempt(), 1),
        vec![result(1, "completed", "", Some(verified()))],
    );
    let request = trace(&h, &context());
    let mut items: Vec<&Value> = Vec::new();
    for span in spans(&request).iter() {
        let span: &'static Value = Box::leak(Box::new(span.clone()));
        items.push(span);
        items.extend(span["events"].as_array().unwrap());
    }
    for item in items {
        for a in item["attributes"].as_array().unwrap() {
            let value = &a["value"];
            let key = a["key"].as_str().unwrap();
            assert!(
                key.starts_with("gen_ai.")
                    || key.starts_with("stack_agent.")
                    || key == "error.type",
                "{key}"
            );
            if let Some(s) = value["stringValue"].as_str() {
                assert!(s.len() <= 128, "{key}");
                assert!(
                    s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._:/@~- ".contains(&b)),
                    "{key}={s}"
                );
            } else {
                assert!(
                    value["intValue"].is_string() || value["boolValue"].is_boolean(),
                    "{key}={value}"
                );
            }
        }
    }
}
