//! Runs every executor fixture in tests/fixtures/executor/.
//!
//! Each fixture is a request, a capability script and an expected result, all as
//! contract JSON, and cites the oracle behavior it mirrors. Expected results live
//! in data so the same files can drive the Node and WASM adapters in R4 (BM-14).

use std::path::PathBuf;

use fx_core::testing::{FakeCapability, Script, StartBehavior, Step};
use fx_core::*;
use serde_json::{Value, json};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/executor")
}

/// `CamelCase` debug name (variant only) to `snake_case`.
fn snake(debug: &str) -> String {
    let name = debug.split(['(', ' ', '{']).next().unwrap_or(debug);
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}

fn event(doc: &Value) -> Event {
    decode_event(&bytes(&json!({"v": 1, "event": doc})), &Limits::DEFAULT).unwrap()
}

fn events(docs: &Value) -> Vec<Event> {
    docs.as_array().unwrap().iter().map(event).collect()
}

fn capability_error(doc: &Value) -> CapabilityError {
    decode_capability_error(&bytes(&json!({"v": 1, "error": doc})), &Limits::DEFAULT).unwrap()
}

fn delivery(doc: &Value) -> DeliveryCertainty {
    match doc.as_str().unwrap() {
        "definitely_unsent" => DeliveryCertainty::DefinitelyUnsent,
        "possibly_sent" => DeliveryCertainty::PossiblySent,
        "confirmed_response" => DeliveryCertainty::ConfirmedResponse,
        other => panic!("unknown delivery {other}"),
    }
}

fn script(doc: &Value) -> Script {
    let start = if let Some(error) = doc["start"].get("error") {
        StartBehavior::Fail(capability_error(error))
    } else {
        StartBehavior::Ok(delivery(&doc["start"]["delivery"]))
    };
    let steps = doc["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| {
            if let Some(error) = step.get("error") {
                Step::Fail(capability_error(error))
            } else {
                Step::Batch {
                    events: events(&step["events"]),
                    delivery: delivery(&step["delivery"]),
                    state: match step["state"].as_str().unwrap() {
                        "open" => StreamState::Open,
                        "ended" => StreamState::Ended,
                        other => panic!("unknown stream state {other}"),
                    },
                }
            }
        })
        .collect();
    Script {
        start,
        steps,
        cancel: CancelStatus::Requested,
    }
}

fn limits(doc: &Value) -> Limits {
    let mut limits = Limits::DEFAULT;
    if let Some(overrides) = doc.as_object() {
        for (key, value) in overrides {
            let value = value.as_u64().unwrap() as usize;
            match key.as_str() {
                "max_event_bytes" => limits.max_event_bytes = value,
                "max_completion_bytes" => limits.max_completion_bytes = value,
                "max_tool_arguments_bytes" => limits.max_tool_arguments_bytes = value,
                other => panic!("unsupported limit override {other}"),
            }
        }
    }
    limits
}

#[derive(Default)]
struct FixtureHost {
    cancel_before_start: bool,
    cancel_after_events: Option<usize>,
    events: Vec<Event>,
}

impl Host for FixtureHost {
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

/// Runs one fixture and returns the list of mismatches.
fn run_case(case: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    let mut check = |what: &str, actual: String, expected: String| {
        if actual != expected {
            problems.push(format!("{what}: expected {expected}, got {actual}"));
        }
    };

    let limits = limits(&case["limits"]);
    let mut host = FixtureHost {
        cancel_before_start: case["host"]["cancel_before_start"] == json!(true),
        cancel_after_events: case["host"]["cancel_after_events"]
            .as_u64()
            .map(|n| n as usize),
        events: Vec::new(),
    };
    let mut fake = FakeCapability::new(script(&case["script"]));
    let report = execute_document(&mut fake, &bytes(&case["request"]), &limits, &mut host);
    let expect = &case["expect"];

    let expected_outcome = decode_outcome(
        &bytes(&json!({"v": 1, "outcome": expect["outcome"]})),
        &Limits::DEFAULT,
    )
    .unwrap();
    check(
        "outcome",
        format!("{:?}", report.outcome),
        format!("{expected_outcome:?}"),
    );
    check(
        "events",
        format!("{:?}", host.events),
        format!("{:?}", events(&expect["events"])),
    );
    check(
        "disposition",
        report
            .disposition
            .map_or("null".to_owned(), |d| snake(&format!("{d:?}"))),
        expect["disposition"].as_str().unwrap_or("null").to_owned(),
    );
    let checks: Vec<Value> = report
        .proposal_checks
        .iter()
        .map(|c| {
            json!({
                "identity": snake(&format!("{:?}", c.identity)),
                "integrity": snake(&format!("{:?}", c.integrity)),
                "source": snake(&format!("{:?}", c.source)),
            })
        })
        .collect();
    check(
        "checks",
        Value::Array(checks).to_string(),
        expect["checks"].to_string(),
    );
    let admission = match admit_proposals(&report.proposal_checks) {
        Ok(()) => Value::Null,
        Err(e) => {
            let reason = match e.reason {
                AdmissionRejection::MissingArguments => "missing_arguments".to_owned(),
                AdmissionRejection::Identity(identity) => snake(&format!("{identity:?}")),
            };
            json!({"index": e.index, "reason": reason})
        }
    };
    check(
        "admission",
        admission.to_string(),
        expect["admission"].to_string(),
    );

    let d = &report.diagnostics;
    let diagnostics = json!({
        "terminal": snake(&format!("{:?}", d.terminal)),
        "starts": d.starts,
        "polls": d.polls,
        "cancels": d.cancels,
        "closes": d.closes,
    });
    check(
        "diagnostics",
        diagnostics.to_string(),
        expect["diagnostics"].to_string(),
    );
    // The fake agrees with the executor's own accounting.
    let o = &fake.observations;
    check(
        "fake observations",
        format!(
            "{} {} {} {} {}",
            o.starts, o.polls, o.cancels, o.closes, o.misuse
        ),
        format!("{} {} {} {} 0", d.starts, d.polls, d.cancels, d.closes),
    );
    problems
}

#[test]
fn every_executor_fixture_produces_its_expected_result() {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    assert!(
        paths.len() >= 20,
        "executor fixture corpus unexpectedly small"
    );
    let mut failures = Vec::new();
    for path in &paths {
        let case: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let name = case["name"].as_str().unwrap();
        assert_eq!(
            path.file_stem().unwrap().to_str().unwrap(),
            name,
            "fixture name must match its file"
        );
        for key in ["behavior", "oracle"] {
            assert!(
                case[key].as_str().is_some_and(|s| !s.is_empty()),
                "{name}: every fixture cites its {key}"
            );
        }
        for problem in run_case(&case) {
            failures.push(format!("{name}: {problem}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn executor_fixtures_are_deterministic() {
    let path = fixtures_dir().join("observed_tool_round_trip_proposal.json");
    let case: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(run_case(&case).is_empty());
    assert!(run_case(&case).is_empty());
}
