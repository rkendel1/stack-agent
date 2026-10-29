//! The mapping from a work's durable record to OTLP spans, on records alone: no work runs here.

mod common;

use chip_otel::{Context, trace, trace_id};
use common::*;

#[test]
fn an_attempt_is_an_invoke_agent_span_with_chat_and_execute_tool_children() {
    let h = history(
        records(1, &completed_attempt(), 1_700_000_000_000),
        vec![result(1, "completed", "done", Some(verified()))],
    );
    let request = trace(&h, &context());
    let spans = common::spans(&request);
    assert_eq!(spans.len(), 3);

    let root = &spans[0];
    assert_eq!(root["name"], "invoke_agent stack-agent");
    assert!(root.get("parentSpanId").is_none());
    assert_eq!(
        str_attr(root, "gen_ai.operation.name"),
        Some("invoke_agent")
    );
    assert_eq!(str_attr(root, "gen_ai.agent.name"), Some("stack-agent"));
    assert_eq!(str_attr(root, "gen_ai.conversation.id"), Some(TASK));
    assert_eq!(str_attr(root, "gen_ai.provider.name"), Some("anthropic"));
    assert_eq!(
        str_attr(root, "gen_ai.request.model"),
        Some("claude-sonnet-4-5")
    );
    assert_eq!(str_attr(root, "stack_agent.outcome"), Some("completed"));
    assert_eq!(bool_attr(root, "stack_agent.verified"), Some(true));
    assert_eq!(bool_attr(root, "stack_agent.accepted"), Some(true));
    assert_eq!(bool_attr(root, "stack_agent.interrupted"), Some(false));
    assert_eq!(int_attr(root, "stack_agent.model_calls"), Some(1));
    assert_eq!(int_attr(root, "stack_agent.executions"), Some(1));
    assert_eq!(int_attr(root, "gen_ai.usage.input_tokens"), Some(1200));
    assert_eq!(int_attr(root, "gen_ai.usage.output_tokens"), Some(80));
    assert_eq!(root["status"], serde_json::json!({}));
    assert_eq!(root["startTimeUnixNano"], "1700000000000000000");
    assert_eq!(root["endTimeUnixNano"], "1700000000014000000");

    let chat = &spans[1];
    assert_eq!(chat["name"], "chat claude-sonnet-4-5");
    assert_eq!(chat["kind"], 3, "a model call is a client span");
    assert_eq!(chat["parentSpanId"], root["spanId"]);
    assert_eq!(str_attr(chat, "gen_ai.operation.name"), Some("chat"));
    assert_eq!(
        str_attr(chat, "gen_ai.response.model"),
        Some("claude-sonnet-4-5-20250929")
    );
    assert_eq!(int_attr(chat, "gen_ai.usage.input_tokens"), Some(1200));
    assert_eq!(int_attr(chat, "gen_ai.usage.output_tokens"), Some(80));
    assert_eq!(
        int_attr(chat, "gen_ai.usage.cache_read.input_tokens"),
        Some(1000)
    );
    assert_eq!(
        int_attr(chat, "gen_ai.usage.cache_creation.input_tokens"),
        Some(0)
    );
    assert_eq!(
        int_attr(chat, "stack_agent.usage.reasoning_tokens"),
        Some(30)
    );
    assert_eq!(str_attr(chat, "stack_agent.usage.cost"), Some("0.00421"));
    assert_eq!(int_attr(chat, "stack_agent.context_bytes"), Some(2048));
    assert_eq!(bool_attr(chat, "stack_agent.usage.reported"), Some(true));
    // From the escalation to the call's return.
    assert_eq!(chat["startTimeUnixNano"], "1700000000004000000");
    assert_eq!(chat["endTimeUnixNano"], "1700000000005000000");

    let tool = &spans[2];
    assert_eq!(tool["name"], "execute_tool project.test");
    assert_eq!(tool["parentSpanId"], root["spanId"]);
    assert_eq!(
        str_attr(tool, "gen_ai.operation.name"),
        Some("execute_tool")
    );
    assert_eq!(str_attr(tool, "gen_ai.tool.name"), Some("project.test"));
    assert_eq!(
        str_attr(tool, "gen_ai.tool.call.id"),
        Some(&*format!("{TASK}-exec-1"))
    );
    assert_eq!(int_attr(tool, "stack_agent.output_bytes"), Some(15));
    assert_eq!(
        str_attr(tool, "stack_agent.output_sha256").map(str::len),
        Some(64)
    );

    // Every other event is a span event on the root, in order.
    let names: Vec<&str> = root["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "WorkStarted",
            "CapabilitiesRequested",
            "CapabilitiesAvailable",
            "DecisionStarted",
            "DecisionMade",
            "CapabilityRequested",
            "ObservationRecorded",
            "EvidenceRecorded",
            "GoalEvaluated",
            "WorkCompleted"
        ]
    );
    let decision = &root["events"][4];
    assert_eq!(
        str_attr(decision, "stack_agent.decision"),
        Some("request project.test")
    );
    let observed = &root["events"][6];
    assert_eq!(
        str_attr(observed, "stack_agent.observation"),
        Some("execution.completed")
    );
}

