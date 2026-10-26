//! Controlled workflow execution through the real binary: `work --workflows` following an eligible
//! workflow candidate through Chip's own loop, its fallback to the planner, its records, and its
//! behaviour across interruption and restart.
//!
//! Every command is a new process of the real binary with the real capability declarations
//! (`project.write`, `pax.test`, ...) over the project's default FeltDB database. The model is the
//! scripted loopback endpoint, which records every request. `pax` is a stand-in that passes exactly
//! when `src/lib.rs` defines `fn two`, so verification depends on what this work wrote; it can be
//! gated to hold while "running tests".

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use gated::{FAILED, Model, PASSED, Pax, Reply, command, project, unique, wait_until};
use serde_json::Value;

const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const WRITE_BAD: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\n"}}"#;
const WRITE_TWO: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/two.rs","content":"pub fn helper() -> u8 { 2 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;
const GOAL: &str = "Add a function `two` returning 2.";
const GUIDED: &str = "workflow candidate wfp1.";

/// A `pax` that passes when `src/lib.rs` defines `fn two`. With `<dir>/gate` present, a test run
/// writes `<dir>/started` and waits for `<dir>/release`.
fn checking_pax(tag: &str) -> Pax {
    let dir = unique(&format!("wfx-pax-{tag}"));
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'pax 0.4.1'; exit 0; fi\nif [ -e '{d}/gate' ]; then touch '{d}/started'; i=0; while [ ! -e '{d}/release' ] && [ $i -lt 6000 ]; do sleep 0.01; i=$((i+1)); done; fi\nif grep -q 'fn two' src/lib.rs 2>/dev/null; then echo '{PASSED}'; else echo '{FAILED}'; fi\nexit 0\n",
        d = dir.display()
    );
    std::fs::write(dir.join("pax"), script).unwrap();
    std::fs::set_permissions(dir.join("pax"), std::fs::Permissions::from_mode(0o755)).unwrap();
    Pax { dir }
}

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

async fn work(dir: &Path, model: &Model, pax: &Pax, data: &Path, args: &[&str]) -> Output {
    let mut c = binary(dir, model, pax, data);
    c.arg("work").args(args).arg("--json");
    output(c).await
}

async fn agent_state(dir: &Path, pax: &Pax, data: &Path, args: &[&str]) -> Value {
    let model = Model::start(vec![]).await;
    let mut c = binary(dir, &model, pax, data);
    c.arg("agent-state").args(args).arg("--json");
    let out = output(c).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    report(&out)
}

fn reset(dir: &Path) {
    std::fs::write(dir.join("src/lib.rs"), gated::LIB).unwrap();
    let _ = std::fs::remove_file(dir.join("src/two.rs"));
}

fn database_file(data: &Path) -> PathBuf {
    let found: Vec<PathBuf> = std::fs::read_dir(data.join("agent-state"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "feltdb"))
        .collect();
    assert_eq!(found.len(), 1, "{found:?}");
    found[0].clone()
}

