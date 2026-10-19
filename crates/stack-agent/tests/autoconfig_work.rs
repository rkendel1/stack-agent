//! `stack-agent work --auto` through the real binary: selection, the feedback loop from real outcome
//! records, refusal, narrowing, cancellation, secrets, and that nothing changes without `--auto`.
//! A scripted model and a stand-in `pax`; no network, no key.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::path::Path;
use std::process::Output;

use gated::{FAILED, LIB, Model, PASSED, Pax, Reply, command, project, unique};
use serde_json::Value;

const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;
const DONE: &str = r#"{"decision":"complete","summary":"`one` is in src/lib.rs."}"#;
const DEDUP: &str = "change.cargo-verified@1";
const FULL: &str = "change.cargo-verified-full-context@1";
const SENTINEL: &str = "sk-SENTINEL-autoconfig-0123456789";

struct Run {
    out: Output,
    requests: usize,
}

async fn work(dir: &Path, script: Vec<Reply>, pax: &Pax, args: &[&str]) -> Run {
    let model = Model::start(script).await;
    let mut c = command(dir, &model.url, pax);
    c.arg("work").args(args).env("CHIP_API_KEY", SENTINEL);
    let out = tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap();
    Run {
        out,
        requests: model.requests(),
    }
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn auto(v: &Value) -> &Value {
    &v["automatic_configuration"]
}

fn selected(v: &Value) -> Option<&str> {
    auto(v)["decision"]["outcome"]["configuration"].as_str()
}

#[tokio::test(flavor = "multi_thread")]
async fn auto_selects_a_configuration_runs_it_and_records_the_verified_outcome() {
    let pax = Pax::new("auto-ok", PASSED, false);
    let dir = project("auto-ok");
    let r = work(
        &dir,
        vec![Reply::Stream(WRITE), Reply::Stream(TEST)],
        &pax,
        &["--auto", "--json", "Add `two`."],
    )
    .await;
    let v = json(&r.out);
    assert_eq!(r.out.status.code(), Some(0), "{v}");
    assert_eq!(selected(&v), Some(DEDUP));
    assert_eq!(auto(&v)["decision"]["confidence"], "evidence_limited");
    assert_eq!(
        auto(&v)["profile"]["primary_build_system"]["value"],
        "cargo"
    );
    let record = &auto(&v)["outcome_record"];
    assert_eq!(record["outcome"], "verified_success");
    assert_eq!(record["configuration"], DEDUP);
    assert_eq!(record["origin"], "production");
    assert_eq!(record["selection_id"], auto(&v)["decision"]["id"]);
    assert_eq!(record["capabilities_invoked"]["pax.test"], 1);
    assert_eq!(record["verification_refs"].as_array().unwrap().len(), 1);
    assert!(
        auto(&v)["persistence"]
            .as_str()
            .unwrap()
            .contains("learning documents")
    );
    // The work itself is the same verified run as without --auto.
    assert_eq!(v["verified"], true);
    assert_eq!(r.requests, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn real_outcome_records_feed_the_next_selection() {
    // Three real runs of the default configuration whose tests fail after the change.
    let mut records = Vec::new();
    for i in 0..3 {
        let pax = Pax::new(&format!("loop-{i}"), FAILED, false);
        let dir = project(&format!("loop-{i}"));
        let r = work(
            &dir,
            vec![
                Reply::Stream(WRITE),
                Reply::Stream(TEST),
                Reply::Stream(BLOCK),
            ],
            &pax,
            &["--auto", "--json", "Add `two`."],
        )
        .await;
        let v = json(&r.out);
        assert_eq!(selected(&v), Some(DEDUP));
        assert_eq!(
            auto(&v)["outcome_record"]["outcome"],
            "verified_failure",
            "{v}"
        );
        records.push(auto(&v)["outcome_record"].clone());
    }
    let snapshot = unique("loop-snapshot").join("evidence.json");
    std::fs::write(&snapshot, Value::Array(records.clone()).to_string()).unwrap();

    // The next selection, given that evidence, ranks the default below neutral.
    let mut decisions = Vec::new();
    for i in 0..2 {
        let pax = Pax::new(&format!("loop-next-{i}"), PASSED, false);
        let dir = project(&format!("loop-next-{i}"));
        let r = work(
            &dir,
            vec![Reply::Stream(WRITE), Reply::Stream(TEST)],
            &pax,
            &[
                "--auto",
                "--json",
                "--selection-evidence",
                snapshot.to_str().unwrap(),
                "Add `two`.",
            ],
        )
        .await;
        let v = json(&r.out);
        assert_eq!(r.out.status.code(), Some(0), "{v}");
        assert_eq!(selected(&v), Some(FULL), "{}", auto(&v)["decision"]);
        let used: Vec<&str> = auto(&v)["decision"]["evidence_used"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        let ids: Vec<&str> = records.iter().map(|r| r["id"].as_str().unwrap()).collect();
        let mut ids_sorted = ids.clone();
        ids_sorted.sort();
        assert_eq!(used, ids_sorted, "the decision names the evidence it used");
        assert_eq!(auto(&v)["outcome_record"]["configuration"], FULL);
        decisions.push((
            auto(&v)["decision"]["outcome"].clone(),
            auto(&v)["decision"]["assessments"].clone(),
        ));
    }
    assert_eq!(
        decisions[0], decisions[1],
        "the same project, evidence and day give the same decision"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_eligible_configuration_refuses_before_any_model_call() {
    let pax = Pax::new("npm", PASSED, false);
    let dir = unique("npm-project");
    std::fs::write(
        dir.join("package.json"),
        "{\"name\":\"x\",\"scripts\":{\"test\":\"true\"}}",
    )
    .unwrap();
    let r = work(
        &dir,
        vec![Reply::Stream(WRITE)],
        &pax,
        &["--auto", "--json", "Fix the bug."],
    )
    .await;
    assert_eq!(r.out.status.code(), Some(3));
    assert_eq!(r.requests, 0, "nothing was sent");
    let v = json(&r.out);
    assert_eq!(v["ran"], false);
    let reasons = auto(&v)["decision"]["outcome"]["reasons"].to_string();
    assert!(
        reasons.contains("no authoritative test semantics"),
        "{reasons}"
    );
    assert!(!pax.started(), "no test ran");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prohibited_capability_is_not_offered_and_never_executes() {
    let pax = Pax::new("inspect", PASSED, false);
    let dir = project("inspect-narrow");
    let r = work(
        &dir,
        vec![Reply::Stream(WRITE), Reply::Stream(DONE)],
        &pax,
        &["--auto", "--json", "--kind", "inspect", "Where is `one`?"],
    )
    .await;
    let v = json(&r.out);
    assert_eq!(selected(&v), Some("inspect.read-only@1"));
    let offered = auto(&v)["decision"]["assessments"][3]["offered"].to_string();
    assert!(!offered.contains("project.write"), "{offered}");
    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        LIB,
        "nothing was written"
    );
    assert!(
        v["executions_by_capability"].get("project.write").is_none(),
        "{v}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_and_delivery_are_preserved_in_the_outcome_record() {
    let pax = Pax::new("cancel", PASSED, false);
    let dir = project("auto-cancel");
    let model = Model::start(vec![Reply::Stall(READ)]).await;
    let mut c = command(&dir, &model.url, &pax);
    c.args([
        "work",
        "--auto",
        "--json",
        "--deadline-secs",
        "1",
        "Add `two`.",
    ]);
    let out = tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap();
    model.release();
    let v = json(&out);
    assert_eq!(out.status.code(), Some(1), "{v}");
    let record = &auto(&v)["outcome_record"];
    assert_eq!(record["outcome"], "cancelled");
    assert_eq!(record["delivery"], "confirmed_response");
    assert!(
        record["cancellation"]
            .as_str()
            .unwrap()
            .starts_with("deadline"),
        "{record}"
    );
    assert_eq!(model.requests(), 1, "nothing was retried");
}

#[tokio::test(flavor = "multi_thread")]
async fn records_and_diagnostics_carry_no_secret_and_no_goal_text() {
    let pax = Pax::new("secret", PASSED, false);
    let dir = project("auto-secret");
    let goal = format!("Add `two`; the key is {SENTINEL}");
    let r = work(
        &dir,
        vec![Reply::Stream(WRITE), Reply::Stream(TEST)],
        &pax,
        &["--auto", "--json", &goal],
    )
    .await;
    let section = auto(&json(&r.out)).to_string();
    assert!(!section.contains(SENTINEL));
    assert!(!section.contains("Add `two`"), "the goal is not evidence");
    let human = work(
        &project("auto-secret-h"),
        vec![Reply::Stream(WRITE), Reply::Stream(TEST)],
        &Pax::new("secret-h", PASSED, false),
        &["--auto", &goal],
    )
    .await;
    let text = String::from_utf8_lossy(&human.out.stdout);
    assert!(
        text.contains("Automatic configuration (catalog.v1, score.v1, profile.v1):"),
        "{text}"
    );
    assert!(text.contains("Selected: change.cargo-verified@1"), "{text}");
    // The existing report echoes the goal (the user's own text); the automatic-configuration
    // section, and stderr, never carry it or the key.
    let section = &text[text.find("Automatic configuration").unwrap()..];
    assert!(!section.contains(SENTINEL), "{section}");
    assert!(!String::from_utf8_lossy(&human.out.stderr).contains(SENTINEL));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_auto_nothing_changes_and_evidence_needs_auto() {
    let pax = Pax::new("plain", PASSED, false);
    let dir = project("auto-plain");
    let r = work(
        &dir,
        vec![Reply::Stream(WRITE), Reply::Stream(TEST)],
        &pax,
        &["--json", "Add `two`."],
    )
    .await;
    let v = json(&r.out);
    assert!(v.get("automatic_configuration").is_none());
    assert_eq!(v["verified"], true);
    let snapshot = unique("plain-snapshot").join("e.json");
    std::fs::write(&snapshot, "[]").unwrap();
    let r = work(
        &dir,
        vec![],
        &pax,
        &["--selection-evidence", snapshot.to_str().unwrap(), "x"],
    )
    .await;
    assert_eq!(r.out.status.code(), Some(2));
    std::fs::write(&snapshot, "[{\"schema\":\"forged\"}]").unwrap();
    let r = work(
        &dir,
        vec![],
        &pax,
        &[
            "--auto",
            "--selection-evidence",
            snapshot.to_str().unwrap(),
            "x",
        ],
    )
    .await;
    assert_eq!(r.out.status.code(), Some(2));
    assert_eq!(r.requests, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn experiment_evidence_is_recorded_but_never_changes_production_selection() {
    let wd = unique("autoconfig-experiment");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_stack-agent"))
        .args([
            "experiment",
            "run",
            "autoconfig.change-context",
            "--json",
            "--work-dir",
        ])
        .arg(&wd)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report = json(&out);
    assert_eq!(
        report["comparison"]["conclusion"]["name"],
        "same_verified_outcomes"
    );
    let evidence = report["selection_evidence"].as_array().unwrap().clone();
    assert_eq!(evidence.len(), 2);
    for (record, config) in evidence.iter().zip([DEDUP, FULL]) {
        assert_eq!(record["origin"], "experiment");
        assert_eq!(record["configuration"], config);
        assert_eq!(record["outcome"], "verified_success");
    }
    // Ten copies' worth of experiment success for the full-context configuration...
    let mut many = Vec::new();
    for _ in 0..5 {
        many.extend(evidence.iter().cloned());
    }
    let snapshot = unique("autoconfig-experiment-snapshot").join("e.json");
    std::fs::write(&snapshot, Value::Array(many).to_string()).unwrap();
    // ...leave the production choice exactly as it was.
    let pax = Pax::new("exp-prod", PASSED, false);
    let r = work(
        &project("exp-prod"),
        vec![Reply::Stream(WRITE), Reply::Stream(TEST)],
        &pax,
        &[
            "--auto",
            "--json",
            "--selection-evidence",
            snapshot.to_str().unwrap(),
            "Add `two`.",
        ],
    )
    .await;
    let v = json(&r.out);
    assert_eq!(selected(&v), Some(DEDUP));
    assert_eq!(auto(&v)["decision"]["evidence_used"], serde_json::json!([]));
    assert_eq!(auto(&v)["decision"]["confidence"], "evidence_limited");
}
