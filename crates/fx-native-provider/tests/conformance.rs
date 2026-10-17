//! Native-provider conformance through the real FX executor, against a mock HTTP server.
//!
//! This is *mocked-transport* conformance. It proves the adapter's contract behavior
//! deterministically and offline. It says nothing about any live provider; that needs
//! credentials and is verified separately (docs/rebuild/NATIVE_PROVIDER.md).

mod support;

use std::time::{Duration, Instant};

use fx_core::*;
use fx_native_provider::{OpenAiCompatible, OpenAiConfig, Secret};
use fx_single_attempt::SingleAttemptCapability;
use support::*;

const KEY: &str = "sk-native-conformance-0123456789abcdef";

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
        model: ModelRef("mock-1".to_owned()),
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

fn run_at(base_url: &str, request: &Request, host: &mut TestHost) -> ExecutionReport {
    let config = OpenAiConfig::new(base_url, Secret::new(KEY)).unwrap();
    let mut capability = SingleAttemptCapability::new(OpenAiCompatible::new(config));
    execute(&mut capability, request, &Limits::DEFAULT, host)
}

fn run(server: &Server, request: &Request, host: &mut TestHost) -> ExecutionReport {
    run_at(&server.base_url, request, host)
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
    assert!(!all.contains(KEY), "the credential leaked: {all}");
    assert!(
        !all.contains("0123456789abcdef"),
        "part of the credential leaked"
    );
}

// ---------------------------------------------------------------------------
// Success and normalization

#[test]
fn a_text_stream_completes_with_usage_model_and_the_mapped_finish_reason() {
    let server = Server::start(vec![sse(vec![
        delta("hello "),
        delta("world"),
        finish("stop"),
        usage(12, 3),
        done(),
    ])]);
    let mut host = TestHost::default();
    let report = run(&server, &request(), &mut host);
    let c = completion(&report);
    assert_eq!(c.content, "hello world");
    assert_eq!(c.finish_reason, FinishReason::Stop);
    let u = c.usage.as_ref().expect("usage");
    assert_eq!((u.input_tokens, u.output_tokens), (Some(12), Some(3)));
    assert_eq!(u.cache_read_tokens, Some(2));
    assert_eq!(c.metadata.model_reported.as_deref(), Some("mock-1"));
    assert_eq!(report.disposition, Some(Disposition::Complete));
    assert_eq!(
        report.diagnostics.delivery,
        DeliveryCertainty::ConfirmedResponse
    );
    assert_eq!(server.connections(), 1);
    no_secret(&report, &host);
}

#[test]
fn the_request_is_one_post_with_the_credential_only_in_the_authorization_header() {
    let server = Server::start(vec![sse(vec![delta("ok"), finish("stop"), done()])]);
    let report = run(&server, &request(), &mut TestHost::default());
    completion(&report);
    let seen = server.recorded();
    assert_eq!(seen.len(), 1);
    let head = seen[0].head.to_ascii_lowercase();
    assert!(head.starts_with("post /v1/chat/completions "), "{head}");
    assert!(head.contains(&format!(
        "authorization: bearer {}",
        KEY.to_ascii_lowercase()
    )));
    assert!(
        !seen[0].body.contains(KEY),
        "the credential is not in the body"
    );
    let body: serde_json::Value = serde_json::from_str(&seen[0].body).unwrap();
    assert_eq!(body["model"], "mock-1");
    assert_eq!(body["stream"], true);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["content"], "say hi");
    assert!(body.get("tools").is_none(), "no tool surface is sent");
}

#[test]
fn a_length_finish_and_a_missing_usage_are_preserved_as_reported() {
    let server = Server::start(vec![sse(vec![delta("cut"), finish("length"), done()])]);
    let report = run(&server, &request(), &mut TestHost::default());
    let c = completion(&report);
    assert_eq!(c.finish_reason, FinishReason::Length);
    assert_eq!(c.usage, None, "absent usage stays unknown");
}

#[test]
fn a_refusal_is_reported_as_a_content_filter_finish_with_its_text() {
    let refusal = event(
        r#"{"model":"mock-1","choices":[{"index":0,"delta":{"refusal":"I cannot help with that."},"finish_reason":null}]}"#,
    );
    let server = Server::start(vec![sse(vec![refusal, finish("stop"), done()])]);
    let report = run(&server, &request(), &mut TestHost::default());
    let c = completion(&report);
    assert_eq!(c.finish_reason, FinishReason::ContentFilter);
    assert_eq!(c.content, "I cannot help with that.");
}

