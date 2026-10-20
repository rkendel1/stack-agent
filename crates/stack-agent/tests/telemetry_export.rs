//! OpenTelemetry export through the real binary: a work's durable record reaches a loopback
//! collector and a file as one OTLP/JSON trace, the goal, the model's words, the project's files
//! and the provider key never do, and an export that fails or is refused changes nothing about
//! the work.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;

use gated::{Model, PASSED, Pax, Reply, command, project, unique, wait_until};
use serde_json::Value;

const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const SENTINEL: &str = "TELEMETRYSENTINEL";

/// The provider key, assembled at run time so this file holds no credential-shaped literal.
fn key() -> String {
    ["sk-ant-api03-", "Tq7Wm2Xr9Lb4Nc8Vd1Fg5Hj3Kp6Zs0Ya"].concat()
}

fn block() -> String {
    format!(
        r#"{{"decision":"block","reason":"{SENTINEL} model reply: the key is {}"}}"#,
        key()
    )
}

fn goal() -> String {
    format!("Add `two`. {SENTINEL} goal; my token={}", key())
}

/// A project whose only source file holds the sentinel and a key.
fn secret_project(tag: &str) -> PathBuf {
    let dir = project(tag);
    std::fs::write(
        dir.join("src/lib.rs"),
        format!(
            "// {SENTINEL} file content: API_KEY={}\npub fn one() -> u8 {{ 1 }}\n",
            key()
        ),
    )
    .unwrap();
    dir
}

fn binary(dir: &Path, model: &Model, pax: &Pax, data: &Path) -> Command {
    let mut c = command(dir, &model.url, pax);
    c.env("STACK_AGENT_DATA_DIR", data)
        .env("CHIP_API_KEY", key());
    c
}

async fn output(mut c: Command) -> Output {
    tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap()
}

fn report(out: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(stdout.lines().last().unwrap_or_default())
        .unwrap_or_else(|_| panic!("{stdout}\n{}", String::from_utf8_lossy(&out.stderr)))
}

/// A collector on loopback that answers 200 to every request and hands over each body and its
/// header names.
fn collector() -> (u16, mpsc::Receiver<(String, Vec<String>, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let (mut headers, mut length) = (Vec::new(), 0);
            loop {
                let mut h = String::new();
                reader.read_line(&mut h).unwrap();
                let h = h.trim_end().to_string();
                if h.is_empty() {
                    break;
                }
                let (name, value) = h.split_once(':').unwrap();
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse().unwrap();
                }
                headers.push(name.trim().to_ascii_lowercase());
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let mut stream = stream;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = tx.send((
                line.trim_end().to_string(),
                headers,
                String::from_utf8(body).unwrap(),
            ));
        }
    });
    (port, rx)
}

fn assert_clean(what: &str, text: &str) {
    assert!(
        !text.contains(SENTINEL),
        "{what} carries the sentinel: {text}"
    );
    assert!(!text.contains(&key()), "{what} carries the key");
    assert!(
        !text.contains(&key()[14..30]),
        "{what} carries part of the key"
    );
    assert!(
        !text.contains("/tmp/") && !text.contains(&*std::env::temp_dir().to_string_lossy()),
        "{what} carries a host path"
    );
}

fn spans(request: &Value) -> Vec<Value> {
    request["resourceSpans"][0]["scopeSpans"][0]["spans"]
        .as_array()
        .unwrap()
        .clone()
}

fn attr<'a>(span: &'a Value, key: &str) -> Option<&'a Value> {
    span["attributes"]
        .as_array()?
        .iter()
        .find(|a| a["key"] == key)
        .map(|a| &a["value"])
}

