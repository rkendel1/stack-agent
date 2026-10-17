//! Anthropic Messages API conformance through the real FX executor, against a mock server.
//!
//! Mocked-transport evidence, like `conformance.rs`: it proves how the adapter and FX behave for
//! these bytes, not how the live service behaves and not the TLS path.

mod support;

use std::time::{Duration, Instant};

use fx_core::*;
use fx_native_provider::{Anthropic, AnthropicConfig, Secret};
use fx_single_attempt::SingleAttemptCapability;
use support::*;

const KEY: &str = "sk-ant-conformance-0123456789abcdef";

#[derive(Default)]
struct TestHost {
    events: Vec<Event>,
    cancel_before_start: bool,
    cancel_after_events: Option<usize>,
}

impl fx_core::Host for TestHost {
    fn cancel_requested(&mut self) -> bool {
        self.cancel_before_start
            || self
                .cancel_after_events
                .is_some_and(|n| self.events.len() >= n)
    }
    fn on_event(&mut self, event: &Event) -> Flow {
        self.events.push(event.clone());
        Flow::Continue
    }
}

fn request() -> Request {
    Request {
        model: ModelRef("claude-mock".to_owned()),
        messages: vec![
            Message {
                role: Role::System,
                parts: vec![Part::Text("be brief".to_owned())],
                cache: CacheHint::Default,
            },
            Message {
                role: Role::User,
                parts: vec![Part::Text("say hi".to_owned())],
                cache: CacheHint::Default,
            },
        ],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        options: GenerationOptions::default(),
        correlation: Correlation {
            request_id: RequestId([7; 16]),
            affinity_key: None,
        },
        budget: None,
    }
}

fn run(server: &Server, request: &Request, host: &mut TestHost) -> ExecutionReport {
    let config = AnthropicConfig::new(&server.base_url, Secret::new(KEY)).unwrap();
    let mut capability = SingleAttemptCapability::new(Anthropic::new(config));
    execute(&mut capability, request, &Limits::DEFAULT, host)
}

fn completion(report: &ExecutionReport) -> &Completion {
    match &report.outcome {
        Outcome::Completed(c) => c,
        other => panic!("expected completion, got {other:?}"),
    }
}

fn failed(report: &ExecutionReport) -> &Failure {
    match &report.outcome {
        Outcome::Failed(f) => f,
        other => panic!("expected failure, got {other:?}"),
    }
}

fn no_secret(report: &ExecutionReport, host: &TestHost) {
    let all = format!("{report:?} {:?}", host.events);
    assert!(
        !all.contains(KEY) && !all.contains("0123456789abcdef"),
        "leak: {all}"
    );
}

#[test]
fn a_stream_completes_with_text_usage_model_and_the_providers_stop_reason() {
    let server = Server::start(vec![sse(a_reply(&["Hel", "lo"], "end_turn"))]);
    let mut host = TestHost::default();
    let report = run(&server, &request(), &mut host);
    let c = completion(&report);
    assert_eq!(c.content, "Hello");
    assert_eq!(c.finish_reason, FinishReason::Stop);
    let u = c.usage.as_ref().expect("usage");
    assert_eq!((u.input_tokens, u.output_tokens), (Some(12), Some(3)));
    assert_eq!(u.cache_read_tokens, Some(2));
    assert_eq!(c.metadata.model_reported.as_deref(), Some("claude-mock"));
    assert_eq!(
        report.diagnostics.delivery,
        DeliveryCertainty::ConfirmedResponse
    );
    assert_eq!(server.connections(), 1);
    no_secret(&report, &host);
}