// ---------------------------------------------------------------------------
// Provider errors and HTTP status

#[test]
fn http_errors_keep_status_and_retry_after_and_are_possibly_sent() {
    let cases = [
        (401, ErrorKind::Unauthenticated),
        (403, ErrorKind::Forbidden),
        (404, ErrorKind::ModelUnavailable),
        (429, ErrorKind::RateLimited),
        (500, ErrorKind::ProviderUnavailable),
        (503, ErrorKind::ProviderUnavailable),
        (400, ErrorKind::Transport),
    ];
    for (status, kind) in cases {
        let server = Server::start(vec![
            http(
                status,
                vec![("retry-after", "7".to_owned())],
                r#"{"error":{"message":"nope"}}"#,
            ),
            sse(vec![
                delta("a second call would succeed"),
                finish("stop"),
                done(),
            ]),
        ]);
        let mut host = TestHost::default();
        let report = run(&server, &request(), &mut host);
        let f = failed(&report);
        assert_eq!(f.kind, kind, "status {status}");
        assert_eq!(f.status_hint, Some(status));
        assert_eq!(f.retry_after, Some(Duration::from_secs(7)));
        assert_eq!(
            f.delivery,
            DeliveryCertainty::PossiblySent,
            "status {status}"
        );
        assert_eq!(f.origin, FailureOrigin::Capability);
        assert_eq!(server.connections(), 1, "no hidden retry after {status}");
    }
}

#[test]
fn a_redirect_is_a_failure_and_is_not_followed() {
    let other = Server::start(vec![sse(vec![delta("elsewhere"), finish("stop"), done()])]);
    let server = Server::start(vec![http(
        302,
        vec![("location", format!("{}/chat/completions", other.base_url))],
        "",
    )]);
    let report = run(&server, &request(), &mut TestHost::default());
    assert_eq!(failed(&report).status_hint, Some(302));
    assert_eq!(
        other.connections(),
        0,
        "the credential was not sent elsewhere"
    );
}

