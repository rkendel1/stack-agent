//! Target resolution: scope from the failing test's crate and imports, the call chain through test
//! helpers and one level below, ranking before truncation, and explicit ambiguity.
//!
//! The fixture is a small workspace shaped like coding-benchmark-v1 T3: the defect is in
//! `Policy::intersect`, which the test reaches only through its own `compose` helper and
//! `EffectivePolicy::compose`; another crate has unrelated functions with the helpers' names.

use std::fs;
use std::path::Path;

use chip_symbol_loop::acceptance::{AcceptanceRun, Diagnostic, DiagnosticKind};
use chip_symbol_loop::symbols::{SymbolIndex, sha256};
use chip_symbol_loop::target::{MAX_CANDIDATES, Target, decide, rank};

const POLICY_LIB: &str = r#"#[derive(Clone)]
pub struct Policy {
    pub allowed_runtime_versions: Vec<String>,
}

impl Policy {
    pub fn from_json(text: &str) -> Policy {
        Policy { allowed_runtime_versions: text.split(',').map(str::to_string).collect() }
    }

    /// The defect lives here.
    pub fn intersect(&self, other: &Self) -> Self {
        let allowed_runtime_versions = self
            .allowed_runtime_versions
            .iter()
            .filter(|v| other.allowed_runtime_versions.contains(v))
            .cloned()
            .collect();
        Policy { allowed_runtime_versions }
    }

    pub fn label(&self) -> String {
        "policy".to_string()
    }
}

pub struct EffectivePolicy {
    pub policy: Policy,
}

impl EffectivePolicy {
    pub fn compose(sources: &[Policy]) -> Self {
        let mut policy: Policy = sources[0].clone();
        for source in &sources[1..] {
            policy = policy.intersect(source);
        }
        EffectivePolicy { policy }
    }
}

pub fn admit(policy: &Policy, version: &str) -> bool {
    policy.allowed_runtime_versions.iter().any(|v| version.starts_with(v.as_str()))
}
"#;

const ACCEPTANCE: &str = r#"use policy_core::*;

fn policy(text: &str) -> Policy {
    Policy::from_json(text)
}

fn compose(sources: &[&str]) -> Policy {
    let sources: Vec<Policy> = sources.iter().map(|s| policy(s)).collect();
    EffectivePolicy::compose(&sources).policy
}

fn versions(v: &str) -> String {
    let unrelated = Unknown::default();
    let _ = unrelated.path();
    v.to_string()
}

fn assert_exact(sources: &[&str], probe: &str) {
    let composed = compose(sources);
    let each = sources.iter().all(|s| admit(&policy(s), probe));
    assert_eq!(admit(&composed, probe), each, "{probe} under allowed_runtime_versions");
}

#[test]
fn a_broad_and_a_narrow_restriction_compose() {
    assert_exact(&[&versions("3"), &versions("3.13")], "3.13.1");
}
"#;

const CLI_LIB: &str = r#"pub fn policy() -> u32 {
    1
}

pub fn compose() -> u32 {
    2
}

pub fn versions() -> u32 {
    3
}

pub struct Reader;

impl Reader {
    pub fn path(&self) -> u32 {
        4
    }
}
"#;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn workspace() -> tempfile::TempDir {
    let d = tempfile::Builder::new()
        .prefix("chip-targeting-")
        .tempdir()
        .unwrap();
    let r = d.path();
    write(
        r,
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/policy-core\", \"crates/cli\"]\n",
    );
    write(
        r,
        "crates/policy-core/Cargo.toml",
        "[package]\nname = \"policy-core\"\nversion = \"0.1.0\"\n",
    );
    write(r, "crates/policy-core/src/lib.rs", POLICY_LIB);
    write(r, "crates/policy-core/tests/acceptance.rs", ACCEPTANCE);
    write(
        r,
        "crates/cli/Cargo.toml",
        "[package]\nname = \"cli\"\nversion = \"0.1.0\"\n",
    );
    write(r, "crates/cli/src/lib.rs", CLI_LIB);
    d
}

