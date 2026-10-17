//! Executor behavior against the deterministic fake capability.
//!
//! Expected behavior comes from docs/rebuild/BEHAVIOR_MATRIX.md and the Zig oracle
//! tests it cites; test names are referenced from BEHAVIOR_MATRIX.md §9.

mod common;

use std::time::Duration;

use common::*;
use fx_core::testing::{FakeCapability, Script, StartBehavior, Step};
use fx_core::*;

// ---------------------------------------------------------------------------
// Hosts

#[derive(Default)]
struct Recorder {
    events: Vec<Event>,
    cancel_now: bool,
    cancel_after_events: Option<usize>,
    stop_after_events: Option<usize>,
    idle_stops_after: Option<u32>,
    idles: u32,
    cancel_checks: u32,
}

impl Host for Recorder {
    fn cancel_requested(&mut self) -> bool {
        self.cancel_checks += 1;
        self.cancel_now
            || self
                .cancel_after_events
                .is_some_and(|n| self.events.len() >= n)
    }
    fn on_event(&mut self, event: &Event) -> Flow {
        self.events.push(event.clone());
        match self.stop_after_events {
            Some(n) if self.events.len() >= n => Flow::Stop,
            _ => Flow::Continue,
        }
    }
    fn idle(&mut self) -> Flow {
        self.idles += 1;
        match self.idle_stops_after {
            Some(n) if self.idles >= n => Flow::Stop,
            _ => Flow::Continue,
        }
    }
}

fn run(script: Script, request: &Request) -> (ExecutionReport, FakeCapability, Recorder) {
    run_with(script, request, &Limits::DEFAULT, Recorder::default())
}

fn run_with(
    script: Script,
    request: &Request,
    limits: &Limits,
    mut host: Recorder,
) -> (ExecutionReport, FakeCapability, Recorder) {
    let mut fake = FakeCapability::new(script);
    let report = execute(&mut fake, request, limits, &mut host);
    (report, fake, host)
}

fn finish(reason: FinishReason) -> Event {
    Event::Finish { reason }
}

fn delta(text: &str) -> Event {
    Event::TextDelta(text.to_owned())
}

fn tool_call(id: &str, name: &str, args: &str) -> Event {
    Event::ToolCall(ToolCallProposal {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments_json: args.to_owned(),
    })
}

fn batch(events: Vec<Event>, state: StreamState) -> Step {
    Step::Batch {
        events,
        delivery: DeliveryCertainty::ConfirmedResponse,
        state,
    }
}

fn completion(report: &ExecutionReport) -> &Completion {
    match &report.outcome {
        Outcome::Completed(c) => c,
        other => panic!("expected completion, got {other:?}"),
    }
}

fn failure(report: &ExecutionReport) -> &Failure {
    match &report.outcome {
        Outcome::Failed(f) => f,
        other => panic!("expected failure, got {other:?}"),
    }
}

fn cancellation(report: &ExecutionReport) -> &Cancellation {
    match &report.outcome {
        Outcome::Cancelled(c) => c,
        other => panic!("expected cancellation, got {other:?}"),
    }
}

fn cap_error(kind: ErrorKind, delivery: DeliveryCertainty) -> CapabilityError {
    CapabilityError {
        kind,
        delivery,
        retry_after: None,
        status_hint: None,
        detail: None,
    }
}

fn with_provider_tool() -> Request {
    let mut request = minimal();
    request.tools = vec![
        tool("list_files"),
        ToolDefinition {
            name: "perplexity_search".into(),
            description: "Search the web.".into(),
            input_schema_json: r#"{"type":"object"}"#.into(),
            execution: ToolExecution::Provider,
            provider_args_json: None,
        },
    ];
    request
}

// ---------------------------------------------------------------------------
// Validation and invocation

#[test]
fn invalid_requests_never_invoke_the_capability() {
    let mut invalid = Vec::new();
    let mut r = minimal();
    r.model = ModelRef(String::new());
    invalid.push(r);
    invalid.push(history_with(vec![message(
        Role::Tool,
        vec![result("c1", "t", "x")],
    )]));
    let mut r = minimal();
    r.tool_choice = ToolChoice::Named("missing".into());
    invalid.push(r);
    for request in invalid {
        let (report, fake, host) = run(Script::text(&["hi"]), &request);
        assert_eq!(fake.observations.starts, 0);
        assert_eq!(fake.observations.polls, 0);
        assert_eq!(host.cancel_checks, 0, "validation precedes everything");
        let f = failure(&report);
        assert_eq!(f.kind, ErrorKind::InvalidRequest);
        assert_eq!(f.origin, FailureOrigin::Executor);
        assert_eq!(f.delivery, DeliveryCertainty::DefinitelyUnsent);
        assert!(matches!(
            report.diagnostics.terminal,
            TerminalCause::InvalidRequest(_)
        ));
        assert_eq!(report.diagnostics.starts, 0);
    }
}

fn history_with(messages: Vec<Message>) -> Request {
    let mut request = minimal();
    request.messages = messages;
    request
}

#[test]
fn undecodable_documents_never_invoke_the_capability() {
    let mut fake = FakeCapability::new(Script::text(&["hi"]));
    for doc in [&b"{"[..], br#"{"v":2}"#, b"\xff"] {
        let report = execute_document(&mut fake, doc, &Limits::DEFAULT, &mut NoHost);
        assert_eq!(failure(&report).kind, ErrorKind::InvalidRequest);
        assert_eq!(
            report.diagnostics.terminal,
            TerminalCause::InvalidRequest(ValidationCode::Undecodable)
        );
    }
    assert_eq!(fake.observations.starts, 0);
    let ok = encode_request(&minimal());
    let report = execute_document(&mut fake, &ok, &Limits::DEFAULT, &mut NoHost);
    assert_eq!(completion(&report).content, "hi");
    assert_eq!(fake.observations.starts, 1);
}

