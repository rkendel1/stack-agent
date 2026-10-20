//! Learning through the real binary: every outcome class recorded from host evidence, retrieval on
//! later runs, scope isolation, the feedback into `--auto` (and none without it), forged records,
//! storage failures, and that learning adds no model request. A scripted model and a stand-in
//! `pax`; no network, no key.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::path::{Path, PathBuf};
use std::process::Output;

use gated::{FAILED, Model, PASSED, Pax, Reply, command, private_runtime_dir, project};
use serde_json::Value;

const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;
const CLAIM: &str = r#"{"decision":"complete","summary":"Done, all tests pass."}"#;
const FORGE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":".stack-agent/learning/runs/forged.json","content":"{}"}}"#;
const DEDUP: &str = "change.cargo-verified@1";
const FULL: &str = "change.cargo-verified-full-context@1";
const SENTINEL: &str = "sk-SENTINEL-learning-0123456789";

/// A write that really changes the file in run `n` of the same project.
fn write(n: usize) -> &'static str {
    Box::leak(
        format!(
            r#"{{"decision":"request_capability","capability":"project.write","inputs":{{"path":"src/lib.rs","content":"pub fn one() -> u8 {{ 1 }}\npub fn two() -> u8 {{ {n} }}\n"}}}}"#
        )
        .into_boxed_str(),
    )
}

/// (tag, model script, PAX result, extra arguments, expected outcome).
type Case = (
    &'static str,
    Vec<Reply>,
    &'static str,
    Vec<&'static str>,
    &'static str,
);

struct Run {
    out: Output,
    requests: usize,
    json: Value,
}

async fn work(dir: &Path, runtime: &Path, script: Vec<Reply>, pax: &Pax, args: &[&str]) -> Run {
    let model = Model::start(script).await;
    let mut c = command(dir, &model.url, pax);
    c.arg("work")
        .arg("--json")
        .args(args)
        .env("STACK_AGENT_DATA_DIR", runtime)
        .env("CHIP_API_KEY", SENTINEL);
    let out = tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    Run {
        out,
        requests: model.requests(),
        json,
    }
}

fn learning(v: &Value) -> &Value {
    &v["learning"]
}

fn written<'a>(v: &'a Value, scope: &str) -> &'a Value {
    &learning(v)[scope]["written"]
}

fn read_doc(dir: &Path, runtime: &Path, v: &Value, scope: &str) -> Value {
    let path = written(v, scope)["path"]
        .as_str()
        .unwrap_or_else(|| panic!("{scope} not written: {v}"));
    let file = match scope {
        "project" => dir.join(path),
        _ => {
            let rel = path.strip_prefix("<runtime data>/").unwrap();
            runtime.join(rel)
        }
    };
    serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap()
}

