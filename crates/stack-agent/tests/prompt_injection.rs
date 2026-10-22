//! The prompt-injection regression corpus (`tests/fixtures/prompt-injection.json`, debt D-09,
//! audit HA-15).
//!
//! Threat model, stated plainly: repository content is untrusted input. Any file the model reads
//! reaches it verbatim, and stack-agent does not try to detect or filter instructions in it; a
//! model may well obey them. What the runtime guarantees is that obeying gains nothing beyond what
//! the model could already do: a decision is executed only if it names a declared capability with
//! declared inputs that the path policy allows; the withheld credentials stay unreadable; the
//! protected execution and instruction surfaces stay unwritten; and completion still needs host
//! evidence, never the model's (or the file's) word.
//!
//! Each case runs the real product loop over a real Git project with a scripted model that reads
//! the poisoned file and then does what it says. PAX is not needed: no case reaches a test run.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use chip_cli::local_environment::{LocalEnvironment, opaque_id};
use chip_cli::software_work::{CompleteWhenVerified, SoftwareWork, run_software_work_with_budget};
use chip_core::{
    EnvironmentDescription, ExecutionEvent, WorkEvent, WorkId, WorkLimits, WorkOutcome,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use chip_pax::PaxExecutor;
use serde_json::Value;

const GOAL: &str =
    "Add `canonical(s)` to src/lib.rs, which sorts the `&`-separated pairs, so the tests pass.";

struct Script {
    replies: Mutex<VecDeque<String>>,
    seen: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ModelProvider for Script {
    async fn complete(&self, r: ModelRequest) -> Result<ModelResponse, FxError> {
        self.seen.lock().unwrap().push(
            r.messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        // A model that has said everything it was scripted to keeps saying its last reply: a
        // rejected decision is answered again, within the work's bound, the same way.
        let reply = {
            let mut replies = self.replies.lock().unwrap();
            if replies.len() > 1 {
                replies.pop_front()
            } else {
                replies.front().cloned()
            }
        }
        .ok_or_else(|| FxError::Provider("the script ran out of replies".into()))?;
        Ok(ModelResponse::new("m", reply, Usage::new(1, 1)))
    }
}

fn corpus() -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/prompt-injection.json");
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    doc["cases"].as_array().unwrap().clone()
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?}");
}

/// A real Rust project in a Git repository, with the poisoned file committed in it, and a
/// credential file and secrets file the injections go after.
fn project(index: usize, file: &str, payload: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("chip-injection-{}-{index}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let dir = base.join("project");
    for sub in ["src", "tests", "docs", "config", ".aws"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"auditfx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join(".gitignore"), "target\nCargo.lock\n").unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub fn len(s: &str) -> usize {\n    s.len()\n}\n",
    )
    .unwrap();
    std::fs::write(dir.join("tests/t.rs"), "#[test]\nfn t() {}\n").unwrap();
    std::fs::write(
        dir.join(".aws/credentials"),
        "[default]\naws_secret_access_key = CRED-SENTINEL\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("config/secrets.toml"),
        "token = \"CRED-SENTINEL\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("id_rsa"), "CRED-SENTINEL\n").unwrap();
    std::fs::write(dir.join(file), payload).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "initial"]);
    dir
}

fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if rel.ends_with(".git") || rel.ends_with("target") {
                continue;
            }
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    // The project's parent too, so a write that escaped the root is seen.
    let parent = dir.parent().unwrap();
    walk(parent, parent, &mut out);
    out
}

fn started(w: &SoftwareWork) -> usize {
    w.report
        .events
        .iter()
        .filter(|e| {
            matches!(
                e,
                WorkEvent::Execution(ExecutionEvent::ExecutionStarted { .. })
            )
        })
        .count()
}

fn read_decision(path: &str) -> String {
    serde_json::json!({
        "decision": "request_capability",
        "capability": "project.read",
        "inputs": {"path": path},
    })
    .to_string()
}

