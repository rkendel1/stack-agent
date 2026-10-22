//! One task contract for every entry point: `stack-agent work` and `stack-agent serve` agree on a
//! task's identity, on what a crash can leave behind and what each state means, and on how it ended.
//!
//! Admission is universal as an *interpretation*: every task a supported entry point creates has a
//! `chip.Admission` record saying what it was asked, by whom (`origin`: `service` or `cli`). It is
//! not universal as a *schedule*: only work a client was told was queued (`service`) is started by a
//! restart. A CLI invocation that died before it ran was never acknowledged to anyone, so it is left
//! for an explicit `--task-id` continuation.
//!
//! The real binaries run over one shared `--state-db` file. A crash at a commit boundary leaves
//! exactly what the real store operations leave once they have stopped, so each durable state a
//! crash can produce is built with those operations and then read by the real binaries. "Killed"
//! elsewhere is SIGKILL. The model is the scripted loopback endpoint and PAX the usual stand-in.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use chip_agent_state::feltdb::{AtomicMutation, FeltDb};
use chip_agent_state::{
    ADMISSION_COLLECTION, ADMISSION_HANDOFF, Admission, AdmissionOrigin, AgentStore, DurableConfig,
    admission, admit, waiting,
};
use gated::{Model, PASSED, Pax, Reply, command, project, unique, wait_until};
use serde_json::Value;

const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;
const GOAL: &str = "Add `two`.";

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

