//! Durable work events through FeltDB, across real process termination and restart.
//!
//! Every `work` and `serve` work is a FeltDB task by default: its intents, observations, every
//! event of its trajectory with the provenance of its fact, and the host's verdict. Each command
//! here is a new process of the real binary over a real database file in a shared runtime data
//! directory; "killed" is SIGKILL while the work waits on a model call, so nothing gets to clean
//! up. The model is the scripted loopback endpoint and PAX the usual stand-in.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use gated::{Model, PASSED, Pax, Reply, command, project, unique, wait_until};
use serde_json::Value;

const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;

/// The binary over `dir`, with its runtime data (and so its default database) in `data`.
fn binary(dir: &Path, model: &Model, pax: &Pax, data: &Path) -> Command {
    let mut c = command(dir, &model.url, pax);
    c.env("STACK_AGENT_DATA_DIR", data);
    c
}

async fn output(mut c: Command) -> Output {
    tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap()
}

fn report(out: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().last().unwrap_or_else(|| {
        panic!(
            "no report; stderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {stdout}"))
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `agent-state <args> --json` over the project's default database.
async fn agent_state(dir: &Path, model: &Model, pax: &Pax, data: &Path, args: &[&str]) -> Value {
    let mut c = binary(dir, model, pax, data);
    c.arg("agent-state").args(args).arg("--json");
    let out = output(c).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    report(&out)
}

fn kinds(events: &Value) -> Vec<String> {
    events
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"]["kind"].as_str().unwrap().to_string())
        .collect()
}

fn provenance_of(events: &Value, kind: &str) -> Vec<String> {
    events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["kind"] == kind)
        .map(|e| e["provenance"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn every_work_has_its_own_id_and_its_events_and_verdict_are_durable() {
    let model = Model::start(vec![
        Reply::Stream(WRITE),
        Reply::Stream(TEST),
        Reply::Stream(WRITE),
        Reply::Stream(TEST),
    ])
    .await;
    let pax = Pax::new("events-verified", PASSED, false);
    let dir = project("events-verified");
    let data = unique("events-verified-data");

    let mut ids = Vec::new();
    for _ in 0..2 {
        // Each run has a change to make: a rewrite to the same content would change nothing.
        std::fs::write(dir.join("src/lib.rs"), gated::LIB).unwrap();
        let mut c = binary(&dir, &model, &pax, &data);
        c.args(["work", "Add `two`.", "--json"]);
        let out = output(c).await;
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        let state = report(&out)["agent_state"].clone();
        assert_eq!(state["recorded"], true, "{state}");
        assert_eq!(state["database"], "default");
        assert!(state["events_recorded"].as_u64().unwrap() > 5, "{state}");
        assert!(state["persistence_failures"].as_array().unwrap().is_empty());
        let id = state["work_id"].as_str().unwrap().to_string();
        assert!(id.starts_with("work_") && id.len() == 21, "{id}");
        assert_eq!(
            state["task_id"],
            id.as_str(),
            "the work's id is its task id"
        );
        ids.push(id);
    }
    assert_ne!(
        ids[0], ids[1],
        "two runs never share an id (before, both were `work`)"
    );
    // The default database is private: it holds the goal and what the work read.
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(data.join("agent-state"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    let shown = agent_state(&dir, &model, &pax, &data, &["show", "--task-id", &ids[0]]).await;
    let events = &shown["events"];
    let k = kinds(events);
    assert_eq!(k.first().map(String::as_str), Some("WorkStarted"));
    assert_eq!(k.last().map(String::as_str), Some("WorkCompleted"));
    for kind in [
        "ModelEscalation",
        "ModelCalled",
        "DecisionMade",
        "CapabilityRequested",
        "ExecutionRequested",
        "ExecutionCompleted",
        "ObservationRecorded",
        "EvidenceRecorded",
    ] {
        assert!(k.iter().any(|e| e == kind), "{kind} in {k:?}");
    }
    // Where each fact came from: reality, the model, or Chip.
    assert!(
        provenance_of(events, "ExecutionCompleted")
            .iter()
            .all(|p| p == "host_observed")
    );
    assert!(
        provenance_of(events, "ModelCalled")
            .iter()
            .all(|p| p == "host_observed")
    );
    assert!(
        provenance_of(events, "DecisionMade")
            .iter()
            .all(|p| p == "model_inferred")
    );
    assert!(
        provenance_of(events, "CapabilityRequested")
            .iter()
            .all(|p| p == "model_inferred")
    );
    assert!(
        provenance_of(events, "WorkCompleted")
            .iter()
            .all(|p| p == "runtime_decided")
    );
    // Every execution's event names the observation record that holds its output.
    let completed = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["event"]["kind"] == "ExecutionCompleted")
        .unwrap();
    assert!(
        completed["event"]["observation_record"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{}~", ids[0]))
    );
    // The host's verdict, beside Chip's outcome.
    let result = &shown["results"][0];
    assert_eq!(result["outcome"], "completed");
    assert_eq!(result["verification"]["verified"], true);
    assert_eq!(result["verification"]["accepted"], true);
    assert_eq!(result["verification"]["exit_status"], 0);
    assert_eq!(shown["task"]["status"], "completed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_work_keeps_what_it_recorded_and_the_next_run_reports_it_interrupted() {
    // The write runs; the next model call never answers, and the process is killed waiting on it.
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stall("")]).await;
    let pax = Pax::new("events-killed", PASSED, false);
    let dir = project("events-killed");
    let data = unique("events-killed-data");

    let mut c = binary(&dir, &model, &pax, &data);
    c.args(["work", "Add `two`.", "--json"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    model.wait_requests(2).await;
    child.kill().unwrap(); // SIGKILL: no handler, no cleanup
    child.wait().unwrap();
    assert!(
        std::fs::read_to_string(dir.join("src/lib.rs"))
            .unwrap()
            .contains("two"),
        "the write took effect before the kill"
    );

    let listed = agent_state(&dir, &model, &pax, &data, &["list"]).await;
    let tasks = listed["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1, "{listed}");
    let killed = tasks[0]["task_id"].as_str().unwrap().to_string();
    assert_eq!(tasks[0]["status"], "interrupted");
    assert_eq!(
        tasks[0]["unresolved_intents"], 0,
        "the write's outcome was recorded"
    );

    let shown = agent_state(&dir, &model, &pax, &data, &["show", "--task-id", &killed]).await;
    let k = kinds(&shown["events"]);
    // Everything up to the stalled call is there, in order, and nothing after it is invented.
    assert_eq!(k.first().map(String::as_str), Some("WorkStarted"));
    assert!(k.iter().any(|e| e == "ExecutionCompleted"), "{k:?}");
    assert_eq!(
        k.last().map(String::as_str),
        Some("ModelEscalation"),
        "the last event is the call it was waiting on: {k:?}"
    );
    assert!(
        !k.iter()
            .any(|e| e.starts_with("Work") && e != "WorkStarted"),
        "{k:?}"
    );
    assert!(
        shown["results"].as_array().unwrap().is_empty(),
        "no result was invented"
    );

    // A new process in the same project: it says what was interrupted, and is its own work.
    let restart = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let mut c = binary(&dir, &restart, &pax, &data);
    c.args(["work", "Add `two`.", "--json"]);
    let out = output(c).await;
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&format!("work {killed} was interrupted")),
        "{}",
        stderr(&out)
    );
    let state = report(&out)["agent_state"].clone();
    assert_eq!(
        state["interrupted"][0]["task_id"],
        killed.as_str(),
        "{state}"
    );
    assert_ne!(state["work_id"], killed.as_str());
    assert_eq!(state["task"]["status"], "blocked");
}

#[tokio::test(flavor = "multi_thread")]
async fn no_state_persists_nothing_and_options_that_need_state_are_usage_errors() {
    let model = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let pax = Pax::new("events-off", PASSED, false);
    let dir = project("events-off");
    let data = unique("events-off-data");
    let mut c = binary(&dir, &model, &pax, &data);
    c.args(["work", "x", "--json", "--no-state"]);
    let out = output(c).await;
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(report(&out)["agent_state"]["recorded"], false);
    assert!(
        !data.join("agent-state").exists(),
        "no database was created"
    );

    // --no-state with options that need state is a usage error.
    for args in [
        &["work", "x", "--no-state", "--task-id", "t"][..],
        &["work", "x", "--no-state", "--lease-secs", "10"],
    ] {
        let mut c = binary(&dir, &model, &pax, &data);
        c.args(args);
        assert_eq!(output(c).await.status.code(), Some(2), "{args:?}");
    }
}

// ---- serve ---------------------------------------------------------------------------------------

struct Server {
    child: std::process::Child,
    addr: String,
    stderr: PathBuf,
    _stdout: BufReader<std::process::ChildStdout>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn serve(dir: &Path, model: &Model, pax: &Pax, data: &Path, tag: &str) -> Server {
    let log = unique(&format!("serve-log-{tag}")).join("stderr");
    let mut child = binary(dir, model, pax, data)
        .args(["serve", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(&log).unwrap())
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
        stderr: log,
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

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_service_answers_for_the_work_a_killed_one_was_running() {
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stall("")]).await;
    let pax = Pax::new("serve-killed", PASSED, false);
    let dir = project("serve-killed");
    let data = unique("serve-killed-data");

    let mut first = serve(&dir, &model, &pax, &data, "first");
    let (status, accepted) = http(
        &first.addr,
        "POST",
        "/v1/work",
        Some(r#"{"goal":"Add `two`."}"#),
    );
    assert_eq!(status, 202, "{accepted}");
    let id = accepted["work_id"].as_str().unwrap().to_string();
    assert!(id.starts_with("work_") && id.len() == 21, "{id}");
    model.wait_requests(2).await;
    first.child.kill().unwrap();
    first.child.wait().unwrap();

    // Before, a restarted service answered every earlier id with 404 (audit HA-18).
    let restart = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let second = serve(&dir, &restart, &pax, &data, "second");
    let (status, work) = http(&second.addr, "GET", &format!("/v1/work/{id}"), None);
    assert_eq!(status, 200, "{work}");
    assert_eq!(work["status"], "interrupted");
    assert_eq!(work["persisted"], true);
    assert_eq!(work["result"], Value::Null, "no outcome is invented for it");
    let (status, events) = http(&second.addr, "GET", &format!("/v1/work/{id}/events"), None);
    assert_eq!(status, 200);
    assert_eq!(events["complete"], true);
    let listed = events["events"].as_array().unwrap();
    let write = listed
        .iter()
        .find(|e| e["kind"] == "ExecutionCompleted")
        .unwrap_or_else(|| panic!("{events}"));
    assert_eq!(write["provenance"], "host_observed");
    assert!(
        listed
            .iter()
            .any(|e| e["kind"] == "DecisionMade" && e["provenance"] == "model_inferred")
    );
    let noted = std::fs::read_to_string(&second.stderr).unwrap();
    assert!(
        noted.contains(&format!("work {id} was interrupted")),
        "{noted}"
    );

    // The new service's own work is a new id, persisted the same way.
    let (status, accepted) = http(
        &second.addr,
        "POST",
        "/v1/work",
        Some(r#"{"goal":"Add `two`."}"#),
    );
    assert_eq!(status, 202);
    let next = accepted["work_id"].as_str().unwrap().to_string();
    assert_ne!(next, id);
    wait_until("the new work to finish", || {
        http(&second.addr, "GET", &format!("/v1/work/{next}"), None).1["status"] == "blocked"
    })
    .await;
    let (_, done) = http(&second.addr, "GET", &format!("/v1/work/{next}"), None);
    assert_eq!(done["result"]["agent_state"]["recorded"], true, "{done}");
    assert_eq!(done["result"]["agent_state"]["task"]["status"], "blocked");
}
