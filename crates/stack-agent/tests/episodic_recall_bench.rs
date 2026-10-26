//! Baseline versus episodic recall, on frozen fixtures (docs/product/episodic-memory.md, section 6).
//!
//! Every run is the real `stack-agent work` process over a real FeltDB database file: Chip's loop,
//! the capability allowlist, the durable executor, the host's verdict, the episode written after
//! it. Each scenario seeds the database with earlier works, then runs one measured work twice, in
//! two fresh copies of the same fixture: arm A with `--no-recall`, arm B with recall. Nothing else
//! differs between the arms: the same goals, project, `pax` stand-in, limits and model.
//!
//! **The model is a scripted stand-in, not a model.** [`policy`] is a fixed function of the request
//! it is sent: it follows a default plan per goal and, when a request carries a recalled verified
//! success, first performs that episode's last write and a test. It is built to use the evidence,
//! so this benchmark measures what recall delivers to a decision that uses it, and whether recall
//! ever reaches a decision it must not (another project, an unverified outcome, other
//! conditions), and whether any authorization or verification check changes. It does not measure
//! whether a real model uses recalled evidence; that needs live providers and is not claimed.
//!
//! `EPISODIC_BENCH_OUT=<file>` writes the per-run measurements and the summary as JSON;
//! `EPISODIC_BENCH_TRIALS=<n>` sets the trials per scenario and arm (default 3).

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

use gated::{FAILED, Model, PASSED, Pax, Reply, command, unique};
use serde_json::{Value, json};

const READ_LIB: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const WRITE_LIB: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const WRITE_TWO: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/two.rs","content":"pub fn two() -> u8 { 2 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"the stand-in has no further step"}"#;

const ADD_TWO: &str = "Add a function `two` returning 2.";
const MAKE_PASS: &str = "Make the failing `two` test pass.";
const RENAME: &str = "Rename the logging module to tracing.";

/// The stand-in's judgment: the next step of its plan for the goal, counted by the requests it has
/// already made (`(model): request` lines of the decision context), or a block when the plan is
/// done. A recalled verified success puts its last write, and a test, first (taken out of where
/// the plan had them).
fn policy(body: &str) -> &'static str {
    let mut plan: Vec<&'static str> = if body.contains(ADD_TWO) {
        vec![READ_LIB, WRITE_LIB, TEST, WRITE_TWO, TEST]
    } else if body.contains(MAKE_PASS) {
        vec![WRITE_TWO, TEST, WRITE_LIB, TEST]
    } else if body.contains(RENAME) {
        vec![WRITE_LIB, TEST]
    } else {
        vec![]
    };
    if body.contains("recalled episode") && body.contains("verified success") {
        let recalled = if body.contains("project.write src/lib.rs (success) -> pax.test (success)")
        {
            Some(WRITE_LIB)
        } else if body.contains("project.write src/two.rs (success) -> pax.test (success)") {
            Some(WRITE_TWO)
        } else {
            None
        };
        if let Some(write) = recalled {
            // The recalled write and its test move to the front; the rest of the plan follows.
            if let Some(at) = plan.iter().position(|step| *step == write) {
                let end = if plan.get(at + 1) == Some(&TEST) {
                    at + 2
                } else {
                    at + 1
                };
                plan.drain(at..end);
            }
            plan.splice(0..0, [write, TEST]);
        }
    }
    let n = body.matches("(model): request ").count();
    plan.get(n).copied().unwrap_or(BLOCK)
}

/// A frozen fixture: a Cargo project, and a `pax` stand-in that passes exactly when `checks` holds
/// `fn two`, reporting `version`.
struct Fixture {
    dir: PathBuf,
    pax: Pax,
}

fn fixture(tag: &str, checks: &str, version: &str) -> Fixture {
    let dir = unique(&format!("bench-project-{tag}"));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), gated::LIB).unwrap();
    let pax_dir = unique(&format!("bench-pax-{tag}"));
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'pax {version}'; exit 0; fi\nif grep -q 'fn two' '{checks}' 2>/dev/null; then echo '{PASSED}'; else echo '{FAILED}'; fi\nexit 0\n"
    );
    std::fs::write(pax_dir.join("pax"), script).unwrap();
    std::fs::set_permissions(pax_dir.join("pax"), std::fs::Permissions::from_mode(0o755)).unwrap();
    Fixture {
        dir,
        pax: Pax { dir: pax_dir },
    }
}

