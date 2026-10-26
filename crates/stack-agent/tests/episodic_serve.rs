//! Episodes through `stack-agent serve`, across real service restarts.
//!
//! The service runs works through the same `begin` / `run_durable` / `finish_verified` lifecycle as
//! `work`, and the same episode steps (`work_state::Episodes`): recall once the task is begun,
//! record once the host's verdict is committed. Each service here is a real process of the binary
//! over the project's default database in a shared runtime data directory; "killed" is SIGKILL.
//! The model is the scripted loopback endpoint, which records every request body.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Stdio;

use gated::{Model, PASSED, Pax, Reply, command, project, unique, wait_until};
use serde_json::Value;

const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;
const RECALLED: &str = "recalled episode chip.Episode:";
const GOAL: &str = "Add a function `two` returning 2.";
const SIMILAR: &str = "Add a function `two` returning 2 to the crate.";

struct Server {
    child: std::process::Child,
    addr: String,
    _stdout: BufReader<std::process::ChildStdout>,
}

impl Server {
    fn kill(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn serve(dir: &Path, model: &Model, pax: &Pax, data: &Path, extra: &[&str]) -> Server {
    let mut c = command(dir, &model.url, pax);
    c.env("STACK_AGENT_DATA_DIR", data);
    let mut child = c
        .args(["serve", "--port", "0"])
        .args(extra)
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
        .unwrap_or_else(|| panic!("unexpected first line: {line:?}"))
        .to_string();
    Server {
        child,
        addr,
        _stdout: stdout,
    }
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

fn submit(addr: &str, goal: &str) -> String {
    let body = format!(r#"{{"goal":"{goal}","kind":"change"}}"#);
    let (status, accepted) = http(addr, "POST", "/v1/work", Some(&body));
    assert_eq!(status, 202, "{accepted}");
    accepted["work_id"].as_str().unwrap().to_string()
}

/// Submits `goal` and waits for the work to end; its result.
async fn finished(server: &Server, goal: &str) -> Value {
    let id = submit(&server.addr, goal);
    let addr = server.addr.clone();
    wait_until("the work to end", || {
        let status = http(&addr, "GET", &format!("/v1/work/{id}"), None).1["status"].clone();
        status != "running" && status != "queued"
    })
    .await;
    http(&server.addr, "GET", &format!("/v1/work/{id}"), None).1
}

fn reset(dir: &Path) {
    std::fs::write(dir.join("src/lib.rs"), gated::LIB).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_records_a_verified_episode_and_a_restarted_service_recalls_it() {
    let pax = Pax::new("serve-episodes", PASSED, false);
    let dir = project("serve-episodes");
    let data = unique("serve-episodes-data");

    // A verified work through the service: recorded as a verified success.
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let mut first = serve(&dir, &model, &pax, &data, &[]);
    let done = finished(&first, GOAL).await;
    assert_eq!(done["status"], "completed", "{done}");
    let recorded = &done["result"]["episodes"]["recorded"];
    assert_eq!(recorded["outcome"], "verified_success", "{done}");
    assert_eq!(
        done["result"]["episodes"]["recall"]["considered"], 0,
        "{done}"
    );
    let episode = recorded["episode"].as_str().unwrap().to_string();
    assert!(model.bodies().iter().all(|b| !b.contains(RECALLED)));
    first.kill();

    // A new service process over the same database: a compatible work is told of the episode in
    // every model request, and is still executed and verified on its own evidence.
    reset(&dir);
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let second = serve(&dir, &model, &pax, &data, &[]);
    let again = finished(&second, SIMILAR).await;
    assert_eq!(again["status"], "completed", "{again}");
    let recall = &again["result"]["episodes"]["recall"];
    assert_eq!(
        recall["recalled"][0]["episode"],
        episode.as_str(),
        "{again}"
    );
    let bodies = model.bodies();
    assert_eq!(bodies.len(), 2);
    assert!(
        bodies
            .iter()
            .all(|b| b.contains(&format!("{RECALLED}{episode}"))),
        "{bodies:?}"
    );
    assert_eq!(again["result"]["verified"], true, "{again}");
    assert_eq!(again["result"]["audit"]["clean"], true, "{again}");
    assert_eq!(
        again["result"]["episodes"]["recorded"]["outcome"],
        "verified_success"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_with_no_recall_sends_exactly_the_request_a_service_without_episodes_sends() {
    let pax = Pax::new("serve-norecall", PASSED, false);
    let data = unique("serve-norecall-data");
    let dir = project("serve-norecall");

    // Seed: one verified episode in this project.
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let mut seed = serve(&dir, &model, &pax, &data, &[]);
    assert_eq!(finished(&seed, GOAL).await["status"], "completed");
    seed.kill();

    // With --no-recall, the first request of a matching work...
    reset(&dir);
    let off = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let mut without = serve(&dir, &off, &pax, &data, &["--no-recall"]);
    let result = finished(&without, GOAL).await;
    assert_eq!(
        result["result"]["episodes"]["recall"]["enabled"], false,
        "{result}"
    );
    assert_eq!(
        result["result"]["episodes"]["recorded"]["outcome"], "unknown",
        "a blocked work is still recorded, and is not a failure"
    );
    without.kill();

    // ...is byte for byte the first request of the same work in a project with no episodes.
    let fresh_dir = project("serve-norecall-fresh");
    let fresh_data = unique("serve-norecall-fresh-data");
    let empty = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let fresh = serve(&fresh_dir, &empty, &pax, &fresh_data, &[]);
    let result = finished(&fresh, GOAL).await;
    assert_eq!(result["result"]["episodes"]["recall"]["considered"], 0);
    assert_eq!(off.bodies()[0], empty.bodies()[0]);
    assert!(!off.bodies()[0].contains(RECALLED));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_service_killed_mid_work_leaves_no_episode_for_it() {
    let pax = Pax::new("serve-killed", PASSED, false);
    let dir = project("serve-killed");
    let data = unique("serve-killed-data");
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stall("")]).await;
    let mut first = serve(&dir, &model, &pax, &data, &[]);
    let _running = submit(&first.addr, GOAL);
    model.wait_requests(2).await;
    first.kill(); // SIGKILL while the work waits on its next model call

    // The next service and its work: the killed work's attempt has no result and no episode, so
    // there is nothing to consider.
    reset(&dir);
    let next = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let second = serve(&dir, &next, &pax, &data, &[]);
    let result = finished(&second, GOAL).await;
    assert_eq!(
        result["result"]["episodes"]["recall"]["considered"], 0,
        "{result}"
    );
    assert!(next.bodies().iter().all(|b| !b.contains(RECALLED)));
}