#[test]
fn a_valid_request_starts_the_capability_exactly_once_and_closes_it_once() {
    let (report, fake, _) = run(Script::text(&["hel", "lo"]), &minimal());
    assert_eq!(completion(&report).content, "hello");
    assert_eq!(fake.observations.starts, 1);
    assert_eq!(fake.observations.closes, 1);
    assert_eq!(fake.observations.cancels, 0);
    assert_eq!(fake.observations.misuse, 0);
    assert_eq!(report.diagnostics.starts, 1);
    assert_eq!(report.diagnostics.closes, 1);
}

#[test]
fn a_start_failure_is_not_retried_and_nothing_is_closed() {
    let (report, fake, _) = run(
        Script::start_failure(
            ErrorKind::Unauthenticated,
            DeliveryCertainty::DefinitelyUnsent,
        ),
        &minimal(),
    );
    let f = failure(&report);
    assert_eq!(f.origin, FailureOrigin::Capability);
    assert_eq!(f.kind, ErrorKind::Unauthenticated);
    assert_eq!(f.delivery, DeliveryCertainty::DefinitelyUnsent);
    assert_eq!(fake.observations.starts, 1);
    assert_eq!(fake.observations.polls, 0);
    assert_eq!(fake.observations.closes, 0, "no handle exists");
    assert_eq!(report.diagnostics.terminal, TerminalCause::StartFailed);
}

#[test]
fn a_poll_failure_is_not_retried_and_keeps_partial_output() {
    let script = Script {
        start: StartBehavior::Ok(DeliveryCertainty::PossiblySent),
        steps: vec![
            batch(vec![delta("visible partial output")], StreamState::Open),
            Step::Fail(CapabilityError {
                kind: ErrorKind::Transport,
                delivery: DeliveryCertainty::PossiblySent,
                retry_after: None,
                status_hint: None,
                detail: None,
            }),
            batch(
                vec![delta("never read"), finish(FinishReason::Stop)],
                StreamState::Ended,
            ),
        ],
        cancel: CancelStatus::Requested,
    };
    let (report, fake, _) = run(script, &minimal());
    let f = failure(&report);
    assert_eq!(f.kind, ErrorKind::Transport);
    assert_eq!(f.origin, FailureOrigin::Capability);
    // Events were observed, so a response was confirmed; the error's lower
    // certainty does not regress it.
    assert_eq!(f.delivery, DeliveryCertainty::ConfirmedResponse);
    assert_eq!(
        f.partial.as_ref().unwrap().content,
        "visible partial output"
    );
    assert_eq!(fake.observations.starts, 1);
    assert_eq!(fake.observations.polls, 2);
    assert_eq!(fake.observations.closes, 1);
}

#[test]
fn independent_executor_calls_have_independent_attempts() {
    let request = minimal();
    let mut first = FakeCapability::new(Script::text(&["one"]));
    let mut second = FakeCapability::new(Script::start_failure(
        ErrorKind::RateLimited,
        DeliveryCertainty::PossiblySent,
    ));
    let a = execute(&mut first, &request, &Limits::DEFAULT, &mut NoHost);
    let b = execute(&mut second, &request, &Limits::DEFAULT, &mut NoHost);
    let c = execute(&mut first, &request, &Limits::DEFAULT, &mut NoHost);
    assert_eq!(completion(&a).content, "one");
    assert_eq!(failure(&b).kind, ErrorKind::RateLimited);
    // A second call on the same capability is a new attempt; the fake's script is
    // exhausted, so the stream ends without a finish (interrupted).
    assert_eq!(failure(&c).kind, ErrorKind::Transport);
    assert_eq!(first.observations.starts, 2);
    assert_eq!(second.observations.starts, 1);
}

#[test]
fn parallel_executions_are_isolated() {
    // BM-11: executions share no state; the crate has no global mutable state
    // (enforced by check-boundaries.sh), so concurrent attempts cannot interfere.
    let handles: Vec<_> = (0..8)
        .map(|i| {
            std::thread::spawn(move || {
                let text = format!("answer {i}");
                let mut fake = FakeCapability::new(Script::text(&[text.as_str()]));
                let report = execute(&mut fake, &minimal(), &Limits::DEFAULT, &mut NoHost);
                (text, report, fake.observations)
            })
        })
        .collect();
    for handle in handles {
        let (text, report, observations) = handle.join().unwrap();
        assert_eq!(completion(&report).content, text);
        assert_eq!((observations.starts, observations.closes), (1, 1));
    }
}

#[test]
fn budget_and_bounds_are_passed_to_the_capability() {
    let mut request = minimal();
    request.budget = Some(AttemptBudget(Duration::from_millis(1500)));
    let limits = Limits {
        max_events_per_poll: 7,
        ..Limits::DEFAULT
    };
    let (_, fake, _) = run_with(Script::text(&["x"]), &request, &limits, Recorder::default());
    assert_eq!(
        fake.observations.budget,
        Some(AttemptBudget(Duration::from_millis(1500)))
    );
    assert_eq!(fake.observations.max_events_requested, 7);
    assert_eq!(fake.observations.messages, 1);
}

// ---------------------------------------------------------------------------
// Events and terminal state

