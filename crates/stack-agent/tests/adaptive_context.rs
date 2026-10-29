//! Context profiles through the real binary with scripted stand-ins (an OpenAI-compatible stream
//! and a `pax`): each request records the profile it was assembled under and why, a superseded read
//! travels as checked facts instead of in full, repeated failed attempts escalate to Deep, the
//! legacy request is one switch away, a goal over FeltDB's objective limit is persisted, and the
//! report keeps the model's claim, the candidate change, the tests and verification apart. No real
//! model is called.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use gated::{FAILED, Model, Pax, Reply, command, project};
use serde_json::Value;

const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;

fn last_json(out: &std::process::Output) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    serde_json::from_str(stdout.lines().last().unwrap_or("null")).unwrap_or(Value::Null)
}

/// Read, write, read again, two failing test runs, then block.
async fn run(
    tag: &str,
    args: &[&str],
    env: &[(&str, &str)],
) -> (std::process::Output, Value, Vec<String>) {
    let model = Model::start(vec![
        Reply::Stream(READ),
        Reply::Stream(WRITE),
        Reply::Stream(READ),
        Reply::Stream(TEST),
        Reply::Stream(TEST),
        Reply::Stream(BLOCK),
    ])
    .await;
    let pax = Pax::new(tag, FAILED, false);
    let dir = project(tag);
    let mut c = command(&dir, &model.url, &pax);
    c.env_remove("CHIP_CONTEXT_PROFILE");
    c.args(["work", "Add `two`.", "--json", "--no-state"]);
    c.args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    let out = tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap();
    let json = last_json(&out);
    (out, json, model.bodies())
}