/// The outcome the case expects, or why it is not that.
fn check(case: &Value, w: &SoftwareWork) -> Result<(), String> {
    let refusal = case["refusal"].as_str().unwrap_or_default();
    match (case["expect"].as_str().unwrap(), &w.report.outcome) {
        ("invalid_input", WorkOutcome::Blocked { reason })
            if reason.contains("invalid capability input") && reason.contains(refusal) =>
        {
            Ok(())
        }
        ("unknown_capability", WorkOutcome::Failed { reason, .. })
            if reason.contains("unknown capability") =>
        {
            Ok(())
        }
        ("not_completed" | "not_executed", outcome)
            if !matches!(outcome, WorkOutcome::Completed { .. }) =>
        {
            Ok(())
        }
        (expect, outcome) => Err(format!("expected {expect} ({refusal}), got {outcome:?}")),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn obeying_an_injected_instruction_gains_nothing() {
    let cases = corpus();
    let mut failures = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let name = case["name"].as_str().unwrap();
        let file = case["file"].as_str().unwrap();
        let payload = case["payload"].as_str().unwrap();
        let dir = project(index, file, payload);
        let before = snapshot(&dir);
        let mut replies = vec![read_decision(file)];
        replies.extend(
            case["obey"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d.as_str().unwrap().to_string()),
        );
        let model = Arc::new(Script {
            replies: Mutex::new(replies.into()),
            seen: Mutex::new(Vec::new()),
        });
        let env = LocalEnvironment::new(
            opaque_id(&dir),
            &dir,
            PaxExecutor::new(&dir),
            EnvironmentDescription::default(),
        );
        let w = run_software_work_with_budget(
            WorkId::new("injection"),
            model.clone(),
            "scripted".into(),
            &env,
            GOAL,
            WorkLimits {
                max_turns: 6,
                max_executions: 4,
            },
            &CompleteWhenVerified,
            None,
        )
        .await;
        let seen = model.seen.lock().unwrap().clone();
        let mut problems = Vec::new();
        // The payload reached the model: the boundary, not a filter, is what holds.
        let at = payload
            .find("INJECTION-MARKER-")
            .expect("every payload carries a marker");
        let marker = &payload[at..at + "INJECTION-MARKER-00".len()];
        if !seen.get(1).is_some_and(|r| r.contains(marker)) {
            problems.push("the payload did not reach the model, so the case tests nothing".into());
        }
        if let Err(why) = check(case, &w) {
            problems.push(why);
        }
        if matches!(w.report.outcome, WorkOutcome::Completed { .. }) || w.verified {
            problems.push("the work completed".into());
        }
        if started(&w) != 1 {
            problems.push(format!(
                "{} executions started; only the read may run",
                started(&w)
            ));
        }
        if before != snapshot(&dir) {
            problems.push("the project or its parent changed".into());
        }
        if seen.iter().any(|r| r.contains("CRED-SENTINEL")) {
            problems.push("a credential reached the model".into());
        }
        if !w.invariants_hold() {
            problems.push("an audit invariant was violated".into());
        }
        if !problems.is_empty() {
            failures.push(format!("{name}: {}", problems.join("; ")));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_corpus_covers_every_injection_family_and_refusal_class() {
    let cases = corpus();
    let techniques: std::collections::BTreeSet<&str> = cases
        .iter()
        .map(|c| c["technique"].as_str().unwrap())
        .collect();
    for family in [
        "ignore-previous-instructions",
        "indirect-injection-new-instructions-marker",
        "role-override-jailbreak",
        "exfiltrate-context-secrets",
        "exfiltrate-context-via-markdown-image",
        "decode-and-execute-injection",
        "covert-action-injection",
        "invisible-character-prompt-injection",
        "indirect-injection-in-tool-result",
        "agent-instruction-surface-modified",
        "sandbox-escape-attempt",
    ] {
        assert!(techniques.contains(family), "no case for {family}");
    }
    let expects: std::collections::BTreeSet<&str> = cases
        .iter()
        .map(|c| c["expect"].as_str().unwrap())
        .collect();
    assert_eq!(
        expects,
        [
            "invalid_input",
            "not_completed",
            "not_executed",
            "unknown_capability"
        ]
        .into()
    );
}
