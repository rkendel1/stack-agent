//! The stack-agent product path: one executable, Chip's work loop over FX inference, no external
//! FX or Chip program, no real model, no API key from the environment.
//!
//! Each test runs the built `stack-agent` binary as a user would. The model is a local scripted
//! chat-completions endpoint reached through the real FX native provider (`chip-fx-native` ->
//! `fx-native-provider` -> `fx-single-attempt` -> `fx-core`); the reply depends on how many
//! observations the request already carries. The project, the filesystem and `git` are real. `pax`
//! is a stand-in script on `PATH` that answers `--version` and records every call, so the tests
//! need no PAX install and fail (rather than skip) if a work starts a PAX run it should not.
//!
//! What this does not establish: any real provider, any real network path beyond loopback, or
//! anything about the streaming wire format other than the OpenAI-compatible event stream the
//! native provider requests. See docs/known-issues.md.

#![cfg(unix)]

#[path = "common/mod.rs"]
mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

const KEY: &str = "sk-stack-agent-product-test-key";
const LIB: &str = "pub fn one() -> u8 { 1 }\n";

fn unique(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("stack-agent-product-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn project(tag: &str) -> PathBuf {
    let dir = unique(&format!("project-{tag}"));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), LIB).unwrap();
    let _ = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status();
    dir
}

/// A `pax` stand-in on `PATH`: answers `--version`, records every other call and fails it.
struct StandInPax {
    dir: PathBuf,
    log: PathBuf,
}

impl StandInPax {
    fn new(tag: &str) -> Self {
        let dir = unique(&format!("pax-{tag}"));
        let log = dir.join("calls.log");
        let path = dir.join("pax");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo version >> '{log}'; echo 'pax 0.4.1'; exit 0; fi\necho run >> '{log}'\nexit 2\n",
                log = log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, log }
    }

    fn runs(&self) -> usize {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| *l == "run")
            .count()
    }

    fn path_env(&self) -> std::ffi::OsString {
        let mut paths = vec![self.dir.clone()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::join_paths(paths).unwrap()
    }
}

/// One request the scripted model received.
struct Seen {
    head: String,
    body: String,
}

/// A chat-completions endpoint whose reply is `script(observations_so_far)`; it keeps every request.
async fn model(script: fn(usize) -> &'static str) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let store = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            let store = store.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 16384];
                let end = loop {
                    let Ok(n) = stream.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                        let len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if buf.len() >= end + 4 + len {
                            break end;
                        }
                    }
                };
                let head = String::from_utf8_lossy(&buf[..end]).into_owned();
                let body = String::from_utf8_lossy(&buf[end + 4..]).into_owned();
                let step = body.matches("Observation:\\nkind:").count();
                let streamed = body.contains("\"stream\":true");
                store.lock().await.push(Seen { head, body });
                let reply = script(step);
                // `HTTP <status>`: the provider fails the request with that status.
                if let Some(status) = reply.strip_prefix("HTTP ") {
                    let error = r#"{"error":{"message":"scripted failure"}}"#;
                    let response = format!(
                        "HTTP/1.1 {status} Scripted\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{error}",
                        error.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                    return;
                }
                // `PLAIN <content>`: a stand-in that answers one JSON body even when asked to stream.
                let (reply, streamed) = match reply.strip_prefix("PLAIN ") {
                    Some(content) => (content, false),
                    None => (reply, streamed),
                };
                let content = serde_json::to_string(reply).unwrap();
                let payload = format!(
                    "{{\"id\":\"m\",\"choices\":[{{\"message\":{{\"role\":\"assistant\",\"content\":{content}}}}}],\"usage\":{{\"prompt_tokens\":1,\"completion_tokens\":1}}}}"
                );
                let response = match streamed
                    .then(|| common::as_event_stream(&payload))
                    .flatten()
                {
                    Some(events) => format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{events}",
                        events.len()
                    ),
                    None => format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                        payload.len()
                    ),
                };
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    (url, seen)
}

