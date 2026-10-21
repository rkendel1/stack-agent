//! Roadmap P0-09 through the real binary, with a scripted model and a stand-in `pax`: the work
//! reads a file, a person edits it, and the work writes based on what it read.
//!
//! - The write carrying the digest it read is refused as `stale_base`; the person's edit survives.
//! - A passing test run afterwards does not verify the work, even when the work had already made
//!   another, valid change: the change it meant to make to the edited file is not in it.
//! - Reading the file again and writing against what it now holds resolves the conflict, and the
//!   work can then be verified.
//! - Writing again without a precondition does not resolve it: Chip cannot tell that write was
//!   decided against the newer content, so the work is not verified.
//!
//! The person's edit happens while the model's reply carrying the write is held open: after the
//! read was observed and before the write is performed, with no timing involved.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::process::Output;

use chip_cli::software_work::{VerifiedChange, unresolved_write_conflicts};
use chip_core::{ExecutionId, ExecutionStatus, Observation, ObservationKind, ObservationPredicate};
use gated::{LIB, Model, PASSED, Pax, Reply, command, project};
use serde_json::Value;
use sha2::{Digest, Sha256};

const USER_EDIT: &str = "pub fn one() -> u8 { 1 }\n// kept by a person\n";
const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const ADD_TWO: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/two.rs","content":"pub fn two() -> u8 { 2 }\n","expect_absent":true}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const CLAIM: &str = r#"{"decision":"complete","summary":"Done."}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;

fn sha(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A write of `content` to src/lib.rs, based on the file holding `based_on` (or on nothing).
fn write_lib(content: &str, based_on: Option<&str>) -> &'static str {
    let mut inputs = serde_json::json!({"path": "src/lib.rs", "content": content});
    if let Some(base) = based_on {
        inputs["expected_sha256"] = sha(base).into();
    }
    let decision = serde_json::json!({
        "decision": "request_capability",
        "capability": "project.write",
        "inputs": inputs,
    });
    Box::leak(decision.to_string().into_boxed_str())
}

/// Runs `chip work` with `script`. The reply at `stall_at` is held open until the person's edit
/// has been made to src/lib.rs.
async fn work(tag: &str, script: Vec<Reply>, stall_at: usize) -> (Output, std::path::PathBuf) {
    let model = Model::start(script).await;
    let pax = Pax::new(tag, PASSED, false);
    let dir = project(tag);
    let mut cmd = command(&dir, &model.url, &pax);
    cmd.args(["work", "--json", "Add `two` and keep the tests passing."]);
    let run = tokio::task::spawn_blocking(move || cmd.output().unwrap());
    // Request `stall_at` is being answered, and held: everything before it has been performed.
    model.wait_requests(stall_at + 1).await;
    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        LIB,
        "nothing wrote the file before the person did"
    );
    std::fs::write(dir.join("src/lib.rs"), USER_EDIT).unwrap();
    model.release();
    (run.await.unwrap(), dir)
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
async fn a_write_based_on_what_was_read_does_not_overwrite_a_later_edit_or_verify() {
    let (out, dir) = work(
        "p009",
        vec![
            Reply::Stream(ADD_TWO),
            Reply::Stream(READ),
            Reply::Stall(write_lib(
                "pub fn one() -> u8 { 1 }\npub mod two;\n",
                Some(LIB),
            )),
            Reply::Stream(TEST),
            Reply::Stream(CLAIM),
            Reply::Stream(BLOCK),
        ],
        2,
    )
    .await;
    let j = json(&out);
    // The person's edit is intact.
    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        USER_EDIT
    );
    // The valid change the work made is there, and is reported as the only write.
    assert_eq!(
        std::fs::read_to_string(dir.join("src/two.rs")).unwrap(),
        "pub fn two() -> u8 { 2 }\n"
    );
    assert_eq!(j["paths_written"], serde_json::json!(["src/two.rs"]), "{j}");
    assert_eq!(j["changed_writes"], 1);
    // The refused write is a conflict, not a write; the passing run after it verifies nothing,
    // and the model's completion claim is not taken.
    assert_eq!(
        j["write_conflicts"],
        serde_json::json!(["src/lib.rs"]),
        "{j}"
    );
    assert_eq!(j["pax"]["last_status"], "passed", "{j}");
    assert_eq!(j["verified"], false, "{j}");
    assert_eq!(out.status.code(), Some(1), "{j}");
    // The refusal was one failed observation the loop carried on from (the model was asked
    // again, ran the tests, and was refused completion), not the end of the work.
    assert_eq!(j["failed_observations"], 1, "{j}");
    assert!(
        j["outcome_reason"]
            .as_str()
            .is_some_and(|r| r.starts_with("completion refused")),
        "{j}"
    );
    assert_eq!(j["terminal_state"], "blocked", "{j}");
    assert_eq!(j["audit"]["clean"], true, "{j}");
    assert_eq!(j["audit"]["false_completions"], 0, "{j}");
}