#[test]
fn the_request_uses_the_messages_shape_with_headers_and_a_required_max_tokens() {
    let server = Server::start(vec![sse(a_reply(&["ok"], "end_turn"))]);
    completion(&run(&server, &request(), &mut TestHost::default()));
    let seen = &server.recorded()[0];
    let head = seen.head.to_ascii_lowercase();
    assert!(head.starts_with("post /v1/messages "), "{head}");
    assert!(head.contains(&format!("x-api-key: {}", KEY.to_ascii_lowercase())));
    assert!(head.contains("anthropic-version: 2023-06-01"));
    assert!(!head.contains("authorization"));
    assert!(!seen.body.contains(KEY));
    let body: serde_json::Value = serde_json::from_str(&seen.body).unwrap();
    assert_eq!(body["model"], "claude-mock");
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["system"], "be brief",
        "system text is top level, not a message"
    );
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(
        body["max_tokens"], 4096,
        "the host's default, since the request had none"
    );
}

#[test]
fn a_requested_output_cap_wins_over_the_hosts_default() {
    let server = Server::start(vec![sse(a_reply(&["ok"], "end_turn"))]);
    let mut r = request();
    r.options.max_output_tokens = Some(77);
    completion(&run(&server, &r, &mut TestHost::default()));
    let body: serde_json::Value = serde_json::from_str(&server.recorded()[0].body).unwrap();
    assert_eq!(body["max_tokens"], 77);
}

#[test]
fn temperature_is_carried_and_above_one_is_refused_before_dispatch() {
    let server = Server::start(vec![sse(a_reply(&["ok"], "end_turn"))]);
    let mut r = request();
    r.options.temperature_milli = Some(0);
    completion(&run(&server, &r, &mut TestHost::default()));
    let body: serde_json::Value = serde_json::from_str(&server.recorded()[0].body).unwrap();
    assert_eq!(body["temperature"], 0.0);

    let refused = Server::start(vec![]);
    r.options.temperature_milli = Some(1001);
    let report = run(&refused, &r, &mut TestHost::default());
    let f = failed(&report);
    assert_eq!(f.kind, ErrorKind::CapabilityUnavailable);
    assert_eq!(f.delivery, DeliveryCertainty::DefinitelyUnsent);
    assert_eq!(refused.connections(), 0);
}

#[test]
fn structured_output_is_refused_before_dispatch() {
    let server = Server::start(vec![]);
    let mut r = request();
    r.options.response_format = Some(ResponseFormat {
        name: "reply".into(),
        description: String::new(),
        schema_json: r#"{"type":"object"}"#.into(),
    });
    let report = run(&server, &r, &mut TestHost::default());
    assert_eq!(failed(&report).kind, ErrorKind::CapabilityUnavailable);
    assert_eq!(
        failed(&report).delivery,
        DeliveryCertainty::DefinitelyUnsent
    );
    assert_eq!(server.connections(), 0);
}

#[test]
fn max_tokens_and_a_refusal_stop_map_to_length_and_content_filter() {
    for (stop, expected) in [
        ("max_tokens", FinishReason::Length),
        ("refusal", FinishReason::ContentFilter),
        ("stop_sequence", FinishReason::Stop),
    ] {
        let server = Server::start(vec![sse(a_reply(&["x"], stop))]);
        let report = run(&server, &request(), &mut TestHost::default());
        assert_eq!(completion(&report).finish_reason, expected, "{stop}");
    }
}

#[test]
fn http_errors_keep_status_and_retry_after_are_possibly_sent_and_not_retried() {
    for (status, kind) in [
        (401, ErrorKind::Unauthenticated),
        (403, ErrorKind::Forbidden),
        (404, ErrorKind::ModelUnavailable),
        (429, ErrorKind::RateLimited),
        (500, ErrorKind::ProviderUnavailable),
        (529, ErrorKind::ProviderUnavailable),
    ] {
        let server = Server::start(vec![
            http(
                status,
                vec![("retry-after", "9".to_owned())],
                r#"{"type":"error","error":{"type":"x","message":"nope"}}"#,
            ),
            sse(a_reply(&["a second call would succeed"], "end_turn")),
        ]);
        let report = run(&server, &request(), &mut TestHost::default());
        let f = failed(&report);
        assert_eq!(f.kind, kind, "status {status}");
        assert_eq!(f.status_hint, Some(status));
        assert_eq!(f.retry_after, Some(Duration::from_secs(9)));
        assert_eq!(f.delivery, DeliveryCertainty::PossiblySent);
        assert_eq!(server.connections(), 1, "no hidden retry after {status}");
    }
}

