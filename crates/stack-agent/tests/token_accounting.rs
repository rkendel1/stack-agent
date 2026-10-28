//! Per-call token accounting and the prompt-cache switch, through the real binary with scripted
//! stand-ins (an OpenAI-compatible stream, an Anthropic Messages stream, a `pax`); and
//! `token-report` on a real historical report. No real model is called.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::sync::{Arc, Mutex};

use gated::{FAILED, Model, PASSED, Pax, Reply, command, project};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;
const GOAL: &str = "Add `two` GOAL-SENTINEL-7f3a.";

fn last_json(out: &std::process::Output) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    serde_json::from_str(stdout.lines().last().unwrap_or("null")).unwrap_or(Value::Null)
}

async fn work_openai(tag: &str, env: &[(&str, &str)]) -> (std::process::Output, Value) {
    let model = Model::start(vec![
        Reply::StreamUsage {
            text: READ,
            model: "served-model",
            usage: Some(
                r#"{"prompt_tokens":1200,"completion_tokens":30,"prompt_tokens_details":{"cached_tokens":1000}}"#,
            ),
        },
        Reply::StreamUsage {
            text: BLOCK,
            model: "served-model",
            usage: None,
        },
    ])
    .await;
    let pax = Pax::new(tag, PASSED, false);
    let dir = project(tag);
    let mut c = command(&dir, &model.url, &pax);
    c.args(["work", GOAL, "--json", "--no-state"]);
    for (k, v) in env {
        c.env(k, v);
    }
    let out = tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap();
    let json = last_json(&out);
    (out, json)
}