#[tokio::test(flavor = "multi_thread")]
async fn reading_again_and_deciding_anew_resolves_the_conflict() {
    let resolved = "pub fn one() -> u8 { 1 }\n// kept by a person\npub mod two;\n";
    let (out, dir) = work(
        "p009-resolved",
        vec![
            Reply::Stream(ADD_TWO),
            Reply::Stream(READ),
            Reply::Stall(write_lib(
                "pub fn one() -> u8 { 1 }\npub mod two;\n",
                Some(LIB),
            )),
            // The refusal is shown; the model reads the file again and writes against it.
            Reply::Stream(READ),
            Reply::Stream(write_lib(resolved, Some(USER_EDIT))),
            Reply::Stream(TEST),
            Reply::Stream(BLOCK),
        ],
        2,
    )
    .await;
    let j = json(&out);
    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        resolved
    );
    assert_eq!(j["write_conflicts"], serde_json::json!([]), "{j}");
    assert_eq!(
        j["paths_written"],
        serde_json::json!(["src/two.rs", "src/lib.rs"]),
        "{j}"
    );
    assert_eq!(j["verified"], true, "{j}");
    assert_eq!(out.status.code(), Some(0), "{j}");
}

#[tokio::test(flavor = "multi_thread")]
async fn writing_again_without_a_precondition_does_not_resolve_it() {
    let (out, dir) = work(
        "p009-blind",
        vec![
            Reply::Stream(ADD_TWO),
            Reply::Stream(READ),
            Reply::Stall(write_lib(
                "pub fn one() -> u8 { 1 }\npub mod two;\n",
                Some(LIB),
            )),
            // The same change again, with no precondition: performed (an unconditional write
            // behaves as it always did), but it says nothing about the person's edit.
            Reply::Stream(write_lib("pub fn one() -> u8 { 1 }\npub mod two;\n", None)),
            Reply::Stream(TEST),
            Reply::Stream(BLOCK),
        ],
        2,
    )
    .await;
    let j = json(&out);
    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        "pub fn one() -> u8 { 1 }\npub mod two;\n",
        "an unconditional write is not prevented"
    );
    assert_eq!(
        j["write_conflicts"],
        serde_json::json!(["src/lib.rs"]),
        "{j}"
    );
    assert_eq!(j["pax"]["last_status"], "passed", "{j}");
    assert_eq!(j["verified"], false, "{j}");
    assert_eq!(out.status.code(), Some(1), "{j}");
}

// ---- the predicate itself, over recorded observations ---------------------------------------------

fn observation(ok: bool, line: Value) -> Observation {
    Observation {
        execution_id: ExecutionId::new("e"),
        kind: if ok {
            ObservationKind::ExecutionCompleted
        } else {
            ObservationKind::ExecutionFailed
        },
        status: if ok {
            ExecutionStatus::Success
        } else {
            ExecutionStatus::Failure
        },
        output: Some(line.to_string()),
        receipt_id: None,
        evidence: None,
    }
}

fn written(path: &str, precondition: Option<&str>) -> Observation {
    let mut line = serde_json::json!({
        "capability": "project.write", "path": path, "operation": "replaced",
        "bytes_written": 1, "changed": true, "sha256": "00",
    });
    if let Some(p) = precondition {
        line["precondition"] = p.into();
    }
    observation(true, line)
}

fn stale(path: &str) -> Observation {
    observation(
        false,
        serde_json::json!({"capability": "project.write", "path": path, "error": "stale_base", "found": "different_content", "precondition": "sha256"}),
    )
}

fn passed() -> Observation {
    Observation {
        execution_id: ExecutionId::new("t"),
        kind: ObservationKind::ExecutionCompleted,
        status: ExecutionStatus::Success,
        output: Some(PASSED.to_string()),
        receipt_id: None,
        evidence: None,
    }
}

#[test]
fn a_stale_write_blocks_verification_until_that_file_is_decided_anew() {
    let verified = |o: &[Observation]| VerifiedChange.satisfied_by_trajectory(o);
    // The baseline: a change, then a pass.
    assert!(verified(&[written("a.rs", None), passed()]));
    // A refused write alone is not a change.
    assert!(!verified(&[stale("b.rs"), passed()]));
    assert_eq!(unresolved_write_conflicts(&[stale("b.rs")]), ["b.rs"]);
    // Another valid change does not cover it, before or after.
    assert!(!verified(&[written("a.rs", None), stale("b.rs"), passed()]));
    assert!(!verified(&[stale("b.rs"), written("a.rs", None), passed()]));
    // A later unconditional write to it does not resolve it.
    assert!(!verified(&[stale("b.rs"), written("b.rs", None), passed()]));
    // A checked write to another file does not either.
    assert!(!verified(&[
        stale("b.rs"),
        written("a.rs", Some("sha256")),
        passed()
    ]));
    // A later checked write to that file does.
    let resolved = [stale("b.rs"), written("b.rs", Some("sha256")), passed()];
    assert!(verified(&resolved));
    assert!(unresolved_write_conflicts(&resolved).is_empty());
    // A checked write before the refusal resolves nothing.
    assert!(!verified(&[
        written("b.rs", Some("sha256")),
        stale("b.rs"),
        passed()
    ]));
    // Repeated refusals of one path are one conflict.
    assert_eq!(
        unresolved_write_conflicts(&[stale("b.rs"), stale("b.rs"), stale("c.rs")]),
        ["b.rs", "c.rs"]
    );
}
