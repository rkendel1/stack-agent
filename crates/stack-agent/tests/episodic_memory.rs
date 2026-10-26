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

/// `agent-state <args> --json` over the project's default database.
async fn agent_state(dir: &Path, model: &Model, pax: &Pax, data: &Path, args: &[&str]) -> Value {
    let mut c = binary(dir, model, pax, data);
    c.arg("agent-state").args(args).arg("--json");
    let out = output(c).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    report(&out)
}

#[tokio::test(flavor = "multi_thread")]
async fn episodes_can_be_inspected_and_a_changed_verifier_binary_ends_their_recall() {
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let pax = Pax::new("episodes-inspect", PASSED, false);
    let dir = project("episodes-inspect");
    let data = unique("episodes-inspect-data");
    let goal = "Add a function `two` returning 2.";
    let out = work(&dir, &model, &pax, &data, &[goal]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let first = report(&out);
    let id = first["episodes"]["recorded"]["episode"]
        .as_str()
        .unwrap()
        .to_string();

    // What is recorded, read back from the database: identity, schema, outcome, time, the records
    // it cites, and that it could be recalled here. No goal words, only how many terms there are.
    let shown = agent_state(&dir, &model, &pax, &data, &["episodes"]).await;
    let e = &shown["episodes"][0];
    assert_eq!(e["episode"], id.as_str(), "{shown}");
    assert_eq!(e["schema"], "chip.episode.v1");
    assert!(e["project"].as_str().unwrap().starts_with("path:"));
    assert_eq!(e["this_project"], true);
    assert_eq!(e["kind"], "change");
    assert_eq!(e["outcome"], "verified_success");
    assert!(e["recorded_at_ms"].as_u64().unwrap() > 0);
    assert_eq!(e["evidence"]["result"], format!("chip.WorkResult:{id}"));
    assert_eq!(e["evidence"]["observations"].as_array().unwrap().len(), 2);
    assert!(e["goal_terms"].as_u64().unwrap() > 0);
    assert_eq!(e["recallable_here"], true, "{e}");
    let text = shown.to_string();
    for word in ["function", "returning", dir.to_str().unwrap()] {
        assert!(!text.contains(word), "{word} in {text}");
    }

    // The verifier binary changes (same version, different program): the episode no longer
    // qualifies, and a matching work is told nothing of it.
    let script = pax.dir.join("pax");
    let mut changed = std::fs::read_to_string(&script).unwrap();
    changed.push_str("# rebuilt\n");
    std::fs::write(&script, changed).unwrap();
    let shown = agent_state(&dir, &model, &pax, &data, &["episodes"]).await;
    assert_eq!(shown["episodes"][0]["recallable_here"], false, "{shown}");
    assert!(
        shown["episodes"][0]["not_recallable_because"]
            .as_str()
            .unwrap()
            .contains("verifier"),
        "{shown}"
    );
    let model2 = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let out = work(&dir, &model2, &pax, &data, &[goal]).await;
    let second = report(&out);
    assert_eq!(
        second["episodes"]["recall"]["recalled"],
        serde_json::json!([]),
        "{second}"
    );
    assert_eq!(
        second["episodes"]["recall"]["excluded"]["preconditions_differ"], 1,
        "{second}"
    );
    assert!(model2.bodies().iter().all(|b| !b.contains(RECALLED)));
}

#[tokio::test(flavor = "multi_thread")]
async fn work_with_no_recall_sends_exactly_the_request_a_project_without_episodes_sends() {
    let pax = Pax::new("episodes-unchanged", PASSED, false);
    let goal = "Add a function `two` returning 2.";
    let dir = project("episodes-unchanged");
    let data = unique("episodes-unchanged-data");
    let seed = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let out = work(&dir, &seed, &pax, &data, &[goal]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));

    let off = Model::start(vec![Reply::Stream(BLOCK)]).await;
    work(&dir, &off, &pax, &data, &[goal, "--no-recall"]).await;
    let fresh_dir = project("episodes-unchanged-fresh");
    let fresh_data = unique("episodes-unchanged-fresh-data");
    let empty = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let out = work(&fresh_dir, &empty, &pax, &fresh_data, &[goal]).await;
    assert_eq!(report(&out)["episodes"]["recall"]["considered"], 0);
    assert_eq!(off.bodies()[0], empty.bodies()[0]);
}