#[test]
fn every_span_and_span_event_says_where_its_fact_came_from() {
    let h = history(records(1, &completed_attempt(), 1), Vec::new());
    let spans = common::spans(&trace(&h, &context()));
    let provenance = |item: &serde_json::Value| {
        str_attr(item, "stack_agent.provenance")
            .unwrap()
            .to_string()
    };
    assert_eq!(provenance(&spans[0]), "runtime_decided");
    assert_eq!(
        provenance(&spans[1]),
        "host_observed",
        "the call returned: observed"
    );
    assert_eq!(provenance(&spans[2]), "host_observed");
    let events = spans[0]["events"].as_array().unwrap();
    let by_name = |n: &str| provenance(events.iter().find(|e| e["name"] == n).unwrap());
    assert_eq!(by_name("DecisionMade"), "model_inferred");
    assert_eq!(by_name("CapabilityRequested"), "model_inferred");
    assert_eq!(by_name("GoalEvaluated"), "host_observed");
    assert_eq!(by_name("WorkCompleted"), "runtime_decided");
    assert!(
        events
            .iter()
            .all(|e| str_attr(e, "stack_agent.provenance").is_some())
    );
}

#[test]
fn unreported_usage_is_absent_not_zero() {
    let mut events = completed_attempt();
    for e in &mut events {
        if let chip_core::WorkEvent::ModelCalled { usage, .. } = e {
            *usage = None;
        }
    }
    let spans = common::spans(&trace(
        &history(records(1, &events, 1), Vec::new()),
        &context(),
    ));
    for span in [&spans[0], &spans[1]] {
        for key in [
            "gen_ai.usage.input_tokens",
            "gen_ai.usage.output_tokens",
            "gen_ai.usage.cache_read.input_tokens",
            "gen_ai.usage.cache_creation.input_tokens",
            "stack_agent.usage.reasoning_tokens",
            "stack_agent.usage.cost",
            "gen_ai.response.model",
        ] {
            assert!(attr(span, key).is_none(), "{key} on {}", span["name"]);
        }
    }
    assert_eq!(
        bool_attr(&spans[1], "stack_agent.usage.reported"),
        Some(false)
    );

    // A partial report keeps exactly the fields the provider gave.
    let mut events = completed_attempt();
    for e in &mut events {
        if let chip_core::WorkEvent::ModelCalled { usage, .. } = e {
            *usage = Some(chip_core::ModelUsage {
                prompt_tokens: Some(10),
                ..Default::default()
            });
        }
    }
    let spans = common::spans(&trace(
        &history(records(1, &events, 1), Vec::new()),
        &context(),
    ));
    assert_eq!(int_attr(&spans[1], "gen_ai.usage.input_tokens"), Some(10));
    assert!(attr(&spans[1], "gen_ai.usage.output_tokens").is_none());
    assert!(attr(&spans[0], "gen_ai.usage.output_tokens").is_none());
}

#[test]
fn the_same_record_always_gives_the_same_request() {
    let h = history(
        records(1, &completed_attempt(), 5),
        vec![result(1, "completed", "", Some(verified()))],
    );
    assert_eq!(trace(&h, &context()), trace(&h, &context()));
    let spans = common::spans(&trace(&h, &context()));
    assert!(spans.iter().all(|s| s["traceId"] == trace_id(TASK)));
    assert_eq!(trace_id(TASK).len(), 32);
    assert_ne!(trace_id(TASK), trace_id("work_00000000000000bb"));
    let ids: std::collections::BTreeSet<&str> = spans
        .iter()
        .map(|s| s["spanId"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), spans.len(), "span ids are distinct");
    assert!(ids.iter().all(|id| id.len() == 16));
}

