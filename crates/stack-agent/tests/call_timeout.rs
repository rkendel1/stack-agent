//! The per-call provider timeout of `chip work`: what the operator configures is the deadline the
//! provider enforces, a call that outlasts it is the typed failure `timeout`, and nothing that
//! arrived before the deadline becomes a decision.
//!
//! The binary, as a user runs it, against mock OpenAI-compatible servers. Delays are whole seconds
//! with a wide margin either side of the deadline, so no test depends on timing at its edge.

#[path = "common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-timeout-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"timeout_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
    dir
}

fn pax_installed() -> bool {
    Command::new("pax")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

macro_rules! needs_pax {
    () => {
        if !pax_installed() {
            eprintln!("SKIPPED: pax is not installed");
            return;
        }
    };
}

fn completion(content: &str, finish_reason: &str) -> String {
    serde_json::json!({
        "id": "resp-timeout",
        "choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": finish_reason}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7},
    })
    .to_string()
}

const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 2 }\n"}}"#;

struct Ran {
    out: Output,
    json: serde_json::Value,
    elapsed: Duration,
    dir: PathBuf,
}

fn run(tag: &str, url: &str, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let dir = fixture(tag);
    let started = Instant::now();
    let mut c = Command::new(env!("CARGO_BIN_EXE_stack-agent"));
    c.args([
        "work",
        "Summarize what src/lib.rs does.",
        "--json",
        "--kind",
        "inspect",
        "--max-turns",
        "2",
    ])
    .args(args)
    .current_dir(&dir)
    .env_remove("PAX_BIN")
    .env_remove("CHIP_CALL_TIMEOUT_SECS")
    .env("CHIP_PROVIDER", "openai-compatible")
    .env("CHIP_MODEL", "mock-model")
    .env("CHIP_ENDPOINT", url)
    .env("CHIP_API_KEY", "sk-timeout-secret-never-printed");
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    Ran {
        out,
        json,
        elapsed: started.elapsed(),
        dir,
    }
}

async fn on_blocking(
    tag: &'static str,
    url: String,
    args: Vec<&'static str>,
    env: Vec<(&'static str, &'static str)>,
) -> Ran {
    tokio::task::spawn_blocking(move || run(tag, &url, &args, &env))
        .await
        .unwrap()
}

fn nothing_executed(r: &Ran) {
    let j = &r.json;
    assert_eq!(j["executions_by_capability"], serde_json::json!({}), "{j}");
    assert_eq!(j["capability_requests"], 0);
    assert_eq!(j["grounded"], false);
    assert_eq!(j["verified"], false);
    assert!(j["answer"].is_null());
    assert_eq!(j["audit"]["clean"], true, "{}", j["audit"]);
    assert_eq!(j["context"]["context_limit_rejections"], 0);
}

// ---- configuration -------------------------------------------------------------------------------

#[tokio::test]
async fn the_default_is_thirty_seconds_and_the_flag_and_variable_follow_the_usual_precedence() {
    needs_pax!();
    let server = common::start(200, &completion(READ, "stop"), Duration::ZERO).await;
    for (tag, args, env, value, source) in [
        ("default", vec![], vec![], 30, "default"),
        (
            "flag",
            vec!["--call-timeout-secs", "77"],
            vec![],
            77,
            "flag:--call-timeout-secs",
        ),
        (
            "var",
            vec![],
            vec![("CHIP_CALL_TIMEOUT_SECS", "55")],
            55,
            "environment:CHIP_CALL_TIMEOUT_SECS",
        ),
        (
            "both",
            vec!["--call-timeout-secs", "77"],
            vec![("CHIP_CALL_TIMEOUT_SECS", "55")],
            77,
            "flag:--call-timeout-secs",
        ),
    ] {
        let r = on_blocking(tag, server.url.clone(), args, env).await;
        let g = &r.json["generation"]["call_timeout_secs"];
        assert_eq!(g["value"], value, "{tag}");
        assert_eq!(g["source"], source, "{tag}");
        // The other settings are untouched by it.
        let gen_ = &r.json["generation"];
        assert_eq!(gen_["max_output_tokens"]["value"], 2048, "{tag}");
        assert_eq!(gen_["temperature"]["value"], 0.0, "{tag}");
        assert_eq!(gen_["structured_output"]["value"], "object", "{tag}");
        assert_eq!(r.json["model"], "mock-model");
    }
}

