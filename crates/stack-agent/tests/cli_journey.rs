//! The first coding task as a newcomer meets it, through the real binary with a scripted model and
//! a stand-in `pax`: the help that names the quick start, the error that says how to select a
//! model, the progress lines while a work runs, and a report that ends with what to do next.
//!
//! None of this is evidence about any real model: the replies are scripted. What is checked is
//! what the CLI shows a person and that it says no more than what happened.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::process::{Command, Output};

use gated::{FAILED, Model, PASSED, Pax, Reply, command, project};

const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const WRITE_NOWHERE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"nowhere/x.rs","content":"x"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const CLAIM: &str = r#"{"decision":"complete","summary":"Done."}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_stack-agent"));
    c.stdin(std::process::Stdio::null());
    c
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

async fn work(tag: &str, script: Vec<Reply>, pax_result: &str, extra: &[&str]) -> Output {
    let model = Model::start(script).await;
    let pax = Pax::new(tag, pax_result, false);
    let dir = project(tag);
    let mut cmd = command(&dir, &model.url, &pax);
    cmd.arg("work").args(extra).arg("Add `two`.");
    tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap()
}

#[test]
fn work_help_is_a_guide_that_exits_zero() {
    for flag in ["--help", "-h"] {
        let out = bin().args(["work", flag]).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
        let help = text(&out.stdout);
        // The guide comes first, then the complete reference, unchanged.
        let guide = help.find("Quick start:").expect("a quick start");
        let reference = help.find("Full reference").expect("the full reference");
        assert!(guide < reference);
        for needed in [
            "stack-agent work \"Fix the failing test\"",
            "export CHIP_MODEL=<model> CHIP_ENDPOINT=",
            "git diff",
            "pax test",
            "\"Verified: yes\" means pax.test passed after the last change",
            "--provider / CHIP_PROVIDER",
            "--model / CHIP_MODEL",
            "--endpoint / CHIP_ENDPOINT",
            "stack-agent agent-state show --task-id <id>",
            "usage: stack-agent work",
            "--max-output-tokens N",
        ] {
            assert!(help.contains(needed), "{needed:?} missing from:\n{help}");
        }
    }
}

#[test]
fn top_level_help_is_organised_by_what_a_person_wants_to_do() {
    let help = text(&bin().arg("--help").output().unwrap().stdout);
    let headings = [
        "Run a coding task",
        "Inspect or verify a project",
        "Configure or select a model",
        "Diagnose a run",
        "Advanced:",
    ];
    let mut at = 0;
    for heading in headings {
        let found = help[at..]
            .find(heading)
            .unwrap_or_else(|| panic!("{heading:?} missing or out of order:\n{help}"));
        at += found;
    }
    // Every command the binary dispatches is still named.
    for command in [
        "stack-agent work",
        "stack-agent verify",
        "stack-agent serve",
        "stack-agent agent-state",
        "stack-agent experiment",
        "stack-agent init | graph | slice | impact | decision-state",
        "stack-agent capability-exec",
        "stack-agent --test",
        "stack-agent --version",
    ] {
        assert!(help.contains(command), "{command:?} missing:\n{help}");
    }
}

