//! The failure coding benchmark run 2 ended on, through the real binary: a task's last step is a
//! small change to a tracked, generated file larger than a whole-file write may carry (AppPort's
//! `docs/audit.json`). With `project.write`'s edit form the change is one bounded edit; an edit that
//! cannot apply is an observation the work recovers from, and nothing is written.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::path::Path;
use std::process::Output;

use gated::{Model, PASSED, Pax, Reply, command, project};
use serde_json::Value;
use sha2::{Digest, Sha256};

const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const CLAIM: &str = r#"{"decision":"complete","summary":"Done."}"#;
const AUDIT: &str = "docs/audit.json";
const ANCHOR: &str = "        \"packages/server/test/desktop-host.test.ts\",\n";
const ADDED: &str = "        \"packages/server/test/event-authorization.test.ts\",\n";

/// A generated JSON file over the whole-write limit, in which the anchor's path occurs twice.
fn audit() -> String {
    let mut s = String::from("{\n  \"workflows\": [\n");
    for n in 0..900 {
        s.push_str(&format!(
            "        \"packages/pkg{n:03}/test/case.test.ts\",\n"
        ));
    }
    s.push_str("        \"packages/server/test/desktop-host.test.ts\"\n  ],\n  \"tests\": [\n");
    s.push_str(ANCHOR);
    s.push_str("        \"end\"\n  ]\n}\n");
    s
}

fn sha(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn leak(v: Value) -> &'static str {
    Box::leak(v.to_string().into_boxed_str())
}

fn request(inputs: Value) -> &'static str {
    leak(
        serde_json::json!({"decision": "request_capability", "capability": "project.write", "inputs": inputs}),
    )
}

fn read_range() -> &'static str {
    leak(serde_json::json!({
        "decision": "request_capability", "capability": "project.read",
        "inputs": {"path": AUDIT, "offset": 0, "length": 4096},
    }))
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

async fn work(tag: &str, script: Vec<Reply>) -> (Output, std::path::PathBuf) {
    let model = Model::start(script).await;
    let pax = Pax::new(tag, PASSED, false);
    let dir = project(tag);
    std::fs::create_dir_all(dir.join("docs")).unwrap();
    std::fs::write(dir.join(AUDIT), audit()).unwrap();
    let mut cmd = command(&dir, &model.url, &pax);
    cmd.args([
        "work",
        "--json",
        "Register the new test file and keep the tests passing.",
    ]);
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    (out, dir)
}

