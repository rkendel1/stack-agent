//! Cancellation, the whole-work deadline and live events, through the real `stack-agent` binary.
//!
//! The model is a local event-stream endpoint that can stall mid-stream until the test releases it;
//! `pax` is a stand-in that can block while "running tests" until released. Every wait is on one of
//! those gates or on the service's own state, never a timing guess. What these tests establish:
//!
//! - `serve` shows a work's events while it runs, in the order the loop records them, and the
//!   finished trajectory begins with exactly what was shown;
//! - a cancellation (service request, Ctrl-C, or the deadline) stops the work at its next boundary:
//!   a model call in flight is abandoned with its delivery certainty and an *unconfirmed* remote
//!   stop, no further model call or execution starts, and nothing is retried;
//! - an execution in flight when the stop arrives finishes and is reported, never "undone";
//! - cancelling queued work, finished work and the same work twice behave as documented.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use gated::{FAILED, LIB, Model, PASSED, Pax, Reply, command, project, wait_until};
use serde_json::Value;

const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted stop"}"#;

struct Serve {
    child: Child,
    addr: String,
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn serve(model: &Model, pax: &Pax, tag: &str, flags: &[&str]) -> Serve {
    let dir = project(tag);
    let mut child = command(&dir, &model.url, pax)
        .args(["serve", "--host", "127.0.0.1", "--port", "0"])
        .args(flags)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addr = line
        .trim()
        .strip_prefix("Chip Runtime Service listening on http://")
        .unwrap_or_else(|| panic!("unexpected first line: {line:?}"))
        .to_string();
    Serve { child, addr }
}

fn http(addr: &str, method: &str, path: &str, body: Option<&str>) -> (u16, Value) {
    let body = body.unwrap_or("");
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap();
    (
        head.split(' ').nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap_or(Value::Null),
    )
}

fn submit(addr: &str) -> String {
    let (status, body) = http(
        addr,
        "POST",
        "/v1/work",
        Some(r#"{"goal":"Make the tests pass."}"#),
    );
    assert_eq!(status, 202, "{body}");
    body["work_id"].as_str().unwrap().to_string()
}

fn work(addr: &str, id: &str) -> Value {
    http(addr, "GET", &format!("/v1/work/{id}"), None).1
}

async fn finished(addr: &str, id: &str) -> Value {
    let mut state = Value::Null;
    wait_until("the work to end", || {
        state = work(addr, id);
        state["status"] != "running" && state["status"] != "queued"
    })
    .await;
    state
}

fn events(addr: &str, id: &str, after: usize) -> Value {
    http(
        addr,
        "GET",
        &format!("/v1/work/{id}/events?after={after}"),
        None,
    )
    .1
}

fn kinds(events: &Value) -> Vec<String> {
    events["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect()
}

// ---- serve: incremental events ------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn serve_shows_events_while_the_work_runs_then_its_terminal_result() {
    let model = Model::start(vec![Reply::Stream(READ), Reply::Stall(BLOCK)]).await;
    let pax = Pax::new("live", PASSED, false);
    let s = serve(&model, &pax, "live", &[]);
    let id = submit(&s.addr);

    // The second model call is stalled: the work is running, its read already happened.
    model.wait_requests(2).await;
    let mut live = Value::Null;
    wait_until("the read to be visible", || {
        live = events(&s.addr, &id, 0);
        kinds(&live).contains(&"ObservationRecorded".to_string())
    })
    .await;
    assert_eq!(work(&s.addr, &id)["status"], "running");
    assert_eq!(live["complete"], false);
    let k = kinds(&live);
    assert_eq!(k.first().map(String::as_str), Some("WorkStarted"));
    assert!(k.contains(&"ExecutionCompleted".to_string()), "{k:?}");
    assert!(
        !k.iter()
            .any(|x| x.starts_with("Work") && x != "WorkStarted"),
        "no terminal event before the end: {k:?}"
    );
    let seen = k.len();
    assert_eq!(live["next"], seen);

    // A poll with the cursor returns only what is new.
    model.release();
    let done = finished(&s.addr, &id).await;
    assert_eq!(done["status"], "blocked");
    let rest = events(&s.addr, &id, seen);
    assert_eq!(rest["complete"], true);
    assert_eq!(rest["after"], seen);
    let rest_kinds = kinds(&rest);
    assert_eq!(rest_kinds.last().map(String::as_str), Some("WorkBlocked"));
    let all = events(&s.addr, &id, 0);
    assert_eq!(
        &all["events"].as_array().unwrap()[..seen],
        &live["events"].as_array().unwrap()[..],
        "the finished trajectory starts with exactly what was shown while it ran"
    );
    assert_eq!(
        all["events"].as_array().unwrap().len(),
        seen + rest_kinds.len()
    );
    assert_eq!(
        http(
            &s.addr,
            "GET",
            &format!("/v1/work/{id}/events?after=x"),
            None
        )
        .0,
        400
    );
}

// ---- serve: cancellation ------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_during_an_active_stream_abandons_the_call_with_its_delivery_and_retries_nothing()
 {
    let model = Model::start(vec![Reply::Stall(READ)]).await;
    let pax = Pax::new("stream", PASSED, false);
    let s = serve(&model, &pax, "stream", &[]);
    let id = submit(&s.addr);
    model.wait_requests(1).await;

    let (status, reply) = http(&s.addr, "POST", &format!("/v1/work/{id}/cancel"), None);
    assert_eq!(status, 200);
    assert_eq!(reply["status"], "cancellation_requested");
    assert_eq!(reply["termination"], "unconfirmed");
    // Repeated: idempotent while it runs.
    let (status, again) = http(&s.addr, "POST", &format!("/v1/work/{id}/cancel"), None);
    assert!(status == 200 || status == 409, "{again}");

    // The stream is still stalled: FX abandons the attempt at its next poll, without waiting for it.
    let done = finished(&s.addr, &id).await;
    assert_eq!(done["status"], "cancelled", "{done}");
    let r = &done["result"];
    assert_eq!(r["terminal_state"], "cancelled");
    assert_eq!(r["exit_status"], 1);
    // Output had arrived before the stop: the provider answered (never "unsent" once invoked).
    assert_eq!(r["delivery"], "confirmed_response");
    let last = events(&s.addr, &id, 0)["events"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["kind"], "WorkCancelled");
    assert_eq!(last["cause"], "cancelled");
    assert_eq!(
        last["remote_cancellation"], "unconfirmed",
        "a blocked read cannot be interrupted, so the remote stop is not claimed"
    );
    assert_eq!(r["executions_by_capability"], serde_json::json!({}));

    model.release();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        model.requests(),
        1,
        "no second provider call, before or after release"
    );
    let (status, _) = http(&s.addr, "POST", &format!("/v1/work/{id}/cancel"), None);
    assert_eq!(status, 409, "cancelling finished work is refused");
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_while_a_tool_runs_lets_it_finish_reports_it_and_starts_nothing_else() {
    let model = Model::start(vec![Reply::Stream(TEST), Reply::Stream(BLOCK)]).await;
    let pax = Pax::new("tool", FAILED, true);
    let s = serve(&model, &pax, "tool", &[]);
    let id = submit(&s.addr);
    wait_until("pax to start", || pax.started()).await;

    let (status, _) = http(&s.addr, "POST", &format!("/v1/work/{id}/cancel"), None);
    assert_eq!(status, 200);
    assert_eq!(
        work(&s.addr, &id)["status"],
        "running",
        "requested is not cancelled"
    );
    assert!(!pax.finished(), "the tool is still running");
    pax.release();

    let done = finished(&s.addr, &id).await;
    assert_eq!(done["status"], "cancelled", "{done}");
    assert!(pax.finished(), "the tool ran to its end; it was not killed");
    let r = &done["result"];
    assert_eq!(
        r["executions_by_capability"]["pax.test"], 1,
        "its effect is reported, not undone"
    );
    assert_eq!(r["delivery"], Value::Null, "no model call was interrupted");
    assert_eq!(model.requests(), 1, "no model call after the stop");
    let k = kinds(&events(&s.addr, &id, 0));
    // It ran to its end (PAX reported `failed`, so the execution is recorded as failed) and was
    // observed; the stop did not cut it short.
    assert!(k.contains(&"ExecutionStarted".to_string()), "{k:?}");
    assert!(
        k.iter()
            .any(|x| x == "ExecutionCompleted" || x == "ExecutionFailed"),
        "{k:?}"
    );
    assert!(k.contains(&"ObservationRecorded".to_string()), "{k:?}");
    assert_eq!(k.last().map(String::as_str), Some("WorkCancelled"));
}

#[tokio::test(flavor = "multi_thread")]
async fn queued_work_cancelled_before_admission_never_reaches_the_model() {
    let model = Model::start(vec![Reply::Stall(BLOCK)]).await;
    let pax = Pax::new("queued", PASSED, false);
    let s = serve(&model, &pax, "queued", &["--max-concurrent-work", "1"]);
    let first = submit(&s.addr);
    model.wait_requests(1).await;
    let (status, body) = http(
        &s.addr,
        "POST",
        "/v1/work",
        Some(r#"{"goal":"Make the tests pass."}"#),
    );
    assert_eq!((status, body["status"].as_str()), (202, Some("queued")));
    let second = body["work_id"].as_str().unwrap().to_string();

    let (status, reply) = http(&s.addr, "POST", &format!("/v1/work/{second}/cancel"), None);
    assert_eq!((status, reply["status"].as_str()), (200, Some("cancelled")));
    model.release();
    finished(&s.addr, &first).await;
    let w = work(&s.addr, &second);
    assert_eq!(w["status"], "cancelled");
    assert_eq!(w["lifecycle"], Value::Null, "it never ran");
    assert_eq!(model.requests(), 1, "only the first work asked the model");
}

// ---- the whole-work deadline --------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_service_deadline_stops_a_stalled_work_and_says_why() {
    let model = Model::start(vec![Reply::Stall(READ)]).await;
    let pax = Pax::new("deadline", PASSED, false);
    let s = serve(&model, &pax, "deadline", &["--work-deadline-secs", "1"]);
    let id = submit(&s.addr);
    let started = Instant::now();
    let done = finished(&s.addr, &id).await;
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(done["status"], "cancelled", "{done}");
    let last = events(&s.addr, &id, 0)["events"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["cause"], "deadline");
    assert_eq!(model.requests(), 1);
    model.release();
}

// ---- the CLI ------------------------------------------------------------------------------------

fn interrupt(child: &Child) {
    let status = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_during_inference_stops_the_work_and_reports_what_happened() {
    let model = Model::start(vec![Reply::Stall(READ)]).await;
    let pax = Pax::new("sigint", PASSED, false);
    let dir = project("sigint");
    let child = command(&dir, &model.url, &pax)
        .args(["work", "--json", "Make the tests pass."])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    model.wait_requests(1).await;
    interrupt(&child);
    let out = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    );
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(stderr.contains("cancellation requested"), "{stderr}");
    let j: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    assert_eq!(j["terminal_state"], "cancelled");
    assert_eq!(j["delivery"], "confirmed_response");
    assert_eq!(j["verified"], false, "never reported as success");
    model.release();
    assert_eq!(model.requests(), 1);
    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        LIB
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_while_a_tool_is_pending_lets_it_finish_and_reports_it() {
    let model = Model::start(vec![Reply::Stream(TEST), Reply::Stream(BLOCK)]).await;
    let pax = Pax::new("sigint-tool", FAILED, true);
    let dir = project("sigint-tool");
    let child = command(&dir, &model.url, &pax)
        .args(["work", "--json", "Make the tests pass."])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("pax to start", || pax.started()).await;
    interrupt(&child);
    pax.release();
    let out = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    let j: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    assert_eq!(j["terminal_state"], "cancelled");
    assert_eq!(j["executions_by_capability"]["pax.test"], 1);
    assert!(pax.finished());
    assert_eq!(model.requests(), 1, "no model call after the interrupt");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_deadline_bounds_the_whole_work() {
    let model = Model::start(vec![Reply::Stall(READ)]).await;
    let pax = Pax::new("cli-deadline", PASSED, false);
    let dir = project("cli-deadline");
    let started = Instant::now();
    let child = command(&dir, &model.url, &pax)
        .args([
            "work",
            "--json",
            "--deadline-secs",
            "1",
            "Make the tests pass.",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let out = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(20));
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    let j: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"));
    assert_eq!(j["terminal_state"], "cancelled");
    assert!(
        j["outcome_reason"].as_str().unwrap().contains("deadline"),
        "{stdout}"
    );
    model.release();
    assert_eq!(model.requests(), 1);
}

#[test]
fn a_bad_deadline_is_a_usage_error() {
    for bad in ["0", "-1", "abc", "86401"] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_stack-agent"))
            .args(["work", "--deadline-secs", bad, "goal"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{bad}");
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_stack-agent"))
            .args(["serve", "--work-deadline-secs", bad])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{bad}");
    }
}
