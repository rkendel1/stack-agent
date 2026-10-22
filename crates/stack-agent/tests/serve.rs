//! `chip serve`, the binary run as a local application would run it, driven over real TCP.
//!
//! The model is a local mock HTTP server with one fixed reply (a request that escapes the project,
//! which Chip refuses: a deterministic, bounded run) and PAX is a shim that only identifies itself,
//! so the whole suite needs no live model and no real PAX. Mocked means mocked: nothing here
//! claims a real model or a real PAX test run.

#[path = "common/mod.rs"]
mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;

fn project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-serve-cli-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
    dir
}

#[cfg(unix)]
fn pax_shim(tag: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("chip-serve-pax-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let shim = dir.join("pax");
    std::fs::write(&shim, "#!/bin/sh\necho 'pax 9.9.9'\n").unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    shim
}

struct Server {
    child: Child,
    addr: String,
    /// Kept open: a closed pipe would end the service for a reason that has nothing to do with it.
    _stdout: BufReader<std::process::ChildStdout>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn completion(content: &str) -> String {
    serde_json::json!({
        "id": "resp-serve",
        "choices": [{"message": {"role": "assistant", "content": content}}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 4},
    })
    .to_string()
}

fn command(dir: &PathBuf, model_url: &str, shim: &PathBuf) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_stack-agent"));
    c.arg("serve")
        .current_dir(dir)
        .env("CHIP_PROVIDER", "openai-compatible")
        .env("CHIP_MODEL", "mock-model")
        .env("CHIP_ENDPOINT", model_url)
        .env("CHIP_API_KEY", "sk-serve-secret-never-printed")
        .env("PAX_BIN", shim);
    c
}

fn launch(mut c: Command, args: &[&str]) -> Server {
    let mut child = c
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    stdout.read_line(&mut line).unwrap();
    let addr = line
        .trim()
        .strip_prefix("Chip Runtime Service listening on http://")
        .unwrap_or_else(|| panic!("unexpected first line: {line:?}"))
        .to_string();
    Server {
        child,
        addr,
        _stdout: stdout,
    }
}

fn http(addr: &str, method: &str, path: &str, body: Option<&str>) -> (u16, String, Value) {
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
        head.to_ascii_lowercase(),
        serde_json::from_str(body).unwrap_or(Value::Null),
    )
}