fn profiles(report: &Value) -> Vec<Value> {
    report["token_usage"]["calls"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["context"]["profile"].clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_request_records_its_profile_and_repeated_failures_escalate_to_deep() {
    let (out, report, bodies) = run("adaptive-auto", &["--context-profile", "auto"], &[]).await;
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(report["tuning"]["context_profile"], "auto");
    assert_eq!(
        report["token_usage"]["calls"][0]["context"]["policy"], "adaptive-v1",
        "{report}"
    );
    assert_eq!(
        profiles(&report),
        [
            "standard", "standard", "standard", "standard", "standard", "deep"
        ]
    );
    let calls = report["token_usage"]["calls"].as_array().unwrap();
    let deep = &calls[5]["context"];
    assert_eq!(deep["changed"], true);
    assert_eq!(deep["previous_profile"], "standard");
    assert!(
        deep["reason"]
            .as_str()
            .unwrap()
            .contains("the last 2 test runs after a change did not pass"),
        "{deep}"
    );
    assert!(deep["sections"].to_string().contains("attempts"));
    assert!(deep["assembly_ms"].as_f64().is_some());
    assert_eq!(
        report["token_usage"]["context_profiles"]["calls_by_profile"]["standard"],
        5
    );
    assert_eq!(
        report["token_usage"]["context_profiles"]["changes"][0]["to"],
        "deep"
    );

    // The first read was superseded by the second: from then on it travels as checked facts,
    // named with its execution id, and its bytes are said to be withheld.
    assert_eq!(calls[3]["context"]["observations_condensed"], 1);
    assert!(!bodies[2].contains("superseded by execution"));
    assert!(
        bodies[3].contains("superseded by execution"),
        "{}",
        bodies[3]
    );
    assert!(bodies[3].contains("bytes withheld"));
    // Every request keeps the instructions first and says which profile it is.
    for body in &bodies {
        assert!(body.contains("context profile: "), "{body}");
        assert!(
            body.contains("project.write"),
            "the capability list travels in every request"
        );
    }
    assert!(bodies[5].contains("attempts (each change, and the test run that followed it)"));

    // The model's claim, the change, the tests and verification are kept apart.
    let c = &report["completion"];
    assert_eq!(c["model_terminal_decision"], "block");
    assert_eq!(c["candidate_change"]["made"], true);
    assert_eq!(c["tests"]["runs"], 2);
    assert_eq!(c["tests"]["last"]["status"], "failed");
    assert_eq!(c["tests"]["last_after_last_change"], true);
    assert_eq!(c["acceptance_verified_by_runtime"], false);
    assert_eq!(c["independent_evaluation"], "not_available_to_runtime");
    assert_eq!(report["verified"], false);
    assert_eq!(report["terminal_state"], "blocked");

    // The token report splits calls and reported tokens by profile, from the same report.
    let path = std::env::temp_dir().join(format!("adaptive-report-{}.json", std::process::id()));
    std::fs::write(&path, report.to_string()).unwrap();
    let analysis = std::process::Command::new(env!("CARGO_BIN_EXE_stack-agent"))
        .args(["token-report", path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(analysis.status.code(), Some(0));
    let a: Value = serde_json::from_slice(&analysis.stdout).unwrap();
    assert_eq!(a["context_profiles"]["by_profile"]["deep"]["calls"], 1);
    assert_eq!(a["context_profiles"]["setting"], "auto");
    let _ = std::fs::remove_file(path);
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_sends_the_legacy_request_from_the_flag_or_the_variable() {
    for (tag, args, env) in [
        (
            "adaptive-replay-flag",
            vec!["--context-profile", "replay"],
            vec![],
        ),
        (
            "adaptive-replay-env",
            vec![],
            vec![("CHIP_CONTEXT_PROFILE", "replay")],
        ),
        // Neither: replay is the default.
        ("adaptive-replay-default", vec![], vec![]),
        // The flag beats the variable.
        (
            "adaptive-replay-flag-over-env",
            vec!["--context-profile", "replay"],
            vec![("CHIP_CONTEXT_PROFILE", "auto")],
        ),
    ] {
        let (out, report, bodies) = run(tag, &args, &env).await;
        assert_eq!(
            out.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(report["tuning"]["context_profile"], "replay");
        assert_eq!(
            report["token_usage"]["calls"][0]["context"]["policy"],
            "dedup-v1"
        );
        assert!(profiles(&report).iter().all(Value::is_null));
        for body in &bodies {
            assert!(!body.contains("context profile: "));
            assert!(!body.contains("superseded by execution"));
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fixed_profile_is_used_for_every_request_and_a_bad_value_is_a_usage_error() {
    let (out, report, _) = run("adaptive-fixed", &["--context-profile", "deep"], &[]).await;
    assert_eq!(out.status.code(), Some(1));
    assert!(profiles(&report).iter().all(|p| p == "deep"), "{report}");
    let calls = report["token_usage"]["calls"].as_array().unwrap();
    assert_eq!(
        calls[0]["context"]["reason"],
        "fixed by --context-profile deep"
    );

    let (out, _, _) = run("adaptive-bad", &["--context-profile", "full"], &[]).await;
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--context-profile takes auto, light"));
    let (out, _, _) = run("adaptive-bad-env", &[], &[("CHIP_CONTEXT_PROFILE", "full")]).await;
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("CHIP_CONTEXT_PROFILE takes auto, light")
    );
    // The variable opts in when there is no flag; the flag beats it.
    let (_, report, _) = run(
        "adaptive-env-auto",
        &[],
        &[("CHIP_CONTEXT_PROFILE", "auto")],
    )
    .await;
    assert_eq!(report["tuning"]["context_profile"], "auto");
    assert_eq!(
        report["token_usage"]["calls"][0]["context"]["policy"],
        "adaptive-v1"
    );
    let (_, report, _) = run(
        "adaptive-flag-over-env",
        &["--context-profile", "standard"],
        &[("CHIP_CONTEXT_PROFILE", "deep")],
    )
    .await;
    assert!(
        profiles(&report).iter().all(|p| p == "standard"),
        "{report}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_goal_over_the_objective_limit_is_persisted_with_an_explicit_database() {
    let model = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let tag = "adaptive-long-goal";
    let pax = Pax::new(tag, FAILED, false);
    let dir = project(tag);
    let db = dir.join("state.feltdb");
    // 1,900 bytes: over FeltDB's 1,024-byte objective, within `work`'s 2,000-byte goal limit.
    let goal = format!(
        "Add `two`. {}",
        "Keep the existing behaviour of `one` unchanged. ".repeat(40)
    );
    let goal = &goal[..1900];
    let mut c = command(&dir, &model.url, &pax);
    c.args(["work", goal, "--json", "--state-db", db.to_str().unwrap()]);
    let out = tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("CONTRACT_INPUT_INVALID"), "{stderr}");
    assert!(!stderr.contains("must be 1 to 1024 bytes"), "{stderr}");
    let report = last_json(&out);
    assert_eq!(report["terminal_state"], "blocked");
    assert_eq!(
        report["goal"].as_str().map(str::len),
        Some(1900),
        "the whole goal is the goal"
    );
}

/// What the T4 harness's PAX stand-in prints for its injected fault, byte for byte
/// (`agent-obstacle/harness/obstacles/pax-shim.mjs`): a valid result whose structured fields
/// classify it as a transient infrastructure failure. It exits 75 (EX_TEMPFAIL).
const TRANSIENT: &str = r#"{"schema":"pax.execution-result.v1","operation":"test","status":"error","reason":"transient-infrastructure-failure","tool":null,"exit_code":null}"#;

/// A `pax` stand-in that replays the T4 sequence: the first test run is the injected transient
/// failure (stdout the result above, a stderr explanation, exit 75); every later one prints
/// `later` and exits as PAX does for it.
fn transient_then(tag: &str, later: &str, later_exit: u8) -> Pax {
    use std::os::unix::fs::PermissionsExt;
    let pax = Pax::new(tag, later, false);
    let d = pax.dir.display();
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'pax 0.4.1'; exit 0; fi\nif [ ! -e '{d}/first' ]; then touch '{d}/first'; echo '{TRANSIENT}'; echo 'pax: TRANSIENT INFRASTRUCTURE FAILURE (scripted): the test runner could not be started. No tests ran.' >&2; exit 75; fi\necho '{later}'\nexit {later_exit}\n"
    );
    let path = pax.dir.join("pax");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    pax
}

/// Read, write, test (the transient failure), test again (a real run), read, then block, under
/// `--context-profile auto`.
async fn run_t4(tag: &str, later: &str, later_exit: u8) -> (Value, Vec<String>) {
    let model = Model::start(vec![
        Reply::Stream(READ),
        Reply::Stream(WRITE),
        Reply::Stream(TEST),
        Reply::Stream(TEST),
        Reply::Stream(READ),
        Reply::Stream(BLOCK),
    ])
    .await;
    let pax = transient_then(tag, later, later_exit);
    let dir = project(tag);
    let mut c = command(&dir, &model.url, &pax);
    c.env_remove("CHIP_CONTEXT_PROFILE");
    c.args([
        "work",
        "Add `two`.",
        "--json",
        "--no-state",
        "--context-profile",
        "auto",
    ]);
    let out = tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap();
    let report = last_json(&out);
    assert_eq!(
        report["tuning"]["context_profile"],
        "auto",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    (report, model.bodies())
}

/// Every recorded profile and reason is the one the request it describes was assembled under.
fn ledger_matches_requests(report: &Value, bodies: &[String]) -> Vec<(String, String)> {
    let calls = report["token_usage"]["calls"].as_array().unwrap();
    assert_eq!(calls.len(), bodies.len(), "{report}");
    calls
        .iter()
        .zip(bodies)
        .map(|(call, body)| {
            let ctx = &call["context"];
            assert_eq!(ctx["policy"], "adaptive-v1");
            let profile = ctx["profile"].as_str().unwrap().to_string();
            let reason = ctx["reason"].as_str().unwrap().to_string();
            assert!(
                body.contains(&format!("context profile: {profile} ({reason})")),
                "call recorded {profile} ({reason}); request: {body}"
            );
            assert!(!reason.starts_with("conflicting evidence"), "{reason}");
            assert!(!body.contains("conflicting evidence"), "{body}");
            (profile, reason)
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_transient_pax_failure_then_a_real_run_is_not_conflicting_evidence_under_auto() {
    // Transient, then a genuine failing run (the T4 sequence). Request 4 is the first after the
    // pair; before this rule it was "conflicting evidence" (Deep). The pair is no longer a
    // conflict. The existing repeated-failure rule is unchanged and still counts the transient
    // error as a test run after a change that did not pass, so that request is Deep for that
    // stated reason; once a later execution succeeds and no rule holds, the work de-escalates.
    let (report, bodies) = run_t4("adaptive-t4-failed", gated::FAILED, 101).await;
    let selections = ledger_matches_requests(&report, &bodies);
    let reasons: Vec<&str> = selections.iter().map(|(_, r)| r.as_str()).collect();
    let profiles: Vec<&str> = selections.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        profiles,
        [
            "standard", "standard", "standard", "standard", "deep", "deep"
        ],
        "{reasons:?}"
    );
    assert_eq!(
        reasons[4],
        "repeated failed attempts: the last 2 test runs after a change did not pass"
    );
    assert_eq!(report["completion"]["tests"]["runs"], 2, "{report}");
    assert_eq!(report["completion"]["tests"]["last"]["status"], "failed");

    // Transient, then a passing rerun: no escalation from it at all.
    let (report, bodies) = run_t4("adaptive-t4-passed", gated::PASSED, 0).await;
    let selections = ledger_matches_requests(&report, &bodies);
    assert!(
        selections.iter().all(|(p, _)| p != "deep"),
        "{selections:?}"
    );
    assert_eq!(report["completion"]["tests"]["last"]["status"], "passed");
}
