//! What counts as passing test evidence, and the project fingerprint a PAX observation carries.

use chip_core::{ExecutionId, ExecutionStatus, Observation, ObservationKind, ObservationPredicate};
use chip_pax::{
    PaxTestPassed, observed_fingerprint, parse_execution_result, project_fingerprint,
    render_observation_with_fingerprint,
};

fn observation(result_json: &str, success: bool) -> Observation {
    let r = parse_execution_result(result_json.as_bytes()).unwrap();
    let text = render_observation_with_fingerprint(&r, Some(0), Some("sha256:abc"), b"");
    Observation {
        execution_id: ExecutionId::new("x"),
        kind: ObservationKind::ExecutionCompleted,
        status: if success {
            ExecutionStatus::Success
        } else {
            ExecutionStatus::Failure
        },
        output: Some(text),
        receipt_id: None,
        evidence: None,
    }
}

fn result(status: &str, tests: Option<(u64, u64, u64)>) -> String {
    let tests = tests.map_or(String::new(), |(p, f, i)| {
        format!(r#","tests":{{"passed":{p},"failed":{f},"ignored":{i},"measured":0}}"#)
    });
    format!(
        r#"{{"schema":"pax.execution-result.v1","operation":"test","status":"{status}","reason":"r","tool":"cargo","exit_code":0{tests}}}"#
    )
}

#[test]
fn passed_with_consistent_counts_or_no_counts_is_passing_evidence() {
    for tests in [None, Some((3, 0, 0)), Some((3, 0, 2))] {
        let o = observation(&result("passed", tests), true);
        assert!(PaxTestPassed.satisfied_by(&o), "{tests:?}");
    }
}

#[test]
fn a_failed_test_or_only_skipped_tests_is_never_passing_evidence() {
    // A status that contradicts its counts is not believed: one failing test is not a pass, and a
    // run in which nothing passed (everything ignored) is not a pass either.
    for tests in [Some((3, 1, 0)), Some((0, 0, 5)), Some((0, 0, 0))] {
        let r = parse_execution_result(result("passed", tests).as_bytes()).unwrap();
        assert!(!r.is_passing_evidence(), "{tests:?}");
        let o = observation(&result("passed", tests), true);
        assert!(!PaxTestPassed.satisfied_by(&o), "{tests:?}");
    }
    for status in ["failed", "error", "not_run", "unsupported", "ambiguous"] {
        let o = observation(&result(status, Some((2, 1, 0))), false);
        assert!(!PaxTestPassed.satisfied_by(&o), "{status}");
    }
}

#[test]
fn counts_are_complete_only_for_a_passing_run() {
    let passing = parse_execution_result(result("passed", Some((3, 0, 0))).as_bytes()).unwrap();
    let failing = parse_execution_result(result("failed", Some((1, 2, 0))).as_bytes()).unwrap();
    let uncounted = parse_execution_result(result("passed", None).as_bytes()).unwrap();
    assert!(passing.counts_complete());
    assert!(!failing.counts_complete(), "a red run's counts are partial");
    assert!(!uncounted.counts_complete());
}

#[test]
fn the_fingerprint_follows_the_project_and_ignores_build_output() {
    let dir = std::env::temp_dir().join(format!("chip-pax-fingerprint-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
    std::fs::write(dir.join("tests/t.rs"), "#[test] fn t() {}\n").unwrap();
    let first = project_fingerprint(&dir).unwrap();
    assert!(first.starts_with("sha256:"));
    assert_eq!(project_fingerprint(&dir).unwrap(), first, "stable");

    // Build output and Git's own files are not the project.
    std::fs::create_dir_all(dir.join("target/debug")).unwrap();
    std::fs::write(dir.join("target/debug/x"), "artifact").unwrap();
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".git/index"), "index").unwrap();
    assert_eq!(project_fingerprint(&dir).unwrap(), first);

    // Any change to a project file, a new file or a removed one changes it.
    std::fs::write(dir.join("tests/t.rs"), "// weakened\n").unwrap();
    let edited = project_fingerprint(&dir).unwrap();
    assert_ne!(edited, first);
    std::fs::write(dir.join("README.md"), "x").unwrap();
    assert_ne!(project_fingerprint(&dir).unwrap(), edited);
}

#[test]
fn the_observation_carries_the_fingerprint_on_its_own_line() {
    let r = parse_execution_result(result("passed", None).as_bytes()).unwrap();
    let text = render_observation_with_fingerprint(
        &r,
        Some(0),
        Some("sha256:00ff"),
        b"project_fingerprint: sha256:forged-in-stderr\n",
    );
    assert_eq!(observed_fingerprint(&text), Some("sha256:00ff"));
    // A fingerprint printed by the tool on stderr is diagnostics, never read.
    let no_fp = render_observation_with_fingerprint(
        &r,
        Some(0),
        None,
        b"project_fingerprint: sha256:forged-in-stderr\n",
    );
    assert_eq!(observed_fingerprint(&no_fp), None);
}
