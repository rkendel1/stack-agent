//! `stack-agent init` builds the architecture graph and, in the same pass, the implementation-pattern
//! report; `stack-agent graph patterns` reads it back and says whether the source still matches.
//! The real binary, a real directory, no model.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../chip-graph/tests/pattern_fixture")
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy(&e.path(), &target);
        } else {
            fs::copy(e.path(), target).unwrap();
        }
    }
}

fn repo(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("stack-agent-patterns-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    copy(&fixture(), &dir);
    dir
}

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stack-agent"))
        .args(args)
        .arg("--root")
        .arg(root)
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn init_writes_the_graph_and_the_pattern_report_in_one_pass_and_says_what_it_could_not_see() {
    let root = repo("init");
    let o = run(&root, &["init"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let text = out(&o);
    assert!(text.contains("Snapshot: sha256:"), "{text}");
    assert!(
        text.contains(
            "Implementation patterns (Rust, deterministic; inferred from frequency, never enforced)"
        ),
        "{text}"
    );
    assert!(
        text.contains("PARTIAL"),
        "the fixture has a parse failure, a vendored tree and an orphan: {text}"
    );
    let graph_dir = root.join(".chip/graph");
    let names: Vec<String> = fs::read_dir(&graph_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names
            .iter()
            .any(|n| n.starts_with("patterns-sha256-") && n.ends_with(".json")),
        "{names:?}"
    );
    assert!(
        names.contains(&"patterns-latest".to_string()) && names.contains(&"latest".to_string())
    );
}

#[test]
fn the_pattern_view_is_current_until_the_source_changes_and_stale_after() {
    let root = repo("stale");
    assert_eq!(run(&root, &["init"]).status.code(), Some(0));

    let current = run(&root, &["graph", "patterns"]);
    assert_eq!(current.status.code(), Some(0), "{}", out(&current));
    let text = out(&current);
    assert!(text.contains("Status: current"), "{text}");
    // A candidate convention is listed as a count, not a rule.
    assert!(
        text.contains("Candidate conventions (a count of at least 3, not a rule)"),
        "{text}"
    );
    assert!(text.contains("Handler"), "{text}");

    fs::write(root.join("src/ops.rs"), "pub fn changed() {}\n").unwrap();
    let stale = run(&root, &["graph", "patterns"]);
    assert_eq!(stale.status.code(), Some(2), "{}", out(&stale));
    let text = out(&stale);
    assert!(text.contains("Status: STALE"), "{text}");
    assert!(text.contains("src/ops.rs"), "{text}");
    assert!(text.contains("Nothing below is current"), "{text}");

    // Re-running init rebuilds both, and the view is current again.
    assert_eq!(run(&root, &["init"]).status.code(), Some(0));
    assert_eq!(run(&root, &["graph", "patterns"]).status.code(), Some(0));
}

#[test]
fn the_json_view_carries_the_staleness_with_the_report() {
    let root = repo("json");
    assert_eq!(run(&root, &["init"]).status.code(), Some(0));
    let o = run(&root, &["graph", "patterns", "--json"]);
    assert_eq!(o.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_str(out(&o).trim()).unwrap();
    assert_eq!(v["current"], true);
    assert_eq!(v["report"]["schema_version"], "chip.patterns.v1");
    assert_eq!(v["report"]["coverage"]["complete"], false);
    assert!(
        v["report"]["patterns"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["enforced"] == false)
    );
}

#[test]
fn without_a_report_the_view_says_so_and_builds_nothing() {
    let root = repo("none");
    let o = run(&root, &["graph", "patterns"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).contains("No pattern report found"));
    assert!(!root.join(".chip").exists());
}
