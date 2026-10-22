//! PAX is a provider for the work that needs a test run, not a prerequisite for the whole agent.
//!
//! The binary runs as a user would run it (`stack-agent work`), with a mock model behind the real
//! `HttpProvider` and a `PATH` that holds no `pax`. A read-only inspection must still list, read and
//! search the project; a kind of work that can only complete through a test run must still be
//! refused before anything runs; and nothing a model says may turn into a verification the
//! evidence does not support.
//!
//! What stays covered elsewhere and is not repeated here: PAX's own result mapping (unsupported,
//! failed, error, not_run, passed) and "exit status 0 is not a pass" are
//! `crates/chip-pax/tests/pax_boundary.rs` (`an_unsupported_tool_is_unsupported_even_though_its_exit_code_is_zero`,
//! `zero_tests_is_not_run_with_exit_code_zero_and_does_not_satisfy_the_goal`,
//! `a_pax_execution_error_is_an_error_not_a_failed_test_suite`, `missing_pax_fails_closed_and_fabricates_nothing`)
//! and `crates/chip-pax/tests/evidence.rs`; a failed PAX resolution is never remembered or retried
//! (`a_failed_resolution_is_not_remembered_and_never_runs_the_tests`).

#[path = "common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

const LIB: &str = "pub fn payload_len(payload: &str) -> usize {\n    payload.len()\n}\n";
const GOAL: &str = "Which file defines payload_len?";

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-paxless-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"paxless\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), LIB).unwrap();
    dir
}

/// A directory that holds no `pax`: the whole `PATH` of the child.
fn no_pax_path() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-paxless-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn completion(content: &str) -> String {
    serde_json::json!({
        "id": "resp-paxless",
        "choices": [{"message": {"role": "assistant", "content": content}}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 4},
    })
    .to_string()
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn json_of(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {}", text(o)))
}

/// Runs `stack-agent work <args>` in `dir` with no `pax` on `PATH`, against a mock model that
/// answers `reply` every turn. Returns the output and how many times the model was asked.
async fn go(reply: &str, dir: &Path, args: &[&str]) -> (Output, usize) {
    let server = common::start(200, &completion(reply), Duration::ZERO).await;
    let (url, d, a): (String, PathBuf, Vec<String>) = (
        server.url.clone(),
        dir.to_path_buf(),
        args.iter().map(|s| s.to_string()).collect(),
    );
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_stack-agent"))
            .arg("work")
            .args(&a)
            .current_dir(&d)
            .env("PATH", no_pax_path())
            .env_remove("PAX_BIN")
            .env("CHIP_PROVIDER", "openai-compatible")
            .env("CHIP_MODEL", "mock-model")
            .env("CHIP_ENDPOINT", &url)
            .env("CHIP_API_KEY", "sk-paxless-secret-never-printed")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let calls = server.captured.lock().await.len();
    (out, calls)
}

fn untouched(dir: &Path) {
    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        LIB
    );
    assert!(!dir.join("target").exists());
}

