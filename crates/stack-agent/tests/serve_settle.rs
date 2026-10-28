//! `POST /v1/work/{id}/settle` is the same settlement `agent-state settle` makes, reachable by a host.
//! The success path (an unobserved intent) is covered where intents are created; this pins the
//! route's contract: strict body, the same refusals, nothing invented.

#[path = "common/mod.rs"]
mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

struct Server {
    child: Child,
    addr: String,
    _stdout: BufReader<std::process::ChildStdout>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn launch(extra: &[&str], model_url: &str, tag: &str) -> Server {
    let dir = std::env::temp_dir().join(format!("chip-settle-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_stack-agent"))
        .args(["serve", "--port", "0"])
        .args(extra)
        .current_dir(&dir)
        .env("STACK_AGENT_DATA_DIR", dir.join("data"))
        .env("CHIP_PROVIDER", "openai-compatible")
        .env("CHIP_MODEL", "mock-model")
        .env("CHIP_ENDPOINT", model_url)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let addr = line
        .trim()
        .strip_prefix("Chip Runtime Service listening on http://")
        .unwrap_or_else(|| panic!("unexpected first line {line:?}"))
        .to_string();
    Server {
        child,
        addr,
        _stdout: stdout,
    }
}

fn post(addr: &str, path: &str, body: Value) -> (u16, Value) {
    let body = body.to_string();
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
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

fn good() -> Value {
    json!({"intent":"k1","executed":false,"decided_by":"ops","evidence":"checked the host's own log"})
}

#[tokio::test(flavor = "multi_thread")]
async fn the_route_is_strict_and_refuses_what_the_command_refuses() {
    let model = common::start(200, "{}", Duration::ZERO).await;
    let server = launch(&[], &model.url, "state");
    let a = &server.addr;
    let (status, body) = post(a, "/v1/work/work_nothere/settle", good());
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (409, Some("settlement_refused")),
        "{body}"
    );
    for bad in [
        json!({"intent":"k1","executed":false,"decided_by":"ops"}),
        json!({"intent":"k1","executed":"no","decided_by":"ops","evidence":"x"}),
        json!({"intent":"k1","executed":false,"decided_by":"ops","evidence":"x","receipt":"forged"}),
    ] {
        assert_eq!(post(a, "/v1/work/work_nothere/settle", bad).0, 400);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn without_durable_state_there_is_nothing_to_settle() {
    let model = common::start(200, "{}", Duration::ZERO).await;
    let server = launch(&["--no-state"], &model.url, "nostate");
    let (status, body) = post(&server.addr, "/v1/work/work_nothere/settle", good());
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (501, Some("no_durable_state"))
    );
}

fn get(addr: &str, path: &str) -> (u16, Value) {
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nconnection: close\r\n\r\n"
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

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn sigterm_drains_ready_flips_new_work_is_refused_and_the_held_work_finishes() {
    let reply = json!({
        "id": "r",
        "choices": [{"message": {"role": "assistant", "content": "{\"decision\":\"block\",\"reason\":\"done\"}"}}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1},
    })
    .to_string();
    let model = common::start(200, &reply, Duration::from_secs(2)).await;
    let mut server = launch(&["--no-state", "--drain-secs", "20"], &model.url, "drain");
    let a = server.addr.clone();
    assert_eq!(get(&a, "/ready"), (200, json!({"ready": true})));
    let (status, body) = post(
        &a,
        "/v1/work",
        json!({"goal": "Look around.", "kind": "inspect"}),
    );
    assert_eq!(status, 202, "{body}");
    let id = body["work_id"].as_str().unwrap().to_string();

    unsafe { libc::kill(server.child.id() as i32, libc::SIGTERM) };
    std::thread::sleep(Duration::from_millis(300));
    let (status, ready) = get(&a, "/ready");
    assert_eq!(
        (status, ready["ready"].clone()),
        (503, json!(false)),
        "{ready}"
    );
    assert_eq!(get(&a, "/health").0, 200, "alive while draining");
    let (status, body) = post(&a, "/v1/work", json!({"goal": "More.", "kind": "inspect"}));
    assert_eq!(
        (status, body["error"]["code"].as_str()),
        (503, Some("draining"))
    );
    // The work it already held is still answered, and finishes.
    let begun = std::time::Instant::now();
    loop {
        // Once the held work finishes the drain is over and the process exits, so a refused
        // connection here means it finished (the exit code below is the check).
        let Ok(mut stream) = TcpStream::connect(&a) else {
            break;
        };
        write!(
            stream,
            "GET /v1/work/{id} HTTP/1.1\r\nHost: {a}\r\nconnection: close\r\n\r\n"
        )
        .unwrap();
        let mut out = String::new();
        let _ = stream.read_to_string(&mut out);
        let w: Value = out
            .split_once("\r\n\r\n")
            .and_then(|(_, b)| serde_json::from_str(b).ok())
            .unwrap_or(Value::Null);
        if w["status"] == "blocked" {
            break;
        }
        assert!(begun.elapsed() < Duration::from_secs(15), "{w}");
        std::thread::sleep(Duration::from_millis(100));
    }
    let status = server.child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
}