#[derive(Clone, Copy)]
enum Seed {
    /// The stand-in policy, goal as given.
    Policy(&'static str),
    /// A model that blocks at once: an outcome that is not verified.
    Blocks(&'static str),
}

struct Scenario {
    name: &'static str,
    /// What prior experience is, and what recall should do with it.
    expectation: &'static str,
    seeds: Vec<Seed>,
    /// The seed runs in another project that shares the database.
    seed_elsewhere: bool,
    goal: &'static str,
    /// What the measured project's `pax` checks, and its version; the seed's are `src/lib.rs`
    /// and 0.4.1.
    measured_checks: &'static str,
    measured_version: &'static str,
    /// Whether recall should recall something here.
    recall_expected: bool,
}

fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "useful-same-task",
            expectation: "a verified success of the same task: recalled",
            seeds: vec![Seed::Policy(ADD_TWO)],
            seed_elsewhere: false,
            goal: ADD_TWO,
            measured_checks: "src/lib.rs",
            measured_version: "0.4.1",
            recall_expected: true,
        },
        Scenario {
            name: "repeated-failure",
            expectation: "a verified success reached after a failing test: recalled",
            seeds: vec![Seed::Policy(MAKE_PASS)],
            seed_elsewhere: false,
            goal: MAKE_PASS,
            measured_checks: "src/lib.rs",
            measured_version: "0.4.1",
            recall_expected: true,
        },
        Scenario {
            name: "irrelevant",
            expectation: "a verified success of a dissimilar task: not recalled",
            seeds: vec![Seed::Policy(RENAME)],
            seed_elsewhere: false,
            goal: ADD_TWO,
            measured_checks: "src/lib.rs",
            measured_version: "0.4.1",
            recall_expected: false,
        },
        Scenario {
            name: "other-project",
            expectation: "a verified success of another project sharing the database: not recalled",
            seeds: vec![Seed::Policy(ADD_TWO)],
            seed_elsewhere: true,
            goal: ADD_TWO,
            measured_checks: "src/lib.rs",
            measured_version: "0.4.1",
            recall_expected: false,
        },
        Scenario {
            name: "unverified-prior",
            expectation: "an earlier attempt that blocked (outcome unknown): not recalled",
            seeds: vec![Seed::Blocks(ADD_TWO)],
            seed_elsewhere: false,
            goal: ADD_TWO,
            measured_checks: "src/lib.rs",
            measured_version: "0.4.1",
            recall_expected: false,
        },
        Scenario {
            name: "changed-conditions",
            expectation: "a verified success under another verifier version: not recalled",
            seeds: vec![Seed::Policy(ADD_TWO)],
            seed_elsewhere: false,
            goal: ADD_TWO,
            measured_checks: "src/lib.rs",
            measured_version: "0.5.0",
            recall_expected: false,
        },
        Scenario {
            name: "misleading",
            expectation: "a verified success whose procedure no longer works here (the tests now check src/two.rs, under the same conditions): recalled, and wrong",
            seeds: vec![Seed::Policy(ADD_TWO)],
            seed_elsewhere: false,
            goal: ADD_TWO,
            measured_checks: "src/two.rs",
            measured_version: "0.4.1",
            recall_expected: true,
        },
    ]
}

async fn run(
    fixture: &Fixture,
    data: &Path,
    db: &Path,
    model: &Model,
    args: &[&str],
) -> (Output, u128) {
    let mut c: Command = command(&fixture.dir, &model.url, &fixture.pax);
    c.env("STACK_AGENT_DATA_DIR", data)
        .arg("work")
        .args(args)
        .args([
            "--state-db",
            db.to_str().unwrap(),
            "--json",
            "--max-turns",
            "8",
        ]);
    let started = Instant::now();
    let out = tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap();
    (out, started.elapsed().as_millis())
}