#[tokio::test]
async fn invalid_timeouts_are_refused_before_any_request_and_named() {
    needs_pax!();
    let server = common::start(200, &completion(READ, "stop"), Duration::ZERO).await;
    let cases: Vec<(Vec<&'static str>, Vec<(&'static str, &'static str)>, &str)> = vec![
        (
            vec!["--call-timeout-secs", "0"],
            vec![],
            "--call-timeout-secs",
        ),
        (
            vec!["--call-timeout-secs", "-3"],
            vec![],
            "--call-timeout-secs",
        ),
        (
            vec!["--call-timeout-secs", "1.5"],
            vec![],
            "--call-timeout-secs",
        ),
        (
            vec!["--call-timeout-secs", "30s"],
            vec![],
            "--call-timeout-secs",
        ),
        (
            vec!["--call-timeout-secs", "86401"],
            vec![],
            "--call-timeout-secs",
        ),
        (
            vec![],
            vec![("CHIP_CALL_TIMEOUT_SECS", "0")],
            "CHIP_CALL_TIMEOUT_SECS",
        ),
        (
            vec![],
            vec![("CHIP_CALL_TIMEOUT_SECS", "soon")],
            "CHIP_CALL_TIMEOUT_SECS",
        ),
    ];
    for (n, (args, env, named)) in cases.into_iter().enumerate() {
        let r = on_blocking("invalid", server.url.clone(), args.clone(), env.clone()).await;
        let stderr = String::from_utf8_lossy(&r.out.stderr).into_owned();
        assert_eq!(
            r.out.status.code(),
            Some(2),
            "{n} {args:?} {env:?}: {stderr}"
        );
        assert!(stderr.contains(named), "{n}: {stderr}");
    }
    assert!(server.captured.lock().await.is_empty(), "nothing was sent");
}

// ---- behaviour -----------------------------------------------------------------------------------

#[tokio::test]
async fn a_slow_reply_times_out_at_the_configured_limit_and_completes_with_a_longer_one() {
    needs_pax!();
    // The server takes 3 s to answer.
    let server = common::start(200, &completion(READ, "stop"), Duration::from_secs(3)).await;

    let short = on_blocking(
        "short",
        server.url.clone(),
        vec!["--call-timeout-secs", "1"],
        vec![],
    )
    .await;
    assert_eq!(short.out.status.code(), Some(4));
    assert_eq!(
        short.json["model_failures"][0]["kind"], "timeout",
        "{}",
        short.json
    );
    // Nothing came back before the deadline, so the request may or may not have reached the
    // server: the delivery classification says so and the work is not replayed.
    assert_eq!(short.json["model_failures"][0]["delivery"], "possibly_sent");
    assert_eq!(short.json["delivery"], "possibly_sent");
    assert_ne!(short.json["model_failures"][0]["kind"], "length_limited");
    nothing_executed(&short);
    assert_eq!(short.json["generation"]["call_timeout_secs"]["value"], 1);
    // The limit is reported as configured; how long the work really took is recorded apart from it.
    assert!(
        short.json["measurement"]["total_latency_ms"]
            .as_f64()
            .unwrap()
            >= 900.0
    );
    assert!(short.elapsed < Duration::from_secs(60));
    assert_eq!(server.captured.lock().await.len(), 1, "no hidden retry");

    let long = on_blocking(
        "long",
        server.url.clone(),
        vec!["--call-timeout-secs", "20"],
        vec![],
    )
    .await;
    assert_eq!(
        long.json["model_failures"],
        serde_json::json!([]),
        "{}",
        long.json
    );
    assert!(
        long.json["executions_by_capability"]["project.read"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert_eq!(long.json["audit"]["clean"], true);
    assert!(long.elapsed >= Duration::from_secs(3));
}

#[tokio::test]
async fn timeout_output_limit_http_error_and_malformed_reply_are_four_different_things() {
    needs_pax!();
    let slow = common::start(200, &completion(READ, "stop"), Duration::from_secs(3)).await;
    let timeout = on_blocking(
        "k-timeout",
        slow.url.clone(),
        vec!["--call-timeout-secs", "1"],
        vec![],
    )
    .await;
    assert_eq!(timeout.json["model_failures"][0]["kind"], "timeout");

    let cut = common::start(200, &completion(&READ[..40], "length"), Duration::ZERO).await;
    let length = on_blocking("k-length", cut.url.clone(), vec![], vec![]).await;
    assert_eq!(length.json["model_failures"][0]["kind"], "length_limited");

    let bad = common::start(200, &completion("not a decision", "stop"), Duration::ZERO).await;
    let malformed = on_blocking("k-malformed", bad.url.clone(), vec![], vec![]).await;
    assert_eq!(malformed.json["model_failures"], serde_json::json!([]));
    assert!(
        malformed.json["outcome_reason"]
            .as_str()
            .unwrap()
            .contains("not a valid decision")
    );

    let http = common::start(500, r#"{"error":{"message":"boom"}}"#, Duration::ZERO).await;
    let http = on_blocking("k-http", http.url.clone(), vec![], vec![]).await;
    let stderr = String::from_utf8_lossy(&http.out.stderr).into_owned();
    assert!(
        stderr.contains("provider_unavailable") && !stderr.contains("timeout"),
        "{stderr}"
    );

    for r in [&timeout, &length, &malformed] {
        nothing_executed(r);
        assert_eq!(r.json["context"]["context_limit_rejections"], 0);
    }
}

/// A server that sends the start of a stream containing a complete, valid decision, never finishes
/// it, and keeps the connection open past the deadline.
async fn stalling_server() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let hits = std::sync::Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let _ = socket.read(&mut buf).await;
                let event = format!(
                    "data: {}\n\n",
                    serde_json::json!({"id":"r","choices":[{"index":0,"delta":{"role":"assistant","content":READ}}]})
                );
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
                let _ = socket.write_all(format!("{head}{event}").as_bytes()).await;
                let _ = socket.flush().await;
                tokio::time::sleep(Duration::from_secs(30)).await;
            });
        }
    });
    (url, hits)
}