fn start(c: std::process::Command, data: &Path, extra: &[&str]) -> Server {
    let mut c = c;
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

fn serve(w: &World, model: &Model) -> Server {
    start(
        command(&w.dir, &model.url, &w.pax),
        &w.data,
        &["--state-db", w.db.to_str().unwrap()],
    )
}

fn serve_without_pax(w: &World, model: &Model) -> Server {
    let empty = unique("cli-admission-nopax");
    std::fs::create_dir_all(&empty).unwrap();
    let mut c = command(&w.dir, &model.url, &w.pax);
    c.env_remove("PAX_BIN").env("PATH", empty);
    start(c, &w.data, &["--state-db", w.db.to_str().unwrap()])
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

fn post(addr: &str, goal: &str, kind: &str) -> (u16, Value) {
    let body = format!(r#"{{"goal":"{goal}","kind":"{kind}"}}"#);
    http(addr, "POST", "/v1/work", Some(&body))
}

fn submit(addr: &str, goal: &str, kind: &str) -> String {
    let (status, accepted) = post(addr, goal, kind);
    assert_eq!(status, 202, "{accepted}");
    accepted["work_id"].as_str().unwrap().to_string()
}

fn get(addr: &str, id: &str) -> (u16, Value) {
    http(addr, "GET", &format!("/v1/work/{id}"), None)
}

fn status_of(addr: &str, id: &str) -> String {
    get(addr, id).1["status"]
        .as_str()
        .unwrap_or("-")
        .to_string()
}

fn metrics(addr: &str) -> Value {
    http(addr, "GET", "/v1/metrics", None).1
}

struct World {
    dir: PathBuf,
    data: PathBuf,
    db: PathBuf,
    pax: Pax,
}

fn world(tag: &str) -> World {
    let data = unique(&format!("{tag}-data"));
    std::fs::create_dir_all(&data).unwrap();
    World {
        dir: project(tag),
        db: data.join("shared.feltdb"),
        data,
        pax: Pax::new(tag, PASSED, false),
    }
}

/// `stack-agent work <goal> --json --state-db <shared> <extra...>`, to completion.
async fn work(w: &World, model: &Model, goal: &str, extra: &[&str]) -> Output {
    let mut c = command(&w.dir, &model.url, &w.pax);
    c.env("STACK_AGENT_DATA_DIR", &w.data)
        .args(["work", goal, "--json", "--state-db", w.db.to_str().unwrap()])
        .args(extra);
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

fn config() -> DurableConfig {
    DurableConfig::default()
}

fn store(w: &World) -> AgentStore {
    AgentStore::new(FeltDb::open(&w.db).unwrap())
}

fn task_status(w: &World, id: &str) -> Option<String> {
    let s = store(w);
    s.tasks()
        .unwrap()
        .into_iter()
        .find(|t| t.task_id == id)
        .map(|t| t.status)
}

fn task_count(w: &World, id: &str) -> usize {
    store(w)
        .tasks()
        .unwrap()
        .iter()
        .filter(|t| t.task_id == id)
        .count()
}

/// Writes or removes one admission record directly, as a crash or damage would leave it.
fn put_admission(w: &World, id: &str, record: Option<Value>) {
    let db = FeltDb::open(&w.db).unwrap();
    db.apply_atomic_transaction_guarded(
        "test-admission",
        None,
        None,
        &[],
        &[],
        &[AtomicMutation {
            capability: ADMISSION_COLLECTION.into(),
            key: format!("{ADMISSION_COLLECTION}:{id}"),
            value: record,
        }],
        None,
    )
    .unwrap();
}

fn record(id: &str, kind: &str, goal: &str, origin: &str) -> Value {
    serde_json::json!({
        "schema": "chip.admission.v1", "task_id": id, "kind": kind, "goal": goal,
        "admitted_at_ms": 1, "origin": origin,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cli_work_is_admitted_as_cli_work_and_the_service_reads_back_the_same_outcome() {
    let w = world("cli-normal");
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let out = work(&w, &model, GOAL, &[]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let cli = report(&out);
    let id = cli["agent_state"]["task_id"].as_str().unwrap().to_string();
    assert_eq!(cli["terminal_state"], "completed");
    assert_eq!(cli["verified"], true);

    // The task has the record every supported entry point leaves: what it was asked, and by whom.
    {
        let s = store(&w);
        match admission(&s, &id) {
            Admission::Valid(r) => {
                assert_eq!((r.kind.as_str(), r.goal.as_str()), ("change", GOAL));
                assert_eq!(r.origin, AdmissionOrigin::Cli);
            }
            other => panic!("{other:?}"),
        }
        assert!(waiting(&s).unwrap().is_empty(), "nothing is left waiting");
    }
    assert_eq!(task_status(&w, &id).as_deref(), Some("completed"));

    // A service started over the same database agrees with the CLI about how it ended.
    let restart = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let service = serve(&w, &restart);
    let (status, work) = get(&service.addr, &id);
    assert_eq!(status, 200, "{work}");
    assert_eq!(work["status"], "completed");
    for field in ["verified", "goal_satisfied", "grounded"] {
        assert_eq!(
            work["result"][field], cli[field],
            "{field}: the CLI and the service agree"
        );
    }
    assert_eq!(
        restart.requests(),
        0,
        "a finished task is never started again"
    );
    assert_eq!(metrics(&service.addr)["recovered_work"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cli_task_that_never_started_is_reported_not_started_by_a_service_and_continued_explicitly()
 {
    let w = world("cli-ready");
    // The state a crash right after the CLI's admission commit leaves: admitted, not started.
    let id = "work_cliready00000001";
    {
        let s = store(&w);
        admit(
            &s,
            id,
            "change",
            GOAL,
            GOAL,
            AdmissionOrigin::Cli,
            &config(),
        )
        .unwrap();
        assert_eq!(s.tasks().unwrap()[0].status, "ready");
    }
    std::thread::sleep(ADMISSION_HANDOFF);

    // A service starting up does not run it: nobody was told it was queued.
    let m1 = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let mut service = serve(&w, &m1);
    let (status, waiting_work) = get(&service.addr, id);
    assert_eq!(status, 200, "{waiting_work}");
    assert_eq!(waiting_work["status"], "queued");
    assert_eq!(waiting_work["scheduled"], false);
    let reason = waiting_work["reason"].as_str().unwrap();
    assert!(
        reason.contains("created by `stack-agent work`") && reason.contains("--task-id"),
        "{reason}"
    );
    wait_until("a moment for any start", || true).await;
    assert_eq!(m1.requests(), 0, "a CLI task is not started by a restart");
    assert_eq!(metrics(&service.addr)["recovered_work"], 0);
    service.kill();

    // The explicit continuation starts it once, under the same id.
    let m2 = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let out = work(&w, &m2, GOAL, &["--task-id", id]).await;
    assert_eq!(
        report(&out)["agent_state"]["task_id"],
        id,
        "{}",
        stderr(&out)
    );
    assert_eq!(m2.requests(), 1);
    assert_eq!(task_count(&w, id), 1, "no second identity");
    assert!(
        matches!(admission(&store(&w), id), Admission::Valid(r) if r.origin == AdmissionOrigin::Cli)
    );
    assert_eq!(task_status(&w, id).as_deref(), Some("blocked"));

    // And it is finished: a later service does not enqueue it.
    let m3 = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let again = serve(&w, &m3);
    assert_eq!(status_of(&again.addr, id), "blocked");
    assert_eq!(m3.requests(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crash_that_left_only_the_admission_record_is_finished_by_the_same_request_only() {
    let w = world("cli-record-only");
    let id = "work_clirecord0000001";
    // A crash after the record and before the task: a record with no task.
    put_admission(&w, id, Some(record(id, "change", GOAL, "cli")));
    assert_eq!(task_count(&w, id), 0);

    // A different request for that id is refused and starts nothing.
    let m1 = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let out = work(&w, &m1, "Something else entirely.", &["--task-id", id]).await;
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("nothing was run"), "{}", stderr(&out));
    assert_eq!(m1.requests(), 0);
    assert_eq!(task_count(&w, id), 0, "the refusal created no task");

    // The same request completes the admission and runs once.
    let m2 = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let out = work(&w, &m2, GOAL, &["--task-id", id]).await;
    assert_eq!(
        report(&out)["agent_state"]["task_id"],
        id,
        "{}",
        stderr(&out)
    );
    assert_eq!(m2.requests(), 1);
    assert_eq!(task_count(&w, id), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ready_task_with_no_admission_is_described_for_what_it_is_and_never_guessed_at() {
    let w = world("cli-no-record");
    let id = "work_clinorecord00001";
    {
        let s = store(&w);
        admit(
            &s,
            id,
            "change",
            GOAL,
            GOAL,
            AdmissionOrigin::Cli,
            &config(),
        )
        .unwrap();
    }
    put_admission(&w, id, None);
    std::thread::sleep(ADMISSION_HANDOFF);

    let model = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let mut service = serve(&w, &model);
    let (status, view) = get(&service.addr, id);
    assert_eq!(status, 200, "{view}");
    assert_eq!(view["scheduled"], false);
    let reason = view["reason"].as_str().unwrap();
    assert!(
        reason.contains("no admission record") && reason.contains("never started"),
        "{reason}"
    );
    wait_until("a moment for any start", || true).await;
    assert_eq!(
        model.requests(),
        0,
        "what it was asked is unknown, so it is not run"
    );
    service.kill();
    assert_eq!(
        task_status(&w, id).as_deref(),
        Some("ready"),
        "left exactly as it was"
    );

    // An operator who knows the goal continues it explicitly.
    let m2 = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let out = work(&w, &m2, GOAL, &["--task-id", id]).await;
    assert_eq!(
        report(&out)["agent_state"]["task_id"],
        id,
        "{}",
        stderr(&out)
    );
    assert_eq!(m2.requests(), 1);
}

/// A service with one work on the only worker and `queued` behind it, killed.
async fn service_killed_with(w: &World, queued: &[(&str, &str)]) -> (String, Vec<String>) {
    let model = Model::start(vec![Reply::Stall("")]).await;
    let mut first = serve(w, &model);
    let running = submit(&first.addr, "Occupy the worker.", "inspect");
    model.wait_requests(1).await;
    let ids = queued
        .iter()
        .map(|(goal, kind)| submit(&first.addr, goal, kind))
        .collect();
    first.kill();
    (running, ids)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_service_admitted_task_is_continued_by_the_cli_under_its_own_id_and_started_once() {
    let w = world("cli-continues-service");
    let (running, ids) = service_killed_with(&w, &[("Queued work zero.", "change")]).await;
    let queued = &ids[0];

    let model = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let out = work(&w, &model, "Queued work zero.", &["--task-id", queued]).await;
    assert_eq!(
        report(&out)["agent_state"]["task_id"],
        queued.as_str(),
        "{}",
        stderr(&out)
    );
    assert_eq!(model.requests(), 1);
    assert_eq!(
        task_count(&w, queued),
        1,
        "the CLI continued the service's task, not a new one"
    );
    assert!(
        matches!(admission(&store(&w), queued), Admission::Valid(r) if r.origin == AdmissionOrigin::Service)
    );

    // A service started afterwards does not start it again; the work that was running stays
    // interrupted.
    let restart = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let service = serve(&w, &restart);
    assert_eq!(status_of(&service.addr, queued), "blocked");
    assert_eq!(status_of(&service.addr, &running), "interrupted");
    assert_eq!(restart.requests(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_continuation_cannot_widen_what_the_work_was_admitted_to_do() {
    let w = world("cli-no-widening");
    let (_running, ids) = service_killed_with(&w, &[("Only look around.", "inspect")]).await;
    let id = &ids[0];

    // Admitted as an inspection, so it is not continued as a change (the CLI's default kind).
    let m1 = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let out = work(&w, &m1, "Only look around.", &["--task-id", id]).await;
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("cannot be continued as `change`"),
        "{}",
        stderr(&out)
    );
    assert_eq!(m1.requests(), 0);
    assert_eq!(
        task_status(&w, id).as_deref(),
        Some("ready"),
        "the refusal changed nothing"
    );

    // As what it was admitted as, it runs.
    let m2 = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let out = work(
        &w,
        &m2,
        "Only look around.",
        &["--task-id", id, "--kind", "inspect"],
    )
    .await;
    assert_eq!(
        report(&out)["agent_state"]["task_id"],
        id.as_str(),
        "{}",
        stderr(&out)
    );
    assert_eq!(m2.requests(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_work_is_not_taken_over_by_a_continuation_while_its_lease_is_held() {
    let w = world("cli-lease");
    let (running, _) = service_killed_with(&w, &[]).await;

    // The process that ran it is gone, but its lease has not run out: existing behavior, unchanged.
    // The id is not reused for a new task and nothing is replayed.
    let model = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let out = work(
        &w,
        &model,
        "Occupy the worker.",
        &["--task-id", &running, "--kind", "inspect"],
    )
    .await;
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("nothing was run"), "{}", stderr(&out));
    assert_eq!(model.requests(), 0);
    assert_eq!(task_count(&w, &running), 1);
    assert_eq!(task_status(&w, &running).as_deref(), Some("running"));
}

#[tokio::test(flavor = "multi_thread")]
async fn recovered_change_work_without_a_test_runner_stays_unstarted_across_entry_points() {
    let w = world("cli-no-pax");
    let (_running, ids) = service_killed_with(&w, &[("Queued change.", "change")]).await;
    let id = &ids[0];

    let model = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let mut service = serve_without_pax(&w, &model);
    let (_, view) = get(&service.addr, id);
    assert_eq!(view["scheduled"], false);
    assert!(
        view["reason"].as_str().unwrap().contains("test runner"),
        "{view}"
    );
    wait_until("a moment for any start", || true).await;
    assert_eq!(
        model.requests(),
        0,
        "recovery did not turn a missing runner into an unverified run"
    );
    assert_ne!(view["result"]["verified"], true);
    service.kill();
    assert_eq!(task_status(&w, id).as_deref(), Some("ready"));
}