#[test]
fn an_error_event_in_the_stream_is_a_provider_failure_with_the_text_so_far() {
    let server = Server::start(vec![sse(vec![
        a_start(1),
        a_block(),
        a_text("so far"),
        aevent(
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        ),
    ])]);
    let report = run(&server, &request(), &mut TestHost::default());
    let f = failed(&report);
    assert_eq!(f.delivery, DeliveryCertainty::ConfirmedResponse);
    assert_eq!(
        f.partial.as_ref().map(|p| p.content.as_str()),
        Some("so far")
    );
}

#[test]
fn a_stream_cut_before_message_stop_keeps_the_partial_and_is_not_a_completion() {
    let server = Server::start(vec![sse(vec![
        a_start(1),
        a_block(),
        a_text("par"),
        a_text("tial"),
    ])]);
    let report = run(&server, &request(), &mut TestHost::default());
    let f = failed(&report);
    assert_eq!(f.kind, ErrorKind::Transport);
    assert_eq!(f.delivery, DeliveryCertainty::ConfirmedResponse);
    assert_eq!(
        f.partial.as_ref().map(|p| p.content.as_str()),
        Some("partial")
    );
}

#[test]
fn message_stop_without_a_stop_reason_is_not_a_completion() {
    let server = Server::start(vec![sse(vec![
        a_start(1),
        a_block(),
        a_text("x"),
        a_stop(),
    ])]);
    let report = run(&server, &request(), &mut TestHost::default());
    assert_eq!(failed(&report).kind, ErrorKind::ProtocolViolation);
}

#[test]
fn a_tool_use_the_provider_was_never_offered_is_a_protocol_violation() {
    let tool_block = aevent(
        "content_block_start",
        r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"rm","input":{}}}"#,
    );
    let server = Server::start(vec![sse(vec![
        a_start(1),
        a_text("a"),
        tool_block,
        a_delta("tool_use", 1),
        a_stop(),
    ])]);
    let report = run(&server, &request(), &mut TestHost::default());
    assert_eq!(failed(&report).kind, ErrorKind::ProtocolViolation);
}

#[test]
fn malformed_json_in_the_stream_fails_with_the_text_so_far_preserved() {
    let server = Server::start(vec![sse(vec![
        a_start(1),
        a_text("ok so far"),
        b"event: x\ndata: {nope\n\n".to_vec(),
    ])]);
    let report = run(&server, &request(), &mut TestHost::default());
    let f = failed(&report);
    assert_eq!(f.kind, ErrorKind::ProtocolViolation);
    assert_eq!(
        f.partial.as_ref().map(|p| p.content.as_str()),
        Some("ok so far")
    );
}

#[test]
fn unknown_event_types_are_ignored_and_thinking_deltas_carry_no_output() {
    let server = Server::start(vec![sse(vec![
        a_start(1),
        aevent("future_event", r#"{"type":"future_event","x":1}"#),
        aevent(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}"#,
        ),
        a_text("answer"),
        a_delta("end_turn", 2),
        a_stop(),
    ])]);
    let report = run(&server, &request(), &mut TestHost::default());
    assert_eq!(completion(&report).content, "answer");
}

#[test]
fn unsupported_features_are_refused_before_anything_is_sent() {
    let mut cases = Vec::new();
    let mut r = request();
    r.tools = vec![ToolDefinition {
        name: "t".into(),
        description: "d".into(),
        input_schema_json: r#"{"type":"object"}"#.into(),
        execution: ToolExecution::Client,
        provider_args_json: None,
    }];
    cases.push(("tools", r));
    let mut r = request();
    r.options.speed = Speed::Fast;
    cases.push(("speed", r));
    let mut r = request();
    r.options.reasoning = ReasoningEffort::Named("high".into());
    cases.push(("reasoning", r));
    let server = Server::start(vec![]);
    for (name, r) in cases {
        let report = run(&server, &r, &mut TestHost::default());
        let f = failed(&report);
        assert_eq!(f.kind, ErrorKind::CapabilityUnavailable, "{name}");
        assert_eq!(f.delivery, DeliveryCertainty::DefinitelyUnsent, "{name}");
    }
    assert_eq!(server.connections(), 0);
}