/// A project whose database holds one verified work that followed `seed`, and so one candidate.
async fn seeded(tag: &str, seed: Vec<Reply>) -> (PathBuf, PathBuf, Pax) {
    let pax = checking_pax(tag);
    let dir = project(tag);
    let data = unique(&format!("{tag}-data"));
    let model = Model::start(seed).await;
    let out = work(&dir, &model, &pax, &data, &[GOAL, "--no-recall"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        report(&out)["episodes"]["workflows"]["written"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    reset(&dir);
    (dir, data, pax)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_eligible_workflow_runs_through_the_loop_with_one_model_call_and_fresh_verification() {
    let (dir, data, pax) = seeded("wfx-run", vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let model = Model::start(vec![Reply::Stream(WRITE)]).await;
    let out = work(
        &dir,
        &model,
        &pax,
        &data,
        &[GOAL, "--no-recall", "--workflows"],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let r = report(&out);
    let w = &r["workflow"];
    assert_eq!(w["followed"], true, "{w}");
    assert_eq!(w["selection"]["rule"], "the one eligible candidate");
    assert_eq!(
        (w["steps"].as_u64(), w["completed_steps"].as_u64()),
        (Some(2), Some(2))
    );
    assert_eq!(w["local_steps"], 1, "the test run was Chip's proposal");
    assert_eq!(w["stopped"], Value::Null);
    // One model call: for the write's content. The test and the completion were Chip's.
    assert_eq!(model.requests(), 1);
    let body = &model.bodies()[0];
    assert!(body.contains(GUIDED), "{body}");
    assert!(body.contains("step 1 of 2 is project.write with path=src/lib.rs; supply content"));
    // Verified on this work's own fresh evidence, with a clean audit.
    assert_eq!(r["verified"], true);
    assert_eq!(r["audit"]["clean"], true);
    assert_eq!(r["pax_executions"], 1);
    assert!(
        std::fs::read_to_string(dir.join("src/lib.rs"))
            .unwrap()
            .contains("fn two")
    );

    // The execution record, read by a new process; inspection changes nothing in the database.
    let db = database_file(&data);
    let before = std::fs::read(&db).unwrap();
    let runs = agent_state(&dir, &pax, &data, &["workflow-runs"]).await;
    let workflows = agent_state(&dir, &pax, &data, &["workflows"]).await;
    assert_eq!(
        std::fs::read(&db).unwrap(),
        before,
        "inspection writes nothing"
    );
    let run = &runs["workflow_runs"][0];
    assert_eq!(run["candidate"]["id"], w["selection"]["selected"]);
    let steps = &run["execution_status"]["steps"];
    assert_eq!(steps[0]["status"], "completed");
    assert_eq!(steps[0]["decided_by"], "model");
    assert_eq!(steps[1]["decided_by"], "local");
    for s in steps.as_array().unwrap() {
        assert!(
            s["observation_record"]
                .as_str()
                .unwrap()
                .starts_with("chip.Observation:"),
            "{s}"
        );
    }
    assert_eq!(run["execution_status"]["fallback_to_planner"], false);
    assert_eq!(run["final_verification"]["outcome"], "verified_success");
    assert_eq!(run["final_verification"]["verified"], true);
    // Eligibility is the candidate's; execution status and verdict are the run's.
    assert!(workflows["workflows"][0]["validation"].is_object());
    let text = runs.to_string();
    assert!(!text.contains("pub fn two") && !text.contains("returning"));
    // The executed steps went through the loop's own decisions: Chip's proposal, then validation
    // and execution like any other.
    let task = r["agent_state"]["task_id"].as_str().unwrap();
    let shown = agent_state(&dir, &pax, &data, &["show", "--task-id", task]).await;
    let kinds: Vec<&str> = shown["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["event"]["kind"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"LocalDecision"), "{kinds:?}");
    assert_eq!(
        kinds.iter().filter(|k| **k == "ExecutionCompleted").count(),
        2,
        "{kinds:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_flag_or_an_eligible_candidate_the_work_is_planned_exactly_as_before() {
    let (dir, data, pax) = seeded("wfx-off", vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    // Workflows off: the model plans every step; nothing is followed or recorded.
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let out = work(&dir, &model, &pax, &data, &[GOAL, "--no-recall"]).await;
    let r = report(&out);
    assert_eq!(r["workflow"], serde_json::json!({"enabled": false}));
    assert_eq!(model.requests(), 2);
    assert!(model.bodies().iter().all(|b| !b.contains(GUIDED)));
    // On, with a task the candidate does not match: not selected, and the same plan as without.
    reset(&dir);
    let other = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let out = work(
        &dir,
        &other,
        &pax,
        &data,
        &[
            "Document the deployment checklist.",
            "--no-recall",
            "--workflows",
        ],
    )
    .await;
    let r = report(&out);
    assert_eq!(r["workflow"]["followed"], false, "{r}");
    assert!(
        r["workflow"]["selection"]["ineligible"]["task_mismatch"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert!(other.bodies().iter().all(|b| !b.contains(GUIDED)));
    // And a changed verifier binary: the candidate's precondition fails; planned as before.
    reset(&dir);
    let script = pax.dir.join("pax");
    let mut changed = std::fs::read_to_string(&script).unwrap();
    changed.push_str("# rebuilt\n");
    std::fs::write(&script, changed).unwrap();
    let stale = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let out = work(
        &dir,
        &stale,
        &pax,
        &data,
        &[GOAL, "--no-recall", "--workflows"],
    )
    .await;
    let r = report(&out);
    assert_eq!(r["workflow"]["followed"], false, "{r}");
    assert!(
        r["workflow"]["selection"]["ineligible"]["precondition_unmet"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert_eq!(stale.requests(), 2);
    assert!(stale.bodies().iter().all(|b| !b.contains(GUIDED)));
    let runs = agent_state(&dir, &pax, &data, &["workflow-runs"]).await;
    assert_eq!(runs["workflow_runs"], serde_json::json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deviation_or_a_failed_verification_hands_the_work_to_the_planner_with_what_was_observed()
{
    let (dir, data, pax) = seeded("wfx-dev", vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    // The model writes another file at the guided step: the candidate is no longer followed, and
    // the planner (the model) finishes the work, verified on its own evidence.
    let model = Model::start(vec![
        Reply::Stream(WRITE_TWO),
        Reply::Stream(WRITE),
        Reply::Stream(TEST),
    ])
    .await;
    let out = work(
        &dir,
        &model,
        &pax,
        &data,
        &[GOAL, "--no-recall", "--workflows"],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let r = report(&out);
    assert_eq!(r["workflow"]["stopped"]["code"], "deviated", "{r}");
    assert_eq!(r["workflow"]["fallback"], true);
    assert_eq!(r["verified"], true);
    assert_eq!(model.requests(), 3);
    assert!(
        !model.bodies()[1].contains(GUIDED),
        "no guidance once it stopped"
    );

    // (The work just verified by another procedure is now a candidate too. For the same task, the
    // two conflict, and neither is eligible: tested in its own project below.)
    let followed = agent_state(&dir, &pax, &data, &["workflows", "--goal", GOAL]).await;
    for w in followed["workflows"].as_array().unwrap() {
        assert!(
            w["validation"]["codes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c == "conflicting_procedure"),
            "{w}"
        );
    }

    // The write's content makes the test fail: the test was Chip's step, its failure stops the
    // procedure, and the planner recovers from the failing observation. No step is repeated by the
    // candidate.
    let (dir, data, pax) = seeded(
        "wfx-verify",
        vec![Reply::Stream(WRITE), Reply::Stream(TEST)],
    )
    .await;
    let model = Model::start(vec![
        Reply::Stream(WRITE_BAD),
        Reply::Stream(WRITE),
        Reply::Stream(TEST),
    ])
    .await;
    let out = work(
        &dir,
        &model,
        &pax,
        &data,
        &[GOAL, "--no-recall", "--workflows"],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let r = report(&out);
    let w = &r["workflow"];
    assert_eq!(w["stopped"]["code"], "verification_failed", "{w}");
    assert_eq!(w["stopped"]["step"], 1);
    assert_eq!(w["completed_steps"], 1);
    assert_eq!(w["fallback"], true);
    assert_eq!(r["verified"], true);
    assert_eq!(r["pax_executions"], 2);
    // The second model request carries the failing test run as an observation.
    assert!(
        model.bodies()[1].contains("failed"),
        "{}",
        model.bodies()[1]
    );
    let runs = agent_state(&dir, &pax, &data, &["workflow-runs"]).await;
    let last = runs["workflow_runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["task_id"] == r["agent_state"]["task_id"])
        .unwrap();
    assert_eq!(last["execution_status"]["steps"][1]["status"], "failed");
    assert_eq!(last["execution_status"]["fallback_to_planner"], true);
    assert_eq!(last["final_verification"]["verified"], true);
}

fn intents_of(shown: &Value, capability: &str) -> usize {
    shown["intents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["action"].as_str().unwrap().contains(capability))
        .count()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_workflow_resumes_from_its_completed_steps_without_repeating_them() {
    // A three-step candidate: two writes the model supplies, then Chip's test run.
    let (dir, data, pax) = seeded(
        "wfx-resume",
        vec![
            Reply::Stream(WRITE),
            Reply::Stream(WRITE_TWO),
            Reply::Stream(TEST),
        ],
    )
    .await;
    // The first write runs; the model never answers for the second, and the process is killed.
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stall("")]).await;
    let mut c = binary(&dir, &model, &pax, &data);
    c.args([
        "work",
        GOAL,
        "--no-recall",
        "--workflows",
        "--json",
        "--lease-secs",
        "1",
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    model.wait_requests(2).await;
    child.kill().unwrap();
    child.wait().unwrap();
    let listed = agent_state(&dir, &pax, &data, &["list"]).await;
    let task = listed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["status"] == "interrupted")
        .unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    // The killed process's lease has to expire before the task can be continued.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    // Continued: the completed write is restored, not performed again; the model is asked for the
    // second; Chip runs the test.
    let again = Model::start(vec![Reply::Stream(WRITE_TWO)]).await;
    let out = work(
        &dir,
        &again,
        &pax,
        &data,
        &[GOAL, "--no-recall", "--workflows", "--task-id", &task],
    )
    .await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let r = report(&out);
    assert_eq!(r["workflow"]["completed_steps"], 3, "{r}");
    assert_eq!(again.requests(), 1);
    assert!(
        again.bodies()[0].contains("step 2 of 3"),
        "{}",
        again.bodies()[0]
    );
    let shown = agent_state(&dir, &pax, &data, &["show", "--task-id", &task]).await;
    assert_eq!(
        intents_of(&shown, "project.write"),
        2,
        "each write once, never repeated"
    );
    assert_eq!(intents_of(&shown, "pax.test"), 1);
    let runs = agent_state(&dir, &pax, &data, &["workflow-runs"]).await;
    let run = runs["workflow_runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["task_id"] == task.as_str())
        .unwrap();
    let by: Vec<&str> = run["execution_status"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["decided_by"].as_str().unwrap())
        .collect();
    assert_eq!(by, ["earlier_attempt", "model", "local"]);
    assert_eq!(run["final_verification"]["verified"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workflow_killed_mid_test_run_is_not_resumed_while_the_test_runs_outcome_is_unknown() {
    let (dir, data, pax) = seeded(
        "wfx-uncertain",
        vec![Reply::Stream(WRITE), Reply::Stream(TEST)],
    )
    .await;
    std::fs::write(pax.dir.join("gate"), "").unwrap();
    let model = Model::start(vec![Reply::Stream(WRITE)]).await;
    let mut c = binary(&dir, &model, &pax, &data);
    c.args([
        "work",
        GOAL,
        "--no-recall",
        "--workflows",
        "--json",
        "--lease-secs",
        "1",
    ])
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    let started = pax.dir.join("started");
    wait_until("Chip's test run to start", || started.exists()).await;
    child.kill().unwrap(); // the test run's outcome is now unknown
    child.wait().unwrap();
    std::fs::write(pax.dir.join("release"), "").unwrap();
    let listed = agent_state(&dir, &pax, &data, &["list"]).await;
    let task = listed["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["status"] == "interrupted")
        .unwrap()["task_id"]
        .as_str()
        .unwrap()
        .to_string();
    // The killed process's lease has to expire before the task can be continued.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    // `pax.test` declares itself consequential: its unknown outcome blocks continuation, and the
    // workflow does not get around that.
    let again = Model::start(vec![Reply::Stream(TEST)]).await;
    let out = work(
        &dir,
        &again,
        &pax,
        &data,
        &[GOAL, "--no-recall", "--workflows", "--task-id", &task],
    )
    .await;
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("outcome"), "{}", stderr(&out));
    assert_eq!(again.requests(), 0);
    let shown = agent_state(&dir, &pax, &data, &["show", "--task-id", &task]).await;
    assert_eq!(
        intents_of(&shown, "project.write"),
        1,
        "the write is not repeated"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_workflow_stops_and_says_so() {
    let (dir, data, pax) = seeded(
        "wfx-cancel",
        vec![Reply::Stream(WRITE), Reply::Stream(TEST)],
    )
    .await;
    let model = Model::start(vec![Reply::Stall("")]).await;
    let mut c = binary(&dir, &model, &pax, &data);
    c.args(["work", GOAL, "--no-recall", "--workflows", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = c.spawn().unwrap();
    model.wait_requests(1).await;
    // SIGTERM: the run's own cancellation path.
    let _ = std::process::Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    model.release();
    let out = tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap();
    let r = report(&out);
    assert_eq!(r["terminal_state"], "cancelled", "{r}");
    assert_eq!(r["workflow"]["stopped"]["code"], "cancelled", "{r}");
    assert_eq!(r["verified"], false);
    let runs = agent_state(&dir, &pax, &data, &["workflow-runs"]).await;
    let run = &runs["workflow_runs"][0];
    assert_eq!(run["execution_status"]["stopped"]["code"], "cancelled");
    assert_eq!(run["final_verification"]["outcome"], "interrupted");
    assert_eq!(run["execution_status"]["steps"][0]["status"], "not_reached");
}
