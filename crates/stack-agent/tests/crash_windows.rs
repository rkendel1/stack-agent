//! Crash windows around tool dispatch and model responses, under a real `SIGKILL` of the real
//! binary, resumed by a fresh process from the same FeltDB file.
//!
//! The tool is `pax.test` (consequential: running it again may repeat what it did). Its stand-in
//! appends one line to its own ledger each time it actually performs the run, outside anything
//! stack-agent records, and kills its parent (`kill -9 $PPID`: no handler, no cleanup) at the
//! window a test arms:
//!
//! | window | where the process dies | what the next start must do |
//! | --- | --- | --- |
//! | `probe` | the model chose `pax.test`, the request is being validated; no intent yet | answer the turn from the recorded response, no second model call; run once |
//! | `before-effect` | the intent is durable; the run has not happened | refuse as `OutcomeUncertain`; after `executed=false`, run once |
//! | `after-effect` | the run happened; its answer never came back | refuse as `OutcomeUncertain`; after `executed=true`, never run again |
//!
//! plus a run that completed and was recorded, followed by a kill in the next model call: it is
//! restored, not run again. Every test asserts the ledger (effects that happened), the model
//! endpoint's request count (calls that were paid for) and the authoritative FeltDB state.
//!
//! The same windows under gdb-placed kills inside individual FeltDB commits are in
//! `docs/research/agent-runtime-primitives/experiments/stack-agent/` (not run by `cargo test`).

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::Duration;

use gated::{Model, PASSED, Pax, Reply, command, project, unique};
use serde_json::Value;

const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const COMPLETE: &str = r#"{"decision":"complete","summary":"the tests pass"}"#;
const GOAL: &str = "Check that the tests pass.";
const TASK: &str = "crash-window-task";

/// The `pax` stand-in with a ledger and kill points. A kill point is armed by a file of its name in
/// the stand-in's directory and fires once.
struct Tool {
    pax: Pax,
}

impl Tool {
    fn new(tag: &str) -> Self {
        let dir = unique(&format!("crash-pax-{tag}"));
        let d = dir.display();
        let script = format!(
            "#!/bin/sh\n\
             fire() {{ if [ -e \"{d}/$1\" ]; then rm -f \"{d}/$1\"; kill -9 $PPID; sleep 5; exit 0; fi; }}\n\
             if [ \"$1\" = \"--version\" ]; then fire probe; echo 'pax 0.4.1'; exit 0; fi\n\
             fire before-effect\n\
             echo run >> '{d}/ledger'\n\
             fire after-effect\n\
             echo '{PASSED}'\n\
             exit 0\n"
        );
        let path = dir.join("pax");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { pax: Pax { dir } }
    }

    fn arm(&self, window: &str) {
        std::fs::write(self.pax.dir.join(window), "").unwrap();
    }

    /// Runs the tool actually performed: its own ledger, not anything stack-agent recorded.
    fn performed(&self) -> usize {
        std::fs::read_to_string(self.pax.dir.join("ledger"))
            .map(|l| l.lines().count())
            .unwrap_or(0)
    }
}

struct Case {
    dir: PathBuf,
    db: PathBuf,
    tool: Tool,
}

impl Case {
    fn new(tag: &str) -> Self {
        let state = unique(&format!("crash-state-{tag}"));
        Self {
            dir: project(&format!("crash-{tag}")),
            db: state.join("agent-state.feltdb"),
            tool: Tool::new(tag),
        }
    }

    fn binary(&self, model: &Model) -> Command {
        command(&self.dir, &model.url, &self.tool.pax)
    }

    /// `work --kind verify` on the case's database and task, with a 1 s lease so a dead owner's
    /// lease runs out quickly.
    async fn work(&self, model: &Model) -> Output {
        let mut c = self.binary(model);
        c.args([
            "work",
            GOAL,
            "--kind",
            "verify",
            "--json",
            "--lease-secs",
            "1",
        ])
        .arg("--state-db")
        .arg(&self.db)
        .args(["--task-id", TASK]);
        output(c).await
    }