fn line_of(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|l| l.contains(needle))
        .expect("needle")
        + 1
}

fn run(diagnostics: Vec<Diagnostic>) -> AcceptanceRun {
    AcceptanceRun {
        exit_status: Some(101),
        timed_out: false,
        spawn_error: None,
        duration_ms: 1,
        passed_tests: 0,
        failed_tests: 1,
        passed: false,
        diagnostics,
        output_tail: String::new(),
        output_digest: sha256(b""),
    }
}

/// What `cargo test` reports for the fixture's failing test: the failed test and its panic, located
/// in the test file's `assert_exact` helper.
fn t3_failure() -> AcceptanceRun {
    run(vec![
        Diagnostic {
            kind: DiagnosticKind::FailedTest,
            message: "test a_broad_and_a_narrow_restriction_compose ... FAILED".into(),
            path: None,
            line: None,
            column: None,
            test: Some("a_broad_and_a_narrow_restriction_compose".into()),
        },
        Diagnostic {
            kind: DiagnosticKind::Panic,
            message: "assertion `left == right` failed: 3.13.1 under allowed_runtime_versions"
                .into(),
            path: Some("crates/policy-core/tests/acceptance.rs".into()),
            line: Some(line_of(ACCEPTANCE, "assert_eq!(admit")),
            column: None,
            test: Some("a_broad_and_a_narrow_restriction_compose".into()),
        },
    ])
}

const INTERSECT: &str = "symbol:crates/policy-core/src/lib.rs:Policy::intersect";
const COMPOSE: &str = "symbol:crates/policy-core/src/lib.rs:EffectivePolicy::compose";

#[test]
fn the_defect_one_call_below_the_tests_reach_is_a_ranked_candidate() {
    let d = workspace();
    let index = SymbolIndex::build(d.path()).unwrap();
    let ranking = rank(&index, &t3_failure());
    let ids: Vec<&str> = ranking.candidates.iter().map(|c| c.id.as_str()).collect();
    assert!(ids.contains(&INTERSECT), "{ids:?}");
    assert!(ids.contains(&COMPOSE), "{ids:?}");
    let intersect = &ranking.candidates[ranking.position(INTERSECT).unwrap()];
    assert_eq!(intersect.depth, 2);
    assert!(
        intersect
            .reasons
            .iter()
            .any(|r| r.contains("called by a function the failing test calls"))
    );
    assert!(
        intersect
            .reasons
            .iter()
            .any(|r| r.contains("allowed_runtime_versions"))
    );
    // Offered to the bounded classification, and nothing is selected on proximity alone.
    match decide(&ranking) {
        Target::Ambiguous(offered) => {
            assert!(offered.contains(&INTERSECT.to_string()), "{offered:?}")
        }
        other => panic!("expected an explicit ambiguity, got {other:?}"),
    }
    assert_eq!(ranking.scope, vec!["policy_core".to_string()]);
}

#[test]
fn same_named_functions_outside_the_tests_scope_and_unresolvable_calls_are_not_candidates() {
    let d = workspace();
    let index = SymbolIndex::build(d.path()).unwrap();
    let ranking = rank(&index, &t3_failure());
    for c in &ranking.candidates {
        assert!(
            !c.id.contains("crates/cli/"),
            "{} is outside the test's crate and imports",
            c.id
        );
        assert!(
            !c.id.contains("acceptance.rs"),
            "test helpers are part of the test, not targets: {}",
            c.id
        );
    }
    // `Unknown::default()` names no indexed function, and `.path()` is a method of a type the caller
    // never names: neither gives an edge, even though `Reader::path` exists (out of scope anyway).
    assert!(
        ranking
            .candidates
            .iter()
            .all(|c| !c.id.ends_with("::default") && !c.id.ends_with("::path"))
    );
}