#[test]
fn each_attempt_is_its_own_root_in_one_trace() {
    let mut events = records(1, &limit_attempt(), 1);
    events.extend(records(2, &completed_attempt(), 100));
    let h = history(
        events,
        vec![
            result(1, "limit_reached", "", None),
            result(2, "completed", "", Some(verified())),
        ],
    );
    let spans = common::spans(&trace(&h, &context()));
    let roots = named(&spans, "invoke_agent");
    assert_eq!(roots.len(), 2);
    assert_eq!(int_attr(roots[0], "stack_agent.attempt"), Some(1));
    assert_eq!(
        str_attr(roots[0], "stack_agent.outcome"),
        Some("limit_reached")
    );
    assert!(
        attr(roots[0], "stack_agent.verified").is_none(),
        "a v1 result has no verdict"
    );
    let limit = roots[0]["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "WorkLimitReached")
        .unwrap();
    assert_eq!(str_attr(limit, "stack_agent.limit"), Some("turns"));
    assert_eq!(int_attr(roots[1], "stack_agent.attempt"), Some(2));
    assert_ne!(roots[0]["spanId"], roots[1]["spanId"]);
    for child in named(&spans, "chat")
        .into_iter()
        .chain(named(&spans, "execute_tool"))
    {
        assert_eq!(child["parentSpanId"], roots[1]["spanId"]);
    }
}

#[test]
fn an_interrupted_attempt_ends_at_its_last_event_and_says_so() {
    // The process died while a model call and an execution were in flight.
    let full = completed_attempt();
    let mut events = full[..5].to_vec(); // through ModelEscalation
    events.push(full[8].clone()); // ExecutionRequested
    let spans = common::spans(&trace(
        &history(records(1, &events, 10), Vec::new()),
        &context(),
    ));
    let root = &spans[0];
    assert_eq!(bool_attr(root, "stack_agent.interrupted"), Some(true));
    assert_eq!(root["status"]["code"], 2);
    assert!(attr(root, "stack_agent.outcome").is_none());
    let chat = named(&spans, "chat")[0];
    assert_eq!(bool_attr(chat, "stack_agent.interrupted"), Some(true));
    assert_eq!(chat["status"]["code"], 2);
    assert_eq!(chat["endTimeUnixNano"], root["endTimeUnixNano"]);
    let tool = named(&spans, "execute_tool")[0];
    assert_eq!(str_attr(tool, "gen_ai.tool.name"), Some("project.test"));
    assert_eq!(tool["status"]["code"], 2);
}

#[test]
fn failures_are_error_spans_with_an_error_type() {
    let w = work();
    let events = vec![
        chip_core::WorkEvent::ModelEscalation {
            work_id: w.clone(),
            turn: 0,
            reason: "r".into(),
            context_policy: "full".into(),
            context: metrics(1),
            profile: None,
        },
        chip_core::WorkEvent::ModelCalled {
            work_id: w.clone(),
            turn: 0,
            usage: None,
            succeeded: false,
        },
        chip_core::WorkEvent::Execution(chip_core::ExecutionEvent::ExecutionRequested {
            id: exec(1),
            intent: "project.test".into(),
        }),
        chip_core::WorkEvent::Execution(chip_core::ExecutionEvent::ExecutionFailed {
            id: exec(1),
            reason: "exit 1".into(),
        }),
        chip_core::WorkEvent::WorkFailed {
            work_id: w,
            reason: "provider said no".into(),
            delivery: Some(chip_core::Delivery::PossiblySent),
        },
    ];
    let spans = common::spans(&trace(
        &history(records(1, &events, 1), Vec::new()),
        &context(),
    ));
    assert_eq!(spans[0]["status"]["code"], 2);
    assert_eq!(str_attr(&spans[0], "error.type"), Some("work_failed"));
    let failed = spans[0]["events"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(
        str_attr(&failed, "stack_agent.delivery"),
        Some("possibly_sent")
    );
    assert_eq!(named(&spans, "chat")[0]["status"]["code"], 2);
    let tool = named(&spans, "execute_tool")[0];
    assert_eq!(tool["status"]["code"], 2);
    assert_eq!(str_attr(tool, "error.type"), Some("execution_failed"));
}

#[test]
fn a_provider_genai_does_not_name_is_left_to_stack_agents_own_attribute() {
    let ctx = Context {
        provider: Some("openai-compatible".into()),
        ..context()
    };
    let spans = common::spans(&trace(
        &history(records(1, &completed_attempt(), 1), Vec::new()),
        &ctx,
    ));
    assert!(attr(&spans[0], "gen_ai.provider.name").is_none());
    assert_eq!(
        str_attr(&spans[0], "stack_agent.provider"),
        Some("openai-compatible")
    );
}

#[test]
fn any_sequence_of_records_gives_a_request() {
    // Every prefix, suffix and reordering of a real trajectory, and records with fields missing
    // or of the wrong type: the mapping never panics and always gives one root per attempt.
    let all = records(1, &completed_attempt(), 1);
    for cut in 0..=all.len() {
        for part in [all[..cut].to_vec(), all[cut..].to_vec()] {
            let mut reversed = part.clone();
            reversed.reverse();
            for events in [part, reversed] {
                let expect = usize::from(!events.is_empty());
                let spans = common::spans(&trace(&history(events, Vec::new()), &context()));
                assert_eq!(named(&spans, "invoke_agent").len(), expect);
            }
        }
    }
    let mut odd = all.clone();
    for (i, r) in odd.iter_mut().enumerate() {
        r.event = match i % 4 {
            0 => serde_json::json!({}),
            1 => serde_json::json!({"kind": 7, "turn": "x"}),
            2 => serde_json::json!({"kind": "ModelCalled", "prompt_tokens": -1, "cost": 3}),
            _ => serde_json::json!({"kind": "ExecutionCompleted", "execution_id": null}),
        };
    }
    let request = trace(&history(odd, Vec::new()), &context());
    assert_eq!(named(&spans(&request), "invoke_agent").len(), 1);
    // An empty record gives an empty, valid request.
    let empty = trace(&history(Vec::new(), Vec::new()), &context());
    assert!(spans(&empty).is_empty());
}