fn assert_clean_and_unverified(j: &serde_json::Value) {
    assert_eq!(j["verified"], false);
    assert_eq!(j["audit"]["clean"], true);
    assert_eq!(j["writes"], 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_inspection_lists_reads_and_searches_the_project_without_pax() {
    for (capability, reply) in [
        (
            "project.list",
            r#"{"decision":"request_capability","capability":"project.list"}"#,
        ),
        (
            "project.read",
            r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#,
        ),
        (
            "project.search",
            r#"{"decision":"request_capability","capability":"project.search","inputs":{"query":"payload_len"}}"#,
        ),
    ] {
        let dir = fixture(&format!("ok-{}", capability.replace('.', "-")));
        // Two turns: a third identical request would be a loop the stuck guard ends as `blocked`.
        let (out, calls) = go(
            reply,
            &dir,
            &["--kind", "inspect", "--json", "--max-turns", "2", GOAL],
        )
        .await;
        let j = json_of(&out);
        assert_eq!(out.status.code(), Some(1), "{capability}: {}", text(&out));
        assert_eq!(calls, 2, "{capability}: one model call per turn");
        assert_eq!(j["terminal_state"], "limit_reached", "{capability}");
        assert_eq!(
            j["executions_by_capability"][capability], 2,
            "{capability}: performed for real, without PAX"
        );
        assert_eq!(j["measurement"]["executions"], 2, "{capability}");
        assert_clean_and_unverified(&j);
        untouched(&dir);
        assert!(!text(&out).contains("PAX unavailable"), "{}", text(&out));
        assert!(!text(&out).contains("sk-paxless-secret"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn without_pax_an_inspection_is_still_confined_to_the_project() {
    let dir = fixture("confined");
    let outside = dir
        .parent()
        .unwrap()
        .join(format!("chip-paxless-secret-{}.txt", std::process::id()));
    std::fs::write(&outside, "TOP-SECRET-OUTSIDE-THE-PROJECT").unwrap();
    for target in [
        format!("../{}", outside.file_name().unwrap().to_string_lossy()),
        outside.display().to_string(),
    ] {
        let reply = format!(
            r#"{{"decision":"request_capability","capability":"project.read","inputs":{{"path":"{target}"}}}}"#
        );
        let (out, calls) = go(&reply, &dir, &["--kind", "inspect", "--json", GOAL]).await;
        // Refused, told why, asked again within the bound; the mock repeats the request.
        assert_eq!(
            calls,
            1 + chip_core::DEFAULT_MALFORMED_REPLY_LIMIT,
            "{target}"
        );
        assert_eq!(out.status.code(), Some(1), "{target}: {}", text(&out));
        let j = json_of(&out);
        assert_eq!(j["terminal_state"], "blocked", "{target}");
        assert_eq!(j["measurement"]["executions"], 0, "{target}");
        assert_eq!(j["measurement"]["observations"], 0, "{target}");
        assert_clean_and_unverified(&j);
        assert!(
            !text(&out).contains("TOP-SECRET-OUTSIDE-THE-PROJECT"),
            "{target}: outside content reached the output"
        );
    }
    untouched(&dir);
    let _ = std::fs::remove_file(&outside);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_pax_grants_an_inspection_no_new_capability() {
    // An inspection is offered no test run and no write, with or without PAX, and a missing PAX
    // does not change that. A capability that is not offered is "unknown": asking for it is refused
    // within the bound and ends the work as `failed` (exit 4); no file changes and nothing runs.
    for (name, reply, code, state, why) in [
        (
            "test",
            r#"{"decision":"request_capability","capability":"pax.test"}"#,
            4,
            "failed",
            "unknown capability: pax.test",
        ),
        (
            "write",
            r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn payload_len(_: &str) -> usize { 0 }\n"}}"#,
            4,
            "failed",
            "unknown capability: project.write",
        ),
    ] {
        let dir = fixture(&format!("grant-{name}"));
        let (out, calls) = go(reply, &dir, &["--kind", "inspect", "--json", GOAL]).await;
        assert_eq!(
            calls,
            1 + chip_core::DEFAULT_MALFORMED_REPLY_LIMIT,
            "{name}: bounded"
        );
        assert_eq!(out.status.code(), Some(code), "{name}: {}", text(&out));
        let j = json_of(&out);
        assert_eq!(j["terminal_state"], state, "{name}");
        assert!(
            j["outcome_reason"].as_str().unwrap_or("").contains(why),
            "{name}: {}",
            j["outcome_reason"]
        );
        assert_eq!(j["measurement"]["executions"], 0, "{name}");
        assert_clean_and_unverified(&j);
        untouched(&dir);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bare_completion_claim_is_not_verification_without_pax() {
    let dir = fixture("claim");
    let (out, calls) = go(
        r#"{"decision":"complete","summary":"src/lib.rs defines payload_len; all tests pass."}"#,
        &dir,
        &["--kind", "inspect", GOAL],
    )
    .await;
    let t = text(&out);
    // Nothing was observed, so the answer is not grounded: refused, asked again within the bound.
    assert_eq!(calls, 1 + chip_core::DEFAULT_MALFORMED_REPLY_LIMIT, "{t}");
    assert_eq!(out.status.code(), Some(1), "{t}");
    assert!(t.starts_with("Work blocked.\n"), "{t}");
    assert!(t.contains("completion refused"), "{t}");
    assert!(t.contains("Verified: no"), "{t}");
    assert!(t.contains("Last test result: none"), "{t}");
    assert!(t.contains("Executed: none"), "{t}");
    assert!(t.contains("  false completions: 0"), "{t}");
    untouched(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn work_that_can_only_finish_through_a_test_run_stays_closed_without_pax() {
    // `change` (the default) and `verify` complete when pax.test passes. Without a test runner they
    // could not end verified, so they are refused before the model is asked or anything runs.
    for args in [
        vec!["--kind", "change", GOAL],
        vec!["--kind", "verify", GOAL],
        vec![GOAL],
    ] {
        let dir = fixture("closed");
        let (out, calls) = go(r#"{"decision":"complete","summary":"done"}"#, &dir, &args).await;
        assert_eq!(out.status.code(), Some(3), "{args:?}: {}", text(&out));
        assert!(
            text(&out).contains("PAX unavailable") && text(&out).contains("nothing was run"),
            "{args:?}: {}",
            text(&out)
        );
        assert_eq!(calls, 0, "{args:?}: no model is asked");
        assert!(out.stdout.is_empty());
        untouched(&dir);
    }
}

/// A `pax` that is present on `PATH` but is not PAX (like the BSD archiver of the same name).
fn not_pax_path() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-paxless-notpax-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fake = dir.join("pax");
    std::fs::write(
        &fake,
        "#!/bin/sh\necho 'pax: unrecognized option'\nexit 1\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pax_that_is_present_but_not_pax_fails_explicitly_where_it_is_needed() {
    // The up-front check only locates a test runner; whether it is PAX is established when a
    // `pax.test` request first needs it. The work starts, and that one operation reports the
    // provider failure: nothing is executed, nothing is retried, nothing is verified.
    let dir = fixture("notpax");
    let server = common::start(
        200,
        &completion(r#"{"decision":"request_capability","capability":"pax.test"}"#),
        Duration::ZERO,
    )
    .await;
    let (url, d) = (server.url.clone(), dir.clone());
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_stack-agent"))
            .args(["work", "--json", GOAL])
            .current_dir(&d)
            .env("PATH", not_pax_path())
            .env_remove("PAX_BIN")
            .env("CHIP_PROVIDER", "openai-compatible")
            .env("CHIP_MODEL", "mock-model")
            .env("CHIP_ENDPOINT", &url)
            .env("CHIP_API_KEY", "sk-paxless-secret-never-printed")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let calls = server.captured.lock().await.len();
    let j = json_of(&out);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert_eq!(
        calls, 1,
        "the model is not asked again after the provider failure"
    );
    assert_eq!(j["terminal_state"], "blocked");
    let reason = j["outcome_reason"].as_str().unwrap_or("");
    assert!(
        reason.contains("capabilities unavailable") && reason.contains("is not PAX"),
        "{reason}"
    );
    assert_eq!(j["measurement"]["executions"], 0);
    assert_eq!(j["measurement"]["observations"], 0);
    assert_clean_and_unverified(&j);
    untouched(&dir);
}
