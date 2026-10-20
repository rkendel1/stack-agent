//! Usage accounting through the real binary: what the provider reports about each call reaches the
//! work's report field for field (input, output, reasoning and cache tokens, the served model), and
//! what it does not report stays unknown, never 0. The model is a scripted OpenAI-compatible event
//! stream; nothing here prices anything.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use gated::{Model, PASSED, Pax, Reply, command, project};
use serde_json::Value;

const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;

async fn work(model: &Model, tag: &str) -> Value {
    let pax = Pax::new(tag, PASSED, false);
    let dir = project(tag);
    let mut c = command(&dir, &model.url, &pax);
    c.args(["work", "Add `two`.", "--json", "--no-state"]);
    let out = tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    serde_json::from_str(stdout.lines().last().unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn every_reported_field_reaches_the_report_and_unreported_ones_stay_unknown() {
    let model = Model::start(vec![
        // The first call reports everything an OpenAI-compatible provider can.
        Reply::StreamUsage {
            text: READ,
            model: "served-model-2026-10-01",
            usage: Some(
                r#"{"prompt_tokens":1200,"completion_tokens":300,"prompt_tokens_details":{"cached_tokens":1000},"completion_tokens_details":{"reasoning_tokens":120}}"#,
            ),
        },
        // The second reports nothing at all.
        Reply::StreamUsage {
            text: BLOCK,
            model: "served-model-2026-10-01",
            usage: None,
        },
    ])
    .await;
    let report = work(&model, "usage-fields").await;

    let calls = report["context"]["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    let first = &calls[0]["reported_usage"];
    assert_eq!(first["input_tokens"], 1200, "{first}");
    assert_eq!(first["output_tokens"], 300);
    assert_eq!(first["cache_read_tokens"], 1000);
    assert_eq!(first["reasoning_tokens"], 120);
    assert_eq!(
        first["cache_write_tokens"],
        Value::Null,
        "not reported, so unknown"
    );
    assert_eq!(first["cost"], Value::Null);
    assert_eq!(first["model"], "served-model-2026-10-01");
    // A call that reported no usage has unknown counts, not zeros; the model it was served by is
    // still what the provider said.
    let second = &calls[1]["reported_usage"];
    for field in [
        "input_tokens",
        "output_tokens",
        "reasoning_tokens",
        "cache_read_tokens",
    ] {
        assert_eq!(second[field], Value::Null, "{field}: {second}");
    }
    assert_eq!(second["model"], "served-model-2026-10-01");
    assert_eq!(calls[1]["reported_input_tokens"], Value::Null);

    let usage = &report["usage"];
    assert_eq!(usage["calls_reported"], 1, "{usage}");
    assert_eq!(usage["calls_unreported"], 1);
    assert_eq!(usage["input_tokens"], 1200);
    assert_eq!(usage["output_tokens"], 300);
    assert_eq!(usage["reasoning_tokens"], 120);
    assert_eq!(usage["cache_read_tokens"], 1000);
    assert_eq!(usage["cache_write_tokens"], Value::Null);
    assert_eq!(usage["models_reported"][0], "served-model-2026-10-01");
    assert!(usage["costs"].as_array().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_whose_provider_reports_no_usage_says_so() {
    let model = Model::start(vec![Reply::StreamUsage {
        text: BLOCK,
        model: "m",
        usage: None,
    }])
    .await;
    let report = work(&model, "usage-none").await;
    let usage = &report["usage"];
    assert_eq!(usage["calls_reported"], 0, "{usage}");
    assert_eq!(usage["calls_unreported"], 1);
    for field in [
        "input_tokens",
        "output_tokens",
        "reasoning_tokens",
        "cache_read_tokens",
    ] {
        assert_eq!(usage[field], Value::Null, "{field}: unknown, not 0");
    }
    assert_eq!(report["measurement"]["model_tokens"], Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reported_zero_is_reported_as_zero() {
    let model = Model::start(vec![Reply::StreamUsage {
        text: BLOCK,
        model: "m",
        usage: Some(r#"{"prompt_tokens":0,"completion_tokens":0}"#),
    }])
    .await;
    let usage = work(&model, "usage-zero").await["usage"].clone();
    assert_eq!(usage["calls_reported"], 1, "{usage}");
    assert_eq!(usage["input_tokens"], 0);
    assert_eq!(usage["output_tokens"], 0);
    assert_eq!(usage["reasoning_tokens"], Value::Null);
}