#[tokio::test(flavor = "multi_thread")]
async fn a_work_is_exported_to_a_collector_and_a_file_and_nothing_sensitive_goes_with_it() {
    let model = Model::start(vec![
        Reply::Stream(READ),
        Reply::Stream(Box::leak(block().into_boxed_str())),
    ])
    .await;
    let pax = Pax::new("otel-export", PASSED, false);
    let dir = secret_project("otel-export");
    let data = unique("otel-export-data");
    let file = unique("otel-export-file").join("traces.jsonl");
    let (port, received) = collector();
    let mut c = binary(&dir, &model, &pax, &data);
    c.args(["work", &goal(), "--json"])
        .env(
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            format!("http://127.0.0.1:{port}"),
        )
        .env("STACK_AGENT_OTLP_FILE", &file);
    let out = output(c).await;
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(1), "blocked: {stderr}");
    let report = report(&out);
    let telemetry = &report["telemetry"];
    assert_eq!(telemetry["configured"], true, "{telemetry}");
    assert_eq!(telemetry["exported"], true, "{telemetry} {stderr}");
    let task_id = report["agent_state"]["task_id"].as_str().unwrap();
    let sinks = telemetry["sinks"].as_array().unwrap();
    assert_eq!(
        sinks[0]["sink"],
        format!("http://127.0.0.1:{port}/v1/traces")
    );
    assert_eq!(sinks[1]["exported"], true);

    let (request_line, headers, body) = received.recv().unwrap();
    assert_eq!(request_line, "POST /v1/traces HTTP/1.1");
    // No credential header: only what the request itself needs.
    assert_eq!(
        headers,
        ["host", "content-type", "content-length", "connection"]
    );
    let written = std::fs::read_to_string(&file).unwrap();
    assert_eq!(written.lines().count(), 1);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    for (what, text) in [
        ("the collector's body", body.as_str()),
        ("the OTLP file", written.trim()),
    ] {
        assert_clean(what, text);
        let request: Value = serde_json::from_str(text).unwrap();
        let spans = spans(&request);
        assert_eq!(spans[0]["traceId"], telemetry["trace_id"]);
        assert_eq!(spans[0]["name"], "invoke_agent stack-agent");
        assert_eq!(
            attr(&spans[0], "gen_ai.conversation.id").unwrap()["stringValue"],
            task_id
        );
        assert_eq!(
            attr(&spans[0], "stack_agent.outcome").unwrap()["stringValue"],
            "blocked"
        );
        assert_eq!(
            attr(&spans[0], "gen_ai.request.model").unwrap()["stringValue"],
            "scripted"
        );
        let names: Vec<&str> = spans.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(
            names.iter().filter(|n| n.starts_with("chat")).count(),
            2,
            "{names:?}"
        );
        assert!(names.contains(&"execute_tool project.read"), "{names:?}");
    }
    // The work's own report and the durable record still hold what they always did.
    assert_eq!(report["agent_state"]["recorded"], true);

    // The same trace from the record, offline, with `agent-state export`.
    let mut c = binary(&dir, &model, &pax, &data);
    c.args([
        "agent-state",
        "export",
        "--task-id",
        task_id,
        "--model",
        "scripted",
    ]);
    let out = output(c).await;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_clean("agent-state export", &printed);
    let printed: Value = serde_json::from_str(printed.trim()).unwrap();
    let exported: Value = serde_json::from_str(written.trim()).unwrap();
    assert_eq!(spans(&printed).len(), spans(&exported).len());
    assert_eq!(
        spans(&printed)[0]["spanId"],
        spans(&exported)[0]["spanId"],
        "ids derive from the record"
    );
}