fn all_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn each_outcome_class_is_recorded_from_host_evidence_in_both_scopes() {
    let cases: Vec<Case> = vec![
        (
            "success",
            vec![Reply::Stream(WRITE), Reply::Stream(TEST)],
            PASSED,
            vec![],
            "verified_success",
        ),
        (
            "failure",
            vec![
                Reply::Stream(WRITE),
                Reply::Stream(TEST),
                Reply::Stream(BLOCK),
            ],
            FAILED,
            vec![],
            "verified_failure",
        ),
        (
            "blocked",
            vec![Reply::Stream(BLOCK)],
            PASSED,
            vec![],
            "blocked",
        ),
        (
            "limit",
            vec![Reply::Stream(READ)],
            PASSED,
            vec!["--max-turns", "1"],
            "unverified",
        ),
        // The model claims success; the host does not believe it.
        (
            "claim",
            vec![
                Reply::Stream(WRITE),
                Reply::Stream(CLAIM),
                Reply::Stream(BLOCK),
            ],
            PASSED,
            vec![],
            "blocked",
        ),
    ];
    for (tag, script, result, extra, expected) in cases {
        let dir = project(&format!("learn-{tag}"));
        let runtime = private_runtime_dir();
        let pax = Pax::new(&format!("learn-{tag}"), result, false);
        let mut args = extra.clone();
        args.push("Add `two`.");
        let r = work(&dir, &runtime, script, &pax, &args).await;
        let p = read_doc(&dir, &runtime, &r.json, "project");
        let rt = read_doc(&dir, &runtime, &r.json, "runtime");
        assert_eq!(p["outcome"], expected, "{tag}: {p}");
        assert_eq!(rt["outcome"], expected, "{tag}");
        assert_eq!(p["run"], rt["run"], "{tag}: one run, two documents");
        assert!(p["project"].as_str().unwrap().starts_with("proj_"));
        assert_eq!(rt["project"], Value::Null);
        assert_eq!(
            rt["outcome_record"]["verification_refs"],
            serde_json::json!([])
        );
        assert!(
            p["lessons"]
                .as_array()
                .unwrap()
                .iter()
                .all(|l| l["kind"] == "fact" || l["kind"] == "hypothesis")
        );
        // Behaviors only for outcomes that bear on success.
        let updates = matches!(
            expected,
            "verified_success" | "verified_failure" | "unverified"
        );
        assert_eq!(
            p["behaviors"].as_array().unwrap().is_empty(),
            !updates,
            "{tag}"
        );
        // The task's own outcome is unchanged by learning.
        assert_eq!(r.json["verified"], expected == "verified_success", "{tag}");
        if expected == "unverified" {
            assert!(
                p["lessons"].to_string().contains("hypothesis"),
                "{tag}: {}",
                p["lessons"]
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn cancellation_and_delivery_are_preserved_in_the_learning_record() {
    let dir = project("learn-cancel");
    let runtime = private_runtime_dir();
    let pax = Pax::new("learn-cancel", PASSED, false);
    let model = Model::start(vec![Reply::Stall(READ)]).await;
    let mut c = command(&dir, &model.url, &pax);
    c.args(["work", "--json", "--deadline-secs", "1", "Add `two`."])
        .env("STACK_AGENT_DATA_DIR", &runtime);
    let out = tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap();
    model.release();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(1));
    let p = read_doc(&dir, &runtime, &v, "project");
    assert_eq!(p["outcome"], "cancelled");
    assert_eq!(p["outcome_record"]["delivery"], "confirmed_response");
    assert!(
        p["outcome_record"]["cancellation"]
            .as_str()
            .unwrap()
            .starts_with("deadline")
    );
    assert_eq!(model.requests(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn records_carry_no_goal_secret_source_or_host_path_and_runtime_data_stays_out_of_the_project()
 {
    let dir = project("learn-private");
    let runtime = private_runtime_dir();
    let pax = Pax::new("learn-private", PASSED, false);
    let goal = format!("Add `two`; the secret is {SENTINEL}");
    work(
        &dir,
        &runtime,
        vec![Reply::Stream(WRITE), Reply::Stream(TEST)],
        &pax,
        &[&goal],
    )
    .await;
    let host = dir.canonicalize().unwrap().to_string_lossy().into_owned();
    // The learning stores: the project's and the runtime's (`<data>/runtimes/...`). The runtime data
    // directory also holds the project's durable agent state (`<data>/agent-state/`), which records
    // the goal as its task's objective and each observation's output, as `--state-db` always has
    // (`docs/product/agent-state.md`); that is not a learning record.
    for f in all_files(&dir.join(".stack-agent"))
        .into_iter()
        .chain(all_files(&runtime.join("runtimes")))
    {
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(!text.contains(SENTINEL), "{}", f.display());
        assert!(!text.contains("Add `two`"), "goal text in {}", f.display());
        assert!(
            !text.contains("pub fn two"),
            "source content in {}",
            f.display()
        );
        assert!(!text.contains(&host), "host path in {}", f.display());
    }
    // Nothing of the runtime scope is inside the project, and the project's files are untouched
    // apart from the work itself and `.stack-agent`.
    assert!(
        !all_files(&dir)
            .iter()
            .any(|f| f.to_string_lossy().contains("runtimes"))
    );
    assert!(
        !all_files(&runtime)
            .iter()
            .any(|f| f.to_string_lossy().contains(".stack-agent"))
    );
    assert!(
        !dir.join(".gitignore").exists(),
        "no project configuration is changed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn later_runs_retrieve_validated_project_behaviors_and_other_projects_do_not() {
    let dir = project("learn-retrieve");
    let runtime = private_runtime_dir();
    let mut last = Value::Null;
    for i in 0..3 {
        let pax = Pax::new(&format!("learn-retrieve-{i}"), PASSED, false);
        let r = work(
            &dir,
            &runtime,
            vec![Reply::Stream(write(10 + i)), Reply::Stream(TEST)],
            &pax,
            &["Add `two`."],
        )
        .await;
        assert_eq!(r.requests, 2, "learning adds no model request");
        // Each run sees what the earlier runs recorded.
        let behaviors = learning(&r.json)["project"]["retrieved"]["behaviors"]
            .as_array()
            .unwrap()
            .clone();
        match i {
            0 => assert!(behaviors.is_empty()),
            1 => assert_eq!(behaviors[0]["status"], "candidate"),
            _ => {
                assert_eq!(behaviors[0]["status"], "validated");
                assert_eq!(behaviors[0]["support"], 2);
                let refs = behaviors[0]["references"].as_array().unwrap();
                for r in refs {
                    assert!(
                        dir.join(".stack-agent/learning")
                            .join(r.as_str().unwrap())
                            .is_file(),
                        "the reference resolves"
                    );
                }
            }
        }
        last = r.json;
    }
    assert_eq!(
        written(&last, "project")["behaviors"][0]["status"],
        "validated"
    );
    // Project B, same runtime store: it inherits none of A's project lessons, and the runtime
    // behavior is still a candidate (one project's successes do not generalize).
    let other = project("learn-retrieve-b");
    let pax = Pax::new("learn-retrieve-b", PASSED, false);
    let r = work(
        &other,
        &runtime,
        vec![Reply::Stream(BLOCK)],
        &pax,
        &["Add `two`."],
    )
    .await;
    assert_eq!(
        learning(&r.json)["project"]["retrieved"]["behaviors"],
        serde_json::json!([])
    );
    let rt = learning(&r.json)["runtime"]["retrieved"]["behaviors"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rt.len(), 1);
    assert_eq!(rt[0]["status"], "candidate");
    assert_eq!(rt[0]["distinct_projects"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn runtime_behaviors_validate_only_across_projects() {
    let runtime = private_runtime_dir();
    for tag in ["x1", "x2"] {
        let dir = project(&format!("learn-cross-{tag}"));
        let pax = Pax::new(&format!("learn-cross-{tag}"), PASSED, false);
        work(
            &dir,
            &runtime,
            vec![Reply::Stream(WRITE), Reply::Stream(TEST)],
            &pax,
            &["Add `two`."],
        )
        .await;
    }
    let dir = project("learn-cross-x3");
    let pax = Pax::new("learn-cross-x3", PASSED, false);
    let r = work(
        &dir,
        &runtime,
        vec![Reply::Stream(BLOCK)],
        &pax,
        &["Add `two`."],
    )
    .await;
    let rt = &learning(&r.json)["runtime"]["retrieved"]["behaviors"][0];
    assert_eq!(rt["status"], "validated", "{rt}");
    assert_eq!(rt["distinct_projects"], 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn project_learning_feeds_auto_ranking_and_never_selects_without_auto() {
    let dir = project("learn-auto");
    let runtime = private_runtime_dir();
    for i in 0..3 {
        let pax = Pax::new(&format!("learn-auto-{i}"), FAILED, false);
        let r = work(
            &dir,
            &runtime,
            vec![
                Reply::Stream(write(20 + i)),
                Reply::Stream(TEST),
                Reply::Stream(BLOCK),
            ],
            &pax,
            &["--auto", "Add `two`."],
        )
        .await;
        assert_eq!(
            r.json["automatic_configuration"]["decision"]["outcome"]["configuration"],
            DEDUP
        );
    }
    // Without --auto: the rejected behavior is retrieved and reported; nothing is selected.
    let pax = Pax::new("learn-auto-plain", PASSED, false);
    let plain = work(
        &dir,
        &runtime,
        vec![Reply::Stream(write(30)), Reply::Stream(TEST)],
        &pax,
        &["Add `two`."],
    )
    .await;
    assert!(plain.json.get("automatic_configuration").is_none());
    let retrieved = learning(&plain.json)["project"]["retrieved"]["behaviors"].to_string();
    assert!(retrieved.contains("rejected"), "{retrieved}");
    assert_eq!(learning(&plain.json)["evidence_for_auto"], Value::Null);
    // With --auto: the three recorded failures rank the default below neutral.
    let pax = Pax::new("learn-auto-next", PASSED, false);
    let r = work(
        &dir,
        &runtime,
        vec![Reply::Stream(write(31)), Reply::Stream(TEST)],
        &pax,
        &["--auto", "Add `two`."],
    )
    .await;
    let decision = &r.json["automatic_configuration"]["decision"];
    assert_eq!(decision["outcome"]["configuration"], FULL, "{decision}");
    assert_eq!(decision["evidence_used"].as_array().unwrap().len(), 3);
    assert_eq!(learning(&r.json)["evidence_for_auto"], 3);
    assert_eq!(r.out.status.code(), Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn hard_constraints_hold_whatever_was_learned() {
    // A project whose package.json makes change ineligible stays blocked, and its refusal is
    // recorded as a run.
    let dir = gated::unique("learn-npm");
    std::fs::write(dir.join("package.json"), "{}").unwrap();
    let runtime = private_runtime_dir();
    let pax = Pax::new("learn-npm", PASSED, false);
    let r = work(
        &dir,
        &runtime,
        vec![Reply::Stream(WRITE)],
        &pax,
        &["--auto", "Fix it."],
    )
    .await;
    assert_eq!(r.out.status.code(), Some(3));
    assert_eq!(r.requests, 0);
    let p = read_doc(&dir, &runtime, &r.json, "project");
    assert_eq!(p["outcome"], "blocked");
    assert_eq!(p["outcome_record"], Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn forged_records_are_refused_and_the_model_cannot_write_the_store() {
    let dir = project("learn-forge");
    let runtime = private_runtime_dir();
    let pax = Pax::new("learn-forge", PASSED, false);
    // The model tries to write into the learning store: the path is reserved.
    let r = work(
        &dir,
        &runtime,
        vec![Reply::Stream(FORGE), Reply::Stream(BLOCK)],
        &pax,
        &["Add `two`."],
    )
    .await;
    assert!(!dir.join(".stack-agent/learning/runs/forged.json").exists());
    assert!(
        r.json["executions_by_capability"]
            .get("project.write")
            .is_none(),
        "{}",
        r.json
    );
    // Someone edits a run document to claim success: the behavior referencing it is not validated.
    let pax = Pax::new("learn-forge-2", PASSED, false);
    let ok = work(
        &dir,
        &runtime,
        vec![
            Reply::Stream(WRITE),
            Reply::Stream(TEST),
            Reply::Stream(BLOCK),
        ],
        &pax,
        &["Add `two`."],
    )
    .await;
    let path = dir.join(written(&ok.json, "project")["path"].as_str().unwrap());
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    v["actions"]["model_calls"] = 0.into();
    std::fs::write(&path, v.to_string()).unwrap();
    let pax = Pax::new("learn-forge-3", PASSED, false);
    let after = work(
        &dir,
        &runtime,
        vec![Reply::Stream(BLOCK)],
        &pax,
        &["Add `two`."],
    )
    .await;
    let project = &learning(&after.json)["project"]["retrieved"];
    assert_eq!(project["behaviors"][0]["status"], "candidate", "{project}");
    assert!(
        project["diagnostics"]
            .to_string()
            .contains("altered or forged"),
        "{project}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn storage_failures_are_reported_and_never_change_the_outcome() {
    let dir = project("learn-storage");
    std::fs::write(dir.join(".stack-agent"), "not a directory").unwrap();
    let runtime_file = gated::unique("learn-storage-runtime").join("file");
    std::fs::write(&runtime_file, "x").unwrap();
    let pax = Pax::new("learn-storage", PASSED, false);
    let r = work(
        &dir,
        &runtime_file,
        vec![Reply::Stream(WRITE), Reply::Stream(TEST)],
        &pax,
        &["Add `two`."],
    )
    .await;
    assert_eq!(r.out.status.code(), Some(0), "the task is still verified");
    assert_eq!(r.json["verified"], true);
    for scope in ["project", "runtime"] {
        let w = written(&r.json, scope);
        assert_eq!(w["persisted"], false, "{scope}: {w}");
        assert!(
            w["error"].as_str().unwrap().contains("not persisted"),
            "{scope}: {w}"
        );
    }
    let human = {
        let pax = Pax::new("learn-storage-h", PASSED, false);
        let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
        let mut c = command(&dir, &model.url, &pax);
        c.args(["work", "Add `two`."])
            .env("STACK_AGENT_DATA_DIR", &runtime_file);
        let out = tokio::task::spawn_blocking(move || c.output().unwrap())
            .await
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert!(human.contains("NOT persisted"), "{human}");
}