#[tokio::test(flavor = "multi_thread")]
async fn every_call_has_a_record_whose_cached_input_is_counted_once() {
    let (out, report) = work_openai(
        "ledger",
        &[("CHIP_RUN_LABELS", "course=t4,obstacle=transient")],
    )
    .await;
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let t = &report["token_usage"];
    assert_eq!(t["schema"], "stack-agent.token-usage.v1", "{report}");
    assert_eq!(t["accounting"]["input_semantics"], "cache_included");
    assert_eq!(
        t["run"]["labels"],
        json!({"course": "t4", "obstacle": "transient"})
    );
    let calls = t["calls"].as_array().unwrap();
    assert_eq!(
        calls.len(),
        report["context"]["calls"].as_array().unwrap().len()
    );
    assert_eq!(calls.len(), 2);

    let first = &calls[0];
    assert_eq!(first["call"], 1);
    assert_eq!(first["outcome"], "response");
    assert_eq!(first["decision"], "request project.read");
    assert_eq!(first["usage"]["source"], "provider");
    // OpenAI's prompt_tokens include the cached tokens: total input is 1200, not 2200.
    assert_eq!(first["usage"]["reported"]["input_tokens"], 1200);
    assert_eq!(first["usage"]["reported"]["cache_read_tokens"], 1000);
    assert_eq!(first["usage"]["total_input_tokens"], 1200);
    assert_eq!(first["usage"]["uncached_input_tokens"], 200);
    assert_eq!(
        first["usage"]["reported"]["cache_write_tokens"],
        Value::Null
    );
    assert_eq!(first["usage"]["total_tokens"], 1230);
    assert!(first["elapsed_ms"].as_f64().is_some());
    assert!(first["started_unix_ms"].as_u64().unwrap() > 0);
    assert_eq!(first["gap_before_ms"], Value::Null);
    // The request sizes are Chip's own measurement, the same as context.calls.
    assert_eq!(
        first["request"]["content_bytes"],
        report["context"]["calls"][0]["request_bytes"]
    );

    let second = &calls[1];
    assert_eq!(second["decision"], "block");
    assert_eq!(second["usage"]["source"], "unavailable");
    assert_eq!(second["usage"]["total_input_tokens"], Value::Null);
    assert!(second["gap_before_ms"].as_f64().is_some());
    // The instructions are resent unchanged; the observation is new.
    assert!(
        second["request"]["resent_from_previous_call"]["messages"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert!(second["request"]["new_content_bytes"].as_u64().unwrap() > 0);

    // Totals never treat the unreported call as 0.
    assert_eq!(t["totals"]["input_tokens"]["sum"], 1200);
    assert_eq!(t["totals"]["input_tokens"]["calls_unknown"], 1);
    assert_eq!(t["observations"]["observations"], 1);
    assert!(
        t["data_quality"]
            .to_string()
            .contains("1 of 2 calls have no provider-reported usage")
    );

    // Privacy: no goal text, file content, key or path in the section.
    let section = t.to_string();
    for secret in [
        "GOAL-SENTINEL",
        "pub fn one",
        "sk-gated-test-key",
        "src/lib.rs",
    ] {
        assert!(!section.contains(secret), "{secret} leaked: {section}");
    }

    // The report command explains the run from the ledger, without a model.
    let path = gated::unique("ledger-report").with_extension("json");
    std::fs::write(&path, report.to_string()).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_stack-agent"))
        .args(["token-report", path.to_str().unwrap(), "--json"])
        .env_remove("CHIP_MODEL")
        .env_remove("CHIP_ENDPOINT")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let a: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(a["source"], "token_usage");
    assert_eq!(a["totals"]["total_input_tokens"]["sum"], 1200);
    assert_eq!(a["per_call"]["series"][1]["decision"], "block");
    assert!(a["per_call"]["series"][0]["elapsed_ms"].as_f64().is_some());
}

#[tokio::test(flavor = "multi_thread")]
async fn with_diagnostics_off_the_report_is_what_it_was_and_bad_labels_are_refused() {
    let (out, off) = work_openai("ledger-off", &[("CHIP_TOKEN_DIAGNOSTICS", "off")]).await;
    assert_eq!(out.status.code(), Some(1));
    assert!(off.get("token_usage").is_none(), "{off}");
    let (_, on) = work_openai("ledger-on", &[]).await;
    // Every field the report had without the ledger is there with it, in the same shape.
    let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    let mut with: Vec<String> = keys(&on);
    with.retain(|k| k != "token_usage");
    assert_eq!(keys(&off), with);
    for stable in ["usage", "terminal_state", "executions_by_capability"] {
        assert_eq!(off[stable], on[stable], "{stable}");
    }
    assert_eq!(
        off["context"]["calls"].as_array().unwrap().len(),
        on["context"]["calls"].as_array().unwrap().len()
    );
    assert_eq!(off["generation"]["prompt_cache"]["value"], false);
    assert_eq!(off["generation"]["prompt_cache"]["source"], "provider");

    let (out, _) = work_openai("ledger-bad-label", &[("CHIP_RUN_LABELS", "Course=x")]).await;
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("CHIP_RUN_LABELS"));
    let (out, _) = work_openai("prompt-cache-openai", &[("CHIP_PROMPT_CACHE", "on")]).await;
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("CHIP_PROMPT_CACHE on"));
}

// ---- Anthropic -----------------------------------------------------------------------------

/// A Messages API stand-in: request `n` gets `script[n]` (the last once exhausted) as an event
/// stream with the given usage on `message_start`, and every body is recorded.
struct Anthropic {
    url: String,
    bodies: Arc<Mutex<Vec<Value>>>,
}

async fn anthropic(script: Vec<(&'static str, Value)>) -> Anthropic {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = bodies.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let (head_end, length) = loop {
                let n = stream.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break (None, 0);
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..i]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    break (Some(i + 4), length);
                }
            };
            let Some(start) = head_end else { continue };
            while buf.len() < start + length {
                let n = stream.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let body: Value =
                serde_json::from_slice(&buf[start..start + length]).unwrap_or(Value::Null);
            let n = {
                let mut b = seen.lock().unwrap();
                b.push(body);
                b.len() - 1
            };
            let (text, usage) = script[n.min(script.len() - 1)].clone();
            let events = [
                (
                    "message_start",
                    json!({"type":"message_start","message":{"id":"msg","model":"claude-scripted","usage":usage}}),
                ),
                (
                    "content_block_start",
                    json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                ),
                (
                    "content_block_delta",
                    json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
                ),
                (
                    "content_block_stop",
                    json!({"type":"content_block_stop","index":0}),
                ),
                (
                    "message_delta",
                    json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":9}}),
                ),
                ("message_stop", json!({"type":"message_stop"})),
            ];
            let payload: String = events
                .iter()
                .map(|(e, d)| format!("event: {e}\ndata: {d}\n\n"))
                .collect();
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                payload.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(payload.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
    });
    Anthropic { url, bodies }
}

