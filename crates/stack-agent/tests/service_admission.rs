//! Durable admission: when `POST /v1/work` says 202, the work is a FeltDB task, so its id survives a
//! restart and a restart starts it, once, only if it never started.
//!
//! Every test here is a real `serve` process over a real database file in a shared runtime data
//! directory. "Killed" is SIGKILL, so nothing gets to clean up, and "restarted" is a new process.
//! The model is the scripted loopback endpoint and PAX the usual stand-in. A work that was running
//! when its process was killed is `interrupted` and is never run again on its own; a work that was
//! only waiting (FeltDB `ready`) provably never started and is queued again.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use chip_agent_state::feltdb::{AtomicMutation, FeltDb};
use chip_agent_state::{ADMISSION_COLLECTION, AgentStore};
use gated::{Model, PASSED, Pax, Reply, command, project, unique, wait_until};
use serde_json::Value;

const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;

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

fn serve(dir: &Path, model: &Model, pax: &Pax, data: &Path) -> Server {
    start(command(dir, &model.url, pax), data, &[])
}

fn serve_with(dir: &Path, model: &Model, pax: &Pax, data: &Path, extra: &[&str]) -> Server {
    start(command(dir, &model.url, pax), data, extra)
}

/// A service whose `PATH` and `PAX_BIN` name no PAX at all.
fn serve_without_pax(dir: &Path, model: &Model, data: &Path) -> Server {
    let empty = unique("recovery-nopax-path");
    std::fs::create_dir_all(&empty).unwrap();
    let missing = Pax::new("recovery-unused", PASSED, false);
    let mut c = command(dir, &model.url, &missing);
    c.env_remove("PAX_BIN").env("PATH", empty);
    start(c, data, &[])
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

fn events(addr: &str, id: &str) -> Value {
    http(addr, "GET", &format!("/v1/work/{id}/events"), None).1
}

fn started_count(addr: &str, id: &str) -> usize {
    events(addr, id)["events"]
        .as_array()
        .map(|e| e.iter().filter(|e| e["kind"] == "WorkStarted").count())
        .unwrap_or(0)
}

fn metrics(addr: &str) -> Value {
    http(addr, "GET", "/v1/metrics", None).1
}

fn database(data: &Path) -> PathBuf {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|e| e == "feltdb") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(data, &mut found);
    assert_eq!(found.len(), 1, "{found:?}");
    found.remove(0)
}

struct World {
    dir: PathBuf,
    data: PathBuf,
    pax: Pax,
}

fn world(tag: &str) -> World {
    World {
        dir: project(tag),
        data: unique(&format!("{tag}-data")),
        pax: Pax::new(tag, PASSED, false),
    }
}

/// A service with one work occupying the only worker (its model call stalls), which is killed
/// with `queued` works behind it, in order.
struct Stalled {
    running: String,
    queued: Vec<String>,
}