#[tokio::test]
async fn output_that_arrived_before_the_deadline_is_never_a_decision() {
    needs_pax!();
    let (url, hits) = stalling_server().await;
    let r = on_blocking("stall", url, vec!["--call-timeout-secs", "2"], vec![]).await;
    // The decision text was whole and valid, but the stream never finished: a timeout, nothing run.
    assert_eq!(
        r.out.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&r.out.stderr)
    );
    assert_eq!(r.json["model_failures"][0]["kind"], "timeout", "{}", r.json);
    // Bytes did arrive, so the provider did answer; the answer was never finished.
    assert_eq!(
        r.json["model_failures"][0]["delivery"],
        "confirmed_response"
    );
    assert_eq!(r.json["terminal_state"], "failed");
    nothing_executed(&r);
    assert_eq!(r.json["reads"], 0);
    assert_eq!(
        hits.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "no retry"
    );
    assert_eq!(
        std::fs::read_to_string(r.dir.join("src/lib.rs")).unwrap(),
        "pub fn one() -> u8 { 1 }\n"
    );
}

#[tokio::test]
async fn the_inspection_allowlist_holds_under_a_short_and_a_long_timeout() {
    needs_pax!();
    let server = common::start(200, &completion(WRITE, "stop"), Duration::ZERO).await;
    for (tag, secs) in [("w-short", "1"), ("w-long", "600")] {
        let r = on_blocking(
            tag,
            server.url.clone(),
            vec!["--call-timeout-secs", secs],
            vec![],
        )
        .await;
        assert_eq!(
            r.json["generation"]["call_timeout_secs"]["value"].to_string(),
            secs
        );
        assert!(
            r.json["outcome_reason"]
                .as_str()
                .unwrap()
                .contains("project.write"),
            "{}",
            r.json
        );
        nothing_executed(&r);
        assert_eq!(r.json["writes"], 0);
        assert_eq!(
            std::fs::read_to_string(r.dir.join("src/lib.rs")).unwrap(),
            "pub fn one() -> u8 { 1 }\n"
        );
    }
}

#[tokio::test]
async fn the_timeout_belongs_to_the_provider_each_run_builds_and_is_shared_with_no_other() {
    use chip_inference::{Message, MessageRole, ModelProvider, ModelRequest, ProviderConfig};
    let server = common::start(200, &completion(READ, "stop"), Duration::from_secs(3)).await;
    let build = |secs: u64| {
        chip_cli::provider_selection::build_model_from_process(
            ProviderConfig::new("openai-compatible", "mock-model", server.url.clone())
                .with_timeout(Duration::from_secs(secs)),
        )
        .unwrap()
    };
    let (short, long) = (build(1), build(20));
    let ask = |p: std::sync::Arc<dyn ModelProvider>| async move {
        p.complete(ModelRequest::new(
            "mock-model",
            vec![Message::new(MessageRole::User, "hi")],
        ))
        .await
    };
    // Concurrently, with the same server and the same process: each provider keeps its own limit.
    let (a, b) = tokio::join!(ask(short), ask(long));
    let a = a.unwrap_err();
    assert!(
        matches!(&a, chip_inference::FxError::Inference(f) if f.kind == "timeout"),
        "{a:?}"
    );
    assert!(b.is_ok(), "{b:?}");
}