fn report(out: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.lines().last().unwrap_or_else(|| {
        panic!(
            "no report; stderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {stdout}"))
}

fn count(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// One measured run of one arm of one scenario, in fresh fixtures.
async fn measured(s: &Scenario, recall: bool, trial: usize) -> Value {
    let tag = format!("{}-{}-{trial}", s.name, if recall { "b" } else { "a" });
    let data = unique(&format!("bench-data-{tag}"));
    let db = data.join("agent-state.feltdb");
    let here = fixture(
        &format!("{tag}-here"),
        s.measured_checks,
        s.measured_version,
    );
    let seed_fixture = if s.seed_elsewhere {
        fixture(&format!("{tag}-elsewhere"), "src/lib.rs", "0.4.1")
    } else {
        // The seed runs in the measured project, under the seed's verifier and check.
        fixture(&format!("{tag}-seed"), "src/lib.rs", "0.4.1")
    };
    let seed_at: &Fixture = if s.seed_elsewhere {
        &seed_fixture
    } else {
        &here
    };
    // The seed's pax is the seed's; the measured run's is the scenario's.
    let seed_pax = Pax {
        dir: seed_fixture.pax.dir.clone(),
    };
    let seed_project = Fixture {
        dir: seed_at.dir.clone(),
        pax: seed_pax,
    };
    let mut seed_failures = 0;
    for seed in &s.seeds {
        let (model, goal) = match *seed {
            Seed::Policy(goal) => (Model::start(vec![Reply::Policy(policy)]).await, goal),
            Seed::Blocks(goal) => (Model::start(vec![Reply::Stream(BLOCK)]).await, goal),
        };
        let (out, _) = run(&seed_project, &data, &db, &model, &[goal]).await;
        let r = report(&out);
        seed_failures += count(&r["failed_observations"]);
        std::fs::write(seed_project.dir.join("src/lib.rs"), gated::LIB).unwrap();
        let _ = std::fs::remove_file(seed_project.dir.join("src/two.rs"));
    }
    let model = Model::start(vec![Reply::Policy(policy)]).await;
    let mut args = vec![s.goal];
    if !recall {
        args.push("--no-recall");
    }
    let (out, latency_ms) = run(&here, &data, &db, &model, &args).await;
    let r = report(&out);
    let exit = out.status.code();
    let recalled = r["episodes"]["recall"]["recalled"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    // Recall reached a decision it must not: anything recalled where nothing should be, or an
    // outcome that is not verified.
    let recall_violations = recalled
        .iter()
        .filter(|e| {
            !s.recall_expected || !e["outcome"].as_str().unwrap_or("").starts_with("verified_")
        })
        .count();
    let bodies = model.bodies();
    let first = bodies.first().cloned().unwrap_or_default();
    let followed =
        recall && first.contains("recalled episode") && first.contains("verified success");
    let failed = count(&r["failed_observations"]);
    let audit = &r["audit"];
    let regressions = u64::from(audit["clean"] != true)
        + count(&audit["unauthorized_executions"])
        + count(&audit["unauthorized_completions"])
        + count(&audit["false_completions"])
        + u64::from(exit == Some(0) && r["verified"] != true);
    let executions: u64 = r["executions_by_capability"]
        .as_object()
        .map(|m| m.values().map(count).sum())
        .unwrap_or(0);
    json!({
        "scenario": s.name,
        "arm": if recall { "B-recall" } else { "A-baseline" },
        "trial": trial,
        "exit_status": exit,
        "verified": r["verified"] == true && exit == Some(0),
        "episode_outcome": r["episodes"]["recorded"]["outcome"],
        "model_calls": model.requests(),
        "executions": executions,
        "failed_observations": failed,
        "repeated_failure": u64::from(seed_failures > 0 && failed > 0),
        "recalled": recalled.len(),
        "recall_violations": recall_violations,
        "followed_recall_then_failed": u64::from(followed && failed > 0),
        "regressions": regressions,
        "latency_ms": latency_ms,
    })
}

fn median(mut v: Vec<u128>) -> u128 {
    v.sort();
    v[v.len() / 2]
}

#[tokio::test(flavor = "multi_thread")]
async fn baseline_versus_episodic_recall_on_frozen_fixtures() {
    let trials: usize = std::env::var("EPISODIC_BENCH_TRIALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let mut runs = Vec::new();
    for s in scenarios() {
        for trial in 0..trials {
            for recall in [false, true] {
                runs.push(measured(&s, recall, trial).await);
            }
        }
    }
    let mut summary = Vec::new();
    for s in scenarios() {
        for arm in ["A-baseline", "B-recall"] {
            let rows: Vec<&Value> = runs
                .iter()
                .filter(|r| r["scenario"] == s.name && r["arm"] == arm)
                .collect();
            let sum = |k: &str| rows.iter().map(|r| count(&r[k])).sum::<u64>();
            let n = rows.len() as u64;
            summary.push(json!({
                "scenario": s.name,
                "expectation": s.expectation,
                "arm": arm,
                "runs": n,
                "verified": rows.iter().filter(|r| r["verified"] == true).count(),
                "model_calls_per_run": sum("model_calls") as f64 / n as f64,
                "executions_per_run": sum("executions") as f64 / n as f64,
                "failed_observations_per_run": sum("failed_observations") as f64 / n as f64,
                "repeated_failure_runs": sum("repeated_failure"),
                "runs_with_recall": rows.iter().filter(|r| count(&r["recalled"]) > 0).count(),
                "recall_violations": sum("recall_violations"),
                "followed_recall_then_failed": sum("followed_recall_then_failed"),
                "regressions": sum("regressions"),
                "median_latency_ms": median(rows.iter().map(|r| r["latency_ms"].as_u64().unwrap() as u128).collect()),
            }));
        }
    }
    println!(
        "scenario             arm         verified  calls  execs  failed  repeat  recalled  wrong-reuse  violations  regress  p50 ms"
    );
    for row in &summary {
        println!(
            "{:<20} {:<11} {:>3}/{:<4} {:>6.2} {:>6.2} {:>7.2} {:>7} {:>9} {:>12} {:>11} {:>8} {:>7}",
            row["scenario"].as_str().unwrap(),
            row["arm"].as_str().unwrap(),
            row["verified"],
            row["runs"],
            row["model_calls_per_run"].as_f64().unwrap(),
            row["executions_per_run"].as_f64().unwrap(),
            row["failed_observations_per_run"].as_f64().unwrap(),
            row["repeated_failure_runs"],
            row["runs_with_recall"],
            row["followed_recall_then_failed"],
            row["recall_violations"],
            row["regressions"],
            row["median_latency_ms"],
        );
    }
    if let Ok(path) = std::env::var("EPISODIC_BENCH_OUT") {
        let doc = json!({
            "schema": "stack-agent.episodic-recall-bench.v1",
            "model": "scripted stand-in policy (tests/episodic_recall_bench.rs::policy); not a model",
            "trials_per_arm": trials,
            "summary": summary,
            "runs": runs,
        });
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap() + "\n").unwrap();
    }

    // What must hold whatever the numbers: recall never reaches a decision it must not, never
    // changes an authorization or verification result, and the baseline never recalls.
    for r in &runs {
        assert_eq!(r["recall_violations"], 0, "{r}");
        assert_eq!(r["regressions"], 0, "{r}");
        if r["arm"] == "A-baseline" {
            assert_eq!(r["recalled"], 0, "{r}");
        }
    }
    for s in scenarios() {
        let recalled = runs
            .iter()
            .filter(|r| r["scenario"] == s.name && r["arm"] == "B-recall")
            .all(|r| (count(&r["recalled"]) > 0) == s.recall_expected);
        assert!(
            recalled,
            "{}: recall expected {}",
            s.name, s.recall_expected
        );
    }
    // Every arm of every scenario is verified: the fixtures are solvable either way.
    assert!(runs.iter().all(|r| r["verified"] == true), "{runs:#?}");
}
