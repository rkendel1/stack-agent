//! The durability benchmark (`stack-agent --durability-bench`) is repeatable, judges from the world
//! on disk, never counts a refusal as success, and reports what each arm actually did. The
//! findings it currently produces are pinned here so a change to either arm shows up as a changed
//! measurement, not a silent one.

use std::collections::BTreeMap;

use chip_cli::durability_bench::{
    Arm, Scenario, Task, deterministic_part, run_all, run_case, summarize,
};
use serde_json::Value;

fn scratch(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("chip-durability-test-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

async fn case(scenario: Scenario, task: Task, arm: Arm) -> Value {
    let root = scratch(&format!(
        "{}-{}-{}",
        scenario.name(),
        task.name(),
        arm.name()
    ));
    let r = run_case(scenario, task, arm, &root, 0).await;
    let _ = std::fs::remove_dir_all(&root);
    r
}

fn u(r: &Value, k: &str) -> u64 {
    r[k].as_u64()
        .unwrap_or_else(|| panic!("{k} missing in {r}"))
}

#[tokio::test(flavor = "current_thread")]
async fn every_case_is_repeatable_and_judged_from_the_world() {
    let root = scratch("all");
    let records = run_all(&root, 2).await;
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(
        records.len(),
        2 * Scenario::ALL.len() * Task::ALL.len() * Arm::ALL.len()
    );
    let mut by_case: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for r in &records {
        assert_eq!(r["schema"], "chip.durability-bench.result.v1");
        assert_eq!(r["task_set"], "chip.durability-bench.tasks.v1");
        let class = r["classification"].as_str().unwrap();
        assert!(
            [
                "success",
                "false_success",
                "completed_unproven",
                "unresolved",
                "failure"
            ]
            .contains(&class)
        );
        // Success is the world's verdict plus a report, never the report alone.
        assert_eq!(
            class == "success",
            r["verified_complete"] == true && r["reported_success"] == true,
            "{r}"
        );
        // A refusal is never a success by itself: every case with one reached success, if at
        // all, through a later run that ran.
        if u(r, "safe_refusals") > 0 && class == "success" {
            assert!(
                r["runs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|run| run["start"] == "ran" && run["reported_success"] == true)
            );
        }
        let key = format!("{}|{}|{}", r["scenario"], r["task"], r["arm"]);
        by_case.entry(key).or_default().push(deterministic_part(r));
    }
    for (key, reps) in by_case {
        assert!(
            reps.windows(2).all(|w| w[0] == w[1]),
            "{key} differs between repeats"
        );
    }
    let summary = summarize(&records);
    assert_eq!(summary["rows"].as_array().unwrap().len(), 12);
}

#[tokio::test(flavor = "current_thread")]
async fn without_a_fault_both_arms_reach_the_same_verified_outcome_with_the_same_work() {
    for task in Task::ALL {
        let a = case(Scenario::Normal, task, Arm::Legacy).await;
        let b = case(Scenario::Normal, task, Arm::Durable).await;
        for k in [
            "verified_complete",
            "reported_success",
            "model_calls",
            "executions",
            "duplicate_effects",
        ] {
            assert_eq!(a[k], b[k], "{} {k}", task.name());
        }
        assert_eq!(a["classification"], "success");
        assert_eq!(b["persistence_failures"], 0);
        assert!(b["persistence_ms"].as_f64().unwrap() > 0.0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn competing_workers_duplicate_and_both_claim_success_without_state_and_not_with_it() {
    for task in Task::ALL {
        let a = case(Scenario::ConcurrentStart, task, Arm::Legacy).await;
        let b = case(Scenario::ConcurrentStart, task, Arm::Durable).await;
        assert!(u(&a, "duplicate_effects") > 0, "{a}");
        assert_eq!(u(&a, "false_successes"), 2, "both legacy workers claim it");
        assert_eq!(b["classification"], "success", "{b}");
        assert_eq!(u(&b, "duplicate_effects"), 0);
        assert_eq!(b["runs"][1]["start"], "lease_held");
        assert!(
            u(&b, "executions") < u(&a, "executions"),
            "no wasted work in B"
        );
    }
    // A stale owner whose effects are all observable: B fences it and completes once.
    let a = case(Scenario::StaleOwner, Task::Steps, Arm::Legacy).await;
    let b = case(Scenario::StaleOwner, Task::Steps, Arm::Durable).await;
    assert_eq!(u(&a, "false_successes"), 2);
    assert_eq!(b["classification"], "success");
    assert_eq!(u(&b, "false_successes"), 0);
    let stale = b["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["worker"] == "w1")
        .unwrap();
    assert_eq!(stale["reported_success"], false);
    assert!(
        stale["durable"].as_str().unwrap().contains("FENCE_STALE"),
        "{stale}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn an_ambiguous_action_is_refused_safely_but_the_merged_resume_still_repeats_it() {
    // Arm B refuses to resume (OutcomeUncertain) until the outcome is reconciled with evidence...
    let b = case(Scenario::CrashAfterEffect, Task::Notify, Arm::Durable).await;
    assert_eq!(u(&b, "safe_refusals"), 1);
    assert_eq!(u(&b, "operator_interventions"), 1);
    assert_eq!(b["runs"][1]["start"], "outcome_uncertain");
    // ...but the resumed attempt runs the loop from the goal without that history, so an
    // unobservable action that was reconciled as executed is performed again. The duplicate is
    // deferred, not prevented: a measured gap of the merged implementation, not of the benchmark.
    assert_eq!(b["world"]["outbox"], serde_json::json!([1, 1]));
    assert_eq!(b["classification"], "false_success");
    let a = case(Scenario::CrashAfterEffect, Task::Notify, Arm::Legacy).await;
    assert_eq!(a["world"]["outbox"], serde_json::json!([1, 1]));
    // Where the world is observable, both arms recover; B pays a refusal and two lease waits.
    let a = case(Scenario::CrashAfterEffect, Task::Steps, Arm::Legacy).await;
    let b = case(Scenario::CrashAfterEffect, Task::Steps, Arm::Durable).await;
    assert_eq!(a["classification"], "success");
    assert_eq!(b["classification"], "success");
    assert!(u(&b, "lease_wait_ms_modelled") > u(&a, "lease_wait_ms_modelled"));
}

#[tokio::test(flavor = "current_thread")]
async fn the_durable_arm_never_reports_more_false_successes_than_the_legacy_arm() {
    let root = scratch("pairs");
    let records = run_all(&root, 1).await;
    let _ = std::fs::remove_dir_all(&root);
    let summary = summarize(&records);
    for row in summary["rows"].as_array().unwrap() {
        let (a, b) = (&row["A"], &row["B"]);
        assert!(
            b["false_successes"].as_u64() <= a["false_successes"].as_u64(),
            "{row}"
        );
        assert!(
            b["duplicate_effects"].as_u64() <= a["duplicate_effects"].as_u64(),
            "{row}"
        );
    }
}

#[test]
fn the_command_writes_results_a_summary_and_a_table() {
    let out = scratch("cli");
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_stack-agent"))
        .args(["--durability-bench", "--repeats", "1", "--out"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let lines = std::fs::read_to_string(out.join("results.jsonl")).unwrap();
    assert_eq!(lines.lines().count(), 24);
    for l in lines.lines() {
        serde_json::from_str::<Value>(l).unwrap();
    }
    let summary: Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["run"]["repeats"], 1);
    assert!(
        std::fs::read_to_string(out.join("summary.md"))
            .unwrap()
            .contains("| normal |")
    );
    let bad = std::process::Command::new(env!("CARGO_BIN_EXE_stack-agent"))
        .args(["--durability-bench", "--repeats", "0"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&out);
}
