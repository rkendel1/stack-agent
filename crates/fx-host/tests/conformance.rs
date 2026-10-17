//! Runs every host-conformance fixture natively against `Session`.
//!
//! The fixtures in tests/fixtures/conformance/ are host transcripts: the commands a
//! host sends and the response subset each must produce. The Node host runs the same
//! files against the WebAssembly build, so the native and WASM paths are held to one
//! contract. When `FX_CONFORMANCE_OUT` is set, every response is written there so the
//! WASM run can be compared byte for byte (see host/node/test).

use std::path::PathBuf;

use fx_host::{Session, SupportFlags};
use serde_json::{Map, Value};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/conformance")
}

/// Whether `actual` satisfies `expected`: objects by key subset, arrays and scalars exactly.
/// `null` matches an absent key too. Keys starting with `$` are runner directives.
fn check(expected: &Value, actual: &Value, path: &str, errors: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Object(want), Value::Object(have)) => {
            for (key, value) in want {
                if key.starts_with('$') {
                    continue;
                }
                match have.get(key) {
                    Some(found) => check(value, found, &format!("{path}.{key}"), errors),
                    None if value.is_null() => {}
                    None => errors.push(format!("{path}.{key}: missing")),
                }
            }
        }
        (Value::Array(want), Value::Array(have)) => {
            if want.len() != have.len() {
                errors.push(format!(
                    "{path}: expected {} items, got {}",
                    want.len(),
                    have.len()
                ));
                return;
            }
            for (i, (w, h)) in want.iter().zip(have).enumerate() {
                check(w, h, &format!("{path}[{i}]"), errors);
            }
        }
        (w, h) if w == h => {}
        (w, h) => errors.push(format!("{path}: expected {w}, got {h}")),
    }
}

fn lookup<'a>(value: &'a Value, dotted: &str) -> &'a Value {
    dotted
        .split('.')
        .fold(value, |v, key| v.get(key).unwrap_or(&Value::Null))
}

/// Applies `$excludes` and `$excludes_all` directives.
fn check_exclusions(
    expected: &Value,
    raw: &str,
    actual: &Value,
    path: &str,
    errors: &mut Vec<String>,
) {
    if let Some(all) = expected.get("$excludes_all").and_then(Value::as_array) {
        for needle in all.iter().filter_map(Value::as_str) {
            if raw.contains(needle) {
                errors.push(format!("{path}: the response contains a forbidden value"));
            }
        }
    }
    if let Some(rule) = expected.get("$excludes") {
        let scope = lookup(actual, rule["at"].as_str().unwrap_or("")).to_string();
        for needle in rule["values"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if scope.contains(needle) {
                errors.push(format!("{path}: {} contains a forbidden value", rule["at"]));
            }
        }
    }
}

struct Run {
    transcript: Vec<Value>,
    errors: Vec<String>,
}

fn run_case(case: &Value) -> Run {
    let mut run = Run {
        transcript: Vec::new(),
        errors: Vec::new(),
    };
    let name = case["name"].as_str().unwrap_or("?");
    let mut session = Session::new(SupportFlags(case["support"].as_u64().unwrap_or(0) as u32));

    let request: Vec<u8> = match case["request_raw"].as_str() {
        Some(raw) => raw.as_bytes().to_vec(),
        None => serde_json::to_vec(&case["request"]).unwrap(),
    };
    let exchange = |label: String, response: Vec<u8>, expected: &Value, run: &mut Run| {
        let raw = String::from_utf8(response).expect("responses are UTF-8");
        let actual: Value = serde_json::from_str(&raw).expect("responses are JSON");
        let before = run.errors.len();
        check(
            expected,
            &actual,
            &format!("{name} {label}"),
            &mut run.errors,
        );
        check_exclusions(
            expected,
            &raw,
            &actual,
            &format!("{name} {label}"),
            &mut run.errors,
        );
        let _ = before;
        run.transcript.push(actual);
    };

    exchange(
        "begin".into(),
        session.begin(&request),
        &case["begin"],
        &mut run,
    );
    for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
        let command: Vec<u8> = match step["send_raw"].as_str() {
            Some(raw) => raw.as_bytes().to_vec(),
            None => {
                let mut object: Map<String, Value> = step["send"].as_object().unwrap().clone();
                object.insert("v".into(), 1.into());
                serde_json::to_vec(&Value::Object(object)).unwrap()
            }
        };
        exchange(
            format!("step {i}"),
            session.command(&command),
            &step["expect"],
            &mut run,
        );
    }
    run
}

fn load_cases() -> Vec<Value> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let case: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert_eq!(
                path.file_stem().unwrap().to_str().unwrap(),
                case["name"].as_str().unwrap(),
                "a fixture's name must match its file"
            );
            case
        })
        .collect()
}

#[test]
fn every_host_fixture_produces_its_expected_responses_natively() {
    let cases = load_cases();
    assert!(cases.len() >= 20, "fixture corpus unexpectedly small");
    let mut errors = Vec::new();
    let mut transcripts = Map::new();
    for case in &cases {
        if !case["targets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "native")
        {
            continue;
        }
        let run = run_case(case);
        errors.extend(run.errors);
        transcripts.insert(
            case["name"].as_str().unwrap().to_owned(),
            Value::Array(run.transcript),
        );
    }
    if let Ok(path) = std::env::var("FX_CONFORMANCE_OUT") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&Value::Object(transcripts)).unwrap(),
        )
        .unwrap();
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn every_fixture_declares_its_behavior_and_targets() {
    for case in load_cases() {
        let name = case["name"].as_str().unwrap();
        assert!(
            case["behavior"].as_str().is_some_and(|s| !s.is_empty()),
            "{name}: behavior"
        );
        let targets = case["targets"].as_array().unwrap();
        assert!(!targets.is_empty(), "{name}: targets");
        assert!(
            targets.iter().all(|t| t == "native" || t == "wasm"),
            "{name}: targets"
        );
    }
}

#[test]
fn sessions_are_independent() {
    let first = load_cases()
        .into_iter()
        .find(|c| c["name"] == "streaming_text_completes")
        .unwrap();
    let a = run_case(&first);
    let b = run_case(&first);
    assert_eq!(
        a.transcript, b.transcript,
        "two sessions of the same fixture agree exactly"
    );
    assert!(a.errors.is_empty(), "{:?}", a.errors);
}
