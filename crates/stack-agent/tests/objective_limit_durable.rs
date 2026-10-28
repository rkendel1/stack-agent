//! The real T4 goal (Agent Obstacle Course course-001, 1,829 bytes: over FeltDB's 1,024-byte
//! objective) through the real binary with durable state ON (`--state-db`, `--task-id`): admitted,
//! begun, killed in a model call, taken over once the lease has run out, resumed under the Recovery
//! profile with the whole goal, verified, and finished; a continuation that gives the stored
//! objective (the goal's beginning and digest) instead of the goal is refused before anything runs.
//! A scripted OpenAI-compatible stream and a `pax` stand-in; no real model is called.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::time::Duration;

use gated::{Model, PASSED, Pax, Reply, command, project};
use serde_json::Value;

const T4_GOAL: &str = include_str!("fixtures/objective/t4-goal.txt");
const TASK: &str = "work_t4_objective";

const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;

fn last_json(out: &std::process::Output) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    serde_json::from_str(stdout.lines().last().unwrap_or("null")).unwrap_or(Value::Null)
}

/// Every message's content of one request body, in order.
fn message_text(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap();
    v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["content"].as_str().unwrap_or("").to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn durable(
    dir: &std::path::Path,
    model: &Model,
    pax: &Pax,
    db: &std::path::Path,
    goal: &str,
) -> std::process::Command {
    let mut c = command(dir, &model.url, pax);
    c.env_remove("CHIP_CONTEXT_PROFILE");
    c.args(["work", goal, "--json", "--lease-secs", "1"])
        .arg("--state-db")
        .arg(db)
        .args(["--task-id", TASK]);
    c
}

#[tokio::test(flavor = "multi_thread")]
async fn the_t4_goal_is_persisted_resumed_under_recovery_and_never_continued_from_its_prefix() {
    assert_eq!(T4_GOAL.len(), 1829);
    assert!(T4_GOAL.len() > 1024);
    let tag = "t4-objective";
    let pax = Pax::new(tag, PASSED, false);
    let dir = project(tag);
    let db = dir.join("state.feltdb");

    // 1. Admitted and begun with the whole goal; one read is recorded; killed in the next call.
    let first = Model::start(vec![Reply::Stream(READ), Reply::Stall("")]).await;
    let mut c = durable(&dir, &first, &pax, &db, T4_GOAL);
    c.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    first.wait_requests(2).await;
    child.kill().unwrap();
    let killed = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&killed.stderr);
    assert!(!stderr.contains("CONTRACT_INPUT_INVALID"), "{stderr}");
    assert!(!stderr.contains("not persisted"), "{stderr}");
    // The model was given the whole goal, not the stored objective.
    let text = message_text(&first.bodies()[0]);
    assert!(text.contains(T4_GOAL.trim()), "the whole goal reached the model");
    assert!(!text.contains("the whole goal is in chip.Admission"));

    // 2. The stored objective is bounded, names the goal's digest, and is not the goal.
    let objective = chip_cli::work_state::objective(T4_GOAL);
    assert!(objective.len() <= 1024);
    assert_ne!(objective, T4_GOAL);

    // 3. Once the 1 s lease has run out, a continuation that passes the stored objective as its goal
    //    is refused before anything runs (it is within the limit, so it is its own objective and
    //    would otherwise match).
    tokio::time::sleep(Duration::from_millis(1300)).await;
    let wrong = Model::start(vec![Reply::Stream(WRITE)]).await;
    let c = durable(&dir, &wrong, &pax, &db, &objective);
    let out = tokio::task::spawn_blocking(move || {
        let mut c = c;
        c.output().unwrap()
    })
    .await
    .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("was admitted with another goal"), "{stderr}");
    assert!(stderr.contains("nothing was run"), "{stderr}");
    assert_eq!(wrong.requests(), 0, "no model call for a refused continuation");

    // 4. The same goal continues it: the read is restored, the first request is assembled under
    //    Recovery with the whole goal, then the work writes, tests and is verified.
    let second = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let c = durable(&dir, &second, &pax, &db, T4_GOAL);
    let out = tokio::task::spawn_blocking(move || {
        let mut c = c;
        c.output().unwrap()
    })
    .await
    .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let report = last_json(&out);
    assert_eq!(report["verified"], true, "{report}");
    assert_eq!(report["goal"].as_str().map(str::len), Some(1829));
    assert_eq!(report["goal"], T4_GOAL.trim());
    let state = &report["agent_state"];
    assert_eq!(state["restored"]["observations"], 1, "{state}");
    let calls = report["token_usage"]["calls"].as_array().unwrap();
    assert_eq!(calls[0]["context"]["profile"], "recovery", "{report}");
    assert!(
        calls[0]["context"]["reason"]
            .as_str()
            .unwrap()
            .starts_with("resumed from durable state (1 restored observation(s)"),
        "{}",
        calls[0]["context"]["reason"]
    );
    assert_ne!(calls[1]["context"]["profile"], "recovery");
    let resumed = message_text(&second.bodies()[0]);
    assert!(resumed.contains(T4_GOAL.trim()), "the whole goal, after the resume");
    assert!(resumed.contains("was interrupted and resumed"), "{resumed}");
    assert!(!resumed.contains("the whole goal is in chip.Admission"));

    // 5. A finished task is not run again, and says so.
    let third = Model::start(vec![Reply::Stream(WRITE)]).await;
    let c = durable(&dir, &third, &pax, &db, T4_GOAL);
    let out = tokio::task::spawn_blocking(move || {
        let mut c = c;
        c.output().unwrap()
    })
    .await
    .unwrap();
    assert_eq!(third.requests(), 0, "{}", String::from_utf8_lossy(&out.stderr));
}
