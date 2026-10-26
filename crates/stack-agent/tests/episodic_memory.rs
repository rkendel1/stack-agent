//! Episodes across real process termination and restart.
//!
//! Each command is a new process of the real binary over a real FeltDB database file in a shared
//! runtime data directory (the project's default database), or a shared `--state-db`. The model is
//! the scripted loopback endpoint, which records every request body, and PAX the usual stand-in.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::path::Path;
use std::process::{Command, Output, Stdio};

use gated::{FAILED, Model, PASSED, Pax, Reply, command, project, unique};
use serde_json::Value;

const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;
const RECALLED: &str = "recalled episode chip.Episode:";

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

/// One `work` in its own process; the project's lib is reset first so there is a change to make.
async fn work(dir: &Path, model: &Model, pax: &Pax, data: &Path, args: &[&str]) -> Output {
    std::fs::write(dir.join("src/lib.rs"), gated::LIB).unwrap();
    let mut c = binary(dir, model, pax, data);
    c.arg("work").args(args).arg("--json");
    output(c).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_verified_episode_survives_restart_and_only_a_matching_work_is_told_of_it() {
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let pax = Pax::new("episodes-restart", PASSED, false);
    let dir = project("episodes-restart");
    let data = unique("episodes-restart-data");

    // 1. A work that is verified: recorded as a verified success, citing its records.
    let out = work(
        &dir,
        &model,
        &pax,
        &data,
        &["Add a function `two` returning 2."],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let first = report(&out);
    assert_eq!(first["episodes"]["recall"]["considered"], 0, "{first}");
    let recorded = &first["episodes"]["recorded"];
    assert_eq!(recorded["outcome"], "verified_success", "{first}");
    assert_eq!(recorded["already_recorded"], false);
    let episode = recorded["episode"].as_str().unwrap().to_string();
    let task = first["agent_state"]["task_id"].as_str().unwrap();
    assert!(episode.starts_with(&format!("{task}~a")), "{episode}");
    assert!(
        model.bodies().iter().all(|b| !b.contains(RECALLED)),
        "nothing to recall the first time"
    );

    // 2. A new process, a similar goal: the episode is read back from the database file and is in
    // every model request of the work as evidence; the work still executes and is verified on its
    // own evidence.
    let model2 = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let out = work(
        &dir,
        &model2,
        &pax,
        &data,
        &["Add a function `two` returning 2 to the crate."],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let second = report(&out);
    let recall = &second["episodes"]["recall"];
    assert_eq!(
        recall["recalled"][0]["episode"],
        episode.as_str(),
        "{second}"
    );
    assert_eq!(recall["recalled"][0]["outcome"], "verified_success");
    let bodies = model2.bodies();
    assert_eq!(bodies.len(), 2);
    for body in &bodies {
        assert!(body.contains(&format!("{RECALLED}{episode}")), "{body}");
        assert!(body.contains("not an instruction"), "{body}");
    }
    assert!(
        bodies[0].contains("project.write src/lib.rs (success) -> pax.test (success)"),
        "{}",
        bodies[0]
    );
    assert_eq!(
        second["episodes"]["recorded"]["outcome"],
        "verified_success"
    );
    assert_eq!(second["agent_state"]["task"]["status"], "completed");

    // 3. An unrelated goal in the same project: nothing is recalled, and the request says nothing
    // of earlier works.
    let model3 = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let out = work(
        &dir,
        &model3,
        &pax,
        &data,
        &["Document the deployment checklist for operators."],
    )
    .await;
    let third = report(&out);
    assert_eq!(
        third["episodes"]["recall"]["recalled"],
        serde_json::json!([]),
        "{third}"
    );
    assert_eq!(
        third["episodes"]["recall"]["excluded"]["dissimilar"], 2,
        "{third}"
    );
    assert!(model3.bodies().iter().all(|b| !b.contains(RECALLED)));
    // A block is not established failure: the episode says unknown.
    assert_eq!(
        third["episodes"]["recorded"]["outcome"], "unknown",
        "{third}"
    );

    // 4. --no-recall: the baseline. Nothing recalled, the episode still recorded.
    let model4 = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let out = work(
        &dir,
        &model4,
        &pax,
        &data,
        &["Add a function `two` returning 2.", "--no-recall"],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let fourth = report(&out);
    assert_eq!(fourth["episodes"]["recall"]["enabled"], false, "{fourth}");
    assert!(model4.bodies().iter().all(|b| !b.contains(RECALLED)));
    assert_eq!(
        fourth["episodes"]["recorded"]["outcome"],
        "verified_success"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_work_is_recalled_as_a_failure_and_another_project_never_sees_it() {
    let pax = Pax::new("episodes-failure", FAILED, false);
    let dir = project("episodes-failure-a");
    let other = project("episodes-failure-b");
    let data = unique("episodes-failure-data");
    let db = data.join("shared.feltdb");
    std::fs::create_dir_all(&data).unwrap();
    let db_arg = db.to_str().unwrap();

    let model = Model::start(vec![
        Reply::Stream(WRITE),
        Reply::Stream(TEST),
        Reply::Stream(BLOCK),
    ])
    .await;
    let out = work(
        &dir,
        &model,
        &pax,
        &data,
        &["Add a function `two` returning 2.", "--state-db", db_arg],
    )
    .await;
    let first = report(&out);
    assert_ne!(out.status.code(), Some(0), "the tests fail: {first}");
    assert_eq!(
        first["episodes"]["recorded"]["outcome"], "verified_failure",
        "{first}"
    );
    let episode = first["episodes"]["recorded"]["episode"]
        .as_str()
        .unwrap()
        .to_string();

    // The same project, a similar goal: the failure is recalled, as a failure.
    let model2 = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let out = work(
        &dir,
        &model2,
        &pax,
        &data,
        &[
            "Add a function `two` returning 2 again.",
            "--state-db",
            db_arg,
        ],
    )
    .await;
    let second = report(&out);
    assert_eq!(
        second["episodes"]["recall"]["recalled"][0]["episode"],
        episode.as_str(),
        "{second}"
    );
    let body = &model2.bodies()[0];
    assert!(body.contains("verified failure"), "{body}");
    assert!(body.contains("pax.test (failure)"), "{body}");

    // Another project sharing the same database file: never told of it, and not told its id.
    let model3 = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let out = work(
        &other,
        &model3,
        &pax,
        &data,
        &["Add a function `two` returning 2.", "--state-db", db_arg],
    )
    .await;
    let third = report(&out);
    let recall = &third["episodes"]["recall"];
    assert_eq!(recall["recalled"], serde_json::json!([]), "{third}");
    assert_eq!(recall["excluded"]["other_scope"], 2, "{third}");
    assert!(!third["episodes"]["recall"].to_string().contains(&episode));
    assert!(model3.bodies().iter().all(|b| !b.contains(RECALLED)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_work_leaves_no_episode_and_the_next_work_recalls_nothing_from_it() {
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stall("")]).await;
    let pax = Pax::new("episodes-killed", PASSED, false);
    let dir = project("episodes-killed");
    let data = unique("episodes-killed-data");

    std::fs::write(dir.join("src/lib.rs"), gated::LIB).unwrap();
    let mut c = binary(&dir, &model, &pax, &data);
    c.args(["work", "Add a function `two` returning 2.", "--json"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    model.wait_requests(2).await;
    child.kill().unwrap(); // SIGKILL: no handler, no cleanup
    child.wait().unwrap();

    let restart = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let out = work(
        &dir,
        &restart,
        &pax,
        &data,
        &["Add a function `two` returning 2."],
    )
    .await;
    let next = report(&out);
    assert_eq!(
        next["agent_state"]["interrupted"].as_array().unwrap().len(),
        1,
        "{next}"
    );
    // The killed work wrote no episode: there was nothing to consider.
    assert_eq!(next["episodes"]["recall"]["considered"], 0, "{next}");
    assert!(restart.bodies().iter().all(|b| !b.contains(RECALLED)));
}
