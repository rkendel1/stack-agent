//! The adapter through the real FX executor, against a scripted provider port.
//! Offline and deterministic: no network, credentials, clock or threads.

use std::time::Duration;

use fx_core::*;
use fx_single_attempt::testing::{FakePort, done, failure, poll};
use fx_single_attempt::*;

// ---------------------------------------------------------------------------
// Helpers

#[derive(Default)]
struct Host {
    events: Vec<Event>,
    cancel_before_start: bool,
    cancel_after_events: Option<usize>,
}

impl fx_core::Host for Host {
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
        model: ModelRef("example/model-1".to_owned()),
        messages: vec![Message {
            role: Role::User,
            parts: vec![Part::Text("say hi".to_owned())],
            cache: CacheHint::Default,
        }],
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

fn client_tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: "List directory entries.".to_owned(),
        input_schema_json: r#"{"type":"object"}"#.to_owned(),
        execution: ToolExecution::Client,
        provider_args_json: None,
    }
}

fn run(
    port: FakePort,
    request: &Request,
    host: &mut Host,
) -> (ExecutionReport, SingleAttemptCapability<FakePort>) {
    run_with(port, request, &Limits::DEFAULT, host)
}

fn run_with(
    port: FakePort,
    request: &Request,
    limits: &Limits,
    host: &mut Host,
) -> (ExecutionReport, SingleAttemptCapability<FakePort>) {
    let mut capability = SingleAttemptCapability::new(port);
    let report = execute(&mut capability, request, limits, host);
    (report, capability)
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

fn text_item(text: &str) -> PortItem {
    PortItem::Text(text.to_owned())
}

fn call_item(id: &str, name: &str, arguments: Option<&str>) -> PortItem {
    PortItem::ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments_json: arguments.map(str::to_owned),
    }
}

// ---------------------------------------------------------------------------
// Completion and event translation

#[test]
fn a_text_answer_completes_with_usage_and_the_mapped_finish_reason() {
    let (report, capability) = run(
        FakePort::text(&["hello ", "world"]),
        &request(),
        &mut Host::default(),
    );
    let c = completion(&report);
    assert_eq!(c.content, "hello world");
    assert_eq!(c.finish_reason, FinishReason::Stop);
    assert_eq!(c.usage.as_ref().and_then(|u| u.input_tokens), Some(12));
    assert_eq!(report.disposition, Some(Disposition::Complete));
    let seen = &capability.port().observations;
    assert_eq!((seen.invokes, seen.releases, seen.aborts), (1, 1, 0));
}