/// `stack-agent work <goal> <args>` in `dir`, with the model at `url` and only `pax` on `PATH`
/// changed. Provider settings come from the environment the test sets, never from the caller's.
async fn work(dir: &Path, url: &str, pax: &StandInPax, args: &[&str]) -> Output {
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_stack-agent"));
    cmd.arg("work")
        .args(args)
        .current_dir(dir)
        .env_remove("PAX_BIN")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("CHIP_EXECUTOR")
        .env("PATH", pax.path_env())
        .env("CHIP_PROVIDER", "openai-compatible")
        .env("CHIP_MODEL", "scripted")
        .env("CHIP_ENDPOINT", url)
        .env("CHIP_API_KEY", KEY)
        .stdin(Stdio::null());
    cmd.output().await.unwrap()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {}", text(o)))
}

#[test]
fn help_names_the_product_commands_and_exits_zero() {
    for flag in ["--help", "-h"] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_stack-agent"))
            .arg(flag)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        assert!(out.stderr.is_empty(), "{}", text(&out));
        let help = String::from_utf8_lossy(&out.stdout);
        for line in [
            "stack-agent work",
            "stack-agent verify",
            "stack-agent serve",
            "stack-agent --version",
            "RETRYING A TURN MAY REPEAT EXTERNAL EFFECTS",
        ] {
            assert!(help.contains(line), "{line:?} missing from:\n{help}");
        }
    }
}

/// The unified path end to end: the model (through FX) asks for a read, Chip validates and performs
/// it on the host, the observation goes back through FX in the next request, and the model's answer
/// completes the inspection only because it is grounded in that observation.
#[tokio::test(flavor = "multi_thread")]
async fn a_tool_round_trip_runs_chip_over_fx_in_one_process() {
    fn script(step: usize) -> &'static str {
        match step {
            0 => {
                r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#
            }
            _ => r#"{"decision":"complete","summary":"`one` is defined in src/lib.rs."}"#,
        }
    }
    let pax = StandInPax::new("round-trip");
    let dir = project("round-trip");
    let (url, seen) = model(script).await;
    let out = work(
        &dir,
        &url,
        &pax,
        &["--kind", "inspect", "--json", "Where is `one` defined?"],
    )
    .await;

    // Exit 1: an inspection is grounded, never verified (verify.rs EXIT_NOT_VERIFIED).
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    let j = json(&out);
    assert_eq!(j["goal_kind"], "inspect");
    assert_eq!(j["terminal_state"], "completed");
    assert_eq!(j["goal_satisfied"], true);
    assert_eq!(j["grounded"], true);
    assert_eq!(j["verified"], false);
    assert_eq!(j["reads"], 1);
    assert_eq!(j["executions_by_capability"]["project.read"], 1);
    assert!(j["answer"].as_str().unwrap().contains("src/lib.rs"));

    let seen = seen.lock().await;
    assert_eq!(seen.len(), 2, "one model call per turn");
    // The native FX provider asks for an event stream and Chip read it: the streaming path ran.
    assert!(seen[0].body.contains("\"stream\":true"), "{}", seen[0].body);
    // The first request carried no observation; the second carried the host's real read of the file.
    assert!(!seen[0].body.contains("Observation:\\nkind:"));
    assert!(
        seen[1].body.contains("Observation:\\nkind:") && seen[1].body.contains("pub fn one()"),
        "the observation of the real file must reach the model: {}",
        seen[1].body
    );
    // The credential went to the provider and only there.
    assert!(
        seen[0].head.to_ascii_lowercase().contains(&format!(
            "authorization: bearer {}",
            KEY.to_ascii_lowercase()
        )),
        "{}",
        seen[0].head
    );
    assert!(!text(&out).contains(KEY));
    assert_eq!(
        pax.runs(),
        0,
        "an inspection that never asked for tests ran PAX"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        LIB
    );
}

