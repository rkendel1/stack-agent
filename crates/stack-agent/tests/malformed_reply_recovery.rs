//! A malformed model reply through the real binary: a scripted model, a stand-in `pax`, a real
//! project directory. The reply that wraps a write in narration writes nothing, verifies nothing
//! and resolves no conflict; the model is told why and asked again, a bounded number of times.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::process::Output;

use gated::{LIB, Model, PASSED, Pax, Reply, command, project};
use serde_json::Value;
use sha2::{Digest, Sha256};

const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const CLAIM: &str = r#"{"decision":"complete","summary":"Done."}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;
const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const NARRATION: &str = "I've read the code, so next I'm writing the fix.\n";
const USER_EDIT: &str = "pub fn one() -> u8 { 1 }\n// kept by a person\n";

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn sha(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A write of `content` to `path`, as a model sends it.
fn write(path: &str, content: &str, based_on: Option<&str>) -> &'static str {
    let mut inputs = serde_json::json!({"path": path, "content": content});
    match based_on {
        Some(base) => inputs["expected_sha256"] = sha(base).into(),
        None => inputs["expect_absent"] = true.into(),
    }
    leak(
        serde_json::json!({"decision": "request_capability", "capability": "project.write", "inputs": inputs})
            .to_string(),
    )
}

fn narrated(decision: &str) -> &'static str {
    leak(format!("{NARRATION}{decision}"))
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

async fn work(
    tag: &str,
    script: Vec<Reply>,
    extra: &[&str],
) -> (Output, std::path::PathBuf, usize) {
    let model = Model::start(script).await;
    let pax = Pax::new(tag, PASSED, false);
    let dir = project(tag);
    let mut cmd = command(&dir, &model.url, &pax);
    cmd.args(["work", "--json", "Add `two` and keep the tests passing."])
        .args(extra);
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    (out, dir, model.requests())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_narrated_write_writes_nothing_and_the_recovered_write_is_the_only_one() {
    let wrapped = write(
        "src/two.rs",
        "pub fn two() -> u8 { 99 } // from the narrated reply\n",
        None,
    );
    let valid = write("src/two.rs", "pub fn two() -> u8 { 2 }\n", None);
    let (out, dir, requests) = work(
        "narrated-write",
        vec![
            Reply::Stream(narrated(wrapped)),
            Reply::Stream(valid),
            Reply::Stream(TEST),
            Reply::Stream(CLAIM),
        ],
        &[],
    )
    .await;
    let j = json(&out);
    // The narrated write never happened; the recovery's did, once.
    assert_eq!(
        std::fs::read_to_string(dir.join("src/two.rs")).unwrap(),
        "pub fn two() -> u8 { 2 }\n"
    );
    assert_eq!(j["writes"], 1, "{j}");
    assert_eq!(j["paths_written"], serde_json::json!(["src/two.rs"]));
    assert_eq!(j["malformed_replies"], 1, "{j}");
    // The narrated reply, the recovery's write, the test run; PAX's verdict completes the work, so
    // the scripted claim is never asked for.
    assert_eq!(requests, 3);
    // Recovered, and then verified the ordinary way: PAX passed after the last write.
    assert_eq!(j["terminal_state"], "completed", "{j}");
    assert_eq!(j["verified"], true, "{j}");
    assert_eq!(out.status.code(), Some(0), "{j}");
    assert_eq!(j["audit"]["clean"], true, "{j}");
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_malformed_replies_end_the_work_honestly_with_nothing_written() {
    let wrapped = write("src/two.rs", "pub fn two() -> u8 { 2 }\n", None);
    // The scripted model repeats its last reply: every reply is the same narrated write.
    let (out, dir, requests) = work("repeated", vec![Reply::Stream(narrated(wrapped))], &[]).await;
    let j = json(&out);
    assert!(!dir.join("src/two.rs").exists(), "nothing was written");
    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        LIB
    );
    assert_eq!(requests, 3, "the first reply and two recoveries");
    assert_eq!(j["malformed_replies"], 3, "{j}");
    assert_eq!(j["writes"], 0);
    assert_eq!(j["verified"], false);
    assert_eq!(j["terminal_state"], "failed", "{j}");
    assert_eq!(out.status.code(), Some(4), "{j}");
    let reason = j["outcome_reason"].as_str().unwrap();
    assert!(reason.contains("not a valid decision"), "{reason}");
    assert!(
        reason.contains("3 rejected decisions in a row; the limit is 2"),
        "{reason}"
    );
    assert_eq!(j["audit"]["clean"], true, "{j}");
    assert_eq!(j["audit"]["false_completions"], 0, "{j}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_reply_does_not_resolve_a_write_conflict() {
    // A person edits src/lib.rs while the reply carrying a write based on the old content is held
    // open: that write is refused as stale_base. A narrated reply that would re-read and rewrite
    // it is then rejected, so the conflict stands and the passing test run verifies nothing.
    let model = Model::start(vec![
        Reply::Stream(READ),
        Reply::Stall(write(
            "src/lib.rs",
            "pub fn one() -> u8 { 1 }\npub mod two;\n",
            Some(LIB),
        )),
        Reply::Stream(narrated(READ)),
        Reply::Stream(narrated(write(
            "src/lib.rs",
            "pub fn one() -> u8 { 1 }\n// kept by a person\npub mod two;\n",
            Some(USER_EDIT),
        ))),
        Reply::Stream(TEST),
        Reply::Stream(CLAIM),
        Reply::Stream(BLOCK),
    ])
    .await;
    let pax = Pax::new("conflict", PASSED, false);
    let dir = project("conflict");
    let mut cmd = command(&dir, &model.url, &pax);
    cmd.args(["work", "--json", "Add `two` and keep the tests passing."]);
    let run = tokio::task::spawn_blocking(move || cmd.output().unwrap());
    model.wait_requests(2).await;
    std::fs::write(dir.join("src/lib.rs"), USER_EDIT).unwrap();
    model.release();
    let out = run.await.unwrap();
    let j = json(&out);

    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        USER_EDIT,
        "the person's edit is intact and no rejected reply wrote over it"
    );
    // The two narrated replies, and the completion claim refused because nothing was verified.
    assert_eq!(j["malformed_replies"], 3, "{j}");
    assert_eq!(j["writes"], 0, "{j}");
    assert_eq!(
        j["write_conflicts"],
        serde_json::json!(["src/lib.rs"]),
        "{j}"
    );
    assert_eq!(j["pax"]["last_status"], "passed", "{j}");
    assert_eq!(j["verified"], false, "{j}");
    assert_ne!(j["terminal_state"], "completed", "{j}");
    assert_eq!(j["audit"]["clean"], true, "{j}");
    assert_eq!(j["audit"]["false_completions"], 0, "{j}");
}