    async fn agent_state(&self, model: &Model, args: &[&str]) -> (Option<i32>, Value) {
        let mut c = self.binary(model);
        c.arg("agent-state")
            .args(args)
            .arg("--state-db")
            .arg(&self.db)
            .args(["--task-id", TASK, "--json"]);
        let out = output(c).await;
        (out.status.code(), last_json(&out).unwrap_or(Value::Null))
    }

    async fn show(&self, model: &Model) -> Value {
        let (code, shown) = self.agent_state(model, &["show"]).await;
        assert_eq!(code, Some(0), "{shown}");
        shown
    }

    async fn settle(&self, model: &Model, intent: &str, executed: bool) {
        let executed = executed.to_string();
        let (code, settled) = self
            .agent_state(
                model,
                &[
                    "settle",
                    "--intent",
                    intent,
                    "--executed",
                    &executed,
                    "--decided-by",
                    "ops@example",
                    "--evidence",
                    "the pax ledger",
                ],
            )
            .await;
        assert_eq!(code, Some(0), "{settled}");
    }
}

async fn output(mut c: Command) -> Output {
    tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap()
}

fn last_json(out: &Output) -> Option<Value> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(stdout.lines().last()?).ok()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Asserts the process was killed by the stand-in, not ended on its own.
fn killed(out: &Output) {
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        out.status.signal(),
        Some(9),
        "expected SIGKILL; status {:?}, stdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        stderr(out)
    );
}

async fn lease_runs_out() {
    tokio::time::sleep(Duration::from_millis(1_200)).await;
}

