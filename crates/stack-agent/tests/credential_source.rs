//! Which API key a model call carries is an explicit choice, and the report names it.
//! The binary, against a mock OpenAI-compatible server on loopback.

#[path = "common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-cred-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
    dir
}

fn reply() -> String {
    serde_json::json!({
        "id": "r",
        "choices": [{"message": {"role": "assistant",
            "content": "{\"decision\":\"block\",\"reason\":\"nothing\"}"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1},
    })
    .to_string()
}

struct Ran {
    stdout: Vec<u8>,
    stderr: String,
    authorization: Option<String>,
}

async fn run(tag: &str, env: &[(&str, &str)], extra: &[&str]) -> Ran {
    let dir = fixture(tag);
    let server = common::start(200, &reply(), Duration::ZERO).await;
    let url = server.url.clone();
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let extra: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
    let out = tokio::task::spawn_blocking(move || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_stack-agent"));
        c.args([
            "work",
            "Summarize src/lib.rs.",
            "--kind",
            "inspect",
            "--json",
            "--no-state",
        ])
        .args(["--max-turns", "2"])
        .args(&extra)
        .current_dir(&dir)
        .env_remove("PAX_BIN")
        .env_remove("CHIP_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env("CHIP_PROVIDER", "openai-compatible")
        .env("CHIP_MODEL", "mock-model")
        .env("CHIP_ENDPOINT", &url);
        for (k, v) in &env {
            c.env(k, v);
        }
        c.output().unwrap()
    })
    .await
    .unwrap();
    let authorization = server
        .captured
        .lock()
        .await
        .first()
        .and_then(|c| c.header("authorization").map(str::to_string));
    Ran {
        stdout: out.stdout,
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        authorization,
    }
}

fn source(r: &Ran) -> serde_json::Value {
    let j: serde_json::Value =
        serde_json::from_slice(&r.stdout).unwrap_or_else(|_| panic!("no report: {}", r.stderr));
    j["credential_source"].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ambient_provider_key_is_not_sent_to_a_server_on_this_machine() {
    let r = run(
        "ambient",
        &[("OPENAI_API_KEY", "sk-real-key-must-not-leave")],
        &[],
    )
    .await;
    assert_eq!(r.authorization, None, "{r:?}", r = r.stderr);
    assert_eq!(source(&r), "none");
    assert!(!String::from_utf8_lossy(&r.stdout).contains("sk-real-key"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_named_variable_is_used_and_reported_by_name_only() {
    let r = run(
        "named",
        &[
            ("MY_LOCAL_KEY", "sk-chosen"),
            ("OPENAI_API_KEY", "sk-other"),
        ],
        &["--api-key-env", "MY_LOCAL_KEY"],
    )
    .await;
    assert_eq!(r.authorization.as_deref(), Some("Bearer sk-chosen"));
    assert_eq!(source(&r), "MY_LOCAL_KEY");
    assert!(!String::from_utf8_lossy(&r.stdout).contains("sk-chosen"));
}

#[tokio::test(flavor = "multi_thread")]
async fn chip_api_key_is_explicit_and_named() {
    let r = run("chip", &[("CHIP_API_KEY", "sk-chip")], &[]).await;
    assert_eq!(r.authorization.as_deref(), Some("Bearer sk-chip"));
    assert_eq!(source(&r), "CHIP_API_KEY");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_named_variable_that_is_unset_stops_the_run_before_any_call() {
    let r = run("unset", &[], &["--api-key-env", "NOT_SET_ANYWHERE"]).await;
    assert!(r.stdout.is_empty() || serde_json::from_slice::<serde_json::Value>(&r.stdout).is_err());
    assert!(r.stderr.contains("NOT_SET_ANYWHERE"), "{}", r.stderr);
    assert_eq!(r.authorization, None);
}