async fn blocked_work(
    tag: &str,
    env: &[(&str, &str)],
    no_state: bool,
) -> (Option<i32>, Value, String) {
    let reply: &'static str = Box::leak(block().into_boxed_str());
    let model = Model::start(vec![Reply::Stream(reply)]).await;
    let pax = Pax::new(tag, PASSED, false);
    let dir = secret_project(tag);
    let data = unique(&format!("{tag}-data"));
    let mut c = binary(&dir, &model, &pax, &data);
    c.args(["work", &goal(), "--json"]);
    if no_state {
        c.arg("--no-state");
    }
    for (k, v) in env {
        c.env(k, v);
    }
    let out = output(c).await;
    (
        out.status.code(),
        report(&out),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn an_export_that_fails_or_is_refused_changes_nothing_about_the_work() {
    let (code, baseline, _) = blocked_work("otel-baseline", &[], false).await;
    assert_eq!(
        baseline["telemetry"],
        serde_json::json!({"configured": false})
    );

    // Nothing listens on this port.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let unreachable = format!("http://127.0.0.1:{port}");
    let (listener_port, received) = collector();
    let remote = format!("http://collector.example.com:{listener_port}");
    for (tag, env, expect) in [
        (
            "otel-unreachable",
            vec![("OTEL_EXPORTER_OTLP_ENDPOINT", unreachable.as_str())],
            "failed: the collector cannot be reached",
        ),
        (
            "otel-remote",
            vec![("OTEL_EXPORTER_OTLP_ENDPOINT", remote.as_str())],
            "must be on loopback",
        ),
        (
            "otel-headers",
            vec![
                ("OTEL_EXPORTER_OTLP_ENDPOINT", unreachable.as_str()),
                ("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer abc"),
            ],
            "sends no OTLP headers",
        ),
    ] {
        let (status, report, stderr) = blocked_work(tag, &env, false).await;
        assert_eq!(status, code, "{tag}: the exit status is unchanged");
        assert_eq!(report["outcome"], baseline["outcome"], "{tag}");
        assert_eq!(report["agent_state"]["recorded"], true, "{tag}");
        assert_eq!(report["telemetry"]["configured"], true, "{tag}");
        assert_eq!(report["telemetry"]["exported"], false, "{tag}");
        assert!(stderr.contains(expect), "{tag}: {stderr}");
        assert!(
            stderr.contains("the work's outcome is unchanged"),
            "{tag}: {stderr}"
        );
        assert!(
            !stderr.contains("Bearer abc"),
            "{tag}: a header value is never repeated"
        );
    }
    assert!(
        received.try_recv().is_err(),
        "a refused endpoint is never contacted"
    );

    // Export is built from the durable record; without one it says so.
    let file = unique("otel-no-state").join("t.jsonl");
    let (status, report, stderr) = blocked_work(
        "otel-no-state",
        &[("STACK_AGENT_OTLP_FILE", file.to_str().unwrap())],
        true,
    )
    .await;
    assert_eq!(status, code);
    assert_eq!(report["telemetry"]["exported"], false);
    assert!(
        stderr.contains("not persisted in durable agent state"),
        "{stderr}"
    );
    assert!(!file.exists());
}

// ---- serve ---------------------------------------------------------------------------------------

struct Server {
    child: std::process::Child,
    addr: String,
    _stdout: BufReader<std::process::ChildStdout>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn http(addr: &str, method: &str, path: &str, body: Option<&str>) -> (u16, Value) {
    let body = body.unwrap_or("");
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
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

#[tokio::test(flavor = "multi_thread")]
async fn a_served_work_is_exported_when_it_finishes() {
    let reply: &'static str = Box::leak(block().into_boxed_str());
    let model = Model::start(vec![Reply::Stream(reply)]).await;
    let pax = Pax::new("otel-serve", PASSED, false);
    let dir = secret_project("otel-serve");
    let data = unique("otel-serve-data");
    let file = unique("otel-serve-file").join("traces.jsonl");
    let mut child = binary(&dir, &model, &pax, &data)
        .args(["serve", "--port", "0"])
        .env("STACK_AGENT_OTLP_FILE", &file)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let server = Server {
        addr: line
            .trim()
            .strip_prefix("Chip Runtime Service listening on http://")
            .unwrap()
            .to_string(),
        child,
        _stdout: stdout,
    };
    let body = serde_json::json!({"goal": goal()}).to_string();
    let (status, accepted) = http(&server.addr, "POST", "/v1/work", Some(&body));
    assert_eq!(status, 202, "{accepted}");
    let id = accepted["work_id"].as_str().unwrap().to_string();
    wait_until("the work to finish", || {
        http(&server.addr, "GET", &format!("/v1/work/{id}"), None).1["status"] == "blocked"
    })
    .await;
    let (_, done) = http(&server.addr, "GET", &format!("/v1/work/{id}"), None);
    assert_eq!(done["result"]["telemetry"]["exported"], true, "{done}");
    let written = std::fs::read_to_string(&file).unwrap();
    assert_clean("the served work's OTLP file", &written);
    let request: Value = serde_json::from_str(written.trim()).unwrap();
    assert_eq!(
        attr(&spans(&request)[0], "gen_ai.conversation.id").unwrap()["stringValue"],
        id.as_str()
    );
}
