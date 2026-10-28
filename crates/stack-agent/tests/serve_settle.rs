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
