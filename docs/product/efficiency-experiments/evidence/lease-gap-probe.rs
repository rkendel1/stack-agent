//! Ad-hoc probe (not committed): is a live owner's lease renewed across a model call?
//! Lease 1 s; an execution, then a model call held 1.6 s, then a second execution.
#![cfg(unix)]
#[path = "support/gated.rs"]
mod gated;
use gated::{Model, PASSED, Pax, Reply, command, project};
const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_model_call_longer_than_the_lease_between_two_executions() {
    let model = Model::start(vec![Reply::Stream(READ), Reply::Stall(WRITE), Reply::Stream(TEST)]).await;
    let pax = Pax::new("lease-gap", PASSED, false);
    let dir = project("lease-gap");
    let db = dir.join("state.feltdb");
    let mut c = command(&dir, &model.url, &pax);
    c.args(["work", "Add `two`.", "--json", "--lease-secs", "1", "--task-id", "work_lease_gap"])
        .arg("--state-db").arg(&db);
    let child = tokio::task::spawn_blocking(move || c.output().unwrap());
    model.wait_requests(2).await;
    tokio::time::sleep(std::time::Duration::from_millis(1600)).await;
    model.release();
    let out = child.await.unwrap();
    println!("EXIT {:?}", out.status.code());
    println!("STDERR {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.lines().last().unwrap_or("null")).unwrap_or_default();
    println!("terminal {} verified {} executions {} agent_state.task {}", v["terminal_state"], v["verified"], v["executions_by_capability"], v["agent_state"]["task"]);
    println!("REQUESTS {}", model.requests());
}