async fn kill_with_queue(w: &World, kinds: &[&str], extra: &[&str]) -> Stalled {
    let model = Model::start(vec![Reply::Stall("")]).await;
    let mut first = serve_with(&w.dir, &model, &w.pax, &w.data, extra);
    let running = submit(&first.addr, "Occupy the worker.", "inspect");
    model.wait_requests(1).await;
    assert_eq!(status_of(&first.addr, &running), "running");
    let queued: Vec<String> = kinds
        .iter()
        .enumerate()
        .map(|(n, kind)| submit(&first.addr, &format!("Queued work {n}."), kind))
        .collect();
    for id in &queued {
        assert_eq!(status_of(&first.addr, id), "queued");
    }
    first.kill();
    Stalled { running, queued }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_202_is_a_durable_admission_the_id_survives_a_kill_and_the_work_then_runs_once() {
    let w = world("admit-durable");
    let model = Model::start(vec![Reply::Stall("")]).await;
    let mut first = serve(&w.dir, &model, &w.pax, &w.data);
    let running = submit(&first.addr, "Occupy the worker.", "inspect");
    model.wait_requests(1).await;
    let (status, accepted) = post(&first.addr, "Add `two`.", "change");
    assert_eq!(status, 202, "{accepted}");
    let queued = accepted["work_id"].as_str().unwrap().to_string();
    // The acknowledgement says what is true: queued, and durable.
    assert_eq!(accepted["status"], "queued");
    assert_eq!(accepted["durable"], true);
    first.kill();

    let restart = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let second = serve(&w.dir, &restart, &w.pax, &w.data);
    // The same id resolves, and the work was started, once, by the restarted service.
    wait_until("the recovered work to finish", || {
        status_of(&second.addr, &queued) == "blocked"
    })
    .await;
    let (status, work) = get(&second.addr, &queued);
    assert_eq!(status, 200, "{work}");
    assert_eq!(work["work_id"], queued.as_str());
    assert_eq!(work["goal"], "Add `two`.");
    assert_eq!(work["kind"], "change");
    assert_eq!(work["result"]["agent_state"]["task_id"], queued.as_str());
    assert_ne!(work["status"], "completed", "queued is not verified");
    assert_eq!(
        restart.requests(),
        1,
        "one model call: the recovered work, nothing else"
    );
    assert_eq!(started_count(&second.addr, &queued), 1);
    // The work that was running when the process died is not run again.
    assert_eq!(status_of(&second.addr, &running), "interrupted");
    assert_eq!(metrics(&second.addr)["recovered_work"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_never_starts_one_admission_twice_however_often_it_repeats() {
    let w = world("admit-twice");
    let s = kill_with_queue(&w, &["inspect", "inspect"], &[]).await;
    let (b, c) = (&s.queued[0], &s.queued[1]);

    // Restart 1: the oldest queued work starts (and stalls); the other waits. Then it is killed.
    let m1 = Model::start(vec![Reply::Stall("")]).await;
    let mut second = serve(&w.dir, &m1, &w.pax, &w.data);
    m1.wait_requests(1).await;
    assert_eq!(
        status_of(&second.addr, b),
        "running",
        "oldest admission first"
    );
    assert_eq!(status_of(&second.addr, c), "queued");
    second.kill();

    // Restart 2: the one that had started is interrupted, not requeued; the other now starts.
    let m2 = Model::start(vec![Reply::Stall("")]).await;
    let third = serve(&w.dir, &m2, &w.pax, &w.data);
    m2.wait_requests(1).await;
    assert_eq!(status_of(&third.addr, b), "interrupted");
    assert_eq!(status_of(&third.addr, c), "running");
    assert_eq!(status_of(&third.addr, &s.running), "interrupted");
    assert_eq!(
        (m1.requests(), m2.requests()),
        (1, 1),
        "each ran once, in its own restart"
    );
    for id in [b, c] {
        assert_eq!(
            started_count(&third.addr, id),
            1,
            "{id} started exactly once"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_work_cancelled_while_queued_is_recorded_and_not_started_after_a_restart() {
    let w = world("admit-cancel");
    let model = Model::start(vec![Reply::Stall("")]).await;
    let mut first = serve(&w.dir, &model, &w.pax, &w.data);
    let _running = submit(&first.addr, "Occupy the worker.", "inspect");
    model.wait_requests(1).await;
    let queued = submit(&first.addr, "Never start me.", "change");
    let (status, cancelled) = http(
        &first.addr,
        "POST",
        &format!("/v1/work/{queued}/cancel"),
        None,
    );
    assert_eq!(
        (status, &cancelled["status"]),
        (200, &Value::from("cancelled")),
        "{cancelled}"
    );
    first.kill();

    let restart = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let second = serve(&w.dir, &restart, &w.pax, &w.data);
    let (status, work) = get(&second.addr, &queued);
    assert_eq!(status, 200, "{work}");
    assert_eq!(work["status"], "cancelled");
    assert_eq!(work["persisted"], true);
    assert_eq!(restart.requests(), 0, "a cancelled work is never started");
    assert_eq!(metrics(&second.addr)["recovered_work"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn finished_work_is_not_queued_again() {
    let w = world("admit-finished");
    let model = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let mut first = serve(&w.dir, &model, &w.pax, &w.data);
    let done = submit(&first.addr, "Look.", "inspect");
    wait_until("it to end", || status_of(&first.addr, &done) == "blocked").await;
    first.kill();

    let restart = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let second = serve(&w.dir, &restart, &w.pax, &w.data);
    assert_eq!(
        status_of(&second.addr, &done),
        "blocked",
        "the recorded outcome stands"
    );
    assert_eq!(restart.requests(), 0);
    assert_eq!(metrics(&second.addr)["recovered_work"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn queue_limits_hold_and_a_refused_submission_leaves_no_record() {
    let w = world("admit-limit");
    let model = Model::start(vec![Reply::Stall("")]).await;
    let mut first = serve_with(&w.dir, &model, &w.pax, &w.data, &["--max-queued-work", "1"]);
    let _running = submit(&first.addr, "Occupy the worker.", "inspect");
    model.wait_requests(1).await;
    let queued = submit(&first.addr, "Fits.", "inspect");
    let (status, refused) = post(&first.addr, "Does not fit.", "inspect");
    assert_eq!(status, 429, "{refused}");
    assert!(
        refused.get("work_id").is_none(),
        "no id for work that was not accepted"
    );
    first.kill();

    let restart = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let second = serve_with(
        &w.dir,
        &restart,
        &w.pax,
        &w.data,
        &["--max-queued-work", "1"],
    );
    wait_until("the accepted work to run", || {
        status_of(&second.addr, &queued) == "blocked"
    })
    .await;
    assert_eq!(restart.requests(), 1, "only what was accepted is recovered");
    assert_eq!(metrics(&second.addr)["recovered_work"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recovered_queue_never_exceeds_the_limits_a_normal_one_has() {
    let w = world("admit-capacity");
    let s = kill_with_queue(&w, &["inspect", "inspect", "inspect"], &[]).await;
    let (b, c, d) = (&s.queued[0], &s.queued[1], &s.queued[2]);

    // Restarted with room for one queued work: B runs, C queues, D waits in FeltDB for room.
    let model = Model::start(vec![
        Reply::Stall(BLOCK),
        Reply::Stream(BLOCK),
        Reply::Stream(BLOCK),
    ])
    .await;
    let second = serve_with(&w.dir, &model, &w.pax, &w.data, &["--max-queued-work", "1"]);
    model.wait_requests(1).await;
    assert_eq!(status_of(&second.addr, b), "running");
    assert_eq!(status_of(&second.addr, c), "queued");
    let m = metrics(&second.addr);
    assert_eq!(
        m["queued_work"], 1,
        "the queue holds what a normal queue holds: {m}"
    );
    let (status, waiting) = get(&second.addr, d);
    assert_eq!(status, 200, "{waiting}");
    assert_eq!(waiting["status"], "queued");
    assert_eq!(waiting["scheduled"], false);
    assert_eq!(waiting["reason"], "waiting for queue room");

    // As the worker frees, each runs once, oldest first, and the third moves in.
    model.release();
    for id in [b, c, d] {
        wait_until("recovered work to finish", || {
            status_of(&second.addr, id) == "blocked"
        })
        .await;
    }
    assert_eq!(
        model.requests(),
        3,
        "three recovered works, one model call each"
    );
    for id in [b, c, d] {
        assert_eq!(started_count(&second.addr, id), 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recovered_work_still_waiting_for_room_can_be_cancelled_and_never_starts() {
    let w = world("admit-cancel-waiting");
    let s = kill_with_queue(&w, &["inspect", "inspect", "inspect"], &[]).await;
    let (b, c, d) = (&s.queued[0], &s.queued[1], &s.queued[2]);

    // Restarted with room for one queued work: B runs, C queues, D is durably `ready` and is held
    // only in FeltDB, not in this process's registry.
    let model = Model::start(vec![
        Reply::Stall(BLOCK),
        Reply::Stream(BLOCK),
        Reply::Stream(BLOCK),
    ])
    .await;
    let mut second = serve_with(&w.dir, &model, &w.pax, &w.data, &["--max-queued-work", "1"]);
    model.wait_requests(1).await;
    assert_eq!(get(&second.addr, d).1["scheduled"], false);

    // Cancelling it is recorded. It is not "already ended": it had not started.
    let (status, cancelled) = http(&second.addr, "POST", &format!("/v1/work/{d}/cancel"), None);
    assert_eq!(status, 200, "{cancelled}");
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(status_of(&second.addr, d), "cancelled");

    // As the worker frees, B and C run; the cancelled D does not.
    model.release();
    for id in [b, c] {
        wait_until("recovered work to finish", || {
            status_of(&second.addr, id) == "blocked"
        })
        .await;
    }
    wait_until("a moment for any start", || true).await;
    assert_eq!(model.requests(), 2, "B and C ran; D, cancelled, did not");
    assert_eq!(started_count(&second.addr, d), 0);
    second.kill();

    // And a later restart does not bring it back.
    let again = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let third = serve_with(&w.dir, &again, &w.pax, &w.data, &["--max-queued-work", "1"]);
    assert_eq!(status_of(&third.addr, d), "cancelled");
    assert_eq!(again.requests(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_submissions_get_distinct_durable_ids_and_each_is_recovered_once() {
    let w = world("admit-concurrent");
    let model = Model::start(vec![Reply::Stall("")]).await;
    let mut first = serve(&w.dir, &model, &w.pax, &w.data);
    let addr = first.addr.clone();
    let ids: Vec<String> = {
        let handles: Vec<_> = (0..6)
            .map(|n| {
                let addr = addr.clone();
                std::thread::spawn(move || {
                    let (status, accepted) = post(&addr, &format!("Concurrent {n}."), "inspect");
                    assert_eq!(status, 202, "{accepted}");
                    assert_eq!(accepted["durable"], true);
                    accepted["work_id"].as_str().unwrap().to_string()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    };
    let distinct: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(distinct.len(), 6, "no two submissions share an identity");
    model.wait_requests(1).await;
    first.kill();

    let restart = Model::start(vec![Reply::Stream(BLOCK); 6]).await;
    let second = serve(&w.dir, &restart, &w.pax, &w.data);
    wait_until("the recovered works to finish", || {
        ids.iter()
            .filter(|id| status_of(&second.addr, id) == "blocked")
            .count()
            == 5
    })
    .await;
    let states: Vec<String> = ids.iter().map(|id| status_of(&second.addr, id)).collect();
    assert_eq!(
        states.iter().filter(|s| *s == "interrupted").count(),
        1,
        "{states:?}"
    );
    assert_eq!(
        states.iter().filter(|s| *s == "blocked").count(),
        5,
        "{states:?}"
    );
    assert_eq!(
        restart.requests(),
        5,
        "the five that never started ran once each"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn recovered_work_that_needs_a_test_run_is_not_started_without_one() {
    let w = world("admit-pax");
    let s = kill_with_queue(&w, &["change", "inspect"], &[]).await;
    let (change, inspect) = (&s.queued[0], &s.queued[1]);

    // Restarted with no PAX: the inspection runs, the change work is left exactly as it is.
    let m1 = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let mut second = serve_without_pax(&w.dir, &m1, &w.data);
    wait_until("the inspection to run", || {
        status_of(&second.addr, inspect) == "blocked"
    })
    .await;
    let (status, waiting) = get(&second.addr, change);
    assert_eq!(status, 200, "{waiting}");
    assert_eq!(waiting["status"], "queued");
    assert_eq!(waiting["scheduled"], false);
    assert!(
        waiting["reason"].as_str().unwrap().contains("test runner"),
        "{waiting}"
    );
    assert_eq!(
        m1.requests(),
        1,
        "the change work was not started and not run unverifiably"
    );
    second.kill();

    // With a test runner again, the same admission is recovered and runs.
    let m2 = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let third = serve(&w.dir, &m2, &w.pax, &w.data);
    wait_until("the change work to run", || {
        status_of(&third.addr, change) == "blocked"
    })
    .await;
    assert_eq!(m2.requests(), 1);
    assert_eq!(started_count(&third.addr, change), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_admission_that_is_damaged_is_reported_and_left_not_run_and_not_deleted() {
    let w = world("admit-damaged");
    let s = kill_with_queue(&w, &["inspect"], &[]).await;
    let lost = &s.queued[0];
    {
        let db = FeltDb::open(database(&w.data)).unwrap();
        db.apply_atomic_transaction_guarded(
            "lose-admission",
            None,
            None,
            &[],
            &[],
            &[AtomicMutation {
                capability: ADMISSION_COLLECTION.into(),
                key: format!("{ADMISSION_COLLECTION}:{lost}"),
                value: None,
            }],
            None,
        )
        .unwrap();
    }
    for round in 0..2 {
        let model = Model::start(vec![Reply::Stream(BLOCK)]).await;
        let mut server = serve(&w.dir, &model, &w.pax, &w.data);
        let (status, work) = get(&server.addr, lost);
        assert_eq!(status, 200, "round {round}: {work}");
        assert_eq!(work["scheduled"], false);
        let reason = work["reason"].as_str().unwrap();
        assert!(reason.contains("no admission record"), "{work}");
        assert!(
            reason.contains("never started") && reason.contains("--task-id"),
            "it says what it is and how it is continued, not that it is damage: {work}"
        );
        assert_eq!(model.requests(), 0, "round {round}: not run");
        server.kill();
    }
    // Still there after two restarts: reported for diagnosis, never repaired or discarded.
    let store = AgentStore::new(FeltDb::open(database(&w.data)).unwrap());
    assert!(
        store
            .tasks()
            .unwrap()
            .iter()
            .any(|t| &t.task_id == lost && t.status == "ready"),
        "the damaged task is untouched"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_durable_state_the_acknowledgement_says_it_is_not_durable() {
    let w = world("admit-nostate");
    let model = Model::start(vec![Reply::Stall("")]).await;
    let mut first = serve_with(&w.dir, &model, &w.pax, &w.data, &["--no-state"]);
    let (status, accepted) = post(&first.addr, "Look.", "inspect");
    assert_eq!(status, 202, "{accepted}");
    assert_eq!(accepted["durable"], false);
    let id = accepted["work_id"].as_str().unwrap().to_string();
    first.kill();
    let restart = Model::start(vec![]).await;
    let second = serve(&w.dir, &restart, &w.pax, &w.data);
    assert_eq!(
        get(&second.addr, &id).0,
        404,
        "nothing was recorded, and it says so"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn submitting_the_same_request_twice_is_two_admissions_there_is_no_idempotency_key() {
    let w = world("admit-dup");
    let model = Model::start(vec![Reply::Stall("")]).await;
    let first = serve(&w.dir, &model, &w.pax, &w.data);
    let _running = submit(&first.addr, "Occupy the worker.", "inspect");
    model.wait_requests(1).await;
    let a = submit(&first.addr, "Same words.", "inspect");
    let b = submit(&first.addr, "Same words.", "inspect");
    assert_ne!(
        a, b,
        "a retry is indistinguishable from a new request, so it is a new work"
    );
}