#[test]
fn a_two_hundred_that_is_not_an_event_stream_is_a_protocol_violation() {
    let server = Server::start(vec![http(200, vec![], r#"{"choices":[]}"#)]);
    let report = run(&server, &request(), &mut TestHost::default());
    let f = failed(&report);
    assert_eq!(f.kind, ErrorKind::ProtocolViolation);
    assert_eq!(f.delivery, DeliveryCertainty::PossiblySent);
}

#[test]
fn an_error_object_in_the_stream_is_a_provider_failure_with_partial_output() {
    let server = Server::start(vec![sse(vec![
        delta("so far"),
        event(r#"{"error":{"message":"overloaded"}}"#),
    ])]);
    let report = run(&server, &request(), &mut TestHost::default());
    let f = failed(&report);
    assert_eq!(f.delivery, DeliveryCertainty::ConfirmedResponse);
    assert_eq!(
        f.partial.as_ref().map(|p| p.content.as_str()),
        Some("so far")
    );
}

// ---------------------------------------------------------------------------
// Unsupported request shapes are refused before dispatch

#[test]
fn unsupported_features_are_refused_before_anything_is_sent() {
    let png = ImageData {
        media_type: ImageMediaType::Png,
        bytes: vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
    };
    let mut cases: Vec<(&str, Request)> = Vec::new();
    let mut r = request();
    r.messages[1].parts.push(Part::Image(png));
    cases.push(("image", r));
    let mut r = request();
    r.tools = vec![ToolDefinition {
        name: "list_files".into(),
        description: "List.".into(),
        input_schema_json: r#"{"type":"object"}"#.into(),
        execution: ToolExecution::Client,
        provider_args_json: None,
    }];
    cases.push(("tools", r));
    let mut r = request();
    r.tool_choice = ToolChoice::None;
    cases.push(("tool choice", r));
    let mut r = request();
    r.options.reasoning = ReasoningEffort::Named("high".into());
    cases.push(("reasoning effort", r));
    let mut r = request();
    r.options.speed = Speed::Fast;
    cases.push(("speed", r));

    let server = Server::start(vec![]);
    for (name, request) in cases {
        let report = run(&server, &request, &mut TestHost::default());
        let f = failed(&report);
        assert_eq!(f.kind, ErrorKind::CapabilityUnavailable, "{name}");
        assert_eq!(f.delivery, DeliveryCertainty::DefinitelyUnsent, "{name}");
    }
    assert_eq!(server.connections(), 0, "nothing was dispatched");
}

#[test]
fn an_output_token_cap_is_carried_to_the_provider() {
    let server = Server::start(vec![sse(vec![delta("ok"), finish("stop"), done()])]);
    let mut r = request();
    r.options.max_output_tokens = Some(10);
    let report = run(&server, &r, &mut TestHost::default());
    completion(&report);
    let body: serde_json::Value = serde_json::from_str(&server.recorded()[0].body).unwrap();
    assert_eq!(body["max_tokens"], 10);
}

#[test]
fn temperature_is_carried_exactly_including_zero() {
    for (milli, expected) in [(0u16, 0.0), (700, 0.7), (1000, 1.0), (2000, 2.0)] {
        let server = Server::start(vec![sse(vec![delta("ok"), finish("stop"), done()])]);
        let mut r = request();
        r.options.temperature_milli = Some(milli);
        completion(&run(&server, &r, &mut TestHost::default()));
        let body: serde_json::Value = serde_json::from_str(&server.recorded()[0].body).unwrap();
        assert_eq!(body["temperature"], expected, "{milli}");
    }
    let server = Server::start(vec![sse(vec![delta("ok"), finish("stop"), done()])]);
    completion(&run(&server, &request(), &mut TestHost::default()));
    let body: serde_json::Value = serde_json::from_str(&server.recorded()[0].body).unwrap();
    assert!(body.get("temperature").is_none(), "absent stays absent");
}

#[test]
fn a_response_format_is_sent_as_a_json_schema() {
    let server = Server::start(vec![sse(vec![delta("{}"), finish("stop"), done()])]);
    let mut r = request();
    r.options.response_format = Some(ResponseFormat {
        name: "reply".into(),
        description: "One JSON object".into(),
        schema_json: r#"{"type":"object"}"#.into(),
    });
    completion(&run(&server, &r, &mut TestHost::default()));
    let body: serde_json::Value = serde_json::from_str(&server.recorded()[0].body).unwrap();
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["response_format"]["json_schema"]["name"], "reply");
    assert_eq!(
        body["response_format"]["json_schema"]["description"],
        "One JSON object"
    );
    assert_eq!(
        body["response_format"]["json_schema"]["schema"],
        serde_json::json!({"type": "object"})
    );
    assert_eq!(body["response_format"]["json_schema"]["strict"], false);
}

#[test]
fn consecutive_system_messages_are_sent_as_one_and_order_is_kept() {
    let server = Server::start(vec![sse(vec![delta("ok"), finish("stop"), done()])]);
    let mut r = request();
    let system = |text: &str| Message {
        role: Role::System,
        parts: vec![Part::Text(text.to_owned())],
        cache: CacheHint::Default,
    };
    // request() is [system "be brief", user "say hi"]; make it [sys, sys, user, sys].
    r.messages.insert(1, system("and polite"));
    r.messages.push(system("late rule"));
    completion(&run(&server, &r, &mut TestHost::default()));
    let body: serde_json::Value = serde_json::from_str(&server.recorded()[0].body).unwrap();
    let sent: Vec<(String, String)> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["role"].as_str().unwrap().into(),
                m["content"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        sent,
        vec![
            ("system".to_owned(), "be brief\n\nand polite".to_owned()),
            ("user".to_owned(), "say hi".to_owned()),
            ("system".to_owned(), "late rule".to_owned()),
        ]
    );
}

#[test]
fn a_keyless_endpoint_sends_no_authorization_and_extra_headers_are_sent() {
    let server = Server::start(vec![sse(vec![delta("ok"), finish("stop"), done()])]);
    let config = OpenAiConfig::without_key(&server.base_url)
        .unwrap()
        .with_header("HTTP-Referer", "https://example.test")
        .unwrap();
    let mut capability = SingleAttemptCapability::new(OpenAiCompatible::new(config));
    let report = execute(
        &mut capability,
        &request(),
        &Limits::DEFAULT,
        &mut TestHost::default(),
    );
    completion(&report);
    let head = server.recorded()[0].head.to_ascii_lowercase();
    assert!(!head.contains("authorization"), "{head}");
    assert!(
        head.contains("http-referer: https://example.test"),
        "{head}"
    );
}

#[test]
fn headers_the_adapter_owns_cannot_be_set_by_the_host() {
    let config = || OpenAiConfig::new("https://api.example.test/v1", Secret::new(KEY)).unwrap();
    for name in [
        "Authorization",
        "x-api-key",
        "Content-Type",
        "Host",
        "anthropic-version",
    ] {
        assert!(config().with_header(name, "x").is_err(), "{name}");
    }
    assert!(config().with_header("X-Title", "line\nbreak").is_err());
    assert!(config().with_header("bad name", "x").is_err());
}

#[test]
fn an_invalid_request_is_rejected_by_fx_before_the_provider_is_reached() {
    let mut r = request();
    r.messages.clear();
    let server = Server::start(vec![]);
    let report = run(&server, &r, &mut TestHost::default());
    assert_eq!(failed(&report).kind, ErrorKind::InvalidRequest);
    assert_eq!(
        failed(&report).delivery,
        DeliveryCertainty::DefinitelyUnsent
    );
    assert_eq!(server.connections(), 0);
}

#[test]
fn a_tool_call_the_provider_was_never_offered_is_a_protocol_violation_not_a_proposal() {
    let call = event(
        r#"{"model":"mock-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"rm","arguments":"{}"}}]},"finish_reason":null}]}"#,
    );
    let server = Server::start(vec![sse(vec![call, finish("tool_calls"), done()])]);
    let report = run(&server, &request(), &mut TestHost::default());
    assert_eq!(failed(&report).kind, ErrorKind::ProtocolViolation);
}

// ---------------------------------------------------------------------------
// Cancellation, deadlines and connection failure

#[test]
fn cancellation_before_dispatch_never_opens_a_connection() {
    let server = Server::start(vec![sse(vec![delta("x"), finish("stop"), done()])]);
    let report = run(
        &server,
        &request(),
        &mut TestHost {
            cancel_before_start: true,
            ..TestHost::default()
        },
    );
    match &report.outcome {
        Outcome::Cancelled(c) => {
            assert_eq!(c.delivery, DeliveryCertainty::DefinitelyUnsent);
            assert_eq!(c.remote, RemoteCancellation::NotStarted);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(server.connections(), 0);
}

#[test]
fn cancellation_in_flight_returns_promptly_with_partial_output_and_is_unconfirmed() {
    let server = Server::start(vec![Reply::Stall {
        chunks: vec![delta("first")],
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
            assert!(c.delivery.may_have_executed());
        }
        other => panic!("{other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cancel did not wait for the stream"
    );
    assert_eq!(server.connections(), 1);
}

#[test]
fn cancellation_while_waiting_for_the_response_is_possibly_sent() {
    struct AfterPolls(u32);
    impl fx_core::Host for AfterPolls {
        fn cancel_requested(&mut self) -> bool {
            false
        }
        fn on_event(&mut self, _: &Event) -> Flow {
            Flow::Continue
        }
        fn idle(&mut self) -> Flow {
            self.0 += 1;
            if self.0 > 3 {
                Flow::Stop
            } else {
                Flow::Continue
            }
        }
    }
    let server = Server::start(vec![Reply::Silent]);
    let config = OpenAiConfig::new(&server.base_url, Secret::new(KEY)).unwrap();
    let mut capability = SingleAttemptCapability::new(OpenAiCompatible::new(config));
    let report = execute(
        &mut capability,
        &request(),
        &Limits::DEFAULT,
        &mut AfterPolls(0),
    );
    match &report.outcome {
        Outcome::Cancelled(c) => {
            assert_eq!(c.delivery, DeliveryCertainty::PossiblySent);
            assert_eq!(c.remote, RemoteCancellation::Unconfirmed);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_attempt_budget_ends_a_silent_server_as_a_timeout_that_may_have_been_sent() {
    let server = Server::start(vec![Reply::Silent]);
    let mut r = request();
    r.budget = Some(AttemptBudget(Duration::from_millis(300)));
    let started = Instant::now();
    let report = run(&server, &r, &mut TestHost::default());
    let f = failed(&report);
    assert_eq!(f.kind, ErrorKind::Timeout);
    assert_eq!(f.delivery, DeliveryCertainty::PossiblySent);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(server.connections(), 1);
}

#[test]
fn a_stalled_stream_times_out_with_its_partial_output_and_a_confirmed_response() {
    let server = Server::start(vec![Reply::Stall {
        chunks: vec![delta("partial")],
    }]);
    let mut r = request();
    r.budget = Some(AttemptBudget(Duration::from_millis(400)));
    let report = run(&server, &r, &mut TestHost::default());
    let f = failed(&report);
    assert_eq!(f.kind, ErrorKind::Timeout);
    assert_eq!(f.delivery, DeliveryCertainty::ConfirmedResponse);
    assert_eq!(
        f.partial.as_ref().map(|p| p.content.as_str()),
        Some("partial")
    );
}

#[test]
fn a_refused_connection_is_conservatively_possibly_sent() {
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://127.0.0.1:{}/v1", l.local_addr().unwrap().port())
    };
    // The call had already started when the failure was observed, and FX never lowers
    // certainty, so the report is the conservative PossiblySent (documented).
    let report = run_at(&dead, &request(), &mut TestHost::default());
    let f = failed(&report);
    assert_eq!(f.delivery, DeliveryCertainty::PossiblySent);
    assert_eq!(f.kind, ErrorKind::Transport);
}

// ---------------------------------------------------------------------------
// Ambiguous delivery and damaged streams

#[test]
fn a_connection_closed_without_a_response_is_possibly_sent_never_unsent() {
    let server = Server::start(vec![Reply::Close]);
    let report = run(&server, &request(), &mut TestHost::default());
    let f = failed(&report);
    assert_eq!(f.delivery, DeliveryCertainty::PossiblySent);
    assert_eq!(server.connections(), 1, "and it was not retried");
}

#[test]
fn a_stream_cut_before_its_terminal_marker_keeps_the_partial_and_is_not_a_completion() {
    let server = Server::start(vec![sse(vec![delta("par"), delta("tial")])]);
    let report = run(&server, &request(), &mut TestHost::default());
    let f = failed(&report);
    assert_eq!(f.kind, ErrorKind::Transport);
    assert_eq!(f.delivery, DeliveryCertainty::ConfirmedResponse);
    assert_eq!(
        f.partial.as_ref().map(|p| p.content.as_str()),
        Some("partial")
    );
    assert_eq!(server.connections(), 1);
}

#[test]
fn a_complete_but_empty_response_is_a_confirmed_response_that_is_not_a_completion() {
    let server = Server::start(vec![sse(vec![])]);
    let report = run(&server, &request(), &mut TestHost::default());
    let f = failed(&report);
    assert_eq!(f.kind, ErrorKind::Transport);
    assert_eq!(f.origin, FailureOrigin::Executor);
    assert_eq!(f.delivery, DeliveryCertainty::ConfirmedResponse);
}

#[test]
fn text_after_the_terminal_marker_is_never_read() {
    let server = Server::start(vec![sse(vec![
        delta("a "),
        finish("stop"),
        done(),
        delta("LATE"),
    ])]);
    let mut host = TestHost::default();
    let report = run(&server, &request(), &mut host);
    assert_eq!(completion(&report).content, "a ");
    assert!(!format!("{:?}", host.events).contains("LATE"));
}

#[test]
fn the_terminal_marker_without_a_finish_reason_is_not_a_completion() {
    let server = Server::start(vec![sse(vec![delta("x"), done()])]);
    let report = run(&server, &request(), &mut TestHost::default());
    assert_eq!(failed(&report).kind, ErrorKind::ProtocolViolation);
}

#[test]
fn malformed_json_in_the_stream_fails_with_the_text_so_far_preserved() {
    let server = Server::start(vec![sse(vec![delta("ok so far"), event("{not json")])]);
    let report = run(&server, &request(), &mut TestHost::default());
    let f = failed(&report);
    assert_eq!(f.kind, ErrorKind::ProtocolViolation);
    assert_eq!(
        f.partial.as_ref().map(|p| p.content.as_str()),
        Some("ok so far")
    );
}

#[test]
fn events_split_across_reads_and_crlf_line_endings_decode_the_same() {
    let whole = [delta("a"), delta("b"), finish("stop"), done()].concat();
    let crlf = String::from_utf8(whole)
        .unwrap()
        .replace('\n', "\r\n")
        .into_bytes();
    let pieces: Vec<Vec<u8>> = crlf.chunks(7).map(<[u8]>::to_vec).collect();
    let server = Server::start(vec![sse(pieces)]);
    let report = run(&server, &request(), &mut TestHost::default());
    assert_eq!(completion(&report).content, "ab");
}

// ---------------------------------------------------------------------------
// Credentials

#[test]
fn a_credential_echoed_by_the_provider_never_reaches_errors_events_or_diagnostics() {
    let echoed = format!(r#"{{"error":{{"message":"Incorrect API key provided: {KEY}"}}}}"#);
    let in_stream = format!(r#"{{"error":{{"message":"bad key {KEY}"}}}}"#);
    for reply in [
        http(401, vec![], &echoed),
        sse(vec![delta("x"), event(&in_stream)]),
    ] {
        let server = Server::start(vec![reply]);
        let mut host = TestHost::default();
        let report = run(&server, &request(), &mut host);
        failed(&report);
        no_secret(&report, &host);
    }
}

#[test]
fn the_credential_is_not_printable_from_the_configuration() {
    let config = OpenAiConfig::new("https://api.example.test/v1", Secret::new(KEY)).unwrap();
    assert!(!format!("{config:?}").contains(KEY));
    assert!(!format!("{:?}", Secret::new(KEY)).contains(KEY));
}

#[test]
fn configuration_refuses_cleartext_remote_hosts_and_embedded_credentials() {
    let key = || Secret::new(KEY);
    assert!(OpenAiConfig::new("http://api.example.test/v1", key()).is_err());
    assert!(OpenAiConfig::new("https://user:pw@api.example.test/v1", key()).is_err());
    assert!(OpenAiConfig::new("https://api.example.test/v1?key=1", key()).is_err());
    assert!(OpenAiConfig::new("api.example.test", key()).is_err());
    assert!(OpenAiConfig::new("https://api.example.test/v1", Secret::new("")).is_err());
    assert!(OpenAiConfig::new("http://127.0.0.1:1/v1", key()).is_ok());
    assert!(OpenAiConfig::new("https://api.example.test/v1/", key()).is_ok());
}

fn body_with_thinking(setting: Option<bool>) -> serde_json::Value {
    let server = Server::start(vec![sse(vec![delta("ok"), finish("stop"), done()])]);
    let mut config = OpenAiConfig::without_key(&server.base_url).unwrap();
    if let Some(enable) = setting {
        config = config.with_chat_template_thinking(enable);
    }
    let mut capability = SingleAttemptCapability::new(OpenAiCompatible::new(config));
    let report = execute(
        &mut capability,
        &request(),
        &Limits::DEFAULT,
        &mut TestHost::default(),
    );
    completion(&report);
    serde_json::from_str(&server.recorded()[0].body).unwrap()
}

#[test]
fn template_thinking_is_a_top_level_chat_template_kwargs_member_in_both_states() {
    for enable in [false, true] {
        let body = body_with_thinking(Some(enable));
        assert_eq!(
            body["chat_template_kwargs"],
            serde_json::json!({ "enable_thinking": enable })
        );
        // Servers read it from the top level; a nested `extra_body` is ignored by them.
        assert!(body.get("extra_body").is_none());
        // A request field, never prompt text.
        assert_eq!(body["messages"], body_with_thinking(None)["messages"]);
    }
}

#[test]
fn template_thinking_is_absent_unless_the_host_sets_it() {
    assert!(
        body_with_thinking(None)
            .get("chat_template_kwargs")
            .is_none()
    );
}

// ---- plain http on a private LAN: keyless, explicit opt-in only -------------------------------

fn run_lan(base_url: &str) -> ExecutionReport {
    let config = OpenAiConfig::without_key_on_private_lan(base_url).unwrap();
    let mut capability = SingleAttemptCapability::new(OpenAiCompatible::new(config));
    execute(
        &mut capability,
        &request(),
        &Limits::DEFAULT,
        &mut TestHost::default(),
    )
}

#[test]
fn the_opt_in_completes_against_a_private_lan_address_and_sends_no_credential() {
    let Some(ip) = support::private_local_ip() else {
        return;
    };
    let server = Server::start_on(
        &ip,
        vec![sse(vec![delta("lan ok"), finish("stop"), done()])],
    );
    let report = run_lan(&server.base_url);
    assert_eq!(completion(&report).content, "lan ok");
    let head = server.recorded()[0].head.to_ascii_lowercase();
    assert!(!head.contains("authorization"), "{head}");
    assert!(!head.contains("x-api-key"), "{head}");
}

#[test]
fn a_redirect_from_a_lan_endpoint_is_a_failure_and_is_not_followed() {
    let Some(ip) = support::private_local_ip() else {
        return;
    };
    let elsewhere = Server::start(vec![sse(vec![delta("elsewhere"), finish("stop"), done()])]);
    let server = Server::start_on(
        &ip,
        vec![http(
            307,
            vec![(
                "location",
                format!("{}/chat/completions", elsewhere.base_url),
            )],
            "",
        )],
    );
    let report = run_lan(&server.base_url);
    assert_eq!(failed(&report).status_hint, Some(307));
    assert_eq!(elsewhere.connections(), 0, "the redirect was not followed");
    assert_eq!(server.connections(), 1, "one request, no retry");
}
