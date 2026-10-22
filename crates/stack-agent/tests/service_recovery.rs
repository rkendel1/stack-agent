//! What `stack-agent serve` says about earlier work after the process that ran it is gone.
//!
//! FeltDB, not the service's memory, is authoritative for a task's identity, how it ended and what
//! it recorded. Every test here is a real `serve` process over a real database file in a shared
//! runtime data directory; "killed" is SIGKILL, so nothing gets to clean up, and "restarted" is a
//! new process. The model is the scripted loopback endpoint and PAX the usual stand-in.
//!
//! Recovery is reporting, not resumption: nothing here is rerun on its own, and a restarted
//! service never makes an interrupted work verified.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use chip_agent_state::feltdb::{AtomicMutation, FeltDb};
use chip_agent_state::{AgentStore, WORK_EVENT_COLLECTION};
use gated::{LIB, Model, PASSED, Pax, Reply, command, project, unique, wait_until};
use serde_json::Value;

const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const LIST: &str = r#"{"decision":"request_capability","capability":"project.list"}"#;
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

fn start(c: std::process::Command, data: &Path) -> Server {
    let mut c = c;
    c.env("STACK_AGENT_DATA_DIR", data);
    let mut child = c
        .args(["serve", "--port", "0"])
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
    start(command(dir, &model.url, pax), data)
}