#[tokio::test(flavor = "multi_thread")]
async fn feedback_is_recorded_from_host_evidence_and_never_changes_the_episode_it_is_about() {
    let pax = Pax::new("episodes-feedback", PASSED, false);
    let dir = project("episodes-feedback");
    let data = unique("episodes-feedback-data");
    let goal = "Add a function `two` returning 2.";
    let seed = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let first = report(&work(&dir, &seed, &pax, &data, &[goal]).await);
    let id = first["episodes"]["recorded"]["episode"]
        .as_str()
        .unwrap()
        .to_string();
    let before = agent_state(&dir, &seed, &pax, &data, &["episodes"]).await["episodes"][0].clone();
    assert_eq!(before["feedback"]["counts"], serde_json::json!({}));

    // A new process recalls it, performs its write first, and the next test passes: `helpful`.
    let again = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let second = report(&work(&dir, &again, &pax, &data, &[goal]).await);
    let feedback = &second["episodes"]["feedback"];
    assert_eq!(feedback.as_array().unwrap().len(), 1, "{second}");
    assert_eq!(feedback[0]["episode"], id.as_str());
    assert_eq!(feedback[0]["classification"], "helpful", "{feedback}");
    assert_eq!(feedback[0]["attempted"], true);
    assert_eq!(feedback[0]["verification_after"], "success");

    // A --no-recall work is told of nothing and gives no feedback.
    let off = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let third = report(&work(&dir, &off, &pax, &data, &[goal, "--no-recall"]).await);
    assert_eq!(
        third["episodes"]["feedback"],
        serde_json::json!([]),
        "{third}"
    );
    assert!(off.bodies().iter().all(|b| !b.contains(RECALLED)));

    // Inspection shows the feedback beside the episode, which is otherwise exactly as it was.
    let shown = agent_state(&dir, &seed, &pax, &data, &["episodes"]).await;
    let after = shown["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["episode"] == id.as_str())
        .unwrap()
        .clone();
    assert_eq!(
        after["feedback"]["counts"],
        serde_json::json!({"helpful": 1})
    );
    assert_eq!(after["feedback"]["observational_only"], true);
    let strip = |mut e: Value| {
        e.as_object_mut().unwrap().remove("feedback");
        e
    };
    assert_eq!(
        strip(after),
        strip(before),
        "the episode itself is unchanged"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_test_definitions_exclude_an_episode_with_a_stable_reason() {
    let pax = Pax::new("episodes-tests-changed", PASSED, false);
    let dir = project("episodes-tests-changed");
    let data = unique("episodes-tests-changed-data");
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(dir.join("tests/two.rs"), "#[test]\nfn two() {}\n").unwrap();
    let goal = "Add a function `two` returning 2.";
    let seed = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let out = work(&dir, &seed, &pax, &data, &[goal]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let shown = agent_state(&dir, &seed, &pax, &data, &["episodes"]).await;
    assert_eq!(shown["episodes"][0]["recallable_here"], true, "{shown}");
    let checks = &shown["episodes"][0]["eligibility"]["checks"];
    assert_eq!(checks["hard_exclusion"]["status"], "passed", "{checks}");
    assert_eq!(checks["evidence_quality"]["status"], "passed");
    assert_eq!(checks["relevance"]["status"], "not_evaluated");
    assert_eq!(checks["context_compatibility"]["status"], "passed");

    // The project's tests change: the episode passed different tests.
    std::fs::write(
        dir.join("tests/two.rs"),
        "#[test]\nfn two() { assert_eq!(2, 2); }\n",
    )
    .unwrap();
    let shown = agent_state(&dir, &seed, &pax, &data, &["episodes"]).await;
    let e = &shown["episodes"][0];
    assert_eq!(e["recallable_here"], false, "{shown}");
    assert_eq!(e["eligibility"]["dimension"], "context_compatibility");
    assert_eq!(e["eligibility"]["code"], "preconditions_differ");
    assert!(
        e["eligibility"]["reason"]
            .as_str()
            .unwrap()
            .contains("test definitions changed"),
        "{e}"
    );
    // And a work recalls nothing, for the same reason, stated the same way.
    let model = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let next = report(&work(&dir, &model, &pax, &data, &[goal]).await);
    let recall = &next["episodes"]["recall"];
    assert_eq!(recall["recalled"], serde_json::json!([]), "{next}");
    assert_eq!(
        recall["excluded_by_dimension"],
        serde_json::json!({"context_compatibility": 1})
    );
    assert_eq!(
        recall["eligibility"][0]["reason"],
        e["eligibility"]["reason"]
    );
    assert!(model.bodies().iter().all(|b| !b.contains(RECALLED)));
    assert!(!next.to_string().contains(dir.to_str().unwrap()));
}
