//! Completion is established by host-side evidence, never by the model, and evidence that no
//! longer describes the project is not accepted.
//!
//! Through the real binary with a scripted model and a stand-in `pax`:
//! - a change that replaced an existing test file is not verified by a passing run;
//! - failed and skipped-only test results are not passing evidence, and a red run's counts are
//!   reported as incomplete;
//! - a completion claim with no test evidence is refused.
//! Through the real PAX executor and the runtime's acceptance check:
//! - a project changed after the passing run (outside any observed write) invalidates the result.
//! Already covered elsewhere: a model claiming success with no evidence (`product.rs`
//! `a_model_that_only_claims_success_is_not_believed`) and a grounded inspection that is not
//! verified (`product.rs` `a_tool_round_trip_runs_chip_over_fx_in_one_process`).

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::process::Output;

use chip_core::{ExecutionId, ExecutionRequest, Executor, Observation};
use gated::{FAILED, Model, PASSED, Pax, Reply, command, project};
use serde_json::Value;

const WRITE_SRC: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const WRITE_TEST: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"tests/one.rs","content":"// the assertion was removed\n"}}"#;
const ADD_TEST: &str = r##"{"decision":"request_capability","capability":"project.write","inputs":{"path":"tests/two.rs","content":"#[test]\nfn two() {}\n"}}"##;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;
const CLAIM: &str = r#"{"decision":"complete","summary":"Implemented and tested."}"#;
const SKIPPED_ONLY: &str = r#"{"schema":"pax.execution-result.v1","operation":"test","status":"passed","reason":"tests-passed","tool":"cargo","exit_code":0,"tests":{"passed":0,"failed":0,"ignored":4,"measured":0}}"#;

async fn work(tag: &str, script: Vec<Reply>, pax_result: &str, existing_test: bool) -> Output {
    let model = Model::start(script).await;
    let pax = Pax::new(tag, pax_result, false);
    let dir = project(tag);
    if existing_test {
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        std::fs::write(
            dir.join("tests/one.rs"),
            "#[test]\nfn one() { assert_eq!(probe::one(), 1); }\n",
        )
        .unwrap();
    }
    let mut cmd = command(&dir, &model.url, &pax);
    cmd.args(["work", "--json", "Add `two` and keep the tests passing."]);
    tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap()
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "not JSON ({e}): {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_passing_run_after_replacing_an_existing_test_file_verifies_nothing() {
    let out = work(
        "tamper",
        vec![
            Reply::Stream(WRITE_SRC),
            Reply::Stream(WRITE_TEST),
            Reply::Stream(TEST),
            Reply::Stream(BLOCK),
        ],
        PASSED,
        true,
    )
    .await;
    let j = json(&out);
    assert_eq!(out.status.code(), Some(1), "{j}");
    assert_eq!(j["verified"], false);
    assert_eq!(
        j["pax"]["last_status"], "passed",
        "the run passed, and still proves nothing"
    );
    assert_eq!(
        j["evidence"]["test_files_replaced"],
        serde_json::json!(["tests/one.rs"])
    );
    assert_ne!(
        j["terminal_state"], "completed",
        "Chip did not complete on it"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn adding_a_new_test_file_is_not_tampering() {
    let out = work(
        "new-test",
        vec![
            Reply::Stream(WRITE_SRC),
            Reply::Stream(ADD_TEST),
            Reply::Stream(TEST),
        ],
        PASSED,
        true,
    )
    .await;
    let j = json(&out);
    assert_eq!(out.status.code(), Some(0), "{j}");
    assert_eq!(j["verified"], true);
    assert_eq!(j["evidence"]["test_files_replaced"], serde_json::json!([]));
    assert_eq!(j["evidence"]["invalidated"], Value::Null);
    assert_eq!(j["pax"]["last_tests"]["complete"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_and_skipped_only_results_are_not_passing_evidence() {
    for (tag, result, complete) in [("failed", FAILED, false), ("skipped", SKIPPED_ONLY, true)] {
        let out = work(
            tag,
            vec![
                Reply::Stream(WRITE_SRC),
                Reply::Stream(TEST),
                Reply::Stream(BLOCK),
            ],
            result,
            false,
        )
        .await;
        let j = json(&out);
        assert_eq!(out.status.code(), Some(1), "{tag}: {j}");
        assert_eq!(j["verified"], false, "{tag}");
        assert_ne!(j["terminal_state"], "completed", "{tag}");
        // PAX's counts are carried as given, with whether they describe the whole run.
        assert_eq!(j["pax"]["last_tests"]["complete"], complete, "{tag}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_completion_claim_without_test_evidence_is_refused() {
    let out = work(
        "no-evidence",
        vec![
            Reply::Stream(WRITE_SRC),
            Reply::Stream(CLAIM),
            Reply::Stream(BLOCK),
        ],
        PASSED,
        false,
    )
    .await;
    let j = json(&out);
    assert_eq!(out.status.code(), Some(1), "{j}");
    assert_eq!(j["verified"], false);
    assert_eq!(j["pax_executions"], 0, "no test ran");
    assert_ne!(j["terminal_state"], "completed");
}

/// The real PAX executor records the project's fingerprint as the run left it; the runtime's
/// acceptance check refuses that evidence once the project has changed outside any observed write.
#[tokio::test(flavor = "multi_thread")]
async fn a_project_changed_after_the_passing_run_invalidates_the_evidence() {
    let pax = Pax::new("stale", PASSED, false);
    let dir = project("stale");
    let executor = chip_pax::PaxExecutor::new(&dir).with_binary(pax.dir.join("pax"));
    let result = executor
        .execute(ExecutionRequest::new(
            ExecutionId::new("t-1"),
            chip_pax::PAX_TEST_CAPABILITY,
        ))
        .await
        .unwrap();
    let observation = Observation {
        execution_id: result.id.clone(),
        kind: chip_core::ObservationKind::ExecutionCompleted,
        status: result.status,
        output: Some(result.output.clone()),
        receipt_id: None,
        evidence: None,
    };
    assert!(
        chip_pax::observed_fingerprint(&result.output).is_some(),
        "{}",
        result.output
    );
    let observations = vec![observation];
    assert_eq!(
        chip_cli::software_work::stale_test_evidence(&dir, &observations),
        None,
        "unchanged: the evidence still describes the project"
    );

    // Build output does not count; a source change does.
    std::fs::create_dir_all(dir.join("target")).unwrap();
    std::fs::write(dir.join("target/artifact"), "x").unwrap();
    assert_eq!(
        chip_cli::software_work::stale_test_evidence(&dir, &observations),
        None
    );
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() -> u8 { 2 }\n").unwrap();
    let why = chip_cli::software_work::stale_test_evidence(&dir, &observations).unwrap();
    assert!(why.contains("changed after the passing test run"), "{why}");
}
