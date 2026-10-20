//! `stack-agent work --state-db FILE --task-id ID`: the real binary persists its work in a real
//! FeltDB file through `chip-agent-state`. The model is the scripted loopback endpoint and PAX the
//! usual stand-in; the database, the transitions and every restart are real (each command is a new
//! process that reopens the file).

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::path::Path;
use std::process::Output;

use gated::{Model, PASSED, Pax, Reply, command, project, unique};
use serde_json::Value;

const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;

async fn work(
    dir: &Path,
    model: &Model,
    pax: &Pax,
    db: &Path,
    task: &str,
    extra: &[&str],
) -> Output {
    let mut cmd = command(dir, &model.url, pax);
    cmd.args(["work", "Add `two`.", "--json", "--state-db"])
        .arg(db)
        .args(["--task-id", task])
        .args(extra);
    tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap()
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

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn verified_work_is_recorded_complete_and_a_restart_reports_it_without_running_again() {
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let pax = Pax::new("state-done", PASSED, false);
    let dir = project("state-done");
    let db = unique("state-done").join("agent-state.log");

    let out = work(&dir, &model, &pax, &db, "add-two", &[]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v = report(&out);
    let state = &v["agent_state"];
    assert_eq!(state["recorded"], true, "{state}");
    assert_eq!(state["task_id"], "add-two");
    assert_eq!(state["completion_refused"], false);
    assert_eq!(state["task"]["status"], "completed");
    assert_eq!(
        state["task"]["intents"], 2,
        "one per execution: write and test"
    );
    assert_eq!(state["task"]["unresolved_intents"], 0);
    assert!(state["persistence_failures"].as_array().unwrap().is_empty());
    let asked = model.requests();

    // A new process over the same file: FeltDB says the task is done, so nothing runs.
    let again = work(&dir, &model, &pax, &db, "add-two", &[]).await;
    assert_eq!(again.status.code(), Some(0), "{}", stderr(&again));
    let v = report(&again);
    assert_eq!(v["ran"], false);
    assert_eq!(v["agent_state"]["task"]["status"], "completed");
    assert_eq!(model.requests(), asked, "no model call for a finished task");
}

#[tokio::test(flavor = "multi_thread")]
async fn blocked_work_is_recorded_blocked_and_its_lease_refuses_a_second_process() {
    let model = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let pax = Pax::new("state-blocked", PASSED, false);
    let dir = project("state-blocked");
    let db = unique("state-blocked").join("agent-state.log");

    let out = work(&dir, &model, &pax, &db, "blocked-1", &[]).await;
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let v = report(&out);
    assert_eq!(v["agent_state"]["recorded"], true);
    assert_eq!(v["agent_state"]["task"]["status"], "blocked");
    let asked = model.requests();

    // The first process's lease has not expired: the second is refused before anything runs.
    let second = work(&dir, &model, &pax, &db, "blocked-1", &[]).await;
    assert_eq!(second.status.code(), Some(3));
    let err = stderr(&second);
    assert!(
        err.contains("unexpired lease") && err.contains("nothing was run"),
        "{err}"
    );
    assert_eq!(model.requests(), asked);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_database_that_cannot_be_opened_stops_the_work_before_it_starts() {
    let model = Model::start(vec![Reply::Stream(BLOCK)]).await;
    let pax = Pax::new("state-unusable", PASSED, false);
    let dir = project("state-unusable");
    let db = unique("state-unusable").join("agent-state.log");
    std::fs::create_dir_all(&db).unwrap(); // a directory where the file should be

    let out = work(&dir, &model, &pax, &db, "t", &[]).await;
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("durable agent state"));
    assert_eq!(
        model.requests(),
        0,
        "no silent fallback to running without state"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn incomplete_or_invalid_state_options_are_usage_errors() {
    let model = Model::start(vec![]).await;
    let pax = Pax::new("state-usage", PASSED, false);
    let dir = project("state-usage");
    let db = unique("state-usage").join("agent-state.log");
    for args in [
        vec!["work", "x", "--task-id", "t"],
        vec!["work", "x", "--lease-secs", "10"],
    ] {
        let out = command(&dir, &model.url, &pax)
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr(&out));
    }
    let out = command(&dir, &model.url, &pax)
        .args(["work", "x", "--state-db"])
        .arg(&db)
        .args(["--task-id", "not a valid id"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(!db.exists(), "nothing was opened");
    assert_eq!(model.requests(), 0);
}

// ---- operator settlement: `stack-agent agent-state settle` ---------------------------------------

/// The write the scripted model asks for, as Chip validates it.
fn write_request(task: &str) -> chip_core::ExecutionRequest {
    let write_inputs = serde_json::from_str::<Value>(WRITE).unwrap()["inputs"].clone();
    let inputs = write_inputs
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                chip_core::InputValue::Text(v.as_str().unwrap().into()),
            )
        })
        .collect();
    chip_core::ExecutionRequest::new(
        chip_core::ExecutionId::new(format!("{task}-exec-1")),
        "project.write",
    )
    .with_inputs(inputs)
}

/// Never returns: the process dies with the action in flight.
struct Dies;

#[async_trait::async_trait]
impl chip_core::Executor for Dies {
    async fn execute(
        &self,
        _r: chip_core::ExecutionRequest,
    ) -> Result<chip_core::ExecutionResult, chip_core::ExecutionError> {
        std::future::pending().await
    }
}

/// What a `work --state-db` process leaves when it dies inside the write: the durable executor
/// recorded the intent, and nothing after it. Returns the intent's key, once the dead process's
/// (short) lease has expired.
async fn died_inside_the_write(db: &Path, task: &str) -> String {
    let store = chip_agent_state::AgentStore::open(db).unwrap();
    let config = chip_agent_state::DurableConfig {
        lease_ms: 200,
        ..Default::default()
    };
    let Ok(chip_agent_state::Begun::Ready(work)) =
        chip_agent_state::begin(&store, task, "Add `two`.", &config)
    else {
        panic!("a new task")
    };
    let died = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        work.executor(std::sync::Arc::new(Dies))
            .execute(write_request(task)),
    )
    .await;
    assert!(died.is_err());
    let key = chip_agent_state::intent_key(work.attempt(), &format!("{task}-exec-1"));
    drop((work, store));
    std::thread::sleep(std::time::Duration::from_millis(250));
    key
}

fn agent_state(dir: &Path, model: &Model, pax: &Pax, args: &[&str]) -> Output {
    command(dir, &model.url, pax)
        .arg("agent-state")
        .args(args)
        .output()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_effect_settled_as_not_executed_resumes_at_once_and_is_performed_normally() {
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let pax = Pax::new("settle-no", PASSED, false);
    let dir = project("settle-no");
    let db = unique("settle-no").join("agent-state.log");
    let key = died_inside_the_write(&db, "settle-no").await;

    // Refused: the write may or may not have happened.
    let refused = work(&dir, &model, &pax, &db, "settle-no", &[]).await;
    assert_eq!(refused.status.code(), Some(3));
    assert!(
        stderr(&refused).contains("outcome is unknown"),
        "{}",
        stderr(&refused)
    );
    assert_eq!(model.requests(), 0);

    // The refusal gave its lease back: the operator settles straight away.
    let db_s = db.to_str().unwrap();
    let settled = agent_state(
        &dir,
        &model,
        &pax,
        &[
            "settle",
            "--state-db",
            db_s,
            "--task-id",
            "settle-no",
            "--intent",
            &key,
            "--executed",
            "false",
            "--decided-by",
            "alice",
            "--evidence",
            "src/lib.rs still has only `one`",
            "--json",
        ],
    );
    assert_eq!(settled.status.code(), Some(0), "{}", stderr(&settled));
    let v = report(&settled);
    assert_eq!(v["settled"]["kind"], "operator_settlement");
    assert_eq!(v["settled"]["decided_by"], "alice");
    assert_eq!(v["settled"]["executed"], false);

    // And the work resumes at once, without a second operator step, and performs the write.
    let out = work(&dir, &model, &pax, &db, "settle-no", &[]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v = report(&out);
    assert_eq!(v["agent_state"]["task"]["status"], "completed");
    assert_eq!(v["agent_state"]["settled"][0]["executed"], false);
    assert_eq!(v["agent_state"]["settled"][0]["decided_by"], "alice");
    assert!(
        std::fs::read_to_string(dir.join("src/lib.rs"))
            .unwrap()
            .contains("two")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_effect_settled_as_executed_is_not_performed_again_by_the_resumed_work() {
    let model = Model::start(vec![Reply::Stream(WRITE), Reply::Stream(TEST)]).await;
    let pax = Pax::new("settle-yes", PASSED, false);
    let dir = project("settle-yes");
    let db = unique("settle-yes").join("agent-state.log");
    let key = died_inside_the_write(&db, "settle-yes").await;
    let refused = work(&dir, &model, &pax, &db, "settle-yes", &[]).await;
    assert_eq!(refused.status.code(), Some(3));

    let db_s = db.to_str().unwrap();
    let settled = agent_state(
        &dir,
        &model,
        &pax,
        &[
            "settle",
            "--state-db",
            db_s,
            "--task-id",
            "settle-yes",
            "--intent",
            &key,
            "--executed",
            "true",
            "--decided-by",
            "alice",
            "--evidence",
            "the deploy log shows the write",
        ],
    );
    assert_eq!(settled.status.code(), Some(0), "{}", stderr(&settled));

    // The settlement is visible in durable state, as an operator's conclusion.
    let shown = agent_state(
        &dir,
        &model,
        &pax,
        &[
            "show",
            "--state-db",
            db_s,
            "--task-id",
            "settle-yes",
            "--json",
        ],
    );
    assert_eq!(shown.status.code(), Some(0), "{}", stderr(&shown));
    let v = report(&shown);
    let s = &v["settlements"][0];
    assert_eq!(
        (&s["intent"], &s["executed"], &s["decided_by"], &s["kind"]),
        (
            &Value::from(key.clone()),
            &Value::from(true),
            &Value::from("alice"),
            &Value::from("operator_settlement")
        )
    );
    assert_eq!(s["evidence"], "the deploy log shows the write");
    assert_eq!(v["last_checkpoint"]["actor"], "alice");
    assert_eq!(v["intents"][0]["conclusion"], "executed");

    // The model asks for the same write again; it is not performed.
    let out = work(&dir, &model, &pax, &db, "settle-yes", &[]).await;
    assert_eq!(out.status.code(), Some(1), "blocked: {}", stderr(&out));
    let v = report(&out);
    assert_eq!(v["agent_state"]["settled"][0]["executed"], true);
    assert_ne!(v["agent_state"]["task"]["status"], "completed");
    assert!(
        v.to_string().contains("was settled by alice"),
        "the refusal names the settlement: {v}"
    );
    assert!(
        !std::fs::read_to_string(dir.join("src/lib.rs"))
            .unwrap()
            .contains("two"),
        "the write was not performed by the resumed work"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_settlement_is_refused_without_writing_anything() {
    let model = Model::start(vec![]).await;
    let pax = Pax::new("settle-bad", PASSED, false);
    let dir = project("settle-bad");
    let db = unique("settle-bad").join("agent-state.log");
    let key = died_inside_the_write(&db, "settle-bad").await;
    let db_s = db.to_str().unwrap();
    let size = || std::fs::metadata(&db).unwrap().len();
    let before = size();
    let base = |executed: &'static str,
                who: Option<&'static str>,
                evidence: Option<&'static str>,
                intent: &str| {
        let mut a: Vec<String> = [
            "settle",
            "--state-db",
            db_s,
            "--task-id",
            "settle-bad",
            "--intent",
            intent,
            "--executed",
            executed,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        if let Some(who) = who {
            a.extend(["--decided-by".into(), who.into()]);
        }
        if let Some(evidence) = evidence {
            a.extend(["--evidence".into(), evidence.into()]);
        }
        a
    };
    for (args, code, why) in [
        (base("true", None, Some("ledger"), &key), 2, "no identity"),
        (base("true", Some("alice"), None, &key), 2, "no evidence"),
        (
            base("yes", Some("alice"), Some("ledger"), &key),
            2,
            "a malformed conclusion",
        ),
        (
            base("true", Some("alice"), Some(""), &key),
            1,
            "empty evidence",
        ),
        (
            base("true", Some("alice smith"), Some("ledger"), &key),
            1,
            "a malformed identity",
        ),
        (
            base("true", Some("alice"), Some("ledger"), "a9.nope"),
            1,
            "no such intent",
        ),
    ] {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = agent_state(&dir, &model, &pax, &args);
        assert_eq!(out.status.code(), Some(code), "{why}: {}", stderr(&out));
        assert_eq!(size(), before, "{why}: nothing may be written");
    }
    let mut unknown = base("true", Some("alice"), Some("ledger"), &key);
    unknown.extend(["--force".into(), "yes".into()]);
    let unknown: Vec<&str> = unknown.iter().map(String::as_str).collect();
    assert_eq!(
        agent_state(&dir, &model, &pax, &unknown).status.code(),
        Some(2)
    );

    // Settled once; settling again is refused and writes nothing.
    let ok = base("true", Some("alice"), Some("ledger line 1"), &key);
    let ok: Vec<&str> = ok.iter().map(String::as_str).collect();
    assert_eq!(agent_state(&dir, &model, &pax, &ok).status.code(), Some(0));
    let settled = size();
    let again = base("false", Some("bob"), Some("I disagree"), &key);
    let again: Vec<&str> = again.iter().map(String::as_str).collect();
    let out = agent_state(&dir, &model, &pax, &again);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("resolved already"),
        "{}",
        stderr(&out)
    );
    assert_eq!(size(), settled);
    assert_eq!(model.requests(), 0, "settling calls no model");
}

// ---- capability effect declarations: what a fresh process does with an interrupted request --------

/// What a `work --state-db` process leaves when it dies inside `capability`: the durable executor
/// recorded the intent for exactly this validated request, and nothing after it. Returns the
/// intent's key, once the dead process's (short) lease has expired.
async fn died_inside(db: &Path, task: &str, capability: &str, inputs: &[(&str, &str)]) -> String {
    let store = chip_agent_state::AgentStore::open(db).unwrap();
    let config = chip_agent_state::DurableConfig {
        lease_ms: 200,
        ..Default::default()
    };
    let Ok(chip_agent_state::Begun::Ready(work)) =
        chip_agent_state::begin(&store, task, "Add `two`.", &config)
    else {
        panic!("a new task")
    };
    let request = chip_core::ExecutionRequest::new(
        chip_core::ExecutionId::new(format!("{task}-exec-1")),
        capability,
    )
    .with_inputs(
        inputs
            .iter()
            .map(|(k, v)| (k.to_string(), chip_core::InputValue::Text(v.to_string())))
            .collect(),
    );
    let died = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        work.executor(std::sync::Arc::new(Dies)).execute(request),
    )
    .await;
    assert!(died.is_err());
    let key = chip_agent_state::intent_key(work.attempt(), &format!("{task}-exec-1"));
    drop((work, store));
    std::thread::sleep(std::time::Duration::from_millis(250));
    key
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_interrupted_inside_project_read_is_observed_again_and_needs_no_operator() {
    // Scenario A. `project.read` declares itself an observation, so a fresh process performs the
    // request the intent recorded again, through the normal executor, and goes on. The model is
    // asked once, for the next step; the test run happens once.
    let model = Model::start(vec![Reply::Stream(TEST)]).await;
    let pax = Pax::new("read-again", PASSED, false);
    let dir = project("read-again");
    let db = unique("read-again").join("agent-state.log");
    let key = died_inside(&db, "read-again", "project.read", &[("path", "src/lib.rs")]).await;

    let out = work(&dir, &model, &pax, &db, "read-again", &["--kind", "verify"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v = report(&out);
    assert_eq!(v["agent_state"]["task"]["status"], "completed");
    let again = &v["agent_state"]["reobserved"];
    assert_eq!(again.as_array().unwrap().len(), 1, "{again}");
    assert_eq!(again[0]["capability"], "project.read");
    assert_eq!(again[0]["settled_intents"][0], key.as_str());
    assert_eq!(again[0]["fresh_observation"], true);
    assert_eq!(again[0]["status"], "success");
    assert_eq!(model.requests(), 1);
    assert!(pax.finished());
    // The audit trail is in FeltDB: the old intent is resolved by the new observation, no
    // operator settlement was written, and the new intent says what it did.
    let store = chip_agent_state::AgentStore::open(&db).unwrap();
    assert!(store.settlements("read-again").unwrap().is_empty());
    let history = store.history("read-again").unwrap().unwrap();
    assert_eq!(history.task.unresolved_intents, 0);
    let fresh = history
        .intents
        .iter()
        .find(|i| i.value.action.contains("reobserves="))
        .expect("the re-observation is recorded as one");
    assert!(fresh.value.action.contains(&format!("reobserves={key}")));
    assert!(
        history
            .intents
            .iter()
            .find(|i| i.value.key == key)
            .unwrap()
            .value
            .evidence
            .iter()
            .any(|c| c
                .id
                .as_deref()
                .is_some_and(|id| id.ends_with(&fresh.value.key)))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_interrupted_inside_pax_test_is_not_performed_again_and_stays_settleable() {
    // Scenario B. `pax.test` runs the project's own code: consequential. A fresh process refuses,
    // gives its lease back, and performs nothing; the operator settles; nothing runs twice.
    let model = Model::start(vec![Reply::Stream(TEST)]).await;
    let pax = Pax::new("pax-again", PASSED, false);
    let dir = project("pax-again");
    let db = unique("pax-again").join("agent-state.log");
    let key = died_inside(&db, "pax-again", "pax.test", &[]).await;

    let refused = work(&dir, &model, &pax, &db, "pax-again", &["--kind", "verify"]).await;
    assert_eq!(refused.status.code(), Some(3), "{}", stderr(&refused));
    assert!(
        stderr(&refused).contains("outcome is unknown"),
        "{}",
        stderr(&refused)
    );
    assert_eq!(model.requests(), 0);
    assert!(!pax.started(), "the test run was not repeated");

    // The refusal gave its lease back: the operator settles at once, and the work resumes.
    let db_s = db.to_str().unwrap();
    let settled = agent_state(
        &dir,
        &model,
        &pax,
        &[
            "settle",
            "--state-db",
            db_s,
            "--task-id",
            "pax-again",
            "--intent",
            &key,
            "--executed",
            "false",
            "--decided-by",
            "alice",
            "--evidence",
            "the ledger is empty",
            "--json",
        ],
    );
    assert_eq!(settled.status.code(), Some(0), "{}", stderr(&settled));
    let out = work(&dir, &model, &pax, &db, "pax-again", &["--kind", "verify"]).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let v = report(&out);
    assert_eq!(v["agent_state"]["task"]["status"], "completed");
    assert_eq!(v["agent_state"]["reobserved"].as_array().unwrap().len(), 0);
    assert_eq!(v["agent_state"]["settled"][0]["decided_by"], "alice");
    assert!(pax.finished());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_interrupted_inside_a_capability_that_declares_nothing_stays_uncertain() {
    // `project.list` describes itself as navigation, and is read-only in fact, but declares no
    // effect: it is `unknown`, and an interrupted request for it is not performed again.
    let model = Model::start(vec![Reply::Stream(TEST)]).await;
    let pax = Pax::new("list-again", PASSED, false);
    let dir = project("list-again");
    let db = unique("list-again").join("agent-state.log");
    let key = died_inside(&db, "list-again", "project.list", &[("path", ".")]).await;

    let refused = work(&dir, &model, &pax, &db, "list-again", &["--kind", "verify"]).await;
    assert_eq!(refused.status.code(), Some(3), "{}", stderr(&refused));
    assert!(stderr(&refused).contains(&key), "{}", stderr(&refused));
    assert_eq!(model.requests(), 0);
    assert!(!pax.started());
    let store = chip_agent_state::AgentStore::open(&db).unwrap();
    let history = store.history("list-again").unwrap().unwrap();
    assert_eq!(
        history.intents.len(),
        1,
        "nothing was written for a new attempt"
    );
}