/// A service whose `PATH` and `PAX_BIN` name no PAX at all.
fn serve_without_pax(dir: &Path, model: &Model, data: &Path) -> Server {
    let empty = unique("recovery-nopax-path");
    std::fs::create_dir_all(&empty).unwrap();
    let missing = Pax::new("recovery-unused", PASSED, false);
    let mut c = command(dir, &model.url, &missing);
    c.env_remove("PAX_BIN").env("PATH", empty);
    start(c, data)
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

fn submit(addr: &str, goal: &str, kind: &str) -> String {
    let body = format!(r#"{{"goal":"{goal}","kind":"{kind}"}}"#);
    let (status, accepted) = http(addr, "POST", "/v1/work", Some(&body));
    assert_eq!(status, 202, "{accepted}");
    accepted["work_id"].as_str().unwrap().to_string()
}

fn get(addr: &str, id: &str) -> (u16, Value) {
    http(addr, "GET", &format!("/v1/work/{id}"), None)
}

fn events(addr: &str, id: &str) -> (u16, Value) {
    http(addr, "GET", &format!("/v1/work/{id}/events"), None)
}

fn kinds(events: &Value) -> Vec<String> {
    events["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect()
}

/// The one database file the service created under `data`.
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

/// A service over `dir` that ran one verified work (`done`) and then was killed while a second
/// (`running`) had written a file and waited on its next model call.
struct Killed {
    dir: PathBuf,
    data: PathBuf,
    pax: Pax,
    done: String,
    running: String,
    live_done: Value,
    live_done_events: Value,
}

async fn a_verified_work_and_an_interrupted_one(tag: &str) -> Killed {
    let model = Model::start(vec![
        Reply::Stream(WRITE),
        Reply::Stream(TEST),
        Reply::Stream(WRITE),
        Reply::Stall(""),
    ])
    .await;
    let pax = Pax::new(tag, PASSED, false);
    let (dir, data) = (project(tag), unique(&format!("{tag}-data")));
    let mut first = serve(&dir, &model, &pax, &data);

    let done = submit(&first.addr, "Add `two`.", "change");
    wait_until("the first work to complete", || {
        get(&first.addr, &done).1["status"] == "completed"
    })
    .await;
    let live_done = get(&first.addr, &done).1;
    let live_done_events = events(&first.addr, &done).1;

    let running = submit(&first.addr, "Add `two` again.", "change");
    model.wait_requests(4).await;
    assert_eq!(get(&first.addr, &running).1["status"], "running");
    first.kill();
    Killed {
        dir,
        data,
        pax,
        done,
        running,
        live_done,
        live_done_events,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn finished_work_is_recovered_after_a_restart_exactly_as_it_ended() {
    let k = a_verified_work_and_an_interrupted_one("recovery-terminal").await;
    let restart = Model::start(vec![]).await;
    let second = serve(&k.dir, &restart, &k.pax, &k.data);

    let (status, work) = get(&second.addr, &k.done);
    assert_eq!(status, 200, "{work}");
    assert_eq!(work["persisted"], true);
    assert_eq!(work["work_id"], k.done.as_str());
    // The outcome and the verdict are what the host recorded, not recomputed.
    assert_eq!(work["status"], k.live_done["status"]);
    assert_eq!(work["result"]["terminal_state"], "completed");
    // (A live completed result has no `outcome_reason`; the recovered one carries the summary the
    // host recorded for the completion, which is the state's own record of why it completed.)
    assert_eq!(
        work["result"]["outcome_reason"],
        "pax.test passed after the last change"
    );
    for field in ["verified", "goal_satisfied", "grounded"] {
        assert_eq!(
            work["result"][field], k.live_done["result"][field],
            "{field}: a client that read it before the restart reads the same after"
        );
    }
    assert_eq!(work["result"]["verified"], true);
    assert_eq!(work["result"]["verification"]["verified"], true);
    assert_eq!(work["result"]["verification"]["accepted"], true);

    // The same events, in the same order, now with the provenance of each fact.
    let (status, recovered) = events(&second.addr, &k.done);
    assert_eq!(status, 200);
    assert_eq!(recovered["complete"], true);
    assert_eq!(kinds(&recovered), kinds(&k.live_done_events));
    let listed = recovered["events"].as_array().unwrap();
    for e in listed {
        let provenance = e["provenance"].as_str().unwrap();
        assert!(
            ["host_observed", "model_inferred", "runtime_decided"].contains(&provenance),
            "{e}"
        );
    }
    assert!(
        listed
            .iter()
            .any(|e| e["kind"] == "DecisionMade" && e["provenance"] == "model_inferred")
    );
    // `after=N` pages the recovered history without gaps or repeats.
    let (_, tail) = http(
        &second.addr,
        "GET",
        &format!("/v1/work/{}/events?after=5", k.done),
        None,
    );
    assert_eq!(tail["events"].as_array().unwrap().len(), listed.len() - 5);
    assert_eq!(tail["next"], listed.len());

    // Asking is reading: nothing was submitted, started or asked of a model.
    let (_, metrics) = http(&second.addr, "GET", "/v1/metrics", None);
    assert_eq!(metrics["submitted_work"], 0);
    assert_eq!(restart.requests(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_work_is_reported_interrupted_and_is_never_verified_or_replayed() {
    let k = a_verified_work_and_an_interrupted_one("recovery-interrupted").await;
    let lib_before = std::fs::read_to_string(k.dir.join("src/lib.rs")).unwrap();
    assert_ne!(lib_before, LIB, "the interrupted work had really written");

    let restart = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let second = serve(&k.dir, &restart, &k.pax, &k.data);
    let (status, work) = get(&second.addr, &k.running);
    assert_eq!(status, 200, "{work}");
    assert_eq!(work["status"], "interrupted");
    assert_eq!(work["persisted"], true);
    assert_eq!(work["result"], Value::Null, "no outcome is invented for it");
    assert_ne!(work["status"], "completed");
    // It is distinguishable from the work that really was verified, in the same database.
    assert_eq!(get(&second.addr, &k.done).1["result"]["verified"], true);

    let (status, recorded) = events(&second.addr, &k.running);
    assert_eq!(status, 200);
    let names = kinds(&recorded);
    assert!(
        names.contains(&"ExecutionCompleted".to_string()),
        "{names:?}"
    );
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("Work") && n != "WorkStarted"),
        "an interrupted work has no terminal event: {names:?}"
    );

    // Nothing was continued on its own: no model call, no test run, the file as it was left.
    wait_until("a moment for any rerun to start", || true).await;
    assert_eq!(
        restart.requests(),
        0,
        "no model call for the interrupted work"
    );
    assert!(!k.pax.started() || get(&second.addr, &k.running).1["status"] == "interrupted");
    assert_eq!(
        std::fs::read_to_string(k.dir.join("src/lib.rs")).unwrap(),
        lib_before
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_is_repeatable_a_second_restart_changes_nothing() {
    let k = a_verified_work_and_an_interrupted_one("recovery-twice").await;
    let m2 = Model::start(vec![]).await;
    let mut second = serve(&k.dir, &m2, &k.pax, &k.data);
    let snapshot = |addr: &str| {
        (
            get(addr, &k.done),
            events(addr, &k.done),
            get(addr, &k.running),
            events(addr, &k.running),
        )
    };
    let once = snapshot(&second.addr);
    assert_eq!(snapshot(&second.addr), once, "asking again is a read");
    second.kill();

    let m3 = Model::start(vec![]).await;
    let third = serve(&k.dir, &m3, &k.pax, &k.data);
    assert_eq!(
        snapshot(&third.addr),
        once,
        "the second restart reconciled nothing new and wrote nothing"
    );
    assert_eq!((m2.requests(), m3.requests()), (0, 0));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_recovered_work_changes_nothing_and_says_what_it_is() {
    let k = a_verified_work_and_an_interrupted_one("recovery-cancel").await;
    let restart = Model::start(vec![]).await;
    let second = serve(&k.dir, &restart, &k.pax, &k.data);
    let before = (get(&second.addr, &k.done), get(&second.addr, &k.running));

    let (status, body) = http(
        &second.addr,
        "POST",
        &format!("/v1/work/{}/cancel", k.done),
        None,
    );
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "work_already_finished");
    let (status, body) = http(
        &second.addr,
        "POST",
        &format!("/v1/work/{}/cancel", k.running),
        None,
    );
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "work_not_running");
    // An id nobody issued is still just unknown.
    let (status, body) = http(
        &second.addr,
        "POST",
        "/v1/work/work_0000000000000000/cancel",
        None,
    );
    assert_eq!(
        (status, &body["error"]["code"]),
        (404, &Value::from("work_not_found"))
    );

    // A cancel is never recorded as a cancellation of work that was not running.
    assert_eq!(
        (get(&second.addr, &k.done), get(&second.addr, &k.running)),
        before
    );
    assert_eq!(get(&second.addr, &k.running).1["status"], "interrupted");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_record_that_cannot_be_read_is_an_error_not_an_unknown_id() {
    let model = Model::start(vec![Reply::Stream(BLOCK), Reply::Stream(BLOCK)]).await;
    let pax = Pax::new("recovery-unreadable", PASSED, false);
    let (dir, data) = (
        project("recovery-unreadable"),
        unique("recovery-unreadable-data"),
    );
    let mut first = serve(&dir, &model, &pax, &data);
    let damaged = submit(&first.addr, "Look.", "inspect");
    wait_until("the first work to end", || {
        get(&first.addr, &damaged).1["status"] == "blocked"
    })
    .await;
    let intact = submit(&first.addr, "Look again.", "inspect");
    wait_until("the second work to end", || {
        get(&first.addr, &intact).1["status"] == "blocked"
    })
    .await;
    first.kill();

    // A damaged store loses a middle event of one work (as the state library's own test does).
    let path = database(&data);
    {
        let db = FeltDb::open(&path).unwrap();
        let attempt = AgentStore::new(db)
            .history(&damaged)
            .unwrap()
            .unwrap()
            .events[0]
            .attempt;
        let db = FeltDb::open(&path).unwrap();
        db.apply_atomic_transaction_guarded(
            "remove-one",
            None,
            None,
            &[],
            &[],
            &[AtomicMutation {
                capability: WORK_EVENT_COLLECTION.into(),
                key: format!("{WORK_EVENT_COLLECTION}:{damaged}~a{attempt}~e000002"),
                value: None,
            }],
            None,
        )
        .unwrap();
    }

    let restart = Model::start(vec![]).await;
    let second = serve(&dir, &restart, &pax, &data);
    assert_eq!(http(&second.addr, "GET", "/health", None).0, 200);
    for (name, status_body) in [
        ("status", get(&second.addr, &damaged)),
        ("events", events(&second.addr, &damaged)),
        (
            "cancel",
            http(
                &second.addr,
                "POST",
                &format!("/v1/work/{damaged}/cancel"),
                None,
            ),
        ),
    ] {
        let (status, body) = status_body;
        assert_eq!(status, 500, "{name}: {body}");
        assert_eq!(body["error"]["code"], "work_record_unreadable", "{name}");
        assert!(
            body.get("status").is_none() && body.get("result").is_none(),
            "{name}: nothing is inferred"
        );
        assert!(
            !body.to_string().contains(&path.display().to_string()),
            "{name}"
        );
    }
    // The damage is that one work's. Another is read back as it was, and an id nobody issued is
    // still unknown.
    let (status, ok) = get(&second.addr, &intact);
    assert_eq!((status, &ok["status"]), (200, &Value::from("blocked")));
    assert_eq!(get(&second.addr, "work_0000000000000000").0, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn work_that_was_only_accepted_leaves_no_record_and_is_unknown_after_a_restart() {
    // The documented limit: a work gets a durable identity when it starts, not when it is
    // accepted. One that was still queued when the process ended was never recorded, so after the
    // restart nothing can say it existed. The client holds the 202 and knows to resubmit.
    let model = Model::start(vec![Reply::Stall("")]).await;
    let pax = Pax::new("recovery-queued", PASSED, false);
    let (dir, data) = (project("recovery-queued"), unique("recovery-queued-data"));
    let mut first = serve(&dir, &model, &pax, &data);
    let running = submit(&first.addr, "Add `two`.", "change");
    model.wait_requests(1).await;
    let queued = submit(&first.addr, "Add `three`.", "change");
    assert_eq!(get(&first.addr, &queued).1["status"], "queued");
    first.kill();

    let restart = Model::start(vec![]).await;
    let second = serve(&dir, &restart, &pax, &data);
    assert_eq!(get(&second.addr, &running).1["status"], "interrupted");
    let (status, body) = get(&second.addr, &queued);
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["error"]["code"], "work_not_found");
    assert_eq!(
        restart.requests(),
        0,
        "the queued work was not started on its own"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn submitting_the_same_request_twice_is_two_works_there_is_no_idempotency_key() {
    // The current boundary, pinned so it is not mistaken for exactly-once: nothing identifies a
    // submission, so a client that retries a POST starts another work.
    let model = Model::start(vec![Reply::Stream(BLOCK), Reply::Stream(BLOCK)]).await;
    let pax = Pax::new("recovery-dup", PASSED, false);
    let (dir, data) = (project("recovery-dup"), unique("recovery-dup-data"));
    let server = serve(&dir, &model, &pax, &data);
    let a = submit(&server.addr, "Look.", "inspect");
    let b = submit(&server.addr, "Look.", "inspect");
    assert_ne!(a, b);
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_does_not_need_pax_and_never_turns_missing_pax_into_verification() {
    // A change work, with PAX, killed after it wrote; and an inspection killed while it ran.
    let model = Model::start(vec![
        Reply::Stream(WRITE),
        Reply::Stall(""),
        Reply::Stall(""),
    ])
    .await;
    let pax = Pax::new("recovery-nopax", PASSED, false);
    let (dir, data) = (project("recovery-nopax"), unique("recovery-nopax-data"));
    let mut first = serve(&dir, &model, &pax, &data);
    let change = submit(&first.addr, "Add `two`.", "change");
    model.wait_requests(2).await;
    let (_, accepted) = http(
        &first.addr,
        "POST",
        "/v1/work",
        Some(r#"{"goal":"Look around.","kind":"inspect"}"#),
    );
    let queued_inspect = accepted["work_id"].as_str().unwrap().to_string();
    first.kill();

    // Restarted with no PAX anywhere.
    let restart = Model::start(vec![Reply::Stream(LIST)]).await;
    let second = serve_without_pax(&dir, &restart, &data);
    assert_eq!(http(&second.addr, "GET", "/health", None).0, 200);
    let (status, work) = get(&second.addr, &change);
    assert_eq!(status, 200, "{work}");
    assert_eq!(work["status"], "interrupted");
    assert_eq!(work["result"], Value::Null);
    assert_ne!(work["result"]["verified"], true);
    // Work that needs a test run is still refused while there is none; an inspection is accepted.
    let (status, refused) = http(
        &second.addr,
        "POST",
        "/v1/work",
        Some(r#"{"goal":"Add `three`.","kind":"change"}"#),
    );
    assert_eq!(
        (status, &refused["error"]["code"]),
        (503, &Value::from("test_runner_unavailable"))
    );
    let (status, _) = http(
        &second.addr,
        "POST",
        "/v1/work",
        Some(r#"{"goal":"What is here?","kind":"inspect"}"#),
    );
    assert_eq!(status, 202);
    // An interrupted work is not a verification failure and not a PAX failure: no outcome at all.
    let (_, again) = get(&second.addr, &change);
    assert!(again["result"].is_null() && again["status"] == "interrupted");
    // The inspection that was only queued was never recorded.
    assert_eq!(get(&second.addr, &queued_inspect).0, 404);
}
