//! The resumable executor: `Execution::step` must behave exactly like `execute`.

mod common;

use common::*;
use fx_core::testing::{FakeCapability, Script, StartBehavior, Step as Scripted};
use fx_core::*;
use serde_json::Value;

#[derive(Default)]
struct Recorder {
    events: Vec<Event>,
    cancel_now: bool,
    cancel_after_events: Option<usize>,
}

impl Host for Recorder {
    fn cancel_requested(&mut self) -> bool {
        self.cancel_now
            || self
                .cancel_after_events
                .is_some_and(|n| self.events.len() >= n)
    }
    fn on_event(&mut self, event: &Event) -> Flow {
        self.events.push(event.clone());
        Flow::Continue
    }
}

fn delta(text: &str) -> Event {
    Event::TextDelta(text.to_owned())
}

fn finish() -> Event {
    Event::Finish {
        reason: FinishReason::Stop,
    }
}

fn batch(events: Vec<Event>, state: StreamState) -> Scripted {
    Scripted::Batch {
        events,
        delivery: DeliveryCertainty::ConfirmedResponse,
        state,
    }
}

fn script(steps: Vec<Scripted>) -> Script {
    Script {
        start: StartBehavior::Ok(DeliveryCertainty::PossiblySent),
        steps,
        cancel: CancelStatus::Requested,
    }
}

/// Drives an `Execution` by hand, yielding on every `Idle` the way a host with its own
/// event loop would, and returns the report with the call counts.
fn stepped(
    script: Script,
    request: &Request,
    host: &mut Recorder,
    idles: &mut u32,
) -> (ExecutionReport, FakeCapability) {
    let mut capability = FakeCapability::new(script);
    let mut execution = Execution::new(request, &Limits::DEFAULT);
    loop {
        match execution.step(&mut capability, host) {
            Step::Progress => {}
            Step::Idle => *idles += 1,
            Step::Done(report) => {
                assert!(matches!(
                    execution.step(&mut capability, host),
                    Step::Finished
                ));
                return (*report, capability);
            }
            Step::Finished => panic!("finished before a report"),
        }
    }
}

fn scenarios() -> Vec<(&'static str, Script)> {
    vec![
        ("text", Script::text(&["a", "b", "c"])),
        (
            "pending then text",
            script(vec![
                Scripted::Pending,
                Scripted::Pending,
                batch(vec![delta("x"), finish()], StreamState::Ended),
            ]),
        ),
        (
            "ends without finish",
            script(vec![batch(vec![delta("par")], StreamState::Ended)]),
        ),
        (
            "poll failure",
            script(vec![
                batch(vec![delta("so far")], StreamState::Open),
                Scripted::Fail(CapabilityError {
                    kind: ErrorKind::ProviderUnavailable,
                    delivery: DeliveryCertainty::ConfirmedResponse,
                    retry_after: None,
                    status_hint: Some(503),
                    detail: None,
                }),
            ]),
        ),
        (
            "start failure",
            Script::start_failure(ErrorKind::RateLimited, DeliveryCertainty::PossiblySent),
        ),
        ("stall hits the poll limit", script(vec![Scripted::Stall])),
    ]
}

#[test]
fn stepping_matches_execute_for_every_scenario() {
    let request = minimal();
    for (name, scenario) in scenarios() {
        let limits = Limits {
            max_polls_per_attempt: 6,
            ..Limits::DEFAULT
        };
        let mut expected_host = Recorder::default();
        let mut expected_cap = FakeCapability::new(scenario.clone());
        let expected = execute(&mut expected_cap, &request, &limits, &mut expected_host);

        let mut host = Recorder::default();
        let mut capability = FakeCapability::new(scenario);
        let mut execution = Execution::new(&request, &limits);
        let actual = loop {
            match execution.step(&mut capability, &mut host) {
                Step::Progress | Step::Idle => {}
                Step::Done(report) => break *report,
                Step::Finished => panic!("{name}: finished early"),
            }
        };
        assert_eq!(actual, expected, "{name}");
        assert_eq!(host.events, expected_host.events, "{name}");
        assert_eq!(capability.observations, expected_cap.observations, "{name}");
    }
}