/// A model's claim of success is not completion: with no host-side evidence, change work refuses it.
#[tokio::test(flavor = "multi_thread")]
async fn a_model_that_only_claims_success_is_not_believed() {
    fn script(_: usize) -> &'static str {
        r#"{"decision":"complete","summary":"Done: all tests pass."}"#
    }
    let pax = StandInPax::new("claim");
    let dir = project("claim");
    let (url, seen) = model(script).await;
    let out = work(
        &dir,
        &url,
        &pax,
        &["--max-turns", "2", "Make the tests pass."],
    )
    .await;
    let t = text(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");
    assert!(t.contains("completion refused"), "{t}");
    assert!(!seen.lock().await.is_empty(), "the model was asked");
    assert_eq!(pax.runs(), 0);
    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        LIB
    );
}

/// A provider that cannot be reached is an explicit failure (exit 3), with no other provider tried
/// and nothing run.
#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_provider_fails_explicitly_and_runs_nothing() {
    let pax = StandInPax::new("unreachable");
    let dir = project("unreachable");
    let out = work(
        &dir,
        "http://127.0.0.1:1/v1/chat/completions",
        &pax,
        &["--json", "Make the tests pass."],
    )
    .await;
    let t = text(&out);
    assert_eq!(out.status.code(), Some(3), "{t}");
    assert!(t.contains("No other provider or model was tried"), "{t}");
    assert!(!t.contains(KEY), "{t}");
    assert_eq!(pax.runs(), 0);
    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        LIB
    );
}

/// Regression for the stand-in mismatch (chip-rs 2c22fd8): the native FX provider always asks for an
/// event stream, and a stand-in that answers one plain JSON body is refused as a protocol violation,
/// explicitly and with the "unavailable" exit code, never accepted as if it had streamed. The exit
/// code is the current contract (verify.rs EXIT_UNAVAILABLE: the first model call failed with
/// nothing executed); this test pins it so a change is deliberate.
#[tokio::test(flavor = "multi_thread")]
async fn a_non_streaming_stand_in_is_refused_explicitly_not_accepted() {
    fn script(_: usize) -> &'static str {
        r#"PLAIN {"decision":"block","reason":"stand-in"}"#
    }
    let pax = StandInPax::new("plain");
    let dir = project("plain");
    let (url, seen) = model(script).await;
    let out = work(&dir, &url, &pax, &["--json", "Make the tests pass."]).await;
    let t = text(&out);
    assert_eq!(out.status.code(), Some(3), "{t}");
    assert!(
        t.contains("protocol_violation") && t.contains("not an event stream"),
        "{t}"
    );
    assert!(t.contains("not retried"), "{t}");
    assert!(
        out.stdout.is_empty(),
        "no verdict for a run that never began: {t}"
    );
    assert_eq!(seen.lock().await.len(), 1, "one attempt, no retry");
}

/// A model call that fails after work has begun ends the work with FX's typed delivery certainty
/// in the report (a field, not text to parse), exit 4, and no second attempt. An HTTP error status
/// is reported `possibly_sent`: FX marks `confirmed_response` only once model output was observed
/// (fx-single-attempt mapping.rs). See docs/known-issues.md (D).
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_model_call_reports_typed_delivery_and_is_not_retried() {
    fn script(step: usize) -> &'static str {
        match step {
            0 => {
                r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#
            }
            _ => "HTTP 500",
        }
    }
    let pax = StandInPax::new("late-failure");
    let dir = project("late-failure");
    let (url, seen) = model(script).await;
    let out = work(&dir, &url, &pax, &["--json", "Make the tests pass."]).await;
    assert_eq!(out.status.code(), Some(4), "{}", text(&out));
    let j = json(&out);
    assert_eq!(j["terminal_state"], "failed");
    assert_eq!(j["delivery"], "possibly_sent");
    assert_eq!(j["verified"], false);
    assert_eq!(j["executions_by_capability"]["project.read"], 1);
    assert_eq!(
        seen.lock().await.len(),
        2,
        "the failed call was not retried"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        LIB
    );
}
