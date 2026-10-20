//! The stuck guard through the real binary: a scripted model endpoint, the usual `pax` stand-in,
//! real files, and a real FeltDB file for the restart. Only the model is scripted; nothing here
//! says anything about how a real model behaves.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::path::Path;
use std::process::Output;

use gated::{Model, PASSED, Pax, Reply, command, project, unique};
use serde_json::Value;

const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const LIST: &str =
    r#"{"decision":"request_capability","capability":"project.list","inputs":{"path":"."}}"#;
const LIST_SRC: &str =
    r#"{"decision":"request_capability","capability":"project.list","inputs":{"path":"src"}}"#;
const SEARCH: &str =
    r#"{"decision":"request_capability","capability":"project.search","inputs":{"query":"one"}}"#;
const WRITE_1: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const WRITE_2: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\npub fn three() -> u8 { 3 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;

fn replies(texts: &[&'static str]) -> Vec<Reply> {
    texts.iter().map(|t| Reply::Stream(t)).collect()
}

fn work(dir: &Path, model: &Model, pax: &Pax, extra: &[&str]) -> Output {
    command(dir, &model.url, pax)
        .args(["work", "Look into `one`.", "--json"])
        .args(extra)
        .output()
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

fn executions(v: &Value, capability: &str) -> u64 {
    v["executions_by_capability"][capability]
        .as_u64()
        .unwrap_or(0)
}

fn reason(v: &Value) -> String {
    v["outcome_reason"].as_str().unwrap_or_default().to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_identical_loop_ends_as_blocked_and_says_what_it_saw() {
    let model = Model::start(replies(&[READ])).await; // the last reply repeats forever
    let pax = Pax::new("stuck-a", PASSED, false);
    let dir = project("stuck-a");
    let out = work(
        &dir,
        &model,
        &pax,
        &["--max-turns", "20", "--max-executions", "20"],
    );
    let v = report(&out);
    assert_eq!(v["terminal_state"], "blocked", "{v}");
    assert!(
        reason(&v).starts_with("stuck: project.read was requested 3 times in a row"),
        "{}",
        reason(&v)
    );
    assert_eq!(out.status.code(), Some(1));
    // Three asks, three reads, and nothing after.
    assert_eq!(model.requests(), 3);
    assert_eq!(executions(&v, "project.read"), 3);
    assert!(!pax.started());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_alternating_loop_ends_as_blocked() {
    let model = Model::start(replies(&[READ, LIST, READ, LIST, READ, LIST, READ, LIST])).await;
    let pax = Pax::new("stuck-b", PASSED, false);
    let dir = project("stuck-b");
    let out = work(
        &dir,
        &model,
        &pax,
        &["--max-turns", "20", "--max-executions", "20"],
    );
    let v = report(&out);
    assert_eq!(v["terminal_state"], "blocked", "{v}");
    assert!(
        reason(&v).starts_with("stuck: project.read and project.list alternated for 4 requests"),
        "{}",
        reason(&v)
    );
    assert_eq!(model.requests(), 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn legitimate_repeated_calls_that_change_the_world_are_not_stopped() {
    // write, read, write, read, test: two capabilities alternate, and every read sees a new file.
    let model = Model::start(replies(&[WRITE_1, READ, WRITE_2, READ, TEST])).await;
    let pax = Pax::new("stuck-c", PASSED, false);
    let dir = project("stuck-c");
    let out = work(
        &dir,
        &model,
        &pax,
        &["--max-turns", "20", "--max-executions", "20"],
    );
    let v = report(&out);
    assert_eq!(out.status.code(), Some(0), "{v}");
    assert_eq!(v["terminal_state"], "completed");
    assert_eq!(model.requests(), 5);
    assert!(pax.finished());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ordinary_task_completes() {
    let model = Model::start(replies(&[WRITE_1, TEST])).await;
    let pax = Pax::new("stuck-d", PASSED, false);
    let dir = project("stuck-d");
    let out = work(&dir, &model, &pax, &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(report(&out)["terminal_state"], "completed");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_turn_limit_without_a_loop_is_still_the_turn_limit() {
    let model = Model::start(replies(&[READ, LIST, SEARCH, LIST_SRC, READ])).await;
    let pax = Pax::new("stuck-e", PASSED, false);
    let dir = project("stuck-e");
    let out = work(
        &dir,
        &model,
        &pax,
        &["--max-turns", "3", "--max-executions", "20"],
    );
    let v = report(&out);
    assert_eq!(v["terminal_state"], "limit_reached", "{v}");
    assert!(!reason(&v).starts_with("stuck"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stuck_work_resumed_by_a_fresh_process_ends_the_same_way_at_once() {
    let model = Model::start(replies(&[READ])).await;
    let pax = Pax::new("stuck-f", PASSED, false);
    let dir = project("stuck-f");
    let db = unique("stuck-f").join("agent-state.log");
    let state = [
        "--state-db",
        db.to_str().unwrap(),
        "--task-id",
        "stuck-f",
        "--lease-secs",
        "1",
    ];
    let limits = ["--max-turns", "20", "--max-executions", "20"];
    let first = work(&dir, &model, &pax, &[&limits[..], &state[..]].concat());
    let v1 = report(&first);
    assert_eq!(v1["terminal_state"], "blocked", "{v1}");
    assert_eq!(model.requests(), 3);
    assert_eq!(v1["agent_state"]["task"]["status"], "blocked");

    // The blocked task keeps its lease until it expires; then a fresh process resumes it.
    std::thread::sleep(std::time::Duration::from_millis(1300));
    let second = work(&dir, &model, &pax, &[&limits[..], &state[..]].concat());
    let v2 = report(&second);
    assert_eq!(v2["terminal_state"], "blocked", "{v2}");
    assert_eq!(reason(&v2), reason(&v1), "the same reason");
    assert_eq!(model.requests(), 3, "no model call on the resume");
    assert_eq!(
        executions(&v2, "project.read"),
        0,
        "nothing ran on the resume"
    );
    assert_eq!(v2["agent_state"]["task"]["status"], "blocked");
}

#[tokio::test(flavor = "multi_thread")]
async fn work_chip_can_complete_itself_is_not_stopped_for_repeating() {
    // `verify` completes the moment pax passes. A model that would keep asking for the test is never
    // asked again, and the guard has nothing to stop.
    let model = Model::start(replies(&[TEST])).await; // the last reply repeats forever
    let pax = Pax::new("stuck-g", PASSED, false);
    let dir = project("stuck-g");
    let out = work(
        &dir,
        &model,
        &pax,
        &[
            "--kind",
            "verify",
            "--max-turns",
            "20",
            "--max-executions",
            "20",
        ],
    );
    let v = report(&out);
    assert_eq!(out.status.code(), Some(0), "{v}");
    assert_eq!(v["terminal_state"], "completed");
    assert_eq!(model.requests(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_inspection_that_has_its_evidence_but_never_answers_is_still_stopped_and_says_so() {
    // The inspection's observation requirement is met by the first successful read, but only the
    // model's accepted answer completes it, and a model that keeps re-reading never gives one.
    let model = Model::start(replies(&[READ])).await;
    let pax = Pax::new("stuck-h", PASSED, false);
    let dir = project("stuck-h");
    let out = work(
        &dir,
        &model,
        &pax,
        &[
            "--kind",
            "inspect",
            "--max-turns",
            "20",
            "--max-executions",
            "20",
        ],
    );
    let v = report(&out);
    assert_eq!(v["terminal_state"], "blocked", "{v}");
    assert!(reason(&v).starts_with("stuck: "), "{}", reason(&v));
    assert!(reason(&v).contains("already satisfied"), "{}", reason(&v));
    assert_eq!(model.requests(), 3);
}