#[test]
fn ranking_is_deterministic_and_every_score_is_explained() {
    let d = workspace();
    let index = SymbolIndex::build(d.path()).unwrap();
    let a = rank(&index, &t3_failure());
    let b = rank(&SymbolIndex::build(d.path()).unwrap(), &t3_failure());
    assert_eq!(a, b);
    for c in &a.candidates {
        let explained: i64 = c
            .reasons
            .iter()
            .map(|r| {
                r.trim_start_matches('+')
                    .split(' ')
                    .next()
                    .unwrap()
                    .parse::<i64>()
                    .unwrap()
            })
            .sum();
        assert_eq!(explained, c.score, "{} reasons {:?}", c.id, c.reasons);
    }
    assert!(
        a.candidates
            .windows(2)
            .all(|w| w[0].score > w[1].score || (w[0].score == w[1].score && w[0].id < w[1].id))
    );
}

#[test]
fn indistinguishable_candidates_are_reported_as_ambiguous_with_the_tie_noted() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(
        r,
        "Cargo.toml",
        "[package]\nname = \"pair\"\nversion = \"0.1.0\"\n",
    );
    write(
        r,
        "src/lib.rs",
        "pub fn alpha() -> u32 {\n    1\n}\n\npub fn beta() -> u32 {\n    2\n}\n",
    );
    let test = "use pair::*;\n\n#[test]\nfn sum() {\n    assert_eq!(alpha() + beta(), 4);\n}\n";
    write(r, "tests/t.rs", test);
    let index = SymbolIndex::build(r).unwrap();
    let ranking = rank(
        &index,
        &run(vec![Diagnostic {
            kind: DiagnosticKind::FailedTest,
            message: "test sum ... FAILED".into(),
            path: None,
            line: None,
            column: None,
            test: Some("sum".into()),
        }]),
    );
    assert_eq!(ranking.candidates.len(), 2);
    assert_eq!(ranking.candidates[0].score, ranking.candidates[1].score);
    assert!(
        ranking.notes.iter().any(|n| n.contains("tie")),
        "{:?}",
        ranking.notes
    );
    assert_eq!(
        decide(&ranking),
        Target::Ambiguous(vec![
            "symbol:src/lib.rs:alpha".into(),
            "symbol:src/lib.rs:beta".into()
        ])
    );
}

#[test]
fn direct_evidence_selects_one_function_and_two_are_ambiguous() {
    let d = workspace();
    let index = SymbolIndex::build(d.path()).unwrap();
    let inside = |needle: &str| Diagnostic {
        kind: DiagnosticKind::Panic,
        message: "index out of bounds".into(),
        path: Some("crates/policy-core/src/lib.rs".into()),
        line: Some(line_of(POLICY_LIB, needle)),
        column: None,
        test: None,
    };
    let one = rank(&index, &run(vec![inside(".filter(|v| other")]));
    assert_eq!(decide(&one), Target::Single(INTERSECT.into()));
    assert!(one.candidates[0].reasons[0].contains("a panic at crates/policy-core/src/lib.rs"));
    let two = rank(
        &index,
        &run(vec![
            inside(".filter(|v| other"),
            inside("for source in &sources"),
        ]),
    );
    match decide(&two) {
        Target::Ambiguous(ids) => assert_eq!(ids, vec![COMPOSE.to_string(), INTERSECT.to_string()]),
        other => panic!("{other:?}"),
    }
}

