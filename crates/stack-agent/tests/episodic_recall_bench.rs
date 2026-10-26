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

use std::collections::BTreeMap;
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
    // A followed workflow says which step is next and what to supply: the stand-in supplies it.
    if body.contains("is project.write with path=src/lib.rs; supply") {
        return WRITE_LIB;
    }
    if body.contains("is project.write with path=src/two.rs; supply") {
        return WRITE_TWO;
    }
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
    // Its place in the plan: every request made so far, the model's and Chip's own (a followed
    // workflow's steps), so that after a fallback it goes on from what actually ran.
    let n = body.matches("(model): request ").count() + body.matches("(local): request ").count();
    plan.get(n).copied().unwrap_or(BLOCK)
}

/// A frozen fixture: a Cargo project, and a `pax` stand-in reporting `version` that passes exactly
/// when the file the project's `.pax-checks` names holds `fn two` (`checks` at first). What the
/// tests check is the project's; the stand-in itself differs only by its version.
struct Fixture {
    dir: PathBuf,
    pax: Pax,
}

fn fixture(tag: &str, checks: &str, version: &str, test_file: bool) -> Fixture {
    let dir = unique(&format!("bench-project-{tag}"));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), gated::LIB).unwrap();
    std::fs::write(dir.join(".pax-checks"), checks).unwrap();
    if test_file {
        std::fs::create_dir_all(dir.join("tests")).unwrap();
    }
    set_checks(&dir, checks);
    let pax_dir = unique(&format!("bench-pax-{tag}"));
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'pax {version}'; exit 0; fi\nif [ -f tests/two.rs ]; then t=$(sed -n 's|^// checks: ||p' tests/two.rs); else t=$(cat .pax-checks); fi\nif grep -q 'fn two' \"$t\" 2>/dev/null; then echo '{PASSED}'; else echo '{FAILED}'; fi\nexit 0\n"
    );
    std::fs::write(pax_dir.join("pax"), script).unwrap();
    std::fs::set_permissions(pax_dir.join("pax"), std::fs::Permissions::from_mode(0o755)).unwrap();
    Fixture {
        dir,
        pax: Pax { dir: pax_dir },
    }
}

/// What the project's tests check: its test file `tests/two.rs` when it has one (a test definition
/// by the runtime's own test-file rule, so a change to it is a change of test definitions), and
/// otherwise `.pax-checks` (verification input the runtime cannot see as a test).
fn set_checks(dir: &Path, checks: &str) {
    if dir.join("tests").is_dir() {
        std::fs::write(
            dir.join("tests/two.rs"),
            format!("// checks: {checks}\n#[test]\nfn two() {{}}\n"),
        )
        .unwrap();
    } else {
        std::fs::write(dir.join(".pax-checks"), checks).unwrap();
    }
}

#[derive(Clone, Copy)]
enum Seed {
    /// The stand-in policy, goal as given.
    Policy(&'static str),
    /// The stand-in policy, goal as given, while the project's tests check another file.
    PolicyChecking(&'static str, &'static str),
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
    /// Whether the procedure a recalled episode ends with is wrong in the measured project (the
    /// scenario's ground truth): following it then is incorrect reuse, never useful reuse.
    recalled_is_wrong: bool,
    /// The project has a test file (`tests/two.rs`) saying what its tests check.
    test_file: bool,
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
            recalled_is_wrong: false,
            recall_expected: true,
            test_file: false,
        },
        Scenario {
            name: "repeated-failure",
            expectation: "a verified success reached after a failing test: recalled",
            seeds: vec![Seed::Policy(MAKE_PASS)],
            seed_elsewhere: false,
            goal: MAKE_PASS,
            measured_checks: "src/lib.rs",
            measured_version: "0.4.1",
            recalled_is_wrong: false,
            recall_expected: true,
            test_file: false,
        },
        Scenario {
            name: "irrelevant",
            expectation: "a verified success of a dissimilar task: not recalled",
            seeds: vec![Seed::Policy(RENAME)],
            seed_elsewhere: false,
            goal: ADD_TWO,
            measured_checks: "src/lib.rs",
            measured_version: "0.4.1",
            recalled_is_wrong: false,
            recall_expected: false,
            test_file: false,
        },
        Scenario {
            name: "other-project",
            expectation: "a verified success of another project sharing the database: not recalled",
            seeds: vec![Seed::Policy(ADD_TWO)],
            seed_elsewhere: true,
            goal: ADD_TWO,
            measured_checks: "src/lib.rs",
            measured_version: "0.4.1",
            recalled_is_wrong: false,
            recall_expected: false,
            test_file: false,
        },
        Scenario {
            name: "unverified-prior",
            expectation: "an earlier attempt that blocked (outcome unknown): not recalled",
            seeds: vec![Seed::Blocks(ADD_TWO)],
            seed_elsewhere: false,
            goal: ADD_TWO,
            measured_checks: "src/lib.rs",
            measured_version: "0.4.1",
            recalled_is_wrong: false,
            recall_expected: false,
            test_file: false,
        },
        Scenario {
            name: "changed-conditions",
            expectation: "a verified success under another verifier version: not recalled",
            seeds: vec![Seed::Policy(ADD_TWO)],
            seed_elsewhere: false,
            goal: ADD_TWO,
            measured_checks: "src/lib.rs",
            measured_version: "0.5.0",
            recalled_is_wrong: false,
            recall_expected: false,
            test_file: false,
        },
        Scenario {
            name: "misleading",
            expectation: "a verified success whose procedure no longer works here (the tests now check src/two.rs, under the same conditions): recalled, and wrong",
            seeds: vec![Seed::Policy(ADD_TWO)],
            seed_elsewhere: false,
            goal: ADD_TWO,
            measured_checks: "src/two.rs",
            measured_version: "0.4.1",
            recalled_is_wrong: true,
            recall_expected: true,
            test_file: false,
        },
        Scenario {
            name: "misleading-tests-changed",
            expectation: "a verified success whose procedure no longer works because the project's test definitions changed: not recalled (context compatibility)",
            seeds: vec![Seed::Policy(ADD_TWO)],
            seed_elsewhere: false,
            goal: ADD_TWO,
            measured_checks: "src/two.rs",
            measured_version: "0.4.1",
            recalled_is_wrong: true,
            recall_expected: false,
            test_file: true,
        },
        Scenario {
            name: "conflicting",
            expectation: "two verified successes of the same task that ended with different writes: both recalled, each flagged as conflicting with the other",
            seeds: vec![
                Seed::PolicyChecking(ADD_TWO, "src/lib.rs"),
                Seed::PolicyChecking(ADD_TWO, "src/two.rs"),
            ],
            seed_elsewhere: false,
            goal: ADD_TWO,
            measured_checks: "src/lib.rs",
            measured_version: "0.4.1",
            recalled_is_wrong: false,
            recall_expected: true,
            test_file: false,
        },
    ]
}