#[test]
fn an_idle_step_returns_control_without_touching_the_capability_again() {
    let request = minimal();
    let mut idles = 0;
    let script = script(vec![
        Scripted::Pending,
        Scripted::Pending,
        Scripted::Pending,
        batch(vec![delta("late"), finish()], StreamState::Ended),
    ]);
    let (report, capability) = stepped(script, &request, &mut Recorder::default(), &mut idles);
    assert_eq!(idles, 3);
    assert!(matches!(report.outcome, Outcome::Completed(_)));
    let o = &capability.observations;
    assert_eq!((o.starts, o.polls, o.cancels, o.closes), (1, 4, 0, 1));
}

#[test]
fn cancelling_at_an_idle_step_cancels_the_capability_once_and_keeps_delivery() {
    let request = minimal();
    let mut capability = FakeCapability::new(script(vec![
        batch(vec![delta("first")], StreamState::Open),
        Scripted::Stall,
    ]));
    let mut host = Recorder::default();
    let mut execution = Execution::new(&request, &Limits::DEFAULT);
    let mut idle_seen = false;
    let report = loop {
        match execution.step(&mut capability, &mut host) {
            Step::Progress => {}
            Step::Idle => {
                idle_seen = true;
                if let Step::Done(report) = execution.cancel(&mut capability) {
                    break *report;
                }
            }
            Step::Done(report) => break *report,
            Step::Finished => unreachable!(),
        }
    };
    assert!(idle_seen);
    match report.outcome {
        Outcome::Cancelled(c) => {
            assert_eq!(c.delivery, DeliveryCertainty::ConfirmedResponse);
            assert_eq!(c.remote, RemoteCancellation::Unconfirmed);
            assert_eq!(c.partial.map(|p| p.content), Some("first".to_owned()));
        }
        other => panic!("{other:?}"),
    }
    let o = &capability.observations;
    assert_eq!((o.cancels, o.closes), (1, 1));
    assert!(matches!(execution.cancel(&mut capability), Step::Finished));
    assert_eq!(
        capability.observations.cancels, 1,
        "a second cancel does nothing"
    );
}