#[test]
fn missing_or_incomplete_source_information_is_reported_not_guessed() {
    let d = workspace();
    write(
        d.path(),
        "crates/policy-core/tests/broken.rs",
        "fn broken( {\n",
    );
    let index = SymbolIndex::build(d.path()).unwrap();
    let ranking = rank(
        &index,
        &run(vec![
            Diagnostic {
                kind: DiagnosticKind::FailedTest,
                message: "test gone ... FAILED".into(),
                path: None,
                line: None,
                column: None,
                test: Some("tests::gone".into()),
            },
            Diagnostic {
                kind: DiagnosticKind::Panic,
                message: "boom".into(),
                path: Some("crates/policy-core/tests/broken.rs".into()),
                line: Some(1),
                column: None,
                test: None,
            },
        ]),
    );
    assert_eq!(decide(&ranking), Target::None);
    assert!(
        ranking
            .notes
            .iter()
            .any(|n| n.contains("tests::gone is not in the index")),
        "{:?}",
        ranking.notes
    );
    assert!(
        ranking
            .notes
            .iter()
            .any(|n| n.contains("broken.rs is not indexed")),
        "{:?}",
        ranking.notes
    );

    // A test that belongs to no package and imports nothing: the scope is every crate, and that is said.
    let loose = tempfile::tempdir().unwrap();
    write(
        loose.path(),
        "tests/t.rs",
        "#[test]\nfn t() {\n    assert_eq!(helper(), 1);\n}\n",
    );
    write(
        loose.path(),
        "src/lib.rs",
        "pub fn helper() -> u32 {\n    2\n}\n",
    );
    let index = SymbolIndex::build(loose.path()).unwrap();
    let ranking = rank(
        &index,
        &run(vec![Diagnostic {
            kind: DiagnosticKind::FailedTest,
            message: String::new(),
            path: None,
            line: None,
            column: None,
            test: Some("t".into()),
        }]),
    );
    assert!(ranking.scope.is_empty());
    assert!(
        ranking.notes.iter().any(|n| n.contains("every crate")),
        "{:?}",
        ranking.notes
    );
    assert_eq!(
        decide(&ranking),
        Target::Single("symbol:src/lib.rs:helper".into())
    );
}

#[test]
fn truncation_happens_after_ranking_never_by_name_order() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    write(
        r,
        "Cargo.toml",
        "[package]\nname = \"many\"\nversion = \"0.1.0\"\n",
    );
    // Ten functions whose ids sort first are reached only by a shared method name of a type the test
    // names; the one that sorts last is called by its qualified path and uses the failure's term.
    let mut lib = String::from("pub struct Thing;\n\nimpl Thing {\n");
    for i in 0..10 {
        lib.push_str(&format!(
            "    pub fn aaa_{i}(&self) -> u32 {{\n        aaa_shared()\n    }}\n"
        ));
    }
    lib.push_str("    pub fn zzz_target(&self, failure_counter: u32) -> u32 {\n        failure_counter\n    }\n}\n\n");
    lib.push_str("pub fn aaa_shared() -> u32 {\n    0\n}\n");
    write(r, "src/lib.rs", &lib);
    let mut test = String::from("use many::*;\n\n#[test]\nfn t() {\n    let thing = Thing;\n");
    for i in 0..10 {
        test.push_str(&format!("    let _ = thing.aaa_{i}();\n"));
    }
    test.push_str("    assert_eq!(Thing::zzz_target(&thing, 1), 2);\n}\n");
    write(r, "tests/t.rs", &test);
    let index = SymbolIndex::build(r).unwrap();
    let ranking = rank(
        &index,
        &run(vec![
            Diagnostic {
                kind: DiagnosticKind::FailedTest,
                message: String::new(),
                path: None,
                line: None,
                column: None,
                test: Some("t".into()),
            },
            Diagnostic {
                kind: DiagnosticKind::Panic,
                message: "failure_counter was 1".into(),
                path: Some("tests/t.rs".into()),
                line: Some(line_of(&test, "zzz_target")),
                column: None,
                test: Some("t".into()),
            },
        ]),
    );
    assert!(ranking.candidates.len() > MAX_CANDIDATES);
    assert_eq!(
        ranking.candidates[0].id,
        "symbol:src/lib.rs:Thing::zzz_target",
        "{:?}",
        ranking
            .candidates
            .iter()
            .map(|c| (&c.id, c.score))
            .collect::<Vec<_>>()
    );
    match decide(&ranking) {
        Target::Ambiguous(offered) => {
            assert_eq!(offered.len(), MAX_CANDIDATES);
            assert_eq!(offered[0], "symbol:src/lib.rs:Thing::zzz_target");
        }
        other => panic!("{other:?}"),
    }
}