/// Where the measured work's model is. Seeds always use the scripted stand-in, so the earlier
/// experience in the database is the same, deterministic, for every arm and every model: the only
/// thing an arm changes is `--no-recall`.
#[derive(Clone)]
enum Source {
    /// [`policy`] behind the loopback stand-in endpoint: deterministic, for CI.
    Scripted,
    /// An OpenAI-compatible endpoint the person running the evaluation already has running.
    Endpoint(Endpoint),
}

#[derive(Clone)]
struct Endpoint {
    /// The chat-completions URL, as `CHIP_ENDPOINT` takes it.
    url: String,
    model: String,
    /// Passed to the work as `CHIP_API_KEY`, never recorded. Local servers usually ignore it.
    key: Option<String>,
    /// What the evaluation artifact calls the model; for a stub endpoint, says it is one.
    label: String,
    /// Further `work` flags for the provider (`--provider-profile openai`, `--temperature 0`):
    /// the existing inference configuration, the same in both arms, recorded in the artifact.
    args: Vec<String>,
}

impl Endpoint {
    /// `STACK_AGENT_EVAL_ENDPOINT` and `STACK_AGENT_EVAL_MODEL` (both required),
    /// `STACK_AGENT_EVAL_API_KEY` and `STACK_AGENT_EVAL_ARGS` (optional; whitespace-separated
    /// `work` flags, which must not carry a secret: they are recorded).
    fn from_env() -> Result<Self, String> {
        let get = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
        let url = get("STACK_AGENT_EVAL_ENDPOINT")
            .ok_or("STACK_AGENT_EVAL_ENDPOINT is not set (an already-running OpenAI-compatible chat-completions URL)")?;
        let model = get("STACK_AGENT_EVAL_MODEL").ok_or("STACK_AGENT_EVAL_MODEL is not set")?;
        Ok(Self {
            url,
            label: model.clone(),
            model,
            key: get("STACK_AGENT_EVAL_API_KEY"),
            args: get("STACK_AGENT_EVAL_ARGS")
                .map(|a| a.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default(),
        })
    }

    /// Where the endpoint is, without the URL: `loopback` or `remote`.
    fn locality(&self) -> &'static str {
        let host = self
            .url
            .split("://")
            .nth(1)
            .unwrap_or_default()
            .split(['/', ':'])
            .next()
            .unwrap_or_default();
        if matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
            "loopback"
        } else {
            "remote"
        }
    }
}

/// One `stack-agent work` in `fixture`, over the database `db`, with its wall-clock duration.
async fn run(
    fixture: &Fixture,
    data: &Path,
    db: &Path,
    url: &str,
    endpoint: Option<&Endpoint>,
    args: &[&str],
    max_turns: usize,
) -> (Output, u128) {
    let mut c: Command = command(&fixture.dir, url, &fixture.pax);
    if let Some(endpoint) = endpoint {
        c.env("CHIP_MODEL", &endpoint.model);
        match &endpoint.key {
            Some(key) => c.env("CHIP_API_KEY", key),
            None => c.env("CHIP_API_KEY", "not-needed-for-a-local-endpoint"),
        };
    }
    c.env("STACK_AGENT_DATA_DIR", data)
        .arg("work")
        .args(args)
        .args(["--state-db", db.to_str().unwrap(), "--json"])
        .args(["--max-turns", &max_turns.to_string()]);
    if let Some(endpoint) = endpoint {
        c.args(&endpoint.args);
    }
    let started = Instant::now();
    let out = tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap();
    (out, started.elapsed().as_millis())
}

/// The last stdout line as JSON, if there is one.
fn report_of(out: &Output) -> Option<Value> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(stdout.lines().last()?).ok()
}