#[test]
fn cancelling_before_the_first_step_never_starts_the_capability() {
    let request = minimal();
    let mut capability = FakeCapability::new(Script::text(&["x"]));
    let mut execution: Execution<FakeCapability> = Execution::new(&request, &Limits::DEFAULT);
    match execution.cancel(&mut capability) {
        Step::Done(report) => match report.outcome {
            Outcome::Cancelled(c) => {
                assert_eq!(c.delivery, DeliveryCertainty::DefinitelyUnsent);
                assert_eq!(c.remote, RemoteCancellation::NotStarted);
            }
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    }
    assert_eq!(capability.observations.starts, 0);
    assert!(matches!(
        execution.step(&mut capability, &mut Recorder::default()),
        Step::Finished
    ));
    assert_eq!(
        capability.observations.starts, 0,
        "stepping after the end starts nothing"
    );
}

#[test]
fn an_invalid_request_ends_on_the_first_step_without_starting() {
    let mut request = minimal();
    request.model = ModelRef(String::new());
    let mut capability = FakeCapability::new(Script::text(&["x"]));
    let mut execution = Execution::new(&request, &Limits::DEFAULT);
    let Step::Done(report) = execution.step(&mut capability, &mut Recorder::default()) else {
        panic!("expected the report on the first step");
    };
    assert!(
        matches!(report.outcome, Outcome::Failed(ref f) if f.kind == ErrorKind::InvalidRequest)
    );
    assert_eq!(capability.observations.starts, 0);
}

#[test]
fn a_host_cancel_between_steps_is_honored_before_the_next_poll() {
    let request = minimal();
    let mut host = Recorder {
        cancel_after_events: Some(1),
        ..Recorder::default()
    };
    let script = script(vec![
        batch(vec![delta("one")], StreamState::Open),
        batch(vec![delta("two"), finish()], StreamState::Ended),
    ]);
    let mut idles = 0;
    let (report, capability) = stepped(script, &request, &mut host, &mut idles);
    assert!(matches!(report.outcome, Outcome::Cancelled(_)));
    assert_eq!(capability.observations.polls, 1);
    assert!(!host.events.contains(&delta("two")));
}

#[test]
fn no_event_is_forwarded_after_the_report() {
    let request = minimal();
    let mut capability = FakeCapability::new(Script::text(&["a"]));
    let mut host = Recorder::default();
    let mut execution = Execution::new(&request, &Limits::DEFAULT);
    while !matches!(execution.step(&mut capability, &mut host), Step::Done(_)) {}
    let forwarded = host.events.len();
    for _ in 0..3 {
        assert!(matches!(
            execution.step(&mut capability, &mut host),
            Step::Finished
        ));
    }
    assert_eq!(host.events.len(), forwarded);
    assert_eq!(capability.observations.polls, 1);
}

// ---------------------------------------------------------------------------
// The report's JSON encoding

fn report_json(script: Script, request: &Request) -> Value {
    let mut capability = FakeCapability::new(script);
    let report = execute(&mut capability, request, &Limits::DEFAULT, &mut NoHost);
    serde_json::from_slice(&encode_report(&report)).expect("valid JSON")
}

#[test]
fn the_report_encodes_the_outcome_checks_disposition_and_diagnostics() {
    let mut request = minimal();
    request.tools = vec![tool("list_files")];
    let events = vec![
        Event::ToolCall(ToolCallProposal {
            id: "c1".into(),
            name: "list_files".into(),
            arguments_json: "{".into(),
        }),
        Event::Finish {
            reason: FinishReason::ToolCalls,
        },
    ];
    let doc = report_json(Script::events(events), &request);
    assert_eq!(doc["v"], 1);
    let report = &doc["report"];
    assert_eq!(report["outcome"]["type"], "completed");
    assert_eq!(report["outcome"]["proposals"][0]["arguments_json"], "{}");
    assert_eq!(report["disposition"], "tool_proposals");
    assert_eq!(report["proposal_checks"][0]["integrity"], "replaced");
    assert_eq!(report["proposal_checks"][0]["source"], "final");
    let d = &report["diagnostics"];
    assert_eq!(d["terminal"]["type"], "finished");
    assert_eq!(d["delivery"], "confirmed_response");
    assert_eq!(
        (d["starts"].as_u64(), d["closes"].as_u64()),
        (Some(1), Some(1))
    );
    assert_eq!(d["request_id"], "07070707070707070707070707070707");
    assert_eq!(d["request_shape"]["tools"], 1);
}

#[test]
fn terminal_causes_carry_a_detail_only_where_they_have_one() {
    let mut request = minimal();
    request.model = ModelRef(String::new());
    let invalid = report_json(Script::text(&["x"]), &request);
    assert_eq!(
        invalid["report"]["diagnostics"]["terminal"]["type"],
        "invalid_request"
    );
    assert!(invalid["report"]["diagnostics"]["terminal"]["detail"].is_string());
    assert_eq!(invalid["report"]["disposition"], Value::Null);

    let violation = report_json(
        Script::events(vec![Event::TextDelta("0123456789".into())]),
        &minimal(),
    );
    let tiny = Limits {
        max_event_bytes: 8,
        ..Limits::DEFAULT
    };
    let mut capability =
        FakeCapability::new(Script::events(vec![Event::TextDelta("0123456789".into())]));
    let report = execute(&mut capability, &minimal(), &tiny, &mut NoHost);
    let doc: Value = serde_json::from_slice(&encode_report(&report)).unwrap();
    assert_eq!(
        doc["report"]["diagnostics"]["terminal"]["type"],
        "violation"
    );
    assert_eq!(
        doc["report"]["diagnostics"]["terminal"]["detail"],
        "oversized_event"
    );
    assert!(
        violation["report"]["diagnostics"]["terminal"]
            .get("detail")
            .is_none()
    );
}

#[test]
fn the_report_never_contains_request_or_output_content_beyond_the_outcome() {
    let secret = "sk-SECRETSECRETSECRET0123456789";
    let mut request = minimal();
    request.messages = vec![text(Role::User, &format!("prompt {secret}"))];
    request.correlation.affinity_key = Some(format!("session-{secret}"));
    let doc = report_json(Script::text(&["answer"]), &request);
    let diagnostics = doc["report"]["diagnostics"].to_string();
    assert!(!diagnostics.contains(secret) && !diagnostics.contains("prompt"));
    assert!(
        !doc["report"]["proposal_checks"]
            .to_string()
            .contains(secret)
    );
}

#[test]
fn the_outcome_inside_a_report_is_the_standalone_outcome_encoding() {
    let mut capability = FakeCapability::new(Script::text(&["hi"]));
    let report = execute(&mut capability, &minimal(), &Limits::DEFAULT, &mut NoHost);
    let standalone: Value = serde_json::from_slice(&encode_outcome(&report.outcome)).unwrap();
    let doc: Value = serde_json::from_slice(&encode_report(&report)).unwrap();
    assert_eq!(doc["report"]["outcome"], standalone["outcome"]);
}