#[test]
fn tool_calls_are_proposals_and_missing_arguments_are_left_for_fx_to_refuse() {
    let items = vec![
        call_item("call_1", "list_files", Some(r#"{"path":"."}"#)),
        call_item("call_2", "list_files", None),
        done(Some("tool_calls"), Terminal::ProviderMarker),
    ];
    let mut req = request();
    req.tools = vec![client_tool("list_files")];
    let port = FakePort::new(vec![Ok(poll(items, Observed::OutputObserved, true))]);
    let (report, _) = run(port, &req, &mut Host::default());
    let c = completion(&report);
    assert_eq!(c.proposals.len(), 2);
    assert_eq!(c.proposals[0].arguments_json, r#"{"path":"."}"#);
    assert_eq!(
        c.proposals[1].arguments_json, "{}",
        "FX replaced the absent arguments"
    );
    assert_eq!(report.proposal_checks[1].source, ArgumentSource::Missing);
    assert_eq!(
        admit_proposals(&report.proposal_checks).unwrap_err().reason,
        AdmissionRejection::MissingArguments,
        "admission refuses what the adapter did not repair"
    );
    assert!(
        c.provider_records.is_empty(),
        "the port supplies proposals only"
    );
}

#[test]
fn done_expands_to_metadata_usage_and_finish_in_order() {
    let item = PortItem::Done {
        usage: Some(PortUsage {
            output_tokens: Some(4),
            ..PortUsage::default()
        }),
        finish_reason: Some("end_turn".to_owned()),
        model: Some("provider/model-9".to_owned()),
        terminal: Terminal::ProviderMarker,
    };
    let port = FakePort::new(vec![Ok(poll(
        vec![text_item("x"), item],
        Observed::OutputObserved,
        true,
    ))]);
    let mut host = Host::default();
    let (report, _) = run(port, &request(), &mut host);
    assert!(matches!(host.events[0], Event::TextDelta(_)));
    assert!(matches!(host.events[1], Event::Metadata(_)));
    assert!(matches!(host.events[2], Event::Usage(_)));
    assert!(matches!(
        host.events[3],
        Event::Finish {
            reason: FinishReason::Stop
        }
    ));
    let c = completion(&report);
    assert_eq!(
        c.metadata.model_reported.as_deref(),
        Some("provider/model-9")
    );
    assert_eq!(c.usage.as_ref().and_then(|u| u.output_tokens), Some(4));
    assert_eq!(
        c.usage.as_ref().and_then(|u| u.input_tokens),
        None,
        "absent stays unknown"
    );
}

#[test]
fn done_with_no_usage_emits_no_usage_event() {
    let item = PortItem::Done {
        usage: None,
        finish_reason: Some("stop".into()),
        model: None,
        terminal: Terminal::ProviderMarker,
    };
    let mut host = Host::default();
    let port = FakePort::new(vec![Ok(poll(vec![item], Observed::OutputObserved, true))]);
    let (report, _) = run(port, &request(), &mut host);
    assert_eq!(completion(&report).usage, None);
    assert_eq!(host.events.len(), 1);
}

#[test]
fn a_synthesized_done_without_a_reason_is_an_interrupted_attempt_not_a_completion() {
    let items = vec![
        text_item("par"),
        text_item("tial"),
        done(None, Terminal::Unverified),
    ];
    let port = FakePort::new(vec![Ok(poll(items, Observed::OutputObserved, true))]);
    let (report, _) = run(port, &request(), &mut Host::default());
    let f = failed(&report);
    assert_eq!(f.origin, FailureOrigin::Executor);
    assert_eq!(f.kind, ErrorKind::Transport);
    assert_eq!(
        f.partial.as_ref().map(|p| p.content.as_str()),
        Some("partial")
    );
    assert_eq!(f.delivery, DeliveryCertainty::ConfirmedResponse);
}

#[test]
fn a_synthesized_done_that_carries_a_finish_reason_completes() {
    let items = vec![text_item("ok"), done(Some("stop"), Terminal::Unverified)];
    let port = FakePort::new(vec![Ok(poll(items, Observed::OutputObserved, true))]);
    let (report, _) = run(port, &request(), &mut Host::default());
    assert_eq!(completion(&report).finish_reason, FinishReason::Stop);
}

#[test]
fn a_provider_marker_without_a_reason_completes_as_other() {
    let port = FakePort::new(vec![Ok(poll(
        vec![text_item("ok"), done(None, Terminal::ProviderMarker)],
        Observed::OutputObserved,
        true,
    ))]);
    let (report, _) = run(port, &request(), &mut Host::default());
    assert_eq!(completion(&report).finish_reason, FinishReason::Other);
}

#[test]
fn an_incomplete_stream_ends_the_batch_and_fx_reports_the_partial_output() {
    let port = FakePort::new(vec![
        Ok(poll(
            vec![text_item("so far")],
            Observed::OutputObserved,
            false,
        )),
        Err(failure(
            PortFailureKind::IncompleteStream,
            Observed::OutputObserved,
        )),
    ]);
    let (report, capability) = run(port, &request(), &mut Host::default());
    let f = failed(&report);
    assert_eq!(
        (f.origin, f.kind),
        (FailureOrigin::Executor, ErrorKind::Transport)
    );
    assert_eq!(
        f.partial.as_ref().map(|p| p.content.as_str()),
        Some("so far")
    );
    assert_eq!(capability.port().observations.releases, 1);
}

#[test]
fn items_after_the_providers_done_are_not_read_or_forwarded() {
    let items = vec![
        text_item("a"),
        done(Some("stop"), Terminal::ProviderMarker),
        text_item("late"),
    ];
    let port = FakePort::new(vec![Ok(poll(items, Observed::OutputObserved, false))]);
    let mut host = Host::default();
    let (report, capability) = run(port, &request(), &mut host);
    assert_eq!(completion(&report).content, "a");
    assert!(!host.events.contains(&Event::TextDelta("late".into())));
    assert_eq!(
        capability.port().observations.polls,
        1,
        "the port is not polled after done"
    );
}

#[test]
fn a_done_that_expands_beyond_the_poll_bound_is_delivered_across_polls() {
    let limits = Limits {
        max_events_per_poll: 1,
        ..Limits::DEFAULT
    };
    let item = PortItem::Done {
        usage: Some(PortUsage {
            input_tokens: Some(1),
            ..PortUsage::default()
        }),
        finish_reason: Some("stop".into()),
        model: Some("m".into()),
        terminal: Terminal::ProviderMarker,
    };
    let port = FakePort::new(vec![Ok(poll(vec![item], Observed::OutputObserved, true))]);
    let (report, capability) = run_with(port, &request(), &limits, &mut Host::default());
    assert_eq!(completion(&report).finish_reason, FinishReason::Stop);
    assert_eq!(
        report.diagnostics.polls, 3,
        "metadata, usage and finish arrive one per poll"
    );
    assert_eq!(capability.port().observations.polls, 1);
}

#[test]
fn queued_events_are_delivered_before_a_poll_failure_is_reported() {
    let mut failing = failure(PortFailureKind::ProviderError, Observed::OutputObserved);
    failing.status = Some(503);
    let port = FakePort::new(vec![
        Ok(poll(
            vec![text_item("a"), text_item("b")],
            Observed::OutputObserved,
            false,
        )),
        Err(failing),
    ]);
    let limits = Limits {
        max_events_per_poll: 1,
        ..Limits::DEFAULT
    };
    let (report, _) = run_with(port, &request(), &limits, &mut Host::default());
    let f = failed(&report);
    assert_eq!(
        (f.origin, f.kind),
        (FailureOrigin::Capability, ErrorKind::ProviderUnavailable)
    );
    assert_eq!(f.partial.as_ref().map(|p| p.content.as_str()), Some("ab"));
}

// ---------------------------------------------------------------------------
// Failures and delivery

#[test]
fn a_failure_before_the_provider_was_called_is_definitely_unsent() {
    let port = FakePort::invoke_failure(failure(
        PortFailureKind::ProviderNotFound,
        Observed::NotInvoked,
    ));
    let (report, capability) = run(port, &request(), &mut Host::default());
    let f = failed(&report);
    assert_eq!(
        (f.origin, f.kind),
        (FailureOrigin::Capability, ErrorKind::CapabilityUnavailable)
    );
    assert_eq!(f.delivery, DeliveryCertainty::DefinitelyUnsent);
    assert_eq!(
        capability.port().observations.releases,
        0,
        "no call exists to release"
    );
}

#[test]
fn a_failure_after_the_provider_was_called_is_never_reported_as_unsent() {
    for kind in [
        PortFailureKind::ProviderError,
        PortFailureKind::Timeout,
        PortFailureKind::Aborted,
    ] {
        let port = FakePort::invoke_failure(failure(kind, Observed::Invoked));
        let (report, _) = run(port, &request(), &mut Host::default());
        assert_eq!(
            failed(&report).delivery,
            DeliveryCertainty::PossiblySent,
            "{kind:?}"
        );
    }
}

#[test]
fn the_port_is_invoked_once_and_a_failure_is_not_retried_or_redirected() {
    let mut failing = failure(PortFailureKind::ProviderError, Observed::Invoked);
    failing.status = Some(429);
    failing.retry_after = Some(Duration::from_secs(5));
    let (report, capability) = run(
        FakePort::invoke_failure(failing),
        &request(),
        &mut Host::default(),
    );
    let f = failed(&report);
    assert_eq!(f.kind, ErrorKind::RateLimited);
    assert_eq!(f.retry_after, Some(Duration::from_secs(5)));
    assert_eq!(f.status_hint, Some(429));
    assert_eq!(capability.port().observations.invokes, 1);
}

#[test]
fn failure_kinds_map_as_documented_and_never_from_message_text() {
    let limits = Limits::DEFAULT;
    let table = [
        (
            PortFailureKind::ProviderNotFound,
            None,
            ErrorKind::CapabilityUnavailable,
        ),
        (
            PortFailureKind::Unsupported,
            None,
            ErrorKind::CapabilityUnavailable,
        ),
        (
            PortFailureKind::InvalidRequest,
            None,
            ErrorKind::InvalidRequest,
        ),
        (PortFailureKind::Aborted, None, ErrorKind::Cancelled),
        (PortFailureKind::Timeout, None, ErrorKind::Timeout),
        (
            PortFailureKind::MalformedOutput,
            None,
            ErrorKind::ProtocolViolation,
        ),
        (
            PortFailureKind::IncompleteStream,
            None,
            ErrorKind::Transport,
        ),
        (PortFailureKind::ProviderError, None, ErrorKind::Transport),
        (
            PortFailureKind::ProviderError,
            Some(401),
            ErrorKind::Unauthenticated,
        ),
        (
            PortFailureKind::ProviderError,
            Some(403),
            ErrorKind::Forbidden,
        ),
        (
            PortFailureKind::ProviderError,
            Some(404),
            ErrorKind::ModelUnavailable,
        ),
        (
            PortFailureKind::ProviderError,
            Some(429),
            ErrorKind::RateLimited,
        ),
        (
            PortFailureKind::ProviderError,
            Some(500),
            ErrorKind::ProviderUnavailable,
        ),
        (
            PortFailureKind::ProviderError,
            Some(503),
            ErrorKind::ProviderUnavailable,
        ),
        (
            PortFailureKind::ProviderError,
            Some(418),
            ErrorKind::Transport,
        ),
    ];
    for (kind, status, expected) in table {
        let mut f = failure(kind, Observed::Invoked);
        f.status = status;
        f.message = "rate limit exceeded 429 unauthorized".to_owned();
        assert_eq!(
            map_failure(&f, &limits).kind,
            expected,
            "{kind:?} {status:?}"
        );
    }
}

#[test]
fn delivery_is_mapped_from_observation_and_never_lowered() {
    assert_eq!(
        delivery_of(Observed::NotInvoked),
        DeliveryCertainty::DefinitelyUnsent
    );
    assert_eq!(
        delivery_of(Observed::Invoked),
        DeliveryCertainty::PossiblySent
    );
    assert_eq!(
        delivery_of(Observed::OutputObserved),
        DeliveryCertainty::ConfirmedResponse
    );

    // A failure that reports less than was already observed is raised to what was observed.
    let port = FakePort::new(vec![
        Ok(poll(vec![text_item("a")], Observed::OutputObserved, false)),
        Err(failure(PortFailureKind::ProviderError, Observed::Invoked)),
    ]);
    let (report, _) = run(port, &request(), &mut Host::default());
    assert_eq!(
        failed(&report).delivery,
        DeliveryCertainty::ConfirmedResponse
    );
}

#[test]
fn provider_text_in_failures_is_masked_and_bounded() {
    let secret = "sk-SECRETSECRETSECRET0123456789abcdef";
    let mut f = failure(PortFailureKind::ProviderError, Observed::Invoked);
    f.message = format!("request failed for Bearer {secret} {}", "x".repeat(5_000));
    let (report, _) = run(
        FakePort::invoke_failure(f),
        &request(),
        &mut Host::default(),
    );
    let detail = failed(&report)
        .detail
        .as_ref()
        .expect("detail")
        .as_str()
        .to_owned();
    assert!(!detail.contains(secret), "{detail}");
    assert!(detail.len() <= Limits::DEFAULT.max_detail_bytes);
}

#[test]
fn an_empty_provider_message_leaves_no_detail() {
    let f = failure(PortFailureKind::Timeout, Observed::Invoked);
    assert_eq!(map_failure(&f, &Limits::DEFAULT).detail, None);
}

#[test]
fn finish_reasons_map_exactly_and_unknown_values_are_other() {
    let table = [
        ("stop", FinishReason::Stop),
        ("length", FinishReason::Length),
        ("tool_calls", FinishReason::ToolCalls),
        ("content_filter", FinishReason::ContentFilter),
        ("error", FinishReason::ProviderError),
        ("end_turn", FinishReason::Stop),
        ("stop_sequence", FinishReason::Stop),
        ("max_tokens", FinishReason::Length),
        ("tool_use", FinishReason::ToolCalls),
        ("STOP", FinishReason::Stop),
        ("MAX_TOKENS", FinishReason::Length),
        ("SAFETY", FinishReason::ContentFilter),
        ("RECITATION", FinishReason::Other),
        ("Stop", FinishReason::Other),
        ("", FinishReason::Other),
        ("done", FinishReason::Other),
    ];
    for (provider, expected) in table {
        assert_eq!(map_finish_reason(provider), expected, "{provider:?}");
    }
}

// ---------------------------------------------------------------------------
// Request features the port cannot carry

#[test]
fn a_request_the_port_cannot_carry_is_refused_before_anything_is_sent() {
    let png = ImageData {
        media_type: ImageMediaType::Png,
        bytes: b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR".to_vec(),
    };
    let mut with_image = request();
    with_image.messages[0].parts.push(Part::Image(png));
    let mut with_tool = request();
    with_tool.tools = vec![client_tool("list_files")];
    let mut with_provider_tool = request();
    with_provider_tool.tools = vec![ToolDefinition {
        execution: ToolExecution::Provider,
        ..client_tool("search")
    }];
    let mut with_choice = request();
    with_choice.tools = vec![client_tool("list_files")];
    with_choice.tool_choice = ToolChoice::Required;
    let mut with_options = request();
    with_options.options.max_output_tokens = Some(100);
    let mut with_format = request();
    with_format.options.response_format = Some(ResponseFormat {
        name: "n".into(),
        description: "d".into(),
        schema_json: "{}".into(),
    });
    let mut with_history = request();
    with_history.messages = vec![
        with_history.messages[0].clone(),
        Message {
            role: Role::Assistant,
            parts: vec![Part::ToolCall(ToolCallProposal {
                id: "c1".into(),
                name: "list_files".into(),
                arguments_json: "{}".into(),
            })],
            cache: CacheHint::Default,
        },
        Message {
            role: Role::Tool,
            parts: vec![Part::ToolResult(ToolResult {
                call_id: "c1".into(),
                name: "list_files".into(),
                output: ToolOutput::Text("x".into()),
            })],
            cache: CacheHint::Default,
        },
    ];
    with_history.tools = vec![client_tool("list_files")];

    for (label, req) in [
        ("image", with_image),
        ("tool schema", with_tool),
        ("provider tool", with_provider_tool),
        ("tool choice", with_choice),
        ("options", with_options),
        ("format", with_format),
        ("history", with_history),
    ] {
        let mut port = FakePort::text(&["unreachable"]);
        port.support = PortSupport::default();
        let (report, capability) = run(port, &req, &mut Host::default());
        let f = failed(&report);
        assert_eq!(
            (f.origin, f.kind),
            (FailureOrigin::Capability, ErrorKind::CapabilityUnavailable),
            "{label}"
        );
        assert_eq!(f.delivery, DeliveryCertainty::DefinitelyUnsent, "{label}");
        assert_eq!(
            capability.port().observations.invokes,
            0,
            "{label}: the port was never called"
        );
        let detail = f.detail.as_ref().expect("detail").as_str();
        assert!(detail.contains("cannot carry"), "{label}: {detail}");
    }
}

#[test]
fn a_supported_feature_set_is_not_refused() {
    let mut req = request();
    req.tools = vec![client_tool("list_files")];
    req.options.max_output_tokens = Some(10);
    let (report, capability) = run(FakePort::text(&["ok"]), &req, &mut Host::default());
    assert!(matches!(report.outcome, Outcome::Completed(_)));
    assert_eq!(capability.port().observations.invokes, 1);
}

#[test]
fn refusals_never_echo_request_content() {
    let secret = "sk-SECRETSECRETSECRET0123456789abcdef";
    let mut req = request();
    req.messages[0].parts = vec![Part::Text(format!("prompt {secret}"))];
    req.options.max_output_tokens = Some(10);
    let mut port = FakePort::text(&["x"]);
    port.support = PortSupport::default();
    let (report, _) = run(port, &req, &mut Host::default());
    let detail = failed(&report).detail.as_ref().unwrap().as_str().to_owned();
    assert!(!detail.contains(secret) && !detail.contains("prompt"));
}

// ---------------------------------------------------------------------------
// Cancellation and budget

#[test]
fn cancellation_before_start_never_reaches_the_port() {
    let (report, capability) = run(
        FakePort::text(&["x"]),
        &request(),
        &mut Host {
            cancel_before_start: true,
            ..Host::default()
        },
    );
    match &report.outcome {
        Outcome::Cancelled(c) => {
            assert_eq!(c.delivery, DeliveryCertainty::DefinitelyUnsent);
            assert_eq!(c.remote, RemoteCancellation::NotStarted);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(capability.port().observations.invokes, 0);
}

fn cancel_scenario(transport_closed: bool) -> Cancellation {
    let mut port = FakePort::new(vec![
        Ok(poll(
            vec![text_item("first")],
            Observed::OutputObserved,
            false,
        )),
        Ok(poll(
            vec![
                text_item("second"),
                done(Some("stop"), Terminal::ProviderMarker),
            ],
            Observed::OutputObserved,
            true,
        )),
    ]);
    port.abort = AbortReport { transport_closed };
    let mut host = Host {
        cancel_after_events: Some(1),
        ..Host::default()
    };
    let (report, capability) = run(port, &request(), &mut host);
    let seen = &capability.port().observations;
    assert_eq!((seen.aborts, seen.releases), (1, 1));
    assert!(!host.events.contains(&Event::TextDelta("second".into())));
    match report.outcome {
        Outcome::Cancelled(c) => c,
        other => panic!("{other:?}"),
    }
}

#[test]
fn cancellation_mid_stream_is_unconfirmed_unless_the_transport_was_seen_closing() {
    let unconfirmed = cancel_scenario(false);
    assert_eq!(unconfirmed.remote, RemoteCancellation::Unconfirmed);
    let confirmed = cancel_scenario(true);
    assert_eq!(confirmed.remote, RemoteCancellation::Confirmed);
    for c in [unconfirmed, confirmed] {
        assert_eq!(c.delivery, DeliveryCertainty::ConfirmedResponse);
        assert!(
            c.delivery.may_have_executed(),
            "cancellation never proves non-execution"
        );
        assert_eq!(
            c.partial.as_ref().map(|p| p.content.as_str()),
            Some("first")
        );
    }
}

#[test]
fn a_cancel_right_after_start_is_not_reported_as_unsent() {
    // The provider call is made inside invoke, so the first poll has not happened yet.
    let port = FakePort::new(vec![Ok(poll(vec![], Observed::Invoked, false))]);
    let mut capability = SingleAttemptCapability::new(port);
    let request = request();
    let context = AttemptContext {
        request: &request,
        limits: &Limits::DEFAULT,
        budget: None,
    };
    let mut started = capability.start(&context).expect("start");
    assert_eq!(started.delivery, DeliveryCertainty::PossiblySent);
    let status = capability.cancel(&mut started.handle);
    assert_eq!(status, CancelStatus::Requested);
    capability.close(started.handle);
}

#[test]
fn the_budget_reaches_the_port_and_its_timeout_is_a_failure_that_keeps_delivery() {
    let mut req = request();
    req.budget = Some(AttemptBudget(Duration::from_millis(1500)));
    let port = FakePort::new(vec![Err(failure(
        PortFailureKind::Timeout,
        Observed::Invoked,
    ))]);
    let (report, capability) = run(port, &req, &mut Host::default());
    assert_eq!(
        capability.port().observations.budget,
        Some(AttemptBudget(Duration::from_millis(1500)))
    );
    let f = failed(&report);
    assert_eq!(
        (f.origin, f.kind, f.delivery),
        (
            FailureOrigin::Capability,
            ErrorKind::Timeout,
            DeliveryCertainty::PossiblySent
        )
    );
}

// ---------------------------------------------------------------------------
// Invariants owned by FX still hold through the adapter

#[test]
fn an_invalid_request_never_reaches_the_port() {
    let mut req = request();
    req.model = ModelRef(String::new());
    let (report, capability) = run(FakePort::text(&["x"]), &req, &mut Host::default());
    assert_eq!(failed(&report).kind, ErrorKind::InvalidRequest);
    assert_eq!(capability.port().observations.invokes, 0);
}

#[test]
fn a_stalled_port_is_bounded_by_the_poll_limit() {
    let limits = Limits {
        max_polls_per_attempt: 5,
        ..Limits::DEFAULT
    };
    let stall: Vec<_> = (0..20)
        .map(|_| Ok(poll(vec![], Observed::Invoked, false)))
        .collect();
    let (report, capability) = run_with(
        FakePort::new(stall),
        &request(),
        &limits,
        &mut Host::default(),
    );
    assert_eq!(failed(&report).kind, ErrorKind::ResourceLimit);
    assert_eq!(capability.port().observations.aborts, 1);
    assert_eq!(capability.port().observations.releases, 1);
}

#[test]
fn the_port_is_polled_with_the_executors_event_bound() {
    let limits = Limits {
        max_events_per_poll: 7,
        ..Limits::DEFAULT
    };
    let (_, capability) = run_with(
        FakePort::text(&["x"]),
        &request(),
        &limits,
        &mut Host::default(),
    );
    assert_eq!(capability.port().observations.max_items_requested, 7);
}

#[test]
fn independent_attempts_share_nothing() {
    let mut capability = SingleAttemptCapability::new(FakePort::text(&["one"]));
    let first = execute(
        &mut capability,
        &request(),
        &Limits::DEFAULT,
        &mut Host::default(),
    );
    assert_eq!(completion(&first).content, "one");
    // The script is exhausted: a second attempt is a fresh call that ends without finish.
    let second = execute(
        &mut capability,
        &request(),
        &Limits::DEFAULT,
        &mut Host::default(),
    );
    assert_eq!(failed(&second).kind, ErrorKind::Transport);
    assert_eq!(capability.port().observations.invokes, 2);
    assert_eq!(capability.port().observations.releases, 2);
}

#[test]
fn the_adapter_raises_an_error_to_the_delivery_it_already_observed() {
    // Checked on the capability directly, because the executor also advances delivery.
    let port = FakePort::new(vec![
        Ok(poll(vec![text_item("a")], Observed::OutputObserved, false)),
        Err(failure(PortFailureKind::ProviderError, Observed::Invoked)),
    ]);
    let mut capability = SingleAttemptCapability::new(port);
    let request = request();
    let context = AttemptContext {
        request: &request,
        limits: &Limits::DEFAULT,
        budget: None,
    };
    let mut started = capability.start(&context).expect("start");
    let first = capability.poll(&mut started.handle, 8).expect("first poll");
    assert_eq!(first.delivery, DeliveryCertainty::ConfirmedResponse);
    let error = capability
        .poll(&mut started.handle, 8)
        .expect_err("second poll fails");
    assert_eq!(
        error.delivery,
        DeliveryCertainty::ConfirmedResponse,
        "never lowered below what was observed"
    );
    capability.close(started.handle);
}