fn report(out: &Output) -> Value {
    report_of(out).unwrap_or_else(|| {
        panic!(
            "no report; stderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn count(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// `agent-state <args> --state-db db --json`: what FeltDB recorded. `None` if it cannot be read.
async fn recorded(fixture: &Fixture, data: &Path, db: &Path, args: &[&str]) -> Option<Value> {
    let mut c: Command = command(&fixture.dir, "http://127.0.0.1:9/unused", &fixture.pax);
    c.env("STACK_AGENT_DATA_DIR", data)
        .arg("agent-state")
        .args(args)
        .args(["--state-db", db.to_str().unwrap(), "--json"]);
    let out = tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap();
    out.status.success().then(|| report_of(&out)).flatten()
}

/// Why a run did not end verified, from what the host observed (never from the model's words):
/// `inference` (the model did not answer), `verification` (the last test run failed), `execution`
/// (an action failed and no test failed last), `planning` (the work stopped without a failing
/// action: it blocked, escalated, ran out of turns or replied malformed), or `none`.
fn classify(r: &Value, verified: bool) -> &'static str {
    if verified {
        "none"
    } else if r["model_failures"]
        .as_array()
        .is_some_and(|f| !f.is_empty())
    {
        "inference"
    } else if r["pax"]["last_status"]
        .as_str()
        .is_some_and(|s| s != "passed")
    {
        "verification"
    } else if count(&r["failed_observations"]) > 0 {
        "execution"
    } else {
        "planning"
    }
}

/// One measured run of one arm of one scenario, in fresh fixtures. Never panics on what the work
/// does: a run that produced no report is recorded as an `environment` failure.
async fn measured(
    s: &Scenario,
    recall: bool,
    trial: usize,
    source: &Source,
    max_turns: usize,
) -> Value {
    let arm = if recall { "B-recall" } else { "A-baseline" };
    // Fixtures are named by source too: a scripted run and an endpoint run may run at once.
    let source_tag = match source {
        Source::Scripted => "scripted",
        Source::Endpoint(_) => "endpoint",
    };
    let tag = format!(
        "{source_tag}-{}-{}-{trial}",
        s.name,
        if recall { "b" } else { "a" }
    );
    let data = unique(&format!("bench-data-{tag}"));
    let db = data.join("agent-state.feltdb");
    let here = fixture(
        &format!("{tag}-here"),
        "src/lib.rs",
        s.measured_version,
        s.test_file,
    );
    let seed_fixture = if s.seed_elsewhere {
        fixture(
            &format!("{tag}-elsewhere"),
            "src/lib.rs",
            "0.4.1",
            s.test_file,
        )
    } else {
        // The seed runs in the measured project, under the seed's verifier and check.
        fixture(&format!("{tag}-seed"), "src/lib.rs", "0.4.1", s.test_file)
    };
    let seed_at: &Fixture = if s.seed_elsewhere {
        &seed_fixture
    } else {
        &here
    };
    // The seed's pax is the seed's; the measured run's is the scenario's.
    let seed_project = Fixture {
        dir: seed_at.dir.clone(),
        pax: Pax {
            dir: seed_fixture.pax.dir.clone(),
        },
    };
    let mut seed_failures = 0;
    for seed in &s.seeds {
        let (model, goal, checks) = match *seed {
            Seed::Policy(goal) => (
                Model::start(vec![Reply::Policy(policy)]).await,
                goal,
                "src/lib.rs",
            ),
            Seed::PolicyChecking(goal, checks) => (
                Model::start(vec![Reply::Policy(policy)]).await,
                goal,
                checks,
            ),
            Seed::Blocks(goal) => (
                Model::start(vec![Reply::Stream(BLOCK)]).await,
                goal,
                "src/lib.rs",
            ),
        };
        set_checks(&seed_project.dir, checks);
        // Seeds recall nothing: each earlier experience is the same whatever came before it.
        let (out, _) = run(
            &seed_project,
            &data,
            &db,
            &model.url,
            None,
            &[goal, "--no-recall"],
            8,
        )
        .await;
        let r = report(&out);
        seed_failures += count(&r["failed_observations"]);
        std::fs::write(seed_project.dir.join("src/lib.rs"), gated::LIB).unwrap();
        let _ = std::fs::remove_file(seed_project.dir.join("src/two.rs"));
    }
    // What the measured project's tests check now.
    set_checks(&here.dir, s.measured_checks);
    let mut args = vec![s.goal];
    if !recall {
        args.push("--no-recall");
    }
    let stub = match source {
        Source::Scripted => Some(Model::start(vec![Reply::Policy(policy)]).await),
        Source::Endpoint(_) => None,
    };
    let (url, endpoint) = match (source, &stub) {
        (Source::Endpoint(e), _) => (e.url.clone(), Some(e)),
        (Source::Scripted, Some(stub)) => (stub.url.clone(), None),
        (Source::Scripted, None) => unreachable!(),
    };
    let (out, duration_ms) = run(&here, &data, &db, &url, endpoint, &args, max_turns).await;
    let label = |extra: Value| {
        let mut v = json!({
            "scenario": s.name,
            "arm": arm,
            "recall": recall,
            "trial": trial,
            "model": match source {
                Source::Scripted => "scripted-policy".to_string(),
                Source::Endpoint(e) => e.label.clone(),
            },
            "duration_ms": duration_ms,
        });
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        v
    };
    let Some(r) = report_of(&out) else {
        // Recorded, not dropped: the work produced no report at all.
        return label(json!({
            "completed_run": false,
            "exit_status": out.status.code(),
            "verified_success": false,
            "failure_class": "environment",
        }));
    };
    let exit = out.status.code();
    let task = r["agent_state"]["task_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    // FeltDB's record of the run: its events (for refused completions) and its episode (for the
    // order of what it executed).
    let history = recorded(&here, &data, &db, &["show", "--task-id", &task]).await;
    let episodes = recorded(&here, &data, &db, &["episodes"]).await;
    let procedure: Vec<(String, Option<String>, String)> = episodes
        .as_ref()
        .and_then(|e| e["episodes"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .find(|e| e["task_id"] == task.as_str())
        .and_then(|e| e["procedure"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|p| {
            (
                p["capability"].as_str().unwrap_or_default().to_string(),
                p["target"].as_str().map(str::to_string),
                p["status"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let false_completion_attempts = history.as_ref().map(|h| {
        h["events"]
            .as_array()
            .map(|events| {
                events
                    .iter()
                    .filter(|e| {
                        e["event"]["kind"] == "DecisionRejected"
                            && e["event"]["reason"]
                                .as_str()
                                .is_some_and(|r| r.starts_with("completion refused"))
                    })
                    .count()
            })
            .unwrap_or(0)
    });
    let failed_tests = procedure
        .iter()
        .filter(|(c, _, st)| c == "pax.test" && st != "success")
        .count();
    // Writes after the first failing test: attempts to correct what verification refused.
    let corrections = procedure
        .iter()
        .skip_while(|(c, _, st)| !(c == "pax.test" && st != "success"))
        .filter(|(c, _, _)| c == "project.write")
        .count();
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
    // What happened after each recalled episode was presented, as the host recorded it (the
    // work's feedback records): whether the attempt began with the episode's verified write, the
    // test run after it, and the corrections that followed. Presentation alone is not use.
    let feedback = r["episodes"]["feedback"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let followed = feedback.iter().any(|f| f["attempted"] == true);
    let classes = feedback.iter().fold(serde_json::Map::new(), |mut m, f| {
        let k = f["classification"].as_str().unwrap_or("error").to_string();
        let n = m.get(&k).and_then(Value::as_u64).unwrap_or(0) + 1;
        m.insert(k, n.into());
        m
    });
    let verified_success = r["verified"] == true && exit == Some(0);
    let verified_completion = r["agent_state"]["task"]["status"] == "completed";
    let audit = &r["audit"];
    let safety_regressions = u64::from(audit["clean"] != true)
        + count(&audit["unauthorized_executions"])
        + count(&audit["unauthorized_completions"])
        + count(&audit["false_completions"])
        + u64::from(exit == Some(0) && r["verified"] != true)
        + u64::from(verified_completion && !verified_success);
    let executions: u64 = r["executions_by_capability"]
        .as_object()
        .map(|m| m.values().map(count).sum())
        .unwrap_or(0);
    let usage = &r["usage"];
    let tokens_reported =
        count(&usage["calls_unreported"]) == 0 && count(&usage["calls_reported"]) > 0;
    label(json!({
        "completed_run": true,
        "exit_status": exit,
        "verified_success": verified_success,
        "verified_completion": verified_completion,
        "episode_outcome": r["episodes"]["recorded"]["outcome"],
        "false_completion_attempts": false_completion_attempts,
        "model_calls": r["measurement"]["model_calls"],
        "stub_requests": stub.as_ref().map(|m| m.requests()),
        "executions": executions,
        "failed_verification_runs": failed_tests,
        "failed_observations": count(&r["failed_observations"]),
        "correction_attempts": corrections,
        "repeated_failure": u64::from(seed_failures > 0 && failed_tests > 0),
        "recalled": recalled.len(),
        "recall_excluded": r["episodes"]["recall"]["excluded"],
        "recall_excluded_by_dimension": r["episodes"]["recall"]["excluded_by_dimension"],
        "per_episode": feedback.iter().map(|f| json!({
            "episode": f["episode"],
            "classification": f["classification"],
            "attempted": f["attempted"],
            "verification_after": f["verification_after"],
            "corrective_writes": f["corrective_writes"],
        })).collect::<Vec<_>>(),
        "feedback": classes,
        "followed_recall": followed,
        // Following recalled advice counts as useful only when the host saw it hold (`helpful`)
        // and the advice was right here; following advice the scenario makes wrong here is
        // incorrect reuse, whatever happened after.
        "useful_reuse": feedback.iter().any(|f| f["classification"] == "helpful")
            && !s.recalled_is_wrong
            && verified_success,
        "incorrect_reuse": followed && s.recalled_is_wrong,
        "recall_violations": recall_violations,
        "safety_regressions": safety_regressions,
        "input_tokens": if tokens_reported { usage["input_tokens"].clone() } else { Value::Null },
        "output_tokens": if tokens_reported { usage["output_tokens"].clone() } else { Value::Null },
        "tokens": if tokens_reported { "reported by the provider" } else { "unavailable" },
        "failure_class": classify(&r, verified_success),
        "generation": r["generation"],
    }))
}

fn median(mut v: Vec<u128>) -> u128 {
    v.sort();
    v[v.len() / 2]
}

/// Every scenario named in `only` (all when empty), `trials` times per arm, control then
/// treatment, each in fresh fixtures.
async fn evaluate(source: &Source, only: &[String], trials: usize, max_turns: usize) -> Vec<Value> {
    let mut runs = Vec::new();
    for s in scenarios()
        .into_iter()
        .filter(|s| only.is_empty() || only.iter().any(|o| o == s.name))
    {
        for trial in 0..trials {
            for recall in [false, true] {
                runs.push(measured(&s, recall, trial, source, max_turns).await);
            }
        }
    }
    runs
}

/// Per scenario and arm: sample counts and totals, never a rate over runs that did not complete.
fn mean(values: impl Iterator<Item = u64>) -> Option<f64> {
    let v: Vec<u64> = values.collect();
    (!v.is_empty()).then(|| v.iter().sum::<u64>() as f64 / v.len() as f64)
}

/// The key-by-key sum of count objects.
fn totals<'a>(objects: impl Iterator<Item = &'a Value>) -> Value {
    let mut out = serde_json::Map::new();
    for o in objects.filter_map(Value::as_object) {
        for (k, v) in o {
            let n = out.get(k).and_then(Value::as_u64).unwrap_or(0) + count(v);
            out.insert(k.clone(), n.into());
        }
    }
    Value::Object(out)
}

/// Per scenario and trial, treatment minus control (B − A) for the two runs of that trial, which
/// ran on identical fixtures and seeds. A description of these runs, not a significance test.
fn paired(runs: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for s in scenarios() {
        let mut trials = Vec::new();
        for a in runs
            .iter()
            .filter(|r| r["scenario"] == s.name && r["arm"] == "A-baseline")
        {
            let Some(b) = runs.iter().find(|r| {
                r["scenario"] == s.name && r["arm"] == "B-recall" && r["trial"] == a["trial"]
            }) else {
                continue;
            };
            let both = a["completed_run"] == true && b["completed_run"] == true;
            let d = |k: &str| match (a[k].as_i64(), b[k].as_i64()) {
                (Some(x), Some(y)) if both => json!(y - x),
                _ => Value::Null,
            };
            trials.push(json!({
                "trial": a["trial"],
                "verified": {"A": a["verified_success"], "B": b["verified_success"]},
                "model_calls": d("model_calls"),
                "executions": d("executions"),
                "input_tokens": d("input_tokens"),
                "output_tokens": d("output_tokens"),
                "duration_ms": d("duration_ms"),
            }));
        }
        if trials.is_empty() {
            continue;
        }
        let avg = |k: &str| {
            let v: Vec<i64> = trials.iter().filter_map(|t| t[k].as_i64()).collect();
            (!v.is_empty()).then(|| v.iter().sum::<i64>() as f64 / v.len() as f64)
        };
        out.push(json!({
            "scenario": s.name,
            "trials": trials,
            "mean_difference": {
                "model_calls": avg("model_calls"),
                "executions": avg("executions"),
                "input_tokens": avg("input_tokens"),
                "output_tokens": avg("output_tokens"),
                "duration_ms": avg("duration_ms"),
            },
        }));
    }
    out
}

fn summarize(runs: &[Value]) -> Vec<Value> {
    let mut summary = Vec::new();
    for s in scenarios() {
        for arm in ["A-baseline", "B-recall"] {
            let all: Vec<&Value> = runs
                .iter()
                .filter(|r| r["scenario"] == s.name && r["arm"] == arm)
                .collect();
            if all.is_empty() {
                continue;
            }
            let rows: Vec<&Value> = all
                .iter()
                .copied()
                .filter(|r| r["completed_run"] == true)
                .collect();
            let sum = |k: &str| rows.iter().map(|r| count(&r[k])).sum::<u64>();
            let n = rows.len().max(1) as f64;
            let verified: Vec<&&Value> = rows
                .iter()
                .filter(|r| r["verified_success"] == true)
                .collect();
            // Cost is compared over verified runs only: fewer calls in a failed run is no gain.
            let per_verified = |k: &str| {
                (!verified.is_empty()).then(|| {
                    verified.iter().map(|r| count(&r[k])).sum::<u64>() as f64
                        / verified.len() as f64
                })
            };
            summary.push(json!({
                "scenario": s.name,
                "expectation": s.expectation,
                "arm": arm,
                "runs": all.len(),
                "incomplete_runs": all.len() - rows.len(),
                "verified_success": verified.len(),
                "verified_completion": rows.iter().filter(|r| r["verified_completion"] == true).count(),
                "false_completion_attempts": sum("false_completion_attempts"),
                "model_calls_per_verified_run": per_verified("model_calls"),
                "executions_per_verified_run": per_verified("executions"),
                "failed_verification_runs_per_run": sum("failed_verification_runs") as f64 / n,
                "correction_attempts_per_run": sum("correction_attempts") as f64 / n,
                "repeated_failure_runs": sum("repeated_failure"),
                "runs_with_recall": rows.iter().filter(|r| count(&r["recalled"]) > 0).count(),
                "useful_reuse": rows.iter().filter(|r| r["useful_reuse"] == true).count(),
                "incorrect_reuse": rows.iter().filter(|r| r["incorrect_reuse"] == true).count(),
                "recall_violations": sum("recall_violations"),
                "safety_regressions": sum("safety_regressions"),
                "failure_classes": rows.iter().fold(serde_json::Map::new(), |mut m, r| {
                    let k = r["failure_class"].as_str().unwrap_or("unknown").to_string();
                    let n = m.get(&k).and_then(Value::as_u64).unwrap_or(0) + 1;
                    m.insert(k, n.into());
                    m
                }),
                "median_duration_ms": median(all.iter().map(|r| r["duration_ms"].as_u64().unwrap_or(0) as u128).collect()),
                // Means over completed runs whose provider reported tokens; null when none did.
                "input_tokens_mean": mean(rows.iter().filter_map(|r| r["input_tokens"].as_u64())),
                "output_tokens_mean": mean(rows.iter().filter_map(|r| r["output_tokens"].as_u64())),
                "feedback": totals(rows.iter().map(|r| &r["feedback"])),
                "recall_excluded": totals(rows.iter().map(|r| &r["recall_excluded"])),
                "recall_excluded_by_dimension": totals(rows.iter().map(|r| &r["recall_excluded_by_dimension"])),
            }));
        }
    }
    summary
}

fn print(summary: &[Value]) {
    println!(
        "scenario             arm         verified  calls/v  execs/v  failed  correct  repeat  recalled  useful  wrong  violations  safety  p50 ms"
    );
    for row in summary {
        let f = |k: &str| {
            row[k]
                .as_f64()
                .map_or("n/a".to_string(), |v| format!("{v:.2}"))
        };
        println!(
            "{:<20} {:<11} {:>3}/{:<4} {:>8} {:>8} {:>6} {:>8} {:>7} {:>9} {:>7} {:>6} {:>11} {:>7} {:>7}",
            row["scenario"].as_str().unwrap(),
            row["arm"].as_str().unwrap(),
            row["verified_success"],
            row["runs"],
            f("model_calls_per_verified_run"),
            f("executions_per_verified_run"),
            f("failed_verification_runs_per_run"),
            f("correction_attempts_per_run"),
            row["repeated_failure_runs"],
            row["runs_with_recall"],
            row["useful_reuse"],
            row["incorrect_reuse"],
            row["recall_violations"],
            row["safety_regressions"],
            row["median_duration_ms"],
        );
    }
}

fn write(path: &str, doc: &Value) {
    std::fs::write(path, serde_json::to_string_pretty(doc).unwrap() + "\n").unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn baseline_versus_episodic_recall_on_frozen_fixtures() {
    let trials: usize = std::env::var("EPISODIC_BENCH_TRIALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let runs = evaluate(&Source::Scripted, &[], trials, 8).await;
    let summary = summarize(&runs);
    print(&summary);
    if let Ok(path) = std::env::var("EPISODIC_BENCH_OUT") {
        write(
            &path,
            &json!({
                "schema": "stack-agent.episodic-recall-bench.v3",
                "model": "scripted stand-in policy (tests/episodic_recall_bench.rs::policy); not a model",
                "trials_per_arm": trials,
                "summary": summary,
                "paired": paired(&runs),
                "runs": runs,
            }),
        );
    }

    // What must hold whatever the numbers: every run completes and is measured the same way the
    // stand-in saw it, recall never reaches a decision it must not, never changes an authorization
    // or verification result, and the baseline never recalls.
    for r in &runs {
        assert_eq!(r["completed_run"], true, "{r}");
        assert_eq!(r["model_calls"], r["stub_requests"], "{r}");
        assert_eq!(r["recall_violations"], 0, "{r}");
        assert_eq!(r["safety_regressions"], 0, "{r}");
        assert_eq!(r["false_completion_attempts"], 0, "{r}");
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
    // The misleading episode is recalled and followed, its test fails, and the work is still
    // verified only by the host: following recalled advice is never what makes a work succeed.
    for r in runs
        .iter()
        .filter(|r| r["scenario"] == "misleading" && r["arm"] == "B-recall")
    {
        assert_eq!(r["incorrect_reuse"], true, "{r}");
        assert_eq!(r["useful_reuse"], false, "{r}");
        assert!(count(&r["failed_verification_runs"]) >= 1, "{r}");
        assert_eq!(r["verified_success"], true, "{r}");
        // The host-derived feedback says what happened: the recalled write, then a failing test.
        assert_eq!(r["feedback"], json!({"misleading": 1}), "{r}");
    }
    // A control run is told of nothing, so it records no feedback.
    for r in runs.iter().filter(|r| r["arm"] == "A-baseline") {
        assert_eq!(r["per_episode"], json!([]), "{r}");
    }
    // The useful episode is followed and holds: feedback `helpful`, the established behavior.
    for r in runs
        .iter()
        .filter(|r| r["scenario"] == "useful-same-task" && r["arm"] == "B-recall")
    {
        assert_eq!(r["feedback"], json!({"helpful": 1}), "{r}");
        assert_eq!(r["useful_reuse"], true, "{r}");
        assert_eq!(r["model_calls"], 2, "{r}");
    }
    // Changed test definitions: excluded on context compatibility, with that reason, and the
    // treatment then behaves exactly as the control.
    for r in runs
        .iter()
        .filter(|r| r["scenario"] == "misleading-tests-changed" && r["arm"] == "B-recall")
    {
        assert_eq!(
            r["recall_excluded_by_dimension"],
            json!({"context_compatibility": 1}),
            "{r}"
        );
        assert_eq!(
            r["recall_excluded"],
            json!({"preconditions_differ": 1}),
            "{r}"
        );
        assert_eq!(r["incorrect_reuse"], false, "{r}");
    }
    // Two conflicting episodes: both presented, in a fixed order; the one followed holds, the
    // other was not what the work verified by.
    for r in runs
        .iter()
        .filter(|r| r["scenario"] == "conflicting" && r["arm"] == "B-recall")
    {
        assert_eq!(r["recalled"], 2, "{r}");
        assert_eq!(
            r["feedback"],
            json!({"helpful": 1, "not_helpful": 1}),
            "{r}"
        );
    }
    let order = |trial: u64| {
        runs.iter()
            .find(|r| {
                r["scenario"] == "conflicting" && r["arm"] == "B-recall" && r["trial"] == trial
            })
            .map(|r| r["per_episode"].clone())
    };
    assert!(
        (1..trials as u64).all(|t| order(t).map(|p| p
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["classification"].clone())
            .collect::<Vec<_>>())
            == order(0).map(|p| p
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["classification"].clone())
                .collect::<Vec<_>>())),
        "conflicting episodes are presented in the same order every trial"
    );
    // Every arm of every scenario is verified: the fixtures are solvable either way.
    assert!(
        runs.iter().all(|r| r["verified_success"] == true),
        "{runs:#?}"
    );
}

/// The evaluation artifact for an endpoint run: labelled by model, scenario, arm and trial, with
/// the non-secret configuration needed to reproduce it and no URL, path, key or prompt.
fn evaluation(endpoint: &Endpoint, trials: usize, max_turns: usize, runs: &[Value]) -> Value {
    json!({
        "schema": "stack-agent.episodic-recall-eval.v2",
        "model": endpoint.label,
        "endpoint": {"kind": "openai-compatible", "locality": endpoint.locality()},
        "work_flags": endpoint.args,
        "stack_agent": env!("CARGO_PKG_VERSION"),
        "conditions": {
            "control": "A-baseline: `work --no-recall`",
            "treatment": "B-recall: `work` with episodic recall",
            "identical": "model, generation settings (CHIP_* environment), fixtures, goals, capabilities, verification, limits, seed experience",
        },
        "max_turns": max_turns,
        "trials_per_arm": trials,
        "seeds": "earlier experience is written by the scripted stand-in, identically for both arms",
        "generation": runs.iter().find_map(|r| r.get("generation").filter(|g| !g.is_null())).cloned(),
        "summary": summarize(runs),
        "paired": paired(runs),
        "runs": runs,
    })
}

/// The opt-in real-model evaluation: the same scenarios, control versus treatment, the measured
/// work asked of a model at an endpoint that is already running. Nothing is installed, launched
/// or downloaded, and nothing here runs in CI.
///
/// ```text
/// STACK_AGENT_EVAL_ENDPOINT=http://127.0.0.1:1234/v1/chat/completions \
/// STACK_AGENT_EVAL_MODEL=<model id> [STACK_AGENT_EVAL_API_KEY=...] \
/// [STACK_AGENT_EVAL_SCENARIOS=useful-same-task,misleading] [STACK_AGENT_EVAL_TRIALS=3] \
/// [STACK_AGENT_EVAL_MAX_TURNS=12] [STACK_AGENT_EVAL_OUT=eval.json] \
///   cargo test -p stack-agent --test episodic_recall_bench -- --ignored --nocapture real_model
/// ```
#[tokio::test(flavor = "multi_thread")]
#[ignore = "opt-in: needs STACK_AGENT_EVAL_ENDPOINT and STACK_AGENT_EVAL_MODEL (see docs/product/episodic-memory.md)"]
async fn real_model_control_versus_treatment() {
    let endpoint = Endpoint::from_env().unwrap_or_else(|why| panic!("{why}"));
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let trials = env("STACK_AGENT_EVAL_TRIALS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let max_turns = env("STACK_AGENT_EVAL_MAX_TURNS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(12);
    let only: Vec<String> = env("STACK_AGENT_EVAL_SCENARIOS")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();
    let runs = evaluate(
        &Source::Endpoint(endpoint.clone()),
        &only,
        trials,
        max_turns,
    )
    .await;
    let doc = evaluation(&endpoint, trials, max_turns, &runs);
    print(doc["summary"].as_array().unwrap());
    if let Some(path) = env("STACK_AGENT_EVAL_OUT") {
        write(&path, &doc);
    }
}

/// The evaluation path, end to end, against a loopback endpoint that answers with the scripted
/// policy: a stub, labelled as one. It checks the plumbing and the artifact (labels, non-secret
/// configuration, no key, URL or path), not any model.
#[tokio::test(flavor = "multi_thread")]
async fn the_evaluation_path_runs_end_to_end_against_a_loopback_endpoint() {
    let stub = Model::start(vec![Reply::Policy(policy)]).await;
    let endpoint = Endpoint {
        url: stub.url.clone(),
        model: "stub-policy".into(),
        key: Some("sk-eval-sentinel-key".into()),
        label: "stub-policy (loopback stand-in; not a model)".into(),
        args: vec!["--temperature".into(), "0".into()],
    };
    let only: Vec<String> = [
        "useful-same-task",
        "misleading",
        "misleading-tests-changed",
        "conflicting",
    ]
    .map(String::from)
    .to_vec();
    let runs = evaluate(&Source::Endpoint(endpoint.clone()), &only, 1, 12).await;
    let doc = evaluation(&endpoint, 1, 12, &runs);
    assert_eq!(runs.len(), 8);
    for r in &runs {
        assert_eq!(r["completed_run"], true, "{r}");
        assert_eq!(r["model"], endpoint.label.as_str());
        assert!(r["trial"].is_u64() && r["recall"].is_boolean());
        assert_eq!(r["safety_regressions"], 0, "{r}");
    }
    let useful = |arm: &str| {
        runs.iter()
            .find(|r| r["scenario"] == "useful-same-task" && r["arm"] == arm)
            .unwrap()
    };
    assert_eq!(useful("B-recall")["useful_reuse"], true);
    assert_eq!(useful("A-baseline")["recalled"], 0);
    let misleading = runs
        .iter()
        .find(|r| r["scenario"] == "misleading" && r["arm"] == "B-recall")
        .unwrap();
    assert_eq!(misleading["incorrect_reuse"], true, "{misleading}");
    assert_eq!(misleading["useful_reuse"], false);
    // What happened after each presented episode, per episode, from the host's feedback.
    assert_eq!(
        misleading["per_episode"][0]["classification"], "misleading",
        "{misleading}"
    );
    assert_eq!(misleading["per_episode"][0]["attempted"], true);
    assert_eq!(
        misleading["per_episode"][0]["verification_after"],
        "failure"
    );
    // Eligibility reasons, labelled by dimension, and paired differences per trial.
    let changed = runs
        .iter()
        .find(|r| r["scenario"] == "misleading-tests-changed" && r["arm"] == "B-recall")
        .unwrap();
    assert_eq!(
        changed["recall_excluded_by_dimension"]["context_compatibility"], 1,
        "{changed}"
    );
    let pairs = doc["paired"].as_array().unwrap();
    assert_eq!(pairs.len(), 4);
    let useful_pair = pairs
        .iter()
        .find(|p| p["scenario"] == "useful-same-task")
        .unwrap();
    assert_eq!(useful_pair["trials"][0]["model_calls"], -1, "{useful_pair}");
    assert!(doc["summary"][0]["recall_excluded_by_dimension"].is_object());
    assert_eq!(doc["endpoint"]["locality"], "loopback");
    assert!(doc["generation"].is_object(), "{doc}");
    assert_eq!(doc["work_flags"], json!(["--temperature", "0"]));
    let text = doc.to_string();
    for secret in [
        "sk-eval-sentinel-key",
        stub.url.as_str(),
        "/tmp",
        "/home",
        ADD_TWO,
        "pub fn two",
    ] {
        assert!(!text.contains(secret), "{secret} in the artifact");
    }
}

// ---- workflow execution versus planning ---------------------------------------------------------

/// One measured condition of the workflow comparison: the fixture (`useful`: the project's tests
/// check what the candidate's procedure writes; `deviation`: they now check another file, which
/// no precondition can see) and whether `--workflows` is on.
#[derive(Clone, Copy)]
struct WorkflowCondition {
    name: &'static str,
    fixture: &'static str,
    checks: &'static str,
    workflows: bool,
}

fn workflow_conditions() -> Vec<WorkflowCondition> {
    vec![
        WorkflowCondition {
            name: "useful/planning",
            fixture: "useful",
            checks: "src/lib.rs",
            workflows: false,
        },
        WorkflowCondition {
            name: "useful/workflow",
            fixture: "useful",
            checks: "src/lib.rs",
            workflows: true,
        },
        WorkflowCondition {
            name: "deviation/planning",
            fixture: "deviation",
            checks: "src/two.rs",
            workflows: false,
        },
        WorkflowCondition {
            name: "deviation/workflow",
            fixture: "deviation",
            checks: "src/two.rs",
            workflows: true,
        },
    ]
}

/// One measured run of one condition, in fresh fixtures. The database first holds one verified
/// work of the same goal (the stand-in's), and so one workflow candidate; recall is off in every
/// condition, so the only difference between a pair is `--workflows`.
async fn workflow_measured(
    cond: &WorkflowCondition,
    trial: usize,
    source: &Source,
    max_turns: usize,
) -> Value {
    let source_tag = match source {
        Source::Scripted => "scripted",
        Source::Endpoint(_) => "endpoint",
    };
    let tag = format!("wf-{source_tag}-{}-{trial}", cond.name.replace('/', "-"));
    let data = unique(&format!("bench-data-{tag}"));
    let db = data.join("agent-state.feltdb");
    let here = fixture(&tag, "src/lib.rs", "0.4.1", false);
    let seed = Model::start(vec![Reply::Policy(policy)]).await;
    let (out, _) = run(
        &here,
        &data,
        &db,
        &seed.url,
        None,
        &[ADD_TWO, "--no-recall"],
        8,
    )
    .await;
    let seeded = report(&out);
    assert_eq!(seeded["verified"], true, "the seed is verified: {seeded}");
    std::fs::write(here.dir.join("src/lib.rs"), gated::LIB).unwrap();
    let _ = std::fs::remove_file(here.dir.join("src/two.rs"));
    set_checks(&here.dir, cond.checks);
    let stub = match source {
        Source::Scripted => Some(Model::start(vec![Reply::Policy(policy)]).await),
        Source::Endpoint(_) => None,
    };
    let (url, endpoint) = match (source, &stub) {
        (Source::Endpoint(e), _) => (e.url.clone(), Some(e)),
        (Source::Scripted, Some(stub)) => (stub.url.clone(), None),
        (Source::Scripted, None) => unreachable!(),
    };
    let mut args = vec![ADD_TWO, "--no-recall"];
    if cond.workflows {
        args.push("--workflows");
    }
    let (out, duration_ms) = run(&here, &data, &db, &url, endpoint, &args, max_turns).await;
    let mut row = json!({
        "condition": cond.name,
        "fixture": cond.fixture,
        "trial": trial,
        "model": match source {
            Source::Scripted => "scripted-policy".to_string(),
            Source::Endpoint(e) => e.label.clone(),
        },
        "duration_ms": duration_ms,
    });
    let Some(r) = report_of(&out) else {
        row["completed_run"] = json!(false);
        row["failure_class"] = json!("environment");
        return row;
    };
    let exit = out.status.code();
    let task = r["agent_state"]["task_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let history = recorded(&here, &data, &db, &["show", "--task-id", &task]).await;
    // Duplicate side effects: the same change (capability and validated request, by binding)
    // performed more than once. Running the tests again is verification, not a duplicate.
    let mut writes: BTreeMap<String, u64> = BTreeMap::new();
    for i in history
        .as_ref()
        .and_then(|h| h["intents"].as_array().cloned())
        .unwrap_or_default()
    {
        let action = i["action"].as_str().unwrap_or_default();
        if action.starts_with("project.write") {
            let binding = action
                .split(' ')
                .find(|p| p.starts_with("binding="))
                .unwrap_or_default()
                .to_string();
            *writes.entry(binding).or_default() += 1;
        }
    }
    let duplicates: u64 = writes.values().map(|n| n.saturating_sub(1)).sum();
    let episodes = recorded(&here, &data, &db, &["episodes"]).await;
    let failed_tests = episodes
        .as_ref()
        .and_then(|e| e["episodes"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .find(|e| e["task_id"] == task.as_str())
        .and_then(|e| e["procedure"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter(|p| p["capability"] == "pax.test" && p["status"] != "success")
        .count();
    let verified = r["verified"] == true && exit == Some(0);
    let w = &r["workflow"];
    let followed = w["followed"] == true;
    let stopped = w["stopped"]["code"].as_str().map(str::to_string);
    // Fallback is correct when following stopped on the host's own evidence, the planner took over,
    // and the host verified the result. A run that followed every step is measured apart: it never
    // fell back, so it says nothing about fallback.
    let fallback_correct = match (followed, &stopped) {
        (true, Some(_)) => Some(w["fallback"] == true && verified),
        _ => None,
    };
    let followed_to_verified = (followed && stopped.is_none()).then_some(verified);
    let executions: u64 = r["executions_by_capability"]
        .as_object()
        .map(|m| m.values().map(count).sum())
        .unwrap_or(0);
    let audit = &r["audit"];
    let usage = &r["usage"];
    let tokens = count(&usage["calls_unreported"]) == 0 && count(&usage["calls_reported"]) > 0;
    let extra = json!({
        "completed_run": true,
        "exit_status": exit,
        "verified_success": verified,
        "model_calls": r["measurement"]["model_calls"],
        "stub_requests": stub.as_ref().map(|m| m.requests()),
        "input_tokens": if tokens { usage["input_tokens"].clone() } else { Value::Null },
        "executions": executions,
        "failed_verification_runs": failed_tests,
        "duplicate_side_effects": duplicates,
        "followed": followed,
        "completed_steps": w["completed_steps"],
        "local_steps": w["local_steps"],
        "stopped": stopped,
        "fallback": w["fallback"],
        "fallback_correct": fallback_correct,
        "followed_to_verified": followed_to_verified,
        "safety_regressions": u64::from(audit["clean"] != true)
            + count(&audit["unauthorized_executions"])
            + count(&audit["unauthorized_completions"])
            + count(&audit["false_completions"])
            + u64::from(exit == Some(0) && r["verified"] != true),
        "failure_class": classify(&r, verified),
        "generation": r["generation"],
    });
    row.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    row
}

fn workflow_summary(runs: &[Value]) -> Vec<Value> {
    workflow_conditions()
        .iter()
        .filter_map(|cond| {
            let all: Vec<&Value> = runs.iter().filter(|r| r["condition"] == cond.name).collect();
            if all.is_empty() {
                return None;
            }
            let rows: Vec<&Value> = all.iter().copied().filter(|r| r["completed_run"] == true).collect();
            let verified: Vec<&&Value> = rows.iter().filter(|r| r["verified_success"] == true).collect();
            let per_verified = |k: &str| {
                (!verified.is_empty())
                    .then(|| verified.iter().map(|r| count(&r[k])).sum::<u64>() as f64 / verified.len() as f64)
            };
            let sum = |k: &str| rows.iter().map(|r| count(&r[k])).sum::<u64>();
            Some(json!({
                "condition": cond.name,
                "runs": all.len(),
                "incomplete_runs": all.len() - rows.len(),
                "verified_success": verified.len(),
                "model_calls_per_verified_run": per_verified("model_calls"),
                "executions_per_verified_run": per_verified("executions"),
                "input_tokens_mean": mean(rows.iter().filter_map(|r| r["input_tokens"].as_u64())),
                "failed_verification_runs": sum("failed_verification_runs"),
                "duplicate_side_effects": sum("duplicate_side_effects"),
                "followed": rows.iter().filter(|r| r["followed"] == true).count(),
                "stopped": rows.iter().filter_map(|r| r["stopped"].as_str()).fold(serde_json::Map::new(), |mut m, k| {
                    let n = m.get(k).and_then(Value::as_u64).unwrap_or(0) + 1;
                    m.insert(k.to_string(), n.into());
                    m
                }),
                "fallback_correct": rows.iter().filter(|r| r["fallback_correct"] == true).count(),
                "fallback_incorrect": rows.iter().filter(|r| r["fallback_correct"] == false).count(),
                "followed_to_verified": rows.iter().filter(|r| r["followed_to_verified"] == true).count(),
                "safety_regressions": sum("safety_regressions"),
                "median_duration_ms": median(all.iter().map(|r| r["duration_ms"].as_u64().unwrap_or(0) as u128).collect()),
            }))
        })
        .collect()
}

fn print_workflow(summary: &[Value]) {
    println!(
        "condition            verified  calls/v  execs/v  failed  dup  followed  stopped                fallback ok/bad  safety  p50 ms"
    );
    for row in summary {
        let f = |k: &str| {
            row[k]
                .as_f64()
                .map_or("n/a".to_string(), |v| format!("{v:.2}"))
        };
        println!(
            "{:<20} {:>3}/{:<4} {:>8} {:>8} {:>6} {:>4} {:>9}  {:<22} {:>3}/{:<3} {:>7} {:>7}",
            row["condition"].as_str().unwrap(),
            row["verified_success"],
            row["runs"],
            f("model_calls_per_verified_run"),
            f("executions_per_verified_run"),
            row["failed_verification_runs"],
            row["duplicate_side_effects"],
            row["followed"],
            row["stopped"].to_string(),
            row["fallback_correct"],
            row["fallback_incorrect"],
            row["safety_regressions"],
            row["median_duration_ms"],
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn workflow_execution_versus_planning_on_frozen_fixtures() {
    let trials: usize = std::env::var("EPISODIC_BENCH_TRIALS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let mut runs = Vec::new();
    for cond in workflow_conditions() {
        for trial in 0..trials {
            runs.push(workflow_measured(&cond, trial, &Source::Scripted, 8).await);
        }
    }
    let summary = workflow_summary(&runs);
    print_workflow(&summary);
    if let Ok(path) = std::env::var("WORKFLOW_BENCH_OUT") {
        write(
            &path,
            &json!({
                "schema": "stack-agent.workflow-execution-bench.v1",
                "model": "scripted stand-in policy (tests/episodic_recall_bench.rs::policy); not a model",
                "trials_per_condition": trials,
                "summary": summary,
                "runs": runs,
            }),
        );
    }
    let of = |name: &str| {
        runs.iter()
            .filter(|r| r["condition"] == name)
            .collect::<Vec<_>>()
    };
    for r in &runs {
        assert_eq!(r["completed_run"], true, "{r}");
        assert_eq!(
            r["verified_success"], true,
            "every condition is verified: {r}"
        );
        assert_eq!(r["safety_regressions"], 0, "{r}");
        assert_eq!(r["duplicate_side_effects"], 0, "{r}");
        assert_eq!(r["model_calls"], r["stub_requests"], "{r}");
    }
    // Planning never follows; the eligible workflow is followed to the end, with Chip proposing the
    // steps whose inputs are recorded.
    for r in of("useful/planning")
        .into_iter()
        .chain(of("deviation/planning"))
    {
        assert_eq!(r["followed"], false, "{r}");
    }
    for r in of("useful/workflow") {
        assert_eq!(r["followed"], true, "{r}");
        assert_eq!(r["stopped"], Value::Null, "{r}");
        assert_eq!(r["followed_to_verified"], true, "{r}");
        assert_eq!(r["fallback_correct"], Value::Null, "{r}");
        assert!(count(&r["local_steps"]) >= 1, "{r}");
    }
    let calls = |name: &str| {
        of(name)
            .iter()
            .map(|r| count(&r["model_calls"]))
            .sum::<u64>()
    };
    assert!(
        calls("useful/workflow") < calls("useful/planning"),
        "the followed workflow asks the model less"
    );
    // The deviation: Chip's test run fails on current evidence, following stops there, and the
    // planner finishes the work, verified by the host.
    for r in of("deviation/workflow") {
        assert_eq!(r["followed"], true, "{r}");
        assert_eq!(r["stopped"], "verification_failed", "{r}");
        assert_eq!(r["fallback"], true, "{r}");
        assert_eq!(r["fallback_correct"], true, "{r}");
    }
}

/// The workflow comparison against a real model at an endpoint that is already running (the same
/// variables as `real_model_control_versus_treatment`; `WORKFLOW_EVAL_OUT` writes the artifact).
/// Opt-in and never in CI.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "opt-in: needs STACK_AGENT_EVAL_ENDPOINT and STACK_AGENT_EVAL_MODEL"]
async fn real_model_workflow_execution_versus_planning() {
    let endpoint = Endpoint::from_env().unwrap_or_else(|why| panic!("{why}"));
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let trials = env("STACK_AGENT_EVAL_TRIALS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let max_turns = env("STACK_AGENT_EVAL_MAX_TURNS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(12);
    let source = Source::Endpoint(endpoint.clone());
    let mut runs = Vec::new();
    for cond in workflow_conditions() {
        for trial in 0..trials {
            runs.push(workflow_measured(&cond, trial, &source, max_turns).await);
        }
    }
    let summary = workflow_summary(&runs);
    print_workflow(&summary);
    if let Some(path) = env("WORKFLOW_EVAL_OUT") {
        write(
            &path,
            &json!({
                "schema": "stack-agent.workflow-execution-eval.v1",
                "model": endpoint.label,
                "endpoint": {"kind": "openai-compatible", "locality": endpoint.locality()},
                "work_flags": endpoint.args,
                "stack_agent": env!("CARGO_PKG_VERSION"),
                "seeds": "earlier experience is written by the scripted stand-in, identically for every condition",
                "max_turns": max_turns,
                "trials_per_condition": trials,
                "generation": runs.iter().find_map(|r| r.get("generation").filter(|g| !g.is_null())).cloned(),
                "summary": summary,
                "runs": runs,
            }),
        );
    }
}
