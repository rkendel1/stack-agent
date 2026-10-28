//! The published schemas describe what the binary really prints. A small checker (required keys,
//! types, `const` and `enum`) is applied to a real report and a real event stream, so a field that
//! drifts from `docs/schemas/*.json` fails here. Exit codes are pinned as stable.

#[path = "common/mod.rs"]
mod common;

use std::process::Command;
use std::time::Duration;

use serde_json::Value;

const REPORT_SCHEMA: &str = include_str!("../../../docs/schemas/work-report.v1.schema.json");
const EVENT_SCHEMA: &str = include_str!("../../../docs/schemas/work-event.v1.schema.json");

fn type_ok(value: &Value, ty: &Value) -> bool {
    let one = |t: &str| match t {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        _ => false,
    };
    match ty {
        Value::String(t) => one(t),
        Value::Array(ts) => ts.iter().filter_map(Value::as_str).any(one),
        _ => true,
    }
}

/// Returns what does not conform; empty when the value does.
fn check(value: &Value, schema: &Value, at: &str, out: &mut Vec<String>) {
    if let Some(ty) = schema.get("type") {
        if !type_ok(value, ty) {
            out.push(format!("{at}: {value} is not {ty}"));
            return;
        }
    }
    if let Some(c) = schema.get("const") {
        if value != c {
            out.push(format!("{at}: {value} is not {c}"));
        }
    }
    if let Some(Value::Array(options)) = schema.get("enum") {
        if !options.contains(value) {
            out.push(format!("{at}: {value} is not one of {options:?}"));
        }
    }
    if let (Some(Value::Array(required)), Some(object)) =
        (schema.get("required"), value.as_object())
    {
        for key in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(key) {
                out.push(format!("{at}: missing `{key}`"));
            }
        }
    }
    if let (Some(Value::Object(props)), Some(object)) =
        (schema.get("properties"), value.as_object())
    {
        for (key, sub) in props {
            if let Some(v) = object.get(key) {
                check(v, sub, &format!("{at}.{key}"), out);
            }
        }
    }
}

async fn run_with_events() -> (Value, Vec<Value>, Option<i32>) {
    let reply = serde_json::json!({
        "id": "r",
        "choices": [{"message": {"role": "assistant",
            "content": "{\"decision\":\"block\",\"reason\":\"nothing to do\"}"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 2, "completion_tokens": 3},
    })
    .to_string();
    let server = common::start(200, &reply, Duration::ZERO).await;
    let dir = std::env::temp_dir().join(format!("chip-contract-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
    let events = dir.join("events.jsonl");
    let (url, d, e) = (server.url.clone(), dir.clone(), events.clone());
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_stack-agent"))
            .args([
                "work",
                "Look.",
                "--kind",
                "inspect",
                "--json",
                "--no-state",
                "--max-turns",
                "2",
            ])
            .arg("--events")
            .arg(&e)
            .current_dir(d)
            .env_remove("PAX_BIN")
            .env("CHIP_PROVIDER", "openai-compatible")
            .env("CHIP_MODEL", "mock-model")
            .env("CHIP_ENDPOINT", url)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let report = serde_json::from_slice(&out.stdout).unwrap();
    let lines = std::fs::read_to_string(events)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    (report, lines, out.status.code())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_report_and_a_real_event_stream_conform_to_the_published_schemas() {
    let (report, events, code) = run_with_events().await;
    let mut problems = Vec::new();
    check(
        &report,
        &serde_json::from_str(REPORT_SCHEMA).unwrap(),
        "report",
        &mut problems,
    );
    let event_schema: Value = serde_json::from_str(EVENT_SCHEMA).unwrap();
    assert!(!events.is_empty());
    for (i, line) in events.iter().enumerate() {
        check(line, &event_schema, &format!("event[{i}]"), &mut problems);
    }
    assert!(problems.is_empty(), "{problems:#?}");
    // terminal_state, verified and exit_status are the runtime's, and agree with the process.
    assert_eq!(report["exit_status"].as_i64(), code.map(i64::from));
    assert_eq!(report["verified"], false);
}

#[test]
fn the_exit_codes_are_stable_and_documented() {
    use chip_cli::verify::{
        EXIT_NOT_VERIFIED, EXIT_RUNTIME_FAILURE, EXIT_UNAVAILABLE, EXIT_USAGE, EXIT_VERIFIED,
    };
    assert_eq!(
        [
            EXIT_VERIFIED,
            EXIT_NOT_VERIFIED,
            EXIT_USAGE,
            EXIT_UNAVAILABLE,
            EXIT_RUNTIME_FAILURE
        ],
        [0, 1, 2, 3, 4]
    );
    let doc = include_str!("../../../docs/product/report-contract.md");
    for line in [
        "| 0 | verified",
        "| 1 | not verified",
        "| 2 | usage",
        "| 3 | unavailable",
        "| 4 | runtime failure",
        "| 130 |",
    ] {
        assert!(doc.contains(line), "report-contract.md lost `{line}`");
    }
}
