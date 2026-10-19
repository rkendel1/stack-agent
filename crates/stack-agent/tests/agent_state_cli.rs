//! `stack-agent work --state-db FILE --task-id ID`: the real binary persists its work in a real
//! FeltDB file through `chip-agent-state`. The model is the scripted loopback endpoint and PAX the
//! usual stand-in; the database, the transitions and every restart are real (each command is a new
//! process that reopens the file).

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::path::Path;
use std::process::Output;

use gated::{Model, PASSED, Pax, Reply, command, project, unique};
use serde_json::Value;

const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;

async fn work(
    dir: &Path,
    model: &Model,
    pax: &Pax,
    db: &Path,
    task: &str,
    extra: &[&str],
) -> Output {
    let mut cmd = command(dir, &model.url, pax);
    cmd.args(["work", "Add `two`.", "--json", "--state-db"])
        .arg(db)
        .args(["--task-id", task])
        .args(extra);
    tokio::task::spawn_blocking(move || cmd.output().unwrap())
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

#[tokio::test(flavor = "multi_thread")]
async fn verified_work_is_recorded_complete_and_a_restart_reports_it_without_running_again() {
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let pax = Pax::new("state-done", PASSED, false);
    let dir = project("state-done");
    let db = unique("state-done").join("agent-state.log");

    let out = work(&dir, &model, &pax, &db, "add-two", &[]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v = report(&out);
    let state = &v["agent_state"];
    assert_eq!(state["recorded"], true, "{state}");
    assert_eq!(state["task_id"], "add-two");
    assert_eq!(state["completion_refused"], false);
    assert_eq!(state["task"]["status"], "completed");
    assert_eq!(
        state["task"]["intents"], 2,
        "one per execution: write and test"
    );
    assert_eq!(state["task"]["unresolved_intents"], 0);
    assert!(state["persistence_failures"].as_array().unwrap().is_empty());
    let asked = model.requests();

    // A new process over the same file: FeltDB says the task is done, so nothing runs.
    let again = work(&dir, &model, &pax, &db, "add-two", &[]).await;
    assert_eq!(again.status.code(), Some(0), "{}", stderr(&again));
    let v = report(&again);
    assert_eq!(v["ran"], false);
    assert_eq!(v["agent_state"]["task"]["status"], "completed");
    assert_eq!(model.requests(), asked, "no model call for a finished task");
}

#[tokio::test(flavor = "multi_thread")]
async fn blocked_work_is_recorded_blocked_and_its_lease_refuses_a_second_process() {
    let model = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let pax = Pax::new("state-blocked", PASSED, false);
    let dir = project("state-blocked");
    let db = unique("state-blocked").join("agent-state.log");

    let out = work(&dir, &model, &pax, &db, "blocked-1", &[]).await;
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let v = report(&out);
    assert_eq!(v["agent_state"]["recorded"], true);
    assert_eq!(v["agent_state"]["task"]["status"], "blocked");
    let asked = model.requests();

    // The first process's lease has not expired: the second is refused before anything runs.
    let second = work(&dir, &model, &pax, &db, "blocked-1", &[]).await;
    assert_eq!(second.status.code(), Some(3));
    let err = stderr(&second);
    assert!(
        err.contains("unexpired lease") && err.contains("nothing was run"),
        "{err}"
    );
    assert_eq!(model.requests(), asked);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_database_that_cannot_be_opened_stops_the_work_before_it_starts() {
    let model = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let pax = Pax::new("state-unusable", PASSED, false);
    let dir = project("state-unusable");
    let db = unique("state-unusable").join("agent-state.log");
    std::fs::create_dir_all(&db).unwrap(); // a directory where the file should be

    let out = work(&dir, &model, &pax, &db, "t", &[]).await;
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("durable agent state"));
    assert_eq!(
        model.requests(),
        0,
        "no silent fallback to running without state"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn incomplete_or_invalid_state_options_are_usage_errors() {
    let model = Model::start(vec![]).await;
    let pax = Pax::new("state-usage", PASSED, false);
    let dir = project("state-usage");
    let db = unique("state-usage").join("agent-state.log");
    for args in [
        vec!["work", "x", "--task-id", "t"],
        vec!["work", "x", "--lease-secs", "10"],
    ] {
        let out = command(&dir, &model.url, &pax)
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
    }
    let out = command(&dir, &model.url, &pax)
        .args(["work", "x", "--state-db"])
        .arg(&db)
        .args(["--task-id", "not a valid id"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(!db.exists(), "nothing was opened");
    assert_eq!(model.requests(), 0);
}