fn open_intents(shown: &Value) -> Vec<String> {
    shown["intents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["state"] != "resolved")
        .map(|i| i["key"].as_str().unwrap().to_string())
        .collect()
}

fn event_kinds(shown: &Value, attempt: u64) -> Vec<String> {
    shown["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["attempt"] == attempt)
        .map(|e| e["event"]["kind"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn killed_after_the_response_and_before_the_intent_the_turn_is_answered_from_the_record() {
    let case = Case::new("probe");
    case.tool.arm("probe");
    let first = Model::start(vec![Reply::Stream(TEST)]).await;
    let out = case.work(&first).await;
    killed(&out);
    assert_eq!(first.requests(), 1);
    assert_eq!(case.tool.performed(), 0, "the tool never ran");
    let shown = case.show(&first).await;
    assert!(open_intents(&shown).is_empty(), "no intent was recorded");
    assert_eq!(
        shown["model_responses"].as_array().unwrap().len(),
        1,
        "{shown}"
    );
    assert!(
        shown["model_calls_in_flight"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    lease_runs_out().await;
    // Any request to this endpoint would be a second, paid call for the same turn.
    let second = Model::start(vec![Reply::Stream(TEST)]).await;
    let out = case.work(&second).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let report = last_json(&out).unwrap();
    assert_eq!(
        second.requests(),
        0,
        "the recorded response answered the turn: {report}"
    );
    assert_eq!(case.tool.performed(), 1, "the tool ran exactly once");
    let state = &report["agent_state"];
    assert_eq!(state["model_response"]["offered"], true, "{state}");
    assert_eq!(state["task"]["status"], "completed", "{state}");

    let shown = case.show(&second).await;
    let attempt = state["attempt"].as_u64().unwrap();
    let k = event_kinds(&shown, attempt);
    assert!(k.contains(&"ModelResponseRestored".into()), "{k:?}");
    assert!(!k.contains(&"ModelCalled".into()), "{k:?}");
    assert!(open_intents(&shown).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn killed_after_the_intent_and_before_the_effect_it_is_uncertain_until_settled_not_run() {
    let case = Case::new("before-effect");
    case.tool.arm("before-effect");
    let model = Model::start(vec![Reply::Stream(TEST)]).await;
    killed(&case.work(&model).await);
    assert_eq!(case.tool.performed(), 0);

    lease_runs_out().await;
    let out = case.work(&model).await;
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("outcome is unknown"),
        "{}",
        stderr(&out)
    );
    assert_eq!(case.tool.performed(), 0, "the refusal ran nothing");
    let requests = model.requests();
    assert_eq!(requests, 1, "the refusal asked no model");

    // The operator finds nothing in the ledger and says so.
    let intents = open_intents(&case.show(&model).await);
    assert_eq!(intents.len(), 1);
    case.settle(&model, &intents[0], false).await;

    let out = case.work(&model).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let state = &last_json(&out).unwrap()["agent_state"];
    // The response that asked for it was acted on, so it is not given back: the model is asked.
    assert_eq!(state["model_response"]["offered"], false, "{state}");
    assert_eq!(model.requests(), requests + 1);
    assert_eq!(
        case.tool.performed(),
        1,
        "performed once, after the operator's settlement"
    );
    assert_eq!(state["task"]["status"], "completed");
}

#[tokio::test(flavor = "multi_thread")]
async fn killed_after_the_effect_it_is_uncertain_and_once_settled_executed_never_runs_again() {
    let case = Case::new("after-effect");
    case.tool.arm("after-effect");
    let model = Model::start(vec![Reply::Stream(TEST)]).await;
    killed(&case.work(&model).await);
    assert_eq!(
        case.tool.performed(),
        1,
        "the effect happened before the kill"
    );

    lease_runs_out().await;
    let out = case.work(&model).await;
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert_eq!(case.tool.performed(), 1, "not run blindly again");

    let shown = case.show(&model).await;
    let intents = open_intents(&shown);
    assert_eq!(intents.len(), 1, "{shown}");
    case.settle(&model, &intents[0], true).await;

    // The model asks for it again; the settled run is refused before an intent, and the work
    // stops rather than complete on the operator's word or repeat the run.
    let out = case.work(&model).await;
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(case.tool.performed(), 1, "never run a second time");
    let state = &last_json(&out).unwrap()["agent_state"];
    assert_ne!(state["task"]["status"], "completed", "{state}");
    let shown = case.show(&model).await;
    assert!(open_intents(&shown).is_empty());
    assert_eq!(shown["settlements"][0]["executed"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recorded_run_followed_by_a_kill_in_the_next_model_call_is_restored_not_run_again() {
    let case = Case::new("after-commit");
    // `change` work: after the run the model is asked again, and the process dies waiting on it.
    let first = Model::start(vec![Reply::Stream(TEST), Reply::Stall("")]).await;
    let mut c = case.binary(&first);
    c.args(["work", GOAL, "--json", "--lease-secs", "1"])
        .arg("--state-db")
        .arg(&case.db)
        .args(["--task-id", TASK])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = c.spawn().unwrap();
    first.wait_requests(2).await;
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(case.tool.performed(), 1);

    let shown = case.show(&first).await;
    assert!(
        open_intents(&shown).is_empty(),
        "the run's outcome was recorded"
    );
    let in_flight = shown["model_calls_in_flight"].as_array().unwrap();
    assert_eq!(in_flight.len(), 1, "{shown}");
    assert_eq!(in_flight[0]["turn"], 2);

    lease_runs_out().await;
    let second = Model::start(vec![Reply::Stream(COMPLETE)]).await;
    let mut c = case.binary(&second);
    c.args(["work", GOAL, "--json", "--lease-secs", "1"])
        .arg("--state-db")
        .arg(&case.db)
        .args(["--task-id", TASK]);
    let out = output(c).await;
    let state = &last_json(&out).unwrap()["agent_state"];
    assert_eq!(state["restored"]["observations"], 1, "{state}");
    assert_eq!(state["model_call_in_flight"]["turn"], 2, "{state}");
    assert_eq!(
        case.tool.performed(),
        1,
        "the recorded run was restored, not performed again"
    );
    assert_eq!(second.requests(), 1, "the lost call was asked again, once");
}