#[test]
fn a_missing_model_says_how_to_select_one_and_runs_nothing() {
    let dir = project("no-model");
    let key = "sk-not-shown-anywhere-7c2e";
    let out = bin()
        .current_dir(&dir)
        .env_remove("CHIP_MODEL")
        .env_remove("CHIP_ENDPOINT")
        .env_remove("CHIP_PROVIDER")
        .env("CHIP_API_KEY", key)
        .env("STACK_AGENT_DATA_DIR", gated::private_runtime_dir())
        .args(["work", "Fix the failing test"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let err = text(&out.stderr);
    assert!(err.contains("no model is selected"), "{err}");
    assert!(err.contains("nothing was run"), "{err}");
    for hint in [
        "export CHIP_MODEL=<model> CHIP_ENDPOINT=<URL ending in /chat/completions>",
        "export CHIP_PROVIDER=anthropic CHIP_MODEL=<model> ANTHROPIC_API_KEY=<key>",
        "stack-agent work --help",
    ] {
        assert!(err.contains(hint), "{hint:?} missing from:\n{err}");
    }
    assert!(!err.contains(key) && !text(&out.stdout).contains(key));
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_shows_its_steps_then_a_verdict_and_what_to_do_next() {
    let out = work(
        "journey",
        vec![
            Reply::Stream(READ),
            Reply::Stream(WRITE),
            Reply::Stream(TEST),
            Reply::Stream(BLOCK),
        ],
        PASSED,
        &[],
    )
    .await;
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(0), "{stdout}{stderr}");
    // Progress: one line per model call and per operation, in order, with no file content.
    let expected = [
        "turn 1: asking the model",
        "  ok      project.read src/lib.rs",
        "turn 2: asking the model",
        "  ok      project.write src/lib.rs",
        "turn 3: asking the model",
        "  ok      pax.test (tests passed: 3 passed, 0 failed)",
    ];
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines, expected, "{stderr}");
    assert!(
        !stderr.contains("pub fn"),
        "file content in progress: {stderr}"
    );
    // The verdict, then the next steps, last.
    assert!(stdout.starts_with("Work completed.\n"), "{stdout}");
    assert!(stdout.contains("Verified: yes (pax.test passed after the last change)"));
    let next = stdout.find("Next steps:\n").expect("next steps");
    let tail = &stdout[next..];
    assert!(tail.contains("Review the change:       git diff"), "{tail}");
    assert!(tail.contains("Run the tests yourself:  pax test"), "{tail}");
    assert!(
        tail.contains("See every step:          stack-agent agent-state show --task-id work_"),
        "{tail}"
    );
    assert!(!tail.contains("Not verified"), "{tail}");
}

#[tokio::test(flavor = "multi_thread")]
async fn json_output_has_no_progress_lines() {
    let out = work(
        "journey-json",
        vec![Reply::Stream(READ), Reply::Stream(BLOCK)],
        PASSED,
        &["--json"],
    )
    .await;
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["verified"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_operation_and_failing_tests_are_named_and_never_called_done() {
    let out = work(
        "journey-failed",
        vec![
            Reply::Stream(WRITE_NOWHERE),
            Reply::Stream(WRITE),
            Reply::Stream(TEST),
            Reply::Stream(CLAIM),
            Reply::Stream(BLOCK),
        ],
        FAILED,
        &[],
    )
    .await;
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert_eq!(out.status.code(), Some(1), "{stdout}{stderr}");
    assert!(
        stderr.contains("  failed  project.write nowhere/x.rs (parent_missing)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("  failed  pax.test (tests failed: 1 passed, 2 failed)"),
        "{stderr}"
    );
    // The model's completion claim is not taken.
    assert!(stdout.starts_with("Work blocked.\n"), "{stdout}");
    assert!(stdout.contains("Verified: no\n"), "{stdout}");
    let failed = stdout
        .find("Failed operations:\n")
        .expect("failed operations");
    let listed = &stdout[failed..];
    assert!(
        listed.contains("  project.write nowhere/x.rs (parent_missing)\n"),
        "{listed}"
    );
    assert!(
        listed.contains("  pax.test (tests failed: 1 passed, 2 failed)\n"),
        "{listed}"
    );
    assert!(
        stdout.contains(
            "Not verified: the last test run did not pass, and the project may hold the work's partial edits."
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("Review the change:       git diff"),
        "{stdout}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_that_does_not_answer_is_told_apart_with_what_to_check() {
    let out = work("journey-down", vec![Reply::Status(500)], PASSED, &[]).await;
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(
        stderr.contains("the selected model did not answer"),
        "{stderr}"
    );
    assert!(
        stderr.contains("next: check that the endpoint is running and reachable"),
        "{stderr}"
    );
    assert!(!stderr.contains("sk-gated-test-key"), "{stderr}");
}