#[test]
fn deltas_are_forwarded_in_order_and_accumulated() {
    let (report, _, host) = run(Script::text(&["a", "b", "c"]), &minimal());
    let texts: Vec<_> = host
        .events
        .iter()
        .filter_map(|e| match e {
            Event::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["a", "b", "c"]);
    assert_eq!(completion(&report).content, "abc");
    assert_eq!(report.disposition, Some(Disposition::Complete));
    assert!(matches!(host.events.last(), Some(Event::Finish { .. })));
}

#[test]
fn finish_is_terminal_and_later_events_are_dropped_not_forwarded() {
    // Oracle: consumeSseStream returns immediately after valid finish without waiting
    // for done; ... ignores conflicting and late stream events without mutation.
    let script = Script {
        start: StartBehavior::Ok(DeliveryCertainty::PossiblySent),
        steps: vec![
            batch(
                vec![
                    delta("done"),
                    finish(FinishReason::Stop),
                    delta("late"),
                    finish(FinishReason::Length),
                    tool_call("c1", "list_files", "{}"),
                ],
                StreamState::Open,
            ),
            batch(vec![delta("never polled")], StreamState::Ended),
        ],
        cancel: CancelStatus::Requested,
    };
    let (report, fake, host) = run(script, &minimal());
    let c = completion(&report);
    assert_eq!(c.content, "done");
    assert_eq!(c.finish_reason, FinishReason::Stop);
    assert!(c.proposals.is_empty());
    assert_eq!(report.diagnostics.anomalies, 3);
    assert_eq!(fake.observations.polls, 1, "no poll after finish");
    assert_eq!(fake.observations.cancels, 0);
    assert_eq!(fake.observations.closes, 1);
    assert_eq!(host.events.len(), 2);
    assert_eq!(host.events.last(), Some(&finish(FinishReason::Stop)));
}

#[test]
fn a_stream_that_ends_without_finish_is_interrupted_with_partial_output() {
    // Oracle: consumeSseStream preserves partial content without finish proof at EOF;
    // E2E "missing provider finish recovers across framing and content variants".
    let script = Script::events(vec![delta("visible partial text")]);
    let (report, _, _) = run(script, &minimal());
    let f = failure(&report);
    assert_eq!(f.origin, FailureOrigin::Executor);
    assert_eq!(f.kind, ErrorKind::Transport);
    assert_eq!(f.delivery, DeliveryCertainty::ConfirmedResponse);
    assert_eq!(
        f.partial,
        Some(PartialOutput {
            content: "visible partial text".into(),
            content_truncated: false
        })
    );
    assert_eq!(
        report.diagnostics.terminal,
        TerminalCause::EndedWithoutFinish
    );
    assert_eq!(report.disposition, None);
}

#[test]
fn an_empty_stream_without_finish_has_no_partial_output() {
    let script = Script {
        start: StartBehavior::Ok(DeliveryCertainty::PossiblySent),
        steps: vec![Step::Batch {
            events: vec![],
            delivery: DeliveryCertainty::PossiblySent,
            state: StreamState::Ended,
        }],
        cancel: CancelStatus::Requested,
    };
    let (report, _, _) = run(script, &minimal());
    let f = failure(&report);
    assert_eq!(f.kind, ErrorKind::Transport);
    assert_eq!(f.partial, None);
    // No event arrived, so the capability's reported certainty is all we know:
    // the executor never upgrades it to a confirmed response.
    assert_eq!(f.delivery, DeliveryCertainty::PossiblySent);
}

#[test]
fn an_empty_answer_with_finish_completes() {
    let (report, _, _) = run(Script::events(vec![finish(FinishReason::Stop)]), &minimal());
    let c = completion(&report);
    assert_eq!(c.content, "");
    assert_eq!(c.usage, None, "absent usage stays unknown");
    assert_eq!(report.disposition, Some(Disposition::Complete));
}

#[test]
fn classification_matches_the_oracle() {
    // Oracle: classifyProviderCompletion (types.zig:1123-1139).
    let cases = [
        (vec![finish(FinishReason::Stop)], Disposition::Complete),
        (vec![finish(FinishReason::Other)], Disposition::Complete),
        (
            vec![finish(FinishReason::Length)],
            Disposition::LengthLimited,
        ),
        (
            vec![finish(FinishReason::ContentFilter)],
            Disposition::ProviderFailure,
        ),
        (
            vec![finish(FinishReason::ProviderError)],
            Disposition::ProviderFailure,
        ),
        (
            vec![
                tool_call("c1", "list_files", "{}"),
                finish(FinishReason::ToolCalls),
            ],
            Disposition::ToolProposals,
        ),
        (
            vec![
                tool_call("c1", "list_files", "{}"),
                finish(FinishReason::Stop),
            ],
            Disposition::InvalidCompletion,
        ),
        (
            vec![finish(FinishReason::ToolCalls)],
            Disposition::InvalidCompletion,
        ),
    ];
    for (events, expected) in cases {
        let (report, _, _) = run(Script::events(events.clone()), &with_provider_tool());
        assert_eq!(report.disposition, Some(expected), "{events:?}");
    }
    let provider_only = vec![
        Event::ProviderToolRecord(ProviderToolRecord {
            call_id: "ws_1".into(),
            name: "perplexity_search".into(),
            input_json: "{}".into(),
            result_json: Some("{}".into()),
            is_final: true,
        }),
        finish(FinishReason::ToolCalls),
    ];
    let (report, _, _) = run(Script::events(provider_only), &with_provider_tool());
    assert_eq!(report.disposition, Some(Disposition::Complete));
}

#[test]
fn reasoning_is_forwarded_and_counted_but_not_retained() {
    let script = Script::events(vec![
        Event::ReasoningDelta("secret chain of thought".into()),
        delta("answer"),
        finish(FinishReason::Stop),
    ]);
    let (report, _, host) = run(script, &minimal());
    let c = completion(&report);
    assert_eq!(c.content, "answer");
    assert_eq!(c.reasoning_bytes, "secret chain of thought".len() as u64);
    assert!(matches!(host.events[0], Event::ReasoningDelta(_)));
}

// ---------------------------------------------------------------------------
// Tool proposals (I5-I8)

fn stream(id: &str, name: &str, args: &str, end: bool) -> Vec<Event> {
    let mut events = vec![
        Event::ToolInputStart {
            id: id.into(),
            name: name.into(),
        },
        Event::ToolInputDelta {
            id: id.into(),
            delta: args.into(),
        },
    ];
    if end {
        events.push(Event::ToolInputEnd { id: id.into() });
    }
    events
}

fn propose(events: Vec<Event>) -> (Completion, Vec<ProposalCheck>) {
    let mut all = events;
    all.push(finish(FinishReason::ToolCalls));
    let (report, _, _) = run(Script::events(all), &with_provider_tool());
    (completion(&report).clone(), report.proposal_checks)
}

#[test]
fn final_arguments_win_over_streamed_input() {
    // Oracle: consumeSseStream accepts authoritative final input before end.
    let mut events = stream("c1", "list_files", r#"{"path":"streamed"}"#, false);
    events.push(tool_call("c1", "list_files", r#"{"path":"final"}"#));
    let (c, checks) = propose(events);
    assert_eq!(c.proposals[0].arguments_json, r#"{"path":"final"}"#);
    assert_eq!(checks[0].source, ArgumentSource::Final);
    assert_eq!(checks[0].integrity, ArgumentIntegrity::Valid);
}

#[test]
fn absent_arguments_fall_back_to_an_ended_stream_with_the_same_id_and_name() {
    // Oracle: absent outer input uses exact-id ended fallback for compatible names.
    let mut events = stream("c1", "list_files", r#"{"path":"."}"#, true);
    events.push(tool_call("c1", "list_files", ""));
    let (c, checks) = propose(events);
    assert_eq!(c.proposals[0].arguments_json, r#"{"path":"."}"#);
    assert_eq!(checks[0].source, ArgumentSource::StreamedFallback);
    assert_eq!(checks[0].identity, ProposalIdentity::Valid);
}

#[test]
fn an_open_stream_never_lends_arguments() {
    // Oracle: consumeSseStream rejects fallback from an open stream.
    let mut events = stream("c1", "list_files", r#"{"path":"."}"#, false);
    events.push(tool_call("c1", "list_files", ""));
    let (c, checks) = propose(events);
    assert_eq!(c.proposals[0].arguments_json, "{}");
    assert_eq!(checks[0].source, ArgumentSource::Missing);
    assert_eq!(checks[0].integrity, ArgumentIntegrity::Replaced);
}

#[test]
fn a_conflicting_streamed_name_is_flagged_and_lends_nothing() {
    // Oracle: conflicting final tool name cannot inherit streamed arguments.
    let mut events = stream("c1", "read_file", r#"{"path":"victim.txt"}"#, true);
    events.push(tool_call("c1", "delete_file", ""));
    let (c, checks) = propose(events);
    assert_eq!(c.proposals[0].arguments_json, "{}");
    assert_eq!(
        checks[0].identity,
        ProposalIdentity::ConflictingStreamedName
    );
    assert_eq!(
        admit_proposals(&checks),
        Err(AdmissionError {
            index: 0,
            reason: AdmissionRejection::Identity(ProposalIdentity::ConflictingStreamedName)
        })
    );
}

#[test]
fn empty_and_duplicate_ids_are_preserved_flagged_and_refused_by_admission() {
    // Oracle: preserves duplicate final ids for duplicate admission; duplicate final
    // ids do not reuse finalized stream fallback; malformed final identity borrows no
    // streamed state.
    let mut events = stream("c1", "list_files", r#"{"path":"a"}"#, true);
    events.push(tool_call("c1", "list_files", ""));
    events.push(tool_call("c1", "list_files", ""));
    events.push(tool_call("", "list_files", ""));
    let (c, checks) = propose(events);
    assert_eq!(c.proposals.len(), 3);
    assert_eq!(c.proposals[0].arguments_json, r#"{"path":"a"}"#);
    assert_eq!(checks[1].identity, ProposalIdentity::DuplicateId);
    assert_eq!(
        c.proposals[1].arguments_json, "{}",
        "no reuse of a lent stream"
    );
    assert_eq!(checks[2].identity, ProposalIdentity::EmptyId);
    // Duplicates are exempt from the missing-arguments rule, as in the oracle;
    // duplicate rejection covers them.
    assert_eq!(
        admit_proposals(&checks).unwrap_err().reason,
        AdmissionRejection::Identity(ProposalIdentity::DuplicateId)
    );
}

#[test]
fn missing_arguments_are_refused_by_admission() {
    // Oracle: rejects absent outer input without exact ended fallback
    // (invalid_tool_input, incomplete_streamed_input); rejects fallback from an
    // open stream. The "{}" placeholder must never be admitted for execution.
    let cases = [
        vec![tool_call("c1", "list_files", "")],
        stream("c1", "list_files", r#"{"path":"partial"}"#, false)
            .into_iter()
            .chain([tool_call("c1", "list_files", "")])
            .collect(),
        stream("other", "list_files", r#"{"path":"README.md"}"#, true)
            .into_iter()
            .chain([tool_call("c1", "list_files", "")])
            .collect(),
    ];
    for events in cases {
        let (c, checks) = propose(events);
        assert_eq!(c.proposals[0].arguments_json, "{}");
        assert_eq!(checks[0].source, ArgumentSource::Missing);
        assert_eq!(
            admit_proposals(&checks),
            Err(AdmissionError {
                index: 0,
                reason: AdmissionRejection::MissingArguments
            })
        );
    }
    // Missing arguments are checked before identities, as in the oracle.
    let (_, checks) = propose(vec![
        tool_call("", "list_files", "{}"),
        tool_call("c2", "list_files", ""),
    ]);
    assert_eq!(
        admit_proposals(&checks),
        Err(AdmissionError {
            index: 1,
            reason: AdmissionRejection::MissingArguments
        })
    );
}

#[test]
fn malformed_arguments_are_replaced_with_an_empty_object() {
    // Oracle: replaces malformed trailing or duplicate-key serialized final input with safe JSON.
    for bad in [r#"{"path":"."}x"#, r#"{"a":1,"a":2}"#, "{", "[1,]"] {
        let (c, checks) = propose(vec![tool_call("c1", "list_files", bad)]);
        assert_eq!(c.proposals[0].arguments_json, "{}", "{bad}");
        assert_eq!(checks[0].integrity, ArgumentIntegrity::Replaced);
        assert_eq!(checks[0].source, ArgumentSource::Final);
        // Replaced arguments are still admissible; the consumer reports the problem
        // to the model, as the oracle does.
        assert!(admit_proposals(&checks).is_ok());
    }
    // Scalar roots are valid JSON (oracle: preserves valid serialized scalar roots).
    let (c, _) = propose(vec![tool_call("c1", "list_files", "\"x\"")]);
    assert_eq!(c.proposals[0].arguments_json, "\"x\"");
}

#[test]
fn unmatched_streamed_events_are_dropped_as_anomalies() {
    let events = vec![
        Event::ToolInputDelta {
            id: "ghost".into(),
            delta: "{}".into(),
        },
        Event::ToolInputEnd { id: "ghost".into() },
        Event::ToolInputStart {
            id: "c1".into(),
            name: "list_files".into(),
        },
        Event::ToolInputStart {
            id: "c1".into(),
            name: "list_files".into(),
        },
        Event::ToolInputEnd { id: "c1".into() },
        Event::ToolInputEnd { id: "c1".into() },
        finish(FinishReason::Stop),
    ];
    let (report, _, host) = run(Script::events(events), &minimal());
    assert_eq!(report.diagnostics.anomalies, 4);
    assert_eq!(host.events.len(), 3);
}

#[test]
fn proposals_are_data_and_never_execute() {
    // The executor only reports proposals; the fake has no way to run a tool and
    // nothing in the report claims execution.
    let (c, checks) = propose(vec![tool_call("c1", "list_files", r#"{"path":"."}"#)]);
    assert_eq!(c.proposals.len(), 1);
    assert!(c.provider_records.is_empty());
    assert!(admit_proposals(&checks).is_ok());
}

#[test]
fn provider_records_are_accepted_only_for_declared_provider_tools() {
    // Oracle: rejects provider results for local calls; rejects ambiguous provider
    // result correlation.
    let record = |call_id: &str, name: &str| {
        Event::ProviderToolRecord(ProviderToolRecord {
            call_id: call_id.into(),
            name: name.into(),
            input_json: "{}".into(),
            result_json: Some("{}".into()),
            is_final: true,
        })
    };
    let cases = [
        (
            vec![record("ws_1", "list_files")],
            Violation::ProviderRecordForUndeclaredTool,
        ),
        (
            vec![record("ws_1", "unknown")],
            Violation::ProviderRecordForUndeclaredTool,
        ),
        (
            vec![
                tool_call("c1", "list_files", "{}"),
                record("c1", "perplexity_search"),
            ],
            Violation::ProviderRecordIdCollision,
        ),
        (
            vec![
                record("c1", "perplexity_search"),
                tool_call("c1", "list_files", "{}"),
            ],
            Violation::ProviderRecordIdCollision,
        ),
    ];
    for (events, violation) in cases {
        let (report, fake, _) = run(Script::events(events), &with_provider_tool());
        assert_eq!(failure(&report).kind, ErrorKind::ProtocolViolation);
        assert_eq!(
            report.diagnostics.terminal,
            TerminalCause::Violation(violation)
        );
        assert_eq!(fake.observations.cancels, 1);
        assert_eq!(fake.observations.closes, 1);
    }
    let malformed = Event::ProviderToolRecord(ProviderToolRecord {
        call_id: "ws_1".into(),
        name: "perplexity_search".into(),
        input_json: "{".into(),
        result_json: None,
        is_final: false,
    });
    let (report, _, _) = run(Script::events(vec![malformed]), &with_provider_tool());
    assert_eq!(
        report.diagnostics.terminal,
        TerminalCause::Violation(Violation::MalformedProviderRecord)
    );
}

#[test]
fn the_first_final_provider_result_wins() {
    // Oracle: replaces preliminary results and preserves the first final result.
    let record = |result: &str, is_final: bool| {
        Event::ProviderToolRecord(ProviderToolRecord {
            call_id: "ws_1".into(),
            name: "perplexity_search".into(),
            input_json: "{}".into(),
            result_json: Some(result.into()),
            is_final,
        })
    };
    let (report, _, _) = run(
        Script::events(vec![
            record(r#"{"n":1}"#, false),
            record(r#"{"n":2}"#, false),
            record(r#"{"n":3}"#, true),
            record(r#"{"n":4}"#, true),
            record(r#"{"n":5}"#, false),
            finish(FinishReason::ToolCalls),
        ]),
        &with_provider_tool(),
    );
    let c = completion(&report);
    assert_eq!(c.provider_records.len(), 1);
    assert_eq!(
        c.provider_records[0].result_json.as_deref(),
        Some(r#"{"n":3}"#)
    );
    assert!(c.provider_records[0].is_final);
    assert_eq!(report.diagnostics.anomalies, 2);
}

// ---------------------------------------------------------------------------
// Usage and metadata

#[test]
fn usage_merges_field_by_field_and_absent_fields_stay_unknown() {
    let (report, _, _) = run(
        Script::events(vec![
            Event::Usage(Usage {
                input_tokens: Some(10),
                ..Usage::default()
            }),
            Event::Usage(Usage {
                output_tokens: Some(0),
                ..Usage::default()
            }),
            finish(FinishReason::Stop),
        ]),
        &minimal(),
    );
    let usage = completion(&report).usage.clone().unwrap();
    assert_eq!(usage.input_tokens, Some(10));
    assert_eq!(usage.output_tokens, Some(0), "a confirmed zero");
    assert_eq!(usage.reasoning_tokens, None, "unknown, not zero");
    assert_eq!(usage.cost, None);
}

#[test]
fn metadata_keeps_the_first_value_and_counts_conflicts() {
    let meta = |model: &str| {
        Event::Metadata(Metadata {
            model_reported: Some(model.into()),
            generation_id: None,
            provider_timestamp_ms: None,
        })
    };
    let (report, _, _) = run(
        Script::events(vec![
            meta("a/m1"),
            meta("a/m1"),
            meta("b/m2"),
            finish(FinishReason::Stop),
        ]),
        &minimal(),
    );
    assert_eq!(
        completion(&report).metadata.model_reported.as_deref(),
        Some("a/m1")
    );
    assert_eq!(report.diagnostics.anomalies, 1);
}

// ---------------------------------------------------------------------------
// Delivery certainty and cancellation

#[test]
fn an_unsent_start_failure_stays_definitely_unsent() {
    let (report, _, _) = run(
        Script::start_failure(
            ErrorKind::CapabilityUnavailable,
            DeliveryCertainty::DefinitelyUnsent,
        ),
        &minimal(),
    );
    assert_eq!(
        failure(&report).delivery,
        DeliveryCertainty::DefinitelyUnsent
    );
    assert!(!failure(&report).delivery.may_have_executed());
}

#[test]
fn a_failure_after_dispatch_is_possibly_sent() {
    let script = Script {
        start: StartBehavior::Ok(DeliveryCertainty::PossiblySent),
        steps: vec![Step::Fail(CapabilityError {
            kind: ErrorKind::ProviderUnavailable,
            delivery: DeliveryCertainty::PossiblySent,
            retry_after: Some(Duration::from_secs(2)),
            status_hint: Some(503),
            detail: None,
        })],
        cancel: CancelStatus::Requested,
    };
    let (report, _, _) = run(script, &minimal());
    let f = failure(&report);
    assert_eq!(f.delivery, DeliveryCertainty::PossiblySent);
    assert_eq!(f.retry_after, Some(Duration::from_secs(2)));
    assert_eq!(f.status_hint, Some(503));
    assert!(f.delivery.may_have_executed());
}

#[test]
fn a_received_event_confirms_the_response() {
    let script = Script {
        start: StartBehavior::Ok(DeliveryCertainty::DefinitelyUnsent),
        steps: vec![Step::Batch {
            events: vec![delta("x")],
            delivery: DeliveryCertainty::DefinitelyUnsent,
            state: StreamState::Open,
        }],
        cancel: CancelStatus::Requested,
    };
    let (report, _, _) = run(script, &minimal());
    assert_eq!(
        failure(&report).delivery,
        DeliveryCertainty::ConfirmedResponse
    );
}

#[test]
fn delivery_certainty_never_regresses() {
    // A-13: a capability reporting less certainty later is ignored.
    let script = Script {
        start: StartBehavior::Ok(DeliveryCertainty::PossiblySent),
        steps: vec![
            Step::Pending,
            Step::Fail(cap_error(
                ErrorKind::Transport,
                DeliveryCertainty::DefinitelyUnsent,
            )),
        ],
        cancel: CancelStatus::Requested,
    };
    let (report, _, _) = run(script, &minimal());
    assert_eq!(failure(&report).delivery, DeliveryCertainty::PossiblySent);
    assert_eq!(report.diagnostics.delivery, DeliveryCertainty::PossiblySent);
}

#[test]
fn cancellation_before_start_sends_nothing() {
    // C1; oracle: direct gateway cancellation before admission opens no connection.
    let host = Recorder {
        cancel_now: true,
        ..Recorder::default()
    };
    let (report, fake, _) = run_with(Script::text(&["x"]), &minimal(), &Limits::DEFAULT, host);
    let c = cancellation(&report);
    assert_eq!(c.delivery, DeliveryCertainty::DefinitelyUnsent);
    assert_eq!(c.remote, RemoteCancellation::NotStarted);
    assert_eq!(fake.observations.starts, 0);
    assert_eq!(
        report.diagnostics.terminal,
        TerminalCause::CancelledBeforeStart
    );
}

#[test]
fn cancellation_during_the_stream_reports_observed_delivery_and_partial_output() {
    // C2/C3: cancellation is checked between polls; the outcome keeps what was seen.
    let script = Script {
        start: StartBehavior::Ok(DeliveryCertainty::PossiblySent),
        steps: vec![
            batch(vec![delta("partial")], StreamState::Open),
            batch(
                vec![delta(" more"), finish(FinishReason::Stop)],
                StreamState::Ended,
            ),
        ],
        cancel: CancelStatus::Requested,
    };
    let host = Recorder {
        cancel_after_events: Some(1),
        ..Recorder::default()
    };
    let (report, fake, host) = run_with(script, &minimal(), &Limits::DEFAULT, host);
    let c = cancellation(&report);
    assert_eq!(c.delivery, DeliveryCertainty::ConfirmedResponse);
    assert_eq!(c.remote, RemoteCancellation::Unconfirmed);
    assert_eq!(c.partial.as_ref().unwrap().content, "partial");
    assert!(c.delivery.may_have_executed());
    assert_eq!(fake.observations.polls, 1);
    assert_eq!(fake.observations.cancels, 1);
    assert_eq!(fake.observations.closes, 1);
    assert_eq!(host.events.len(), 1, "nothing forwarded after cancellation");
}

#[test]
fn confirmed_remote_cancellation_still_may_have_executed() {
    // Invariant 4: cancellation never implies non-execution without evidence.
    let script = Script {
        start: StartBehavior::Ok(DeliveryCertainty::PossiblySent),
        steps: vec![Step::Stall],
        cancel: CancelStatus::Confirmed,
    };
    let host = Recorder {
        idle_stops_after: Some(3),
        ..Recorder::default()
    };
    let (report, fake, host) = run_with(script, &minimal(), &Limits::DEFAULT, host);
    let c = cancellation(&report);
    assert_eq!(c.remote, RemoteCancellation::Confirmed);
    assert_eq!(c.delivery, DeliveryCertainty::PossiblySent);
    assert!(c.delivery.may_have_executed());
    assert_eq!(host.idles, 3);
    assert_eq!(fake.observations.polls, 3);
}

#[test]
fn observer_abandonment_cancels_and_closes_once() {
    // C5 / BM-09.
    let host = Recorder {
        stop_after_events: Some(1),
        ..Recorder::default()
    };
    let (report, fake, host) = run_with(
        Script::text(&["a", "b", "c"]),
        &minimal(),
        &Limits::DEFAULT,
        host,
    );
    assert_eq!(cancellation(&report).partial.as_ref().unwrap().content, "a");
    assert_eq!(report.diagnostics.terminal, TerminalCause::Abandoned);
    assert_eq!(host.events.len(), 1);
    assert_eq!(fake.observations.cancels, 1);
    assert_eq!(fake.observations.closes, 1);
}

#[test]
fn repeated_cancellation_requests_cancel_once_and_cancel_after_finish_is_ignored() {
    // C4.
    let host = Recorder {
        cancel_after_events: Some(0),
        ..Recorder::default()
    };
    let script = Script {
        start: StartBehavior::Ok(DeliveryCertainty::PossiblySent),
        steps: vec![Step::Pending],
        cancel: CancelStatus::Requested,
    };
    let mut host = host;
    host.cancel_now = false;
    host.cancel_after_events = None;
    // Cancel is requested on every check once set; the capability is cancelled once.
    let mut fake = FakeCapability::new(script);
    struct Always;
    impl Host for Always {
        fn cancel_requested(&mut self) -> bool {
            true
        }
    }
    let mut always = Always;
    let _ = execute(&mut fake, &minimal(), &Limits::DEFAULT, &mut always);
    assert_eq!(
        fake.observations.cancels, 0,
        "cancelled before start: nothing to cancel"
    );

    struct AfterStart(u32);
    impl Host for AfterStart {
        fn cancel_requested(&mut self) -> bool {
            self.0 += 1;
            self.0 > 1
        }
    }
    let mut fake = FakeCapability::new(Script {
        start: StartBehavior::Ok(DeliveryCertainty::PossiblySent),
        steps: vec![Step::Stall],
        cancel: CancelStatus::Requested,
    });
    let report = execute(&mut fake, &minimal(), &Limits::DEFAULT, &mut AfterStart(0));
    assert!(matches!(report.outcome, Outcome::Cancelled(_)));
    assert_eq!(fake.observations.cancels, 1);
    assert_eq!(fake.observations.closes, 1);

    // A finish decides the outcome; a cancel request arriving with it changes nothing.
    let host = Recorder {
        cancel_after_events: Some(1),
        ..Recorder::default()
    };
    let (report, fake, _) = run_with(
        Script::events(vec![finish(FinishReason::Stop)]),
        &minimal(),
        &Limits::DEFAULT,
        host,
    );
    assert!(matches!(report.outcome, Outcome::Completed(_)));
    assert_eq!(fake.observations.cancels, 0);
}

#[test]
fn a_stalled_capability_is_bounded_by_the_poll_limit() {
    let limits = Limits {
        max_polls_per_attempt: 5,
        ..Limits::DEFAULT
    };
    let script = Script {
        start: StartBehavior::Ok(DeliveryCertainty::PossiblySent),
        steps: vec![Step::Stall],
        cancel: CancelStatus::Requested,
    };
    let (report, fake, host) = run_with(script, &minimal(), &limits, Recorder::default());
    assert_eq!(failure(&report).kind, ErrorKind::ResourceLimit);
    assert_eq!(report.diagnostics.terminal, TerminalCause::TooManyPolls);
    assert_eq!(fake.observations.polls, 5);
    assert_eq!(host.idles, 5);
    assert_eq!(fake.observations.cancels, 1);
    assert_eq!(fake.observations.closes, 1);
}

#[test]
fn a_capability_reported_timeout_is_a_failure_with_its_delivery() {
    // Budget enforcement belongs to the capability (it owns the clock).
    let mut request = minimal();
    request.budget = Some(AttemptBudget(Duration::from_millis(10)));
    let script = Script {
        start: StartBehavior::Ok(DeliveryCertainty::PossiblySent),
        steps: vec![Step::Fail(cap_error(
            ErrorKind::Timeout,
            DeliveryCertainty::PossiblySent,
        ))],
        cancel: CancelStatus::Requested,
    };
    let (report, fake, _) = run(script, &request);
    assert_eq!(failure(&report).kind, ErrorKind::Timeout);
    assert_eq!(failure(&report).delivery, DeliveryCertainty::PossiblySent);
    assert_eq!(fake.observations.budget, request.budget);
}

// ---------------------------------------------------------------------------
// Bounds

#[test]
fn oversized_events_are_resource_limits() {
    // A-8; oracle: SseEventReader rejects an over-limit event explicitly.
    let limits = Limits {
        max_event_bytes: 8,
        ..Limits::DEFAULT
    };
    for event in [
        delta("123456789"),
        Event::ReasoningDelta("123456789".into()),
        tool_call(
            &"c".repeat(Limits::DEFAULT.max_tool_call_id_bytes + 1),
            "t",
            "{}",
        ),
    ] {
        let (report, fake, _) = run_with(
            Script::events(vec![event, finish(FinishReason::Stop)]),
            &minimal(),
            &limits,
            Recorder::default(),
        );
        assert_eq!(failure(&report).kind, ErrorKind::ResourceLimit);
        assert_eq!(
            report.diagnostics.terminal,
            TerminalCause::Violation(Violation::OversizedEvent)
        );
        assert_eq!(fake.observations.cancels, 1);
        assert_eq!(fake.observations.closes, 1);
    }
}

#[test]
fn empty_event_identifiers_are_protocol_violations() {
    let (report, _, _) = run(
        Script::events(vec![Event::ToolInputEnd { id: String::new() }]),
        &minimal(),
    );
    assert_eq!(failure(&report).kind, ErrorKind::ProtocolViolation);
    assert_eq!(
        report.diagnostics.terminal,
        TerminalCause::Violation(Violation::EmptyEventField)
    );
}

#[test]
fn a_batch_larger_than_requested_is_a_protocol_violation() {
    let limits = Limits {
        max_events_per_poll: 2,
        ..Limits::DEFAULT
    };
    let (report, fake, host) = run_with(
        Script::text(&["a", "b", "c"]),
        &minimal(),
        &limits,
        Recorder::default(),
    );
    assert_eq!(failure(&report).kind, ErrorKind::ProtocolViolation);
    assert_eq!(report.diagnostics.terminal, TerminalCause::BatchTooLarge);
    assert!(
        host.events.is_empty(),
        "nothing from an oversized batch is forwarded"
    );
    assert_eq!(fake.observations.cancels, 1);
}

#[test]
fn event_and_proposal_counts_are_bounded() {
    let limits = Limits {
        max_events_per_attempt: 3,
        ..Limits::DEFAULT
    };
    let (report, _, _) = run_with(
        Script::text(&["a", "b", "c"]),
        &minimal(),
        &limits,
        Recorder::default(),
    );
    assert_eq!(
        report.diagnostics.terminal,
        TerminalCause::Violation(Violation::TooManyEvents)
    );

    let limits = Limits {
        max_proposals: 2,
        ..Limits::DEFAULT
    };
    let events = (0..3)
        .map(|i| tool_call(&format!("c{i}"), "list_files", "{}"))
        .collect();
    let (report, _, _) = run_with(
        Script::events(events),
        &with_provider_tool(),
        &limits,
        Recorder::default(),
    );
    assert_eq!(
        report.diagnostics.terminal,
        TerminalCause::Violation(Violation::TooManyProposals)
    );

    let events = (0..3)
        .map(|i| Event::ToolInputStart {
            id: format!("c{i}"),
            name: "list_files".into(),
        })
        .collect();
    let (report, _, _) = run_with(
        Script::events(events),
        &with_provider_tool(),
        &limits,
        Recorder::default(),
    );
    assert_eq!(
        report.diagnostics.terminal,
        TerminalCause::Violation(Violation::TooManyStreamedInputs)
    );
}

#[test]
fn accumulation_is_bounded_while_deltas_are_forwarded_in_full() {
    // Oracle: content_capture_limit truncates accumulation; callbacks receive full deltas.
    let limits = Limits {
        max_completion_bytes: 5,
        ..Limits::DEFAULT
    };
    let (report, _, host) = run_with(
        Script::text(&["abc", "déf", "ghi"]),
        &minimal(),
        &limits,
        Recorder::default(),
    );
    let c = completion(&report);
    assert_eq!(c.content, "abcd", "cut at a char boundary within 5 bytes");
    assert!(c.content_truncated);
    assert!(host.events.contains(&delta("déf")));
    assert!(host.events.contains(&delta("ghi")));
    assert_eq!(report.diagnostics.text_bytes, 10);
}

#[test]
fn an_overflowed_stream_never_lends_arguments() {
    let limits = Limits {
        max_tool_arguments_bytes: 8,
        ..Limits::DEFAULT
    };
    // Each delta fits the per-event bound; together they exceed the cap.
    let mut events = stream("c1", "list_files", r#"{"p":"#, false);
    events.push(Event::ToolInputDelta {
        id: "c1".into(),
        delta: r#"".."}"#.into(),
    });
    events.push(Event::ToolInputEnd { id: "c1".into() });
    events.push(tool_call("c1", "list_files", ""));
    events.push(finish(FinishReason::ToolCalls));
    let (report, _, _) = run_with(
        Script::events(events),
        &with_provider_tool(),
        &limits,
        Recorder::default(),
    );
    let c = completion(&report);
    assert_eq!(c.proposals[0].arguments_json, "{}");
    assert_eq!(report.proposal_checks[0].source, ArgumentSource::Missing);
}

// ---------------------------------------------------------------------------
// Diagnostics hygiene (invariant 8)

#[test]
fn diagnostics_never_contain_content() {
    let secret = "sk-SECRETSECRETSECRET0123";
    let mut request = with_provider_tool();
    request.messages = vec![
        text(Role::System, &format!("system {secret}")),
        message(
            Role::User,
            vec![
                Part::Text(format!("user {secret}")),
                Part::Image(ImageData {
                    media_type: ImageMediaType::Png,
                    bytes: [PNG, b"SECRETIMAGEBYTES"].concat(),
                }),
            ],
        ),
        message(
            Role::Assistant,
            vec![call("c0", "list_files", &format!(r#"{{"k":"{secret}"}}"#))],
        ),
        message(Role::Tool, vec![result("c0", "list_files", secret)]),
    ];
    request.correlation.affinity_key = Some("session-SECRETKEY".into());
    let script = Script {
        start: StartBehavior::Ok(DeliveryCertainty::PossiblySent),
        steps: vec![
            batch(
                vec![
                    delta(secret),
                    tool_call("c1", "list_files", &format!(r#"{{"x":"{secret}"}}"#)),
                ],
                StreamState::Open,
            ),
            Step::Fail(CapabilityError {
                kind: ErrorKind::Unauthenticated,
                delivery: DeliveryCertainty::PossiblySent,
                retry_after: None,
                status_hint: Some(401),
                detail: Some(Detail::new(
                    &format!(
                        "bad key Bearer abcdefghijklmnopqrstu {secret} API_KEY=hunter2hunter2"
                    ),
                    &Limits::DEFAULT,
                )),
            }),
        ],
        cancel: CancelStatus::Requested,
    };
    let (report, _, _) = run(script, &request);
    let diagnostics = format!("{:?}", report.diagnostics);
    for needle in [
        "SECRET",
        "hunter2",
        "abcdefghijklmnopqrstu",
        "list_files",
        "user ",
        "system ",
    ] {
        assert!(!diagnostics.contains(needle), "{needle} in {diagnostics}");
    }
    let shape = report.diagnostics.request_shape.unwrap();
    assert_eq!(shape.messages_by_role, [1, 1, 1, 1]);
    assert_eq!(shape.image_parts, 1);
    assert_eq!(shape.tools, 2);
    // The failure detail stays masked and bounded.
    let detail = failure(&report)
        .detail
        .as_ref()
        .unwrap()
        .as_str()
        .to_owned();
    assert!(!detail.contains("hunter2") && !detail.contains("abcdefghijklmnopqrstu"));
    assert!(detail.len() <= Limits::DEFAULT.max_detail_bytes);
}

#[test]
fn diagnostics_count_calls_and_bytes() {
    let (report, _, _) = run(Script::text(&["ab", "cd"]), &minimal());
    let d = &report.diagnostics;
    assert_eq!(d.terminal, TerminalCause::Finished);
    assert_eq!((d.starts, d.polls, d.cancels, d.closes), (1, 1, 0, 1));
    assert_eq!(d.events, 4);
    assert_eq!(d.forwarded, 4);
    assert_eq!(d.text_bytes, 4);
    assert_eq!(d.delivery, DeliveryCertainty::ConfirmedResponse);
    assert_eq!(d.request_id, Some(minimal().correlation.request_id));
}