async fn work_anthropic(tag: &str, args: &[&str]) -> (Value, Vec<Value>) {
    let server = anthropic(vec![
        (
            READ,
            json!({"input_tokens": 900, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 2100}),
        ),
        (
            BLOCK,
            json!({"input_tokens": 300, "cache_read_input_tokens": 2100, "cache_creation_input_tokens": 950}),
        ),
    ])
    .await;
    let pax = Pax::new(tag, PASSED, false);
    let dir = project(tag);
    let mut c = command(&dir, &server.url, &pax);
    c.env("CHIP_PROVIDER", "anthropic")
        .env("CHIP_MODEL", "claude-scripted")
        .args(["work", GOAL, "--json", "--no-state"])
        .args(args);
    let out = tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bodies = server.bodies.lock().unwrap().clone();
    (last_json(&out), bodies)
}

fn system_text(body: &Value) -> String {
    match &body["system"] {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .map(|b| b["text"].as_str().unwrap_or(""))
            .collect(),
        other => panic!("no system text: {other}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn anthropic_marks_the_resent_prefix_cacheable_and_reads_and_writes_are_accounted_apart() {
    let (report, bodies) = work_anthropic("anthropic-cache", &[]).await;
    assert_eq!(bodies.len(), 2);
    assert_eq!(report["generation"]["prompt_cache"]["value"], true);
    assert_eq!(report["generation"]["prompt_cache"]["source"], "default");
    // One breakpoint per request, on the last system block; the turn message is not marked.
    for body in &bodies {
        let blocks = body["system"].as_array().expect("system blocks");
        assert!(blocks.last().unwrap()["cache_control"]["type"] == "ephemeral");
        assert_eq!(body.to_string().matches("cache_control").count(), 1);
        assert!(!body["messages"].to_string().contains("cache_control"));
    }
    // The first request's system blocks are, byte for byte, the start of the second's.
    let (a, b) = (
        bodies[0]["system"].as_array().unwrap(),
        bodies[1]["system"].as_array().unwrap(),
    );
    assert_eq!(b.len(), a.len() + 1, "the observation is appended");
    for (i, block) in a.iter().enumerate() {
        assert_eq!(block["text"], b[i]["text"], "block {i}");
    }

    let calls = report["token_usage"]["calls"].as_array().unwrap();
    assert_eq!(
        report["token_usage"]["accounting"]["input_semantics"],
        "cache_separate"
    );
    let u = &calls[1]["usage"];
    assert_eq!(u["reported"]["input_tokens"], 300);
    assert_eq!(u["reported"]["cache_read_tokens"], 2100);
    assert_eq!(u["reported"]["cache_write_tokens"], 950);
    // Anthropic's input excludes both cache counts: total = 300 + 2100 + 950.
    assert_eq!(u["total_input_tokens"], 3350);
    assert_eq!(u["uncached_input_tokens"], 300);
    let totals = &report["token_usage"]["totals"];
    assert_eq!(totals["cache_write_tokens"]["sum"], 3050);
    assert_eq!(totals["total_input_tokens"]["sum"], 3000 + 3350);
    // The work report's own usage totals carry the cache-write count now too.
    assert_eq!(report["usage"]["cache_write_tokens"], 3050);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_prompt_cache_opt_out_sends_the_request_exactly_as_before() {
    // Stable ids, so the two runs' requests can be compared byte for byte.
    let (on, cached) = work_anthropic("anthropic-on", &["--stable-ids"]).await;
    let (off, plain) =
        work_anthropic("anthropic-off", &["--stable-ids", "--prompt-cache", "off"]).await;
    assert_eq!(off["generation"]["prompt_cache"]["value"], false);
    assert_eq!(
        off["generation"]["prompt_cache"]["source"],
        "flag:--prompt-cache"
    );
    assert_eq!(cached.len(), plain.len());
    for (c, p) in cached.iter().zip(&plain) {
        // Off: one joined string and no cache_control anywhere, as before this setting existed.
        assert!(p["system"].is_string(), "{p}");
        assert!(!p.to_string().contains("cache_control"));
        // On or off, the model reads the same system text and the same messages.
        assert_eq!(system_text(c), system_text(p));
        assert_eq!(c["messages"], p["messages"]);
        assert_eq!(c["max_tokens"], p["max_tokens"]);
    }
    // And the work did the same things either way.
    assert_eq!(
        on["executions_by_capability"],
        off["executions_by_capability"]
    );
    assert_eq!(on["terminal_state"], off["terminal_state"]);
}

/// Every context profile through the Anthropic mapping: system blocks (the instructions, then one
/// per observation), one cache breakpoint on the last system block, the turn last as the only user
/// message. Between two requests under Standard, Deep or Recovery the earlier request's system
/// blocks are the start of the later one's, except at the one request where a superseded read
/// leaves them; Light is the profile that moves the prefix.
#[tokio::test(flavor = "multi_thread")]
async fn every_context_profile_maps_to_a_well_formed_anthropic_request() {
    const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
    const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
    let usage = || json!({"input_tokens": 10, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0});
    for profile in ["auto", "light", "standard", "deep", "recovery", "replay"] {
        let server = anthropic(vec![
            (READ, usage()),
            (WRITE, usage()),
            (READ, usage()),
            (TEST, usage()),
            (TEST, usage()),
            (BLOCK, usage()),
        ])
        .await;
        let tag = format!("anthropic-profile-{profile}");
        let pax = Pax::new(&tag, FAILED, false);
        let dir = project(&tag);
        let mut c = command(&dir, &server.url, &pax);
        c.env("CHIP_PROVIDER", "anthropic")
            .env("CHIP_MODEL", "claude-scripted")
            .env_remove("CHIP_CONTEXT_PROFILE")
            .args(["work", GOAL, "--json", "--no-state", "--context-profile", profile]);
        let out = tokio::task::spawn_blocking(move || c.output().unwrap())
            .await
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{profile}: {}", String::from_utf8_lossy(&out.stderr));
        let report = last_json(&out);
        let bodies = server.bodies.lock().unwrap().clone();
        assert_eq!(bodies.len(), 6, "{profile}: the same trajectory");
        let calls = report["token_usage"]["calls"].as_array().unwrap();
        for (i, body) in bodies.iter().enumerate() {
            let blocks = body["system"].as_array().expect("system blocks");
            assert!(blocks[0]["text"].as_str().unwrap().contains(GOAL), "{profile} {i}: instructions first");
            assert_eq!(blocks.last().unwrap()["cache_control"]["type"], "ephemeral", "{profile} {i}");
            assert_eq!(body.to_string().matches("cache_control").count(), 1, "{profile} {i}");
            let messages = body["messages"].as_array().unwrap();
            assert_eq!(messages.len(), 1, "{profile} {i}: one user message");
            assert_eq!(messages[0]["role"], "user");
            for b in &blocks[1..] {
                assert!(b["text"].as_str().unwrap().starts_with("Observation:\n"), "{profile} {i}");
            }
            assert_eq!(
                blocks.len() - 1,
                calls[i]["context"]["observations_in_full"].as_u64().unwrap() as usize,
                "{profile} {i}: the observations in full are the system blocks after the instructions"
            );
        }
        if profile == "light" || profile == "replay" {
            continue;
        }
        for i in 1..bodies.len() {
            let (a, b) = (
                bodies[i - 1]["system"].as_array().unwrap(),
                bodies[i]["system"].as_array().unwrap(),
            );
            let newly_condensed = calls[i]["context"]["observations_condensed"].as_u64()
                > calls[i - 1]["context"]["observations_condensed"].as_u64();
            let prefix = a.iter().zip(b).all(|(x, y)| x["text"] == y["text"]) && b.len() >= a.len();
            assert_eq!(prefix, !newly_condensed, "{profile}: request {i} against {}", i - 1);
        }
    }
}

// ---- token-report on a historical report ---------------------------------------------------

fn token_report(args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_stack-agent"))
        .arg("token-report")
        .args(args)
        .env_remove("CHIP_MODEL")
        .env_remove("CHIP_ENDPOINT")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .output()
        .unwrap()
}

const HISTORICAL: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/token_report/opus-t4-historical.json"
);

#[test]
fn a_historical_report_is_explained_with_its_gaps_disclosed() {
    let out = token_report(&[HISTORICAL, "--json", "--label", "course=aoc-t4"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let a: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(a["schema"], "stack-agent.token-report.v1");
    assert_eq!(a["source"], "context.calls");
    assert_eq!(a["labels"]["course"], "aoc-t4");
    assert_eq!(a["totals"]["calls"], 42);
    assert_eq!(a["totals"]["executions"], 34);
    assert_eq!(a["totals"]["input_tokens"]["sum"], 2_837_169);
    assert_eq!(a["totals"]["output_tokens"]["sum"], 31_054);
    assert_eq!(a["totals"]["cache_read_tokens"]["sum"], 0);
    // Cache writes were not recorded by that build: unknown, and no total input is derived.
    assert_eq!(a["totals"]["cache_write_tokens"]["sum"], Value::Null);
    assert_eq!(a["totals"]["total_input_tokens"]["calls_known"], 0);
    assert_eq!(a["context_growth"]["first_call"]["input_tokens"], 2482);
    assert_eq!(a["context_growth"]["last_call"]["input_tokens"], 101_180);
    assert_eq!(a["context_growth"]["append_only_requests"], true);
    assert_eq!(a["per_call"]["largest_calls"][0]["call"], 42);
    assert_eq!(
        a["per_call"]["series"][41]["cumulative_input_tokens"],
        2_837_169
    );
    // Timings and decisions were not recorded per call: listed, not reconstructed.
    assert_eq!(a["per_call"]["series"][0]["elapsed_ms"], Value::Null);
    assert_eq!(a["per_call"]["series"][0]["decision"], Value::Null);
    let unavailable = a["data_quality"]["unavailable"].to_string();
    assert!(unavailable.contains("per-call timing"), "{unavailable}");
    assert!(unavailable.contains("tool output sizes"), "{unavailable}");
    // Ranked: caching first (nothing was read from cache), and the seven calls that produced no
    // new observation are named.
    assert_eq!(a["opportunities"][0]["id"], "provider-prompt-cache");
    let rejected = a["opportunities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"] == "rejected-replies")
        .unwrap();
    assert_eq!(
        rejected["evidence"]["calls_followed_by_no_new_observation"],
        json!([23, 28, 32, 33, 35, 38, 41])
    );
    // The human form says the same, without a model call.
    let out = token_report(&[HISTORICAL]);
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("input        2837169 (42 of 42 calls reported)"),
        "{text}"
    );
    assert!(text.contains("cache write  unknown"), "{text}");
}

#[test]
fn token_report_refuses_what_it_cannot_read() {
    assert_eq!(token_report(&[]).status.code(), Some(2));
    assert_eq!(
        token_report(&["/nonexistent/report.json"]).status.code(),
        Some(3)
    );
    assert_eq!(
        token_report(&[HISTORICAL, "--label", "bad label"])
            .status
            .code(),
        Some(2)
    );
    let path = gated::unique("not-json").with_extension("json");
    std::fs::write(&path, "not json").unwrap();
    assert_eq!(
        token_report(&[path.to_str().unwrap()]).status.code(),
        Some(3)
    );
}
