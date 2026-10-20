//! The path policy, case by case, against a real project directory: what may be read, what is
//! withheld from every capability, and what may be read but never written. The cases are data
//! (`tests/fixtures/path-policy.json`), so a rule cannot be added without a case.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chip_core::{
    CapabilityError, CapabilityId, CapabilityProvider, ExecutionId, ExecutionRequest,
    ExecutionStatus, Executor, InputValue,
};
use chip_project::{
    PROJECT_LIST, PROJECT_READ, PROJECT_SEARCH, PROJECT_WRITE, ProjectExecutor, policy,
};
use serde_json::Value;

fn fixtures() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/path-policy.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn project(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("chip-policy-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn text(pairs: &[(&str, &str)]) -> BTreeMap<String, InputValue> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), InputValue::Text(v.to_string())))
        .collect()
}

async fn validate(
    p: &ProjectExecutor,
    capability: &str,
    inputs: &BTreeMap<String, InputValue>,
) -> Result<(), CapabilityError> {
    p.validate_inputs(&CapabilityId::new(capability).unwrap(), inputs)
        .await
}

/// `Ok(())`, or the refusal's class and message.
fn class(result: Result<(), CapabilityError>) -> (String, String) {
    match result {
        Ok(()) => ("allowed".into(), String::new()),
        Err(CapabilityError::InvalidInput(why)) if why.contains("is reserved") => {
            ("withheld".into(), why)
        }
        Err(CapabilityError::InvalidInput(why)) if why.contains("is protected") => {
            ("protected".into(), why)
        }
        Err(other) => ("other".into(), other.to_string()),
    }
}

const SENTINEL: &str = "SENTINEL-policy-content";

#[tokio::test]
async fn every_path_case_is_decided_as_the_fixture_says() {
    let root = project("cases");
    let cases = fixtures()["cases"].as_array().unwrap().clone();
    // Every file exists, with a sentinel, so "withheld" is not just "missing".
    for case in &cases {
        let path = root.join(case["path"].as_str().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{SENTINEL}\n")).unwrap();
    }
    let p = ProjectExecutor::new(&root);
    let mut failures = Vec::new();
    for case in &cases {
        let path = case["path"].as_str().unwrap();
        let rule = case["rule"].as_str().unwrap_or_default();
        for (capability, inputs, expected) in [
            (
                PROJECT_READ,
                text(&[("path", path)]),
                case["read"].as_str().unwrap(),
            ),
            (
                PROJECT_WRITE,
                text(&[("path", path), ("content", "fn main() {}\n")]),
                case["write"].as_str().unwrap(),
            ),
        ] {
            let (got, why) = class(validate(&p, capability, &inputs).await);
            if got != expected || (expected != "allowed" && !why.contains(rule)) {
                failures.push(format!(
                    "{path} {capability}: expected {expected} ({rule}), got {got}: {why}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn a_withheld_file_never_reaches_a_listing_a_search_or_a_read() {
    let root = project("hidden");
    let cases = fixtures()["cases"].as_array().unwrap().clone();
    let mut withheld = BTreeSet::new();
    for case in &cases {
        let path = case["path"].as_str().unwrap();
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, format!("{SENTINEL} {path}\n")).unwrap();
        if case["read"] == "withheld" {
            withheld.insert(path.to_string());
        }
    }
    let p = ProjectExecutor::new(&root);
    let run = |capability: &str, inputs: BTreeMap<String, InputValue>| {
        let request = ExecutionRequest::new(ExecutionId::new("e"), capability).with_inputs(inputs);
        let p = p.clone();
        async move { p.execute(request).await }
    };
    // Searching everything (hidden directories are skipped by search; name them explicitly too).
    let mut seen = String::new();
    let mut dirs: BTreeSet<String> = cases
        .iter()
        .filter_map(|c| {
            let path = c["path"].as_str().unwrap();
            path.rsplit_once('/').map(|(d, _)| d.to_string())
        })
        .collect();
    dirs.insert(".".into());
    for dir in &dirs {
        for (capability, inputs) in [
            (PROJECT_LIST, text(&[("path", dir)])),
            (PROJECT_SEARCH, text(&[("query", SENTINEL), ("path", dir)])),
        ] {
            if let Ok(result) = run(capability, inputs).await {
                seen.push_str(&result.output);
                seen.push('\n');
            }
        }
    }
    // Every path a listing returned, and every path a search matched in.
    let listed: BTreeSet<&str> = seen
        .lines()
        .filter_map(|l| {
            l.strip_prefix("dir ")
                .or_else(|| {
                    l.strip_prefix("file ")
                        .and_then(|r| r.rsplit_once(' ').map(|(p, _)| p))
                })
                .or_else(|| {
                    l.split_once(':')
                        .map(|(p, _)| p)
                        .filter(|p| !p.starts_with('{'))
                })
        })
        .collect();
    for path in &listed {
        assert!(
            path.split('/').all(|c| policy::withheld(c).is_none()),
            "{path}: a withheld name was listed or searched"
        );
    }
    for path in &withheld {
        assert!(
            !seen.contains(&format!("{SENTINEL} {path}")),
            "{path}: its content was returned"
        );
        assert!(!listed.contains(path.as_str()), "{path}: it was listed");
        let refused = run(PROJECT_READ, text(&[("path", path)])).await;
        assert!(refused.is_err(), "{path}: read was executed");
    }
    // The readable files really are found, so the search above was not empty by accident.
    assert!(seen.contains(&format!("{SENTINEL} src/lib.rs")), "{seen}");
}

#[tokio::test]
async fn manifest_writes_are_decided_by_whether_they_change_the_build_script() {
    let root = project("manifests");
    let p = ProjectExecutor::new(&root);
    let mut failures = Vec::new();
    for case in fixtures()["manifests"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let path = case["path"].as_str().unwrap_or("Cargo.toml");
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let _ = std::fs::remove_file(&file);
        if let Some(before) = case["before"].as_str() {
            std::fs::write(&file, before).unwrap();
        }
        let after = case["after"].as_str().unwrap();
        let expected = case["write"].as_str().unwrap();
        let rule = case["rule"].as_str().unwrap_or_default();
        let (got, why) = class(
            validate(
                &p,
                PROJECT_WRITE,
                &text(&[("path", path), ("content", after)]),
            )
            .await,
        );
        if got != expected || !why.contains(rule) {
            failures.push(format!(
                "{name}: expected {expected} ({rule}), got {got}: {why}"
            ));
        }
        // The executor checks again at the moment of the write.
        let result = p
            .execute(
                ExecutionRequest::new(ExecutionId::new("e"), PROJECT_WRITE)
                    .with_inputs(text(&[("path", path), ("content", after)])),
            )
            .await;
        let wrote = result.is_ok_and(|r| r.status == ExecutionStatus::Success);
        if wrote != (expected == "allowed") {
            failures.push(format!("{name}: executor wrote = {wrote}"));
        }
        if expected != "allowed" {
            assert_eq!(
                std::fs::read_to_string(&file).ok().as_deref(),
                case["before"].as_str(),
                "{name}: a refused manifest is unchanged"
            );
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_fixtures_exercise_every_rule() {
    let fixtures = fixtures();
    let named: BTreeSet<&str> = fixtures["cases"]
        .as_array()
        .unwrap()
        .iter()
        .chain(fixtures["manifests"].as_array().unwrap())
        .filter_map(|c| c["rule"].as_str())
        .collect();
    let rules: BTreeSet<&str> = policy::ALL.iter().map(|r| r.id).collect();
    assert_eq!(
        named, rules,
        "every rule has a case, and every case names a real rule"
    );
}