const ESCAPE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"../chip-serve-escaped.txt","content":"pwned"}}"#;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_service_runs_work_over_http_through_the_same_runtime() {
    let model = common::start(200, &completion(ESCAPE), Duration::ZERO).await;
    let (dir, shim, url) = (project("accept"), pax_shim("accept"), model.url.clone());
    let outside = dir.parent().unwrap().join("chip-serve-escaped.txt");
    let _ = std::fs::remove_file(&outside);

    tokio::task::spawn_blocking(move || {
        // Default flags are not used here only because 8765 may be taken; the default is unit-tested.
        let server = launch(
            command(&dir, &url, &shim),
            &["--host", "127.0.0.1", "--port", "0"],
        );
        let addr = server.addr.clone();
        assert!(addr.starts_with("127.0.0.1:"), "{addr}");

        // curl http://127.0.0.1:PORT/health
        let (status, head, body) = http(&addr, "GET", "/health", None);
        assert_eq!((status, body), (200, serde_json::json!({"status": "ok"})));
        assert!(!head.contains("access-control"));

        // curl -X POST /v1/work -d '{"goal":"..."}'
        let (status, _, body) = http(
            &addr,
            "POST",
            "/v1/work",
            Some(r#"{"goal":"Add a function that sorts the payload."}"#),
        );
        assert_eq!(status, 202, "{body}");
        let id = body["work_id"].as_str().unwrap().to_string();
        assert!(id.starts_with("work_"));

        // curl /v1/work/<id> until the runtime reports an end.
        let mut state = Value::Null;
        for _ in 0..500 {
            let (status, _, body) = http(&addr, "GET", &format!("/v1/work/{id}"), None);
            assert_eq!(status, 200);
            if body["status"] != "running" {
                state = body;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(state["status"], "blocked", "{state}");
        assert_eq!(state["lifecycle"], "blocked");
        assert_eq!(state["result"]["verified"], false);
        assert_eq!(state["result"]["audit"]["clean"], true);
        assert_eq!(state["result"]["measurement"]["executions"], 0);
        assert!(!state.to_string().contains("sk-serve-secret"));

        // curl /v1/work/<id>/events
        let (status, _, body) = http(&addr, "GET", &format!("/v1/work/{id}/events"), None);
        assert_eq!(status, 200);
        let kinds: Vec<&str> = body["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds.first(), Some(&"WorkStarted"));
        assert_eq!(kinds.last(), Some(&"WorkBlocked"));
        assert!(!kinds.contains(&"ExecutionStarted"), "{kinds:?}");
        assert!(!kinds.contains(&"EvidenceRecorded"), "{kinds:?}");

        assert_eq!(http(&addr, "GET", "/v1/work/work_nope", None).0, 404);
        assert_eq!(
            http(
                &addr,
                "POST",
                "/v1/work",
                Some(r#"{"goal":"x","executable":"sh"}"#)
            )
            .0,
            400
        );
        assert!(!outside.exists(), "a file was written outside the project");
        assert_eq!(
            std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
            "pub fn one() -> u8 { 1 }\n"
        );
    })
    .await
    .unwrap();
    // The refused request is answered with why, and asked again within the bound; the model
    // repeats it, and nothing is ever written outside the project.
    assert_eq!(
        model.captured.lock().await.len(),
        1 + chip_core::DEFAULT_MALFORMED_REPLY_LIMIT,
        "bounded"
    );
}

#[cfg(unix)]
#[test]
fn without_a_model_nothing_listens() {
    let dir = project("unavailable");
    let shim = pax_shim("unavailable");
    let out = command(&dir, "http://127.0.0.1:1/v1/chat/completions", &shim)
        .env_remove("CHIP_MODEL")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stdout.is_empty(), "it must not claim to be listening");
}

/// The standalone service runs on the local machine and needs nothing else. It has one mutable
/// project directory, so asking it for concurrent work is refused up front rather than allowed to
/// collide.
#[cfg(unix)]
#[test]
fn concurrent_work_on_the_one_local_directory_is_refused_at_startup() {
    let dir = project("concurrency");
    let shim = pax_shim("concurrency");
    for n in ["2", "8"] {
        let out = command(&dir, "http://127.0.0.1:1/v1/chat/completions", &shim)
            .args(["--port", "0", "--max-concurrent-work", n])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{n}");
        assert!(out.stdout.is_empty(), "it must not claim to be listening");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("concurrent work requires isolated environments"),
            "{err}"
        );
        assert!(err.contains("--max-concurrent-work 1"), "{err}");
    }
}

#[cfg(unix)]
#[test]
fn the_local_environment_defaults_to_one_work_at_a_time_and_says_so() {
    let dir = project("onework");
    let shim = pax_shim("onework");
    let mut server = launch(
        command(&dir, "http://127.0.0.1:1/v1/chat/completions", &shim),
        &["--port", "0", "--max-concurrent-work", "1"],
    );
    let (status, _, m) = http(&server.addr, "GET", "/v1/metrics", None);
    assert_eq!((status, m["max_concurrent_work"].as_u64()), (200, Some(1)));
    let mut rest = String::new();
    server._stdout.read_line(&mut rest).unwrap();
    server._stdout.read_line(&mut rest).unwrap();
    assert!(rest.contains("1 isolated environment"), "{rest}");
}

#[cfg(unix)]
#[test]
fn usage_errors_are_exit_2() {
    let dir = project("usage");
    let shim = pax_shim("usage");
    for args in [
        &["--port", "notaport"][..],
        &["--host", "not-an-ip"],
        &["--model", "m"],
        &["--port"],
        &["extra"],
        &["--max-concurrent-work", "0"],
        &["--max-concurrent-work", "x"],
        &["--max-queued-work", "-1"],
        &["--max-queued-work"],
    ] {
        let out = command(&dir, "http://127.0.0.1:1/v1/chat/completions", &shim)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty());
    }
}

const OLD_LIB: &str = "pub fn payload_len(payload: &str) -> usize {\n    payload.len()\n}\n";
const TESTS: &str = "use fpfixture::canonical_fingerprint;\n\n#[test]\nfn pairs_are_sorted_and_joined() {\n    assert_eq!(canonical_fingerprint(\"b=2&a=1\"), \"a=1&b=2\");\n}\n";

fn pax_installed() -> bool {
    Command::new("pax")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Real PAX and real Cargo behind the service, with a mock model that only ever asks for the test
/// run. The project's tests cannot pass (the function does not exist), so the work can never be
/// verified: the point is that the real execution happened, was observed, and was not mistaken
/// for the goal being met. The model is mocked; PAX is real.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn real_pax_executes_through_the_service_and_a_failing_run_is_not_success() {
    if !pax_installed() {
        eprintln!("SKIPPED: PAX is not installed");
        return;
    }
    let dir = std::env::temp_dir().join(format!("chip-serve-realpax-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"fpfixture_serve\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nname = \"fpfixture\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), OLD_LIB).unwrap();
    std::fs::write(dir.join("tests/fingerprint.rs"), TESTS).unwrap();
    let model = common::start(
        200,
        &completion(r#"{"decision":"request_capability","capability":"pax.test"}"#),
        Duration::ZERO,
    )
    .await;
    let url = model.url.clone();
    let d = dir.clone();
    tokio::task::spawn_blocking(move || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_stack-agent"));
        c.arg("serve")
            .current_dir(&d)
            .env_remove("PAX_BIN")
            .env("CHIP_PROVIDER", "openai-compatible")
            .env("CHIP_MODEL", "mock-model")
            .env("CHIP_ENDPOINT", &url)
            .env("CHIP_API_KEY", "sk-serve-secret-never-printed");
        let server = launch(c, &["--port", "0"]);
        let addr = server.addr.clone();
        let (_, _, body) = http(
            &addr,
            "POST",
            "/v1/work",
            Some(r#"{"goal":"Make the tests pass."}"#),
        );
        let id = body["work_id"].as_str().unwrap().to_string();
        let mut state = Value::Null;
        for _ in 0..3000 {
            let (_, _, body) = http(&addr, "GET", &format!("/v1/work/{id}"), None);
            if body["status"] != "running" && body["status"] != "queued" {
                state = body;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(state["result"]["verified"], false, "{state}");
        assert_ne!(state["status"], "completed");
        assert_eq!(state["result"]["audit"]["clean"], true);
        assert!(
            state["result"]["pax_executions"].as_u64().unwrap() >= 1,
            "{state}"
        );
        assert_eq!(state["result"]["pax"]["last_status"], "failed", "{state}");
        let (_, _, ev) = http(&addr, "GET", &format!("/v1/work/{id}/events"), None);
        let kinds: Vec<&str> = ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["kind"].as_str().unwrap())
            .collect();
        // PAX reporting failed tests is a real execution that failed, observed as such.
        assert!(
            kinds.contains(&"ExecutionStarted") && kinds.contains(&"ExecutionFailed"),
            "{kinds:?}"
        );
        assert!(!kinds.contains(&"WorkCompleted"), "{kinds:?}");
        assert!(kinds.contains(&"ObservationRecorded"), "{kinds:?}");
        assert!(
            state["timing"]["execution_ms"].as_f64().unwrap() > 0.0,
            "{state}"
        );
        let (_, _, m) = http(&addr, "GET", "/v1/metrics", None);
        assert_eq!(
            m["max_concurrent_work"], 1,
            "one mutable local directory: one work at a time"
        );
        assert_eq!(m["started_work"], 1);
    })
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn finished_work_beyond_the_retention_limit_answers_410_through_the_real_binary() {
    let model = common::start(200, &completion(ESCAPE), Duration::ZERO).await;
    let (dir, shim, url) = (project("retain"), pax_shim("retain"), model.url.clone());
    tokio::task::spawn_blocking(move || {
        let server = launch(
            command(&dir, &url, &shim),
            &[
                "--host",
                "127.0.0.1",
                "--port",
                "0",
                "--max-retained-work",
                "1",
            ],
        );
        let addr = server.addr.clone();
        let mut ids = Vec::new();
        for _ in 0..3 {
            let (status, _, body) = http(
                &addr,
                "POST",
                "/v1/work",
                Some(r#"{"goal":"Add a function that sorts the payload."}"#),
            );
            assert_eq!(status, 202, "{body}");
            let id = body["work_id"].as_str().unwrap().to_string();
            // Sequential, so the oldest is the first to be evicted.
            for _ in 0..500 {
                let (_, _, m) = http(&addr, "GET", "/v1/metrics", None);
                if m["blocked_work"].as_u64() == Some(ids.len() as u64 + 1) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            ids.push(id);
        }
        for old in &ids[..2] {
            for (method, path) in [
                ("GET", format!("/v1/work/{old}")),
                ("GET", format!("/v1/work/{old}/events")),
                ("POST", format!("/v1/work/{old}/cancel")),
            ] {
                let (status, _, body) = http(&addr, method, &path, None);
                assert_eq!(status, 410, "{method} {path}: {body}");
                assert_eq!(body["error"]["code"], "work_expired");
            }
        }
        let (status, _, body) = http(&addr, "GET", &format!("/v1/work/{}", ids[2]), None);
        assert_eq!((status, body["status"].as_str()), (200, Some("blocked")));
        let (_, _, m) = http(&addr, "GET", "/v1/metrics", None);
        assert_eq!(
            (
                m["submitted_work"].as_u64(),
                m["blocked_work"].as_u64(),
                m["retained_work"].as_u64()
            ),
            (Some(3), Some(3), Some(1))
        );
        assert_eq!(m["max_retained_work"], 1);
        // The flag is validated like its neighbours.
        let out = command(&dir, &url, &shim)
            .args(["--max-retained-work", "0"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
    })
    .await
    .unwrap();
}

// ---- PAX is a provider for the work that needs a test run, not a prerequisite for the service ----------

/// `command`, minus any PAX: no `PAX_BIN`, and a `PATH` that holds nothing.
fn without_pax(tag: &str, dir: &PathBuf, model_url: &str) -> Command {
    let empty = std::env::temp_dir().join(format!("chip-serve-nopax-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    let mut c = command(dir, model_url, &PathBuf::from("/nonexistent/pax"));
    c.env_remove("PAX_BIN").env("PATH", empty);
    c
}

/// Sends a request whose body the server may refuse before it has all been sent. Returns the
/// status if the server answered, `None` if it simply closed the connection (both are a refusal;
/// what matters, and is checked afterwards, is that nothing was started).
fn http_status_or_close(addr: &str, path: &str, body: &str) -> Option<u16> {
    let mut stream = TcpStream::connect(addr).ok()?;
    let _ = write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let mut out = String::new();
    let _ = stream.read_to_string(&mut out);
    out.split(' ').nth(1).and_then(|s| s.parse().ok())
}

const LIST: &str = r#"{"decision":"request_capability","capability":"project.list"}"#;

fn wait_finished(addr: &str, id: &str) -> Value {
    for _ in 0..1000 {
        let (status, _, body) = http(addr, "GET", &format!("/v1/work/{id}"), None);
        assert_eq!(status, 200, "{body}");
        if body["status"] != "running" && body["status"] != "queued" {
            return body;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("work did not finish");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_service_starts_and_is_healthy_without_pax() {
    let model = common::start(200, &completion(LIST), Duration::ZERO).await;
    let (dir, url) = (project("nopax-health"), model.url.clone());
    tokio::task::spawn_blocking(move || {
        let server = launch(without_pax("health", &dir, &url), &["--port", "0"]);
        let (status, _, body) = http(&server.addr, "GET", "/health", None);
        assert_eq!((status, body), (200, serde_json::json!({"status": "ok"})));
        let (status, _, m) = http(&server.addr, "GET", "/v1/metrics", None);
        assert_eq!(status, 200);
        assert_eq!(m["submitted_work"], 0);
    })
    .await
    .unwrap();
    assert_eq!(
        model.captured.lock().await.len(),
        0,
        "no model call at startup"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_inspection_is_accepted_and_runs_through_the_runtime_without_pax() {
    let model = common::start(200, &completion(LIST), Duration::ZERO).await;
    let (dir, url) = (project("nopax-inspect"), model.url.clone());
    let finished = tokio::task::spawn_blocking(move || {
        let server = launch(without_pax("inspect", &dir, &url), &["--port", "0"]);
        let (status, _, accepted) = http(
            &server.addr,
            "POST",
            "/v1/work",
            Some(r#"{"goal":"What does this project contain?","kind":"inspect"}"#),
        );
        assert_eq!(status, 202, "{accepted}");
        let id = accepted["work_id"].as_str().unwrap().to_string();
        let done = wait_finished(&server.addr, &id);
        let (status, _, events) = http(&server.addr, "GET", &format!("/v1/work/{id}/events"), None);
        assert_eq!(status, 200);
        (done, events)
    })
    .await
    .unwrap();
    let (done, events) = finished;
    // The runtime ran it: the project was really listed, the model was really asked, and the
    // service reports the runtime's own terminal state, not a success of its own.
    assert!(model.captured.lock().await.len() >= 2);
    assert!(
        done["result"]["executions_by_capability"]["project.list"]
            .as_u64()
            .unwrap()
            >= 2
    );
    assert_eq!(done["kind"], "inspect");
    assert_eq!(done["result"]["verified"], false);
    assert_eq!(done["result"]["audit"]["clean"], true);
    assert_eq!(done["result"]["writes"], 0);
    assert_eq!(
        done["result"]["pax"]["last_status"],
        Value::Null,
        "no test was run"
    );
    assert_ne!(
        done["status"], "completed",
        "the mock never gave a grounded answer"
    );
    let kinds: Vec<&str> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"ExecutionCompleted"), "{kinds:?}");
    assert!(!done.to_string().contains("PAX unavailable"));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn work_that_needs_a_test_run_is_refused_at_the_door_without_pax() {
    let model = common::start(200, &completion(LIST), Duration::ZERO).await;
    let (dir, url) = (project("nopax-door"), model.url.clone());
    let shown_dir = dir.display().to_string();
    tokio::task::spawn_blocking(move || {
        let server = launch(without_pax("door", &dir, &url), &["--port", "0"]);
        for body in [
            r#"{"goal":"Fix it","kind":"change"}"#,
            r#"{"goal":"Does it pass?","kind":"verify"}"#,
            r#"{"goal":"Fix it"}"#,
        ] {
            let (status, _, refused) = http(&server.addr, "POST", "/v1/work", Some(body));
            assert_eq!(status, 503, "{body}: {refused}");
            assert_eq!(
                refused["error"]["code"], "test_runner_unavailable",
                "{refused}"
            );
            assert!(
                refused.get("work_id").is_none(),
                "nothing was accepted: {refused}"
            );
            let text = refused.to_string();
            assert!(
                !text.contains(&shown_dir) && !text.contains("/nonexistent"),
                "{text}"
            );
        }
        // The refusals started nothing and wedged nothing: an inspection is still accepted.
        let (_, _, m) = http(&server.addr, "GET", "/v1/metrics", None);
        assert_eq!(
            (m["submitted_work"].as_u64(), m["rejected_work"].as_u64()),
            (Some(0), Some(3))
        );
        let (status, _, _) = http(
            &server.addr,
            "POST",
            "/v1/work",
            Some(r#"{"goal":"What is here?","kind":"inspect"}"#),
        );
        assert_eq!(status, 202);
    })
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn with_a_test_runner_a_change_request_is_still_accepted() {
    // The refusal above is about PAX being absent, not about the kind: the original test of this
    // file already submits a default-kind (`change`) work with a PAX present and it is accepted.
    let model = common::start(200, &completion(LIST), Duration::ZERO).await;
    let (dir, shim, url) = (project("withpax"), pax_shim("withpax"), model.url.clone());
    tokio::task::spawn_blocking(move || {
        let server = launch(command(&dir, &url, &shim), &["--port", "0"]);
        for body in [
            r#"{"goal":"Fix it","kind":"change"}"#,
            r#"{"goal":"Does it pass?","kind":"verify"}"#,
        ] {
            let (status, _, accepted) = http(&server.addr, "POST", "/v1/work", Some(body));
            assert_eq!(status, 202, "{body}: {accepted}");
        }
    })
    .await
    .unwrap();
}

// ---- the local boundary over a real listener ---------------------------------------------------------

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_default_address_is_loopback_and_other_hosts_are_not_answered() {
    let model = common::start(200, &completion(LIST), Duration::ZERO).await;
    let (dir, url) = (project("loopback"), model.url.clone());
    tokio::task::spawn_blocking(move || {
        // No --host: the default must be loopback (the port is 0 only so the test cannot collide).
        let server = launch(without_pax("loopback", &dir, &url), &["--port", "0"]);
        assert!(server.addr.starts_with("127.0.0.1:"), "{}", server.addr);

        // A request that names another host (a browser on a rebound DNS name) is refused.
        let mut stream = TcpStream::connect(&server.addr).unwrap();
        write!(
            stream,
            "GET /health HTTP/1.1\r\nHost: attacker.example\r\nconnection: close\r\n\r\n"
        )
        .unwrap();
        let mut out = String::new();
        stream.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 403"), "{out}");
        assert!(out.contains("host_not_allowed"), "{out}");
    })
    .await
    .unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn malformed_oversized_and_authority_bearing_requests_start_nothing() {
    let model = common::start(200, &completion(LIST), Duration::ZERO).await;
    let (dir, url) = (project("refused"), model.url.clone());
    let secret_target = dir.display().to_string();
    tokio::task::spawn_blocking(move || {
        let server = launch(without_pax("refused", &dir, &url), &["--port", "0"]);
        let huge = format!(r#"{{"goal":"{}","kind":"inspect"}}"#, "x".repeat(64 * 1024));
        let escape = format!(r#"{{"goal":"look","kind":"inspect","root":"{secret_target}"}}"#);
        // Refused with a 4xx or by closing the connection; never accepted.
        if let Some(status) = http_status_or_close(&server.addr, "/v1/work", &huge) {
            assert!((400..500).contains(&status), "oversized: {status}");
        }
        for (name, body) in [
            ("not json", "this is not json".to_string()),
            ("empty goal", r#"{"goal":"","kind":"inspect"}"#.to_string()),
            (
                "unknown kind",
                r#"{"goal":"look","kind":"deploy"}"#.to_string(),
            ),
            ("names a workspace", escape),
            (
                "grants a capability",
                r#"{"goal":"look","kind":"inspect","capabilities":["project.write"]}"#.to_string(),
            ),
        ] {
            let (status, _, refused) = http(&server.addr, "POST", "/v1/work", Some(&body));
            assert!((400..500).contains(&status), "{name}: {status} {refused}");
            assert!(refused.get("work_id").is_none(), "{name}: {refused}");
        }
        // Unknown ids and ids that are not ids at all are explicit, and expose nothing.
        for id in ["work_0000000000000000", "..%2f..%2fetc%2fpasswd", "x"] {
            let (status, _, r) = http(&server.addr, "GET", &format!("/v1/work/{id}"), None);
            assert_eq!(status, 404, "{id}: {r}");
        }
        let (_, _, m) = http(&server.addr, "GET", "/v1/metrics", None);
        assert_eq!(m["submitted_work"], 0, "no request above started anything");
    })
    .await
    .unwrap();
    assert_eq!(
        model.captured.lock().await.len(),
        0,
        "the model was never asked"
    );
}