#[test]
fn cancellation_before_dispatch_opens_no_connection() {
    let server = Server::start(vec![sse(a_reply(&["x"], "end_turn"))]);
    let report = run(
        &server,
        &request(),
        &mut TestHost {
            cancel_before_start: true,
            ..TestHost::default()
        },
    );
    assert!(matches!(report.outcome, Outcome::Cancelled(_)));
    assert_eq!(server.connections(), 0);
}

#[test]
fn cancellation_in_flight_is_prompt_unconfirmed_and_keeps_partial_output() {
    let server = Server::start(vec![Reply::Stall {
        chunks: vec![a_start(1), a_block(), a_text("first")],
    }]);
    let started = Instant::now();
    let report = run(
        &server,
        &request(),
        &mut TestHost {
            cancel_after_events: Some(1),
            ..TestHost::default()
        },
    );
    match &report.outcome {
        Outcome::Cancelled(c) => {
            assert_eq!(c.remote, RemoteCancellation::Unconfirmed);
            assert_eq!(c.delivery, DeliveryCertainty::ConfirmedResponse);
            assert_eq!(
                c.partial.as_ref().map(|p| p.content.as_str()),
                Some("first")
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn the_attempt_budget_ends_a_silent_server_as_a_timeout_that_may_have_been_sent() {
    let server = Server::start(vec![Reply::Silent]);
    let mut r = request();
    r.budget = Some(AttemptBudget(Duration::from_millis(300)));
    let report = run(&server, &r, &mut TestHost::default());
    let f = failed(&report);
    assert_eq!(f.kind, ErrorKind::Timeout);
    assert_eq!(f.delivery, DeliveryCertainty::PossiblySent);
}

#[test]
fn a_connection_closed_without_a_response_is_possibly_sent_and_not_retried() {
    let server = Server::start(vec![Reply::Close]);
    let report = run(&server, &request(), &mut TestHost::default());
    assert_eq!(failed(&report).delivery, DeliveryCertainty::PossiblySent);
    assert_eq!(server.connections(), 1);
}

#[test]
fn a_credential_echoed_by_the_provider_never_reaches_errors_events_or_diagnostics() {
    let echoed = format!(r#"{{"type":"error","error":{{"message":"invalid x-api-key {KEY}"}}}}"#);
    let in_stream = format!(r#"{{"type":"error","error":{{"message":"bad key {KEY}"}}}}"#);
    for reply in [
        http(401, vec![], &echoed),
        sse(vec![a_start(1), a_text("x"), aevent("error", &in_stream)]),
    ] {
        let server = Server::start(vec![reply]);
        let mut host = TestHost::default();
        let report = run(&server, &request(), &mut host);
        failed(&report);
        no_secret(&report, &host);
    }
}

#[test]
fn configuration_is_validated_and_the_key_is_not_printable() {
    let key = || Secret::new(KEY);
    assert!(AnthropicConfig::new("http://api.example.test/v1", key()).is_err());
    assert!(AnthropicConfig::new("https://api.anthropic.com/v1", Secret::new("")).is_err());
    let config = AnthropicConfig::new("https://api.anthropic.com/v1", key()).unwrap();
    assert!(!format!("{config:?}").contains(KEY));
    assert!(config.clone().with_version("bad value").is_err());
    assert!(
        config
            .clone()
            .with_header("anthropic-version", "x")
            .is_err()
    );
    assert!(
        config
            .with_header("anthropic-workspace-id", "wrkspc_1")
            .is_ok()
    );
}
