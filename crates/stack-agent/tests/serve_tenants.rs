//! `serve --tenants`: the binary over real TCP with a mock model. Authentication, isolation between
//! tenants, and what a request may and may not name. Mocked model; nothing here shows a live one.

#[path = "common/mod.rs"]
mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

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

fn sha(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn base(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-tenants-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for t in ["alice", "bob"] {
        std::fs::create_dir_all(dir.join(t).join("p1/src")).unwrap();
        std::fs::write(
            dir.join(t).join("p1/src/lib.rs"),
            "pub fn one() -> u8 { 1 }\n",
        )
        .unwrap();
    }
    dir
}

fn completion(content: &str) -> String {
    json!({
        "id": "r",
        "choices": [{"message": {"role": "assistant", "content": content}}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1},
    })
    .to_string()
}

fn launch(tag: &str, model_url: &str) -> (Server, PathBuf) {
    let dir = base(tag);
    let tenants = json!({"schema":"chip.tenants.v1","tenants":[
        {"id":"alice","token_sha256":sha("alice-token"),"workspace_root":dir.join("alice"),
         "kinds":["inspect"],"profiles":["m"],"max_concurrent":1,"max_queued":1,"max_turns":4,"max_executions":4,"max_deadline_secs":60},
        {"id":"bob","token_sha256":sha("bob-token"),"workspace_root":dir.join("bob"),
         "kinds":["inspect"],"profiles":["m"],"max_turns":4,"max_executions":4,"max_deadline_secs":60}]});
    let profiles = json!({"schema":"chip.profiles.v1","profiles":{"m":{
        "provider":"openai-compatible","model":"mock-model","endpoint":model_url,"api_key_env":"MOCK_KEY"}}});
    std::fs::write(dir.join("tenants.json"), tenants.to_string()).unwrap();
    std::fs::write(dir.join("profiles.json"), profiles.to_string()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_stack-agent"))
        .args(["serve", "--port", "0", "--tenants"])
        .arg(dir.join("tenants.json"))
        .arg("--profiles")
        .arg(dir.join("profiles.json"))
        .current_dir(&dir)
        .env_remove("CHIP_MODEL")
        .env_remove("CHIP_ENDPOINT")
        .env("MOCK_KEY", "sk-tenant-key")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let addr = line
        .trim()
        .strip_prefix("Chip Runtime Service listening on http://")
        .unwrap_or_else(|| {
            let mut err = String::new();
            let _ = child.stderr.take().unwrap().read_to_string(&mut err);
            panic!("unexpected first line {line:?}: {err}")
        })
        .to_string();
    (
        Server {
            child,
            addr,
            _stdout: stdout,
        },
        dir,
    )
}

fn http(
    addr: &str,
    token: Option<&str>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let auth = token.map_or(String::new(), |t| format!("authorization: Bearer {t}\r\n"));
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
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

const BLOCK: &str = r#"{"decision":"block","reason":"nothing to do"}"#;

fn wait_finished(addr: &str, token: &str, id: &str) -> Value {
    let started = Instant::now();
    loop {
        let (_, body) = http(addr, Some(token), "GET", &format!("/v1/work/{id}"), None);
        if !matches!(body["status"].as_str(), Some("queued" | "running")) {
            return body;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "{body}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_tenants_token_opens_the_api_and_health_stays_open() {
    let model = common::start(200, &completion(BLOCK), Duration::ZERO).await;
    let (server, _) = launch("auth", &model.url);
    let a = &server.addr;
    assert_eq!(http(a, None, "GET", "/health", None).0, 200);
    for token in [None, Some("nope"), Some("alice")] {
        let (status, body) = http(
            a,
            token,
            "POST",
            "/v1/work",
            Some(json!({"goal":"x","kind":"inspect"})),
        );
        assert_eq!(
            (status, body["error"]["code"].as_str()),
            (401, Some("unauthorized"))
        );
    }
    assert_eq!(
        http(a, Some("alice-token"), "GET", "/v1/metrics", None).0,
        200
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tenant_runs_work_in_its_workspace_and_no_other_tenant_can_see_or_stop_it() {
    let model = common::start(200, &completion(BLOCK), Duration::ZERO).await;
    let (server, _) = launch("iso", &model.url);
    let a = &server.addr;
    let (status, body) = http(
        a,
        Some("alice-token"),
        "POST",
        "/v1/work",
        Some(json!({"goal":"Look around.","kind":"inspect","project":"p1","max_turns":2})),
    );
    assert_eq!(status, 202, "{body}");
    assert_eq!(body["durable"], false);
    let id = body["work_id"].as_str().unwrap().to_string();
    let finished = wait_finished(a, "alice-token", &id);
    assert_eq!(finished["status"], "blocked", "{finished}");
    // The model was called with the profile's key, taken from the environment variable it names.
    let captured = model.captured.lock().await;
    assert_eq!(
        captured[0].header("authorization"),
        Some("Bearer sk-tenant-key")
    );
    drop(captured);
    for (method, path) in [
        ("GET", format!("/v1/work/{id}")),
        ("GET", format!("/v1/work/{id}/events")),
        ("POST", format!("/v1/work/{id}/cancel")),
    ] {
        let (status, body) = http(a, Some("bob-token"), method, &path, None);
        assert_eq!(
            (status, body["error"]["code"].as_str()),
            (404, Some("work_not_found")),
            "{path}"
        );
    }
    let (_, mine) = http(a, Some("alice-token"), "GET", "/v1/metrics", None);
    let (_, theirs) = http(a, Some("bob-token"), "GET", "/v1/metrics", None);
    assert_eq!(mine["finished_retained"], 1);
    assert_eq!(theirs["finished_retained"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_can_only_narrow_what_the_operator_allowed_and_name_only_what_it_may() {
    let model = common::start(200, &completion(BLOCK), Duration::ZERO).await;
    let (server, _) = launch("limits", &model.url);
    let a = &server.addr;
    let post = |body: Value| http(a, Some("alice-token"), "POST", "/v1/work", Some(body));
    let code = |(status, body): (u16, Value)| {
        (
            status,
            body["error"]["code"].as_str().unwrap_or("").to_string(),
        )
    };
    let g = "Look around.";
    assert_eq!(
        code(post(json!({"goal":g,"kind":"inspect","max_turns":99}))),
        (400, "exceeds_tenant_limit".into())
    );
    assert_eq!(
        code(post(json!({"goal":g,"kind":"inspect","deadline_secs":0}))),
        (400, "exceeds_tenant_limit".into())
    );
    assert_eq!(
        code(post(json!({"goal":g,"kind":"change"}))),
        (403, "kind_not_allowed".into())
    );
    assert_eq!(
        code(post(json!({"goal":g,"kind":"act"}))),
        (400, "invalid_kind".into())
    );
    for field in [
        "provider",
        "model",
        "endpoint",
        "api_key",
        "capabilities",
        "env",
    ] {
        let mut body = json!({"goal":g,"kind":"inspect"});
        body[field] = json!("x");
        assert_eq!(code(post(body)), (400, "unknown_field".into()), "{field}");
    }
    for project in ["../bob/p1", "/etc", "p1/../../bob/p1", "missing"] {
        assert_eq!(
            code(post(json!({"goal":g,"kind":"inspect","project":project}))),
            (400, "invalid_project".into()),
            "{project}"
        );
    }
    assert_eq!(
        code(post(json!({"goal":g,"kind":"inspect","profile":"other"}))),
        (400, "unknown_profile".into())
    );
    assert!(
        model.captured.lock().await.is_empty(),
        "nothing refused reached the model"
    );
}

fn sse(addr: &str, token: Option<&str>, path: &str) -> (u16, String) {
    let auth = token.map_or(String::new(), |t| format!("authorization: Bearer {t}\r\n"));
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}accept: text/event-stream\r\nconnection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out);
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    (
        head.split(' ').nth(1).unwrap().parse().unwrap(),
        format!("{head}\n---\n{body}"),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn events_can_be_pushed_as_sse_with_the_same_envelope_and_the_same_isolation() {
    let model = common::start(200, &completion(BLOCK), Duration::from_millis(600)).await;
    let (server, _) = launch("sse", &model.url);
    let a = &server.addr;
    let (_, body) = http(
        a,
        Some("alice-token"),
        "POST",
        "/v1/work",
        Some(json!({"goal":"Look around.","kind":"inspect","max_turns":2})),
    );
    let id = body["work_id"].as_str().unwrap().to_string();
    let path = format!("/v1/work/{id}/events");

    let (status, text) = sse(a, Some("alice-token"), &path);
    assert_eq!(status, 200, "{text}");
    assert!(
        text.to_ascii_lowercase()
            .contains("content-type: text/event-stream")
    );
    let datas: Vec<Value> = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| serde_json::from_str(d).ok())
        .collect();
    let events: Vec<&Value> = datas
        .iter()
        .filter(|d| d["schema"] == "chip.work-event.v1")
        .collect();
    assert_eq!(events[0]["event"]["kind"], "WorkStarted", "{text}");
    for (i, e) in events.iter().enumerate() {
        assert_eq!(e["seq"], i as u64);
        assert_eq!(e["work_id"], id.as_str());
        assert!(e["build"].as_str().unwrap().starts_with("stack-agent "));
    }
    assert_eq!(events.last().unwrap()["event"]["kind"], "WorkBlocked");
    assert!(text.contains("event: end"), "{text}");

    // Resuming after a drop: Last-Event-ID is honoured via ?after=.
    let (_, resumed) = sse(a, Some("alice-token"), &format!("{path}?after=1"));
    assert!(!resumed.contains("\"seq\":0,"), "{resumed}");

    // Another tenant, or nobody, gets the ordinary JSON refusal, not a stream.
    let (status, other) = sse(a, Some("bob-token"), &path);
    assert_eq!(status, 404, "{other}");
    assert!(other.contains("work_not_found") && !other.contains("event-stream"));
    assert_eq!(sse(a, None, &path).0, 401);
}