fn on_disk(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(AUDIT)).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_small_change_to_a_large_generated_file_is_one_bounded_edit_and_verifies() {
    let base = audit();
    assert!(base.len() > chip_project::MAX_WRITE_BYTES);
    let digest = sha(&base);
    // First an edit keyed on the path alone, which occurs twice: refused, nothing written, and
    // the work goes on. Then the edit keyed on the unique line.
    let ambiguous = request(serde_json::json!({
        "path": AUDIT, "expected_sha256": digest,
        "replace": "\"packages/server/test/desktop-host.test.ts\"",
        "with": "\"packages/server/test/desktop-host.test.ts\", \"x\"",
    }));
    let unique = request(serde_json::json!({
        "path": AUDIT, "expected_sha256": digest,
        "replace": ANCHOR, "with": format!("{ANCHOR}{ADDED}"),
    }));
    let (out, dir) = work(
        "audit-edit",
        vec![
            Reply::Stream(read_range()),
            Reply::Stream(ambiguous),
            Reply::Stream(unique),
            Reply::Stream(TEST),
            Reply::Stream(CLAIM),
        ],
    )
    .await;
    let j = json(&out);
    assert_eq!(
        on_disk(&dir),
        base.replacen(ANCHOR, &format!("{ANCHOR}{ADDED}"), 1)
    );
    // One write, counted as a change to that file; the refused edit is a failed observation the
    // loop carried on from, not a write and not the end of the work.
    assert_eq!(j["writes"], 1, "{j}");
    assert_eq!(j["changed_writes"], 1, "{j}");
    assert_eq!(j["paths_written"], serde_json::json!([AUDIT]), "{j}");
    assert_eq!(j["failed_observations"], 1, "{j}");
    assert_eq!(j["write_conflicts"], serde_json::json!([]), "{j}");
    // Verified the ordinary way: PAX passed after the last change.
    assert_eq!(j["verified"], true, "{j}");
    assert_eq!(j["terminal_state"], "completed", "{j}");
    assert_eq!(out.status.code(), Some(0), "{j}");
    assert_eq!(j["audit"]["clean"], true, "{j}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_edit_writes_nothing_and_verifies_nothing() {
    let base = audit();
    // Based on a file that is not the one on disk: refused as stale_base, and the passing test run
    // afterwards verifies nothing, because no change was made.
    let stale = request(serde_json::json!({
        "path": AUDIT, "expected_sha256": sha("some other version of the file"),
        "replace": ANCHOR, "with": format!("{ANCHOR}{ADDED}"),
    }));
    let block = r#"{"decision":"block","reason":"scripted"}"#;
    let (out, dir) = work(
        "stale-edit",
        vec![
            Reply::Stream(stale),
            Reply::Stream(TEST),
            Reply::Stream(CLAIM),
            Reply::Stream(block),
        ],
    )
    .await;
    let j = json(&out);
    assert_eq!(on_disk(&dir), base, "untouched");
    assert_eq!(j["writes"], 0, "{j}");
    assert_eq!(j["write_conflicts"], serde_json::json!([AUDIT]), "{j}");
    assert_eq!(j["verified"], false, "{j}");
    assert_ne!(j["terminal_state"], "completed", "{j}");
    assert_eq!(j["audit"]["clean"], true, "{j}");
}

#[tokio::test(flavor = "multi_thread")]
async fn rewriting_the_large_file_whole_is_still_refused_at_the_limit() {
    let base = audit();
    // What the model sent in run 2: the whole file as content. The limit is unchanged.
    let whole = request(serde_json::json!({
        "path": AUDIT, "expected_sha256": sha(&base),
        "content": base.replacen(ANCHOR, &format!("{ANCHOR}{ADDED}"), 1),
    }));
    let (out, dir) = work("whole", vec![Reply::Stream(whole)]).await;
    let j = json(&out);
    assert_eq!(on_disk(&dir), base);
    assert_eq!(j["writes"], 0, "{j}");
    assert_eq!(j["verified"], false, "{j}");
    assert_ne!(out.status.code(), Some(0), "{j}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_to_a_test_file_is_seen_by_the_weakened_test_check_like_any_replacement() {
    use chip_core::{
        CapabilityId, CapabilityProvider, ExecutionId, ExecutionRequest, ExecutionStatus, Executor,
        InputValue, Observation, ObservationKind,
    };
    let dir = project("edit-test-file");
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    let test = "#[test]\nfn one() { assert_eq!(1, 1); }\n";
    std::fs::write(dir.join("tests/one.rs"), test).unwrap();
    let p = chip_project::ProjectExecutor::new(&dir);
    let inputs: std::collections::BTreeMap<String, InputValue> = [
        ("path", "tests/one.rs".to_string()),
        ("replace", "assert_eq!(1, 1);".to_string()),
        ("with", String::new()),
        ("expected_sha256", sha(test)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), InputValue::Text(v)))
    .collect();
    p.validate_inputs(&CapabilityId::new("project.write").unwrap(), &inputs)
        .await
        .unwrap();
    let r = p
        .execute(ExecutionRequest::new(ExecutionId::new("e"), "project.write").with_inputs(inputs))
        .await
        .unwrap();
    assert_eq!(r.status, ExecutionStatus::Success, "{}", r.output);
    let observation = Observation {
        execution_id: ExecutionId::new("e"),
        kind: ObservationKind::ExecutionCompleted,
        status: r.status,
        output: Some(r.output.clone()),
        receipt_id: None,
        evidence: None,
    };
    // The assertion is gone; the check that refuses verification on weakened tests sees the file.
    assert_eq!(
        chip_cli::software_work::replaced_test_files(&[observation]),
        vec!["tests/one.rs".to_string()]
    );
}
