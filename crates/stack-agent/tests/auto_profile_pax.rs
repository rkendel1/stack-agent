//! The PAX and verify scenarios of `tests/pax_work.rs` (`a_passing_project_completes_because_pax_said_passed`)
//! and `tests/verify.rs` (`a_passing_project_is_verified`, `a_failing_project_is_blocked`) under the
//! adaptive context assembler, opted into with `--context-profile auto`, through `work --kind verify`
//! (the `verify` command and `--test-pax-work` take no context setting; they send the legacy
//! request). The same fixture projects, real PAX and Cargo, a mock model that asks for `pax.test`;
//! the original outcomes are required, and the auto run is compared with a replay run of the same
//! project. If PAX is not installed a test says SKIPPED and returns, as the originals do.

#[path = "common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use serde_json::Value;

const REQUEST: &str =
    r#"{"schema":"chip.work-decision.v1","decision":"request_capability","capability":"pax.test"}"#;

fn completion(content: &str) -> String {
    serde_json::json!({
        "id": "resp-auto",
        "choices": [{"message": {"role": "assistant", "content": content}}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 4},
    })
    .to_string()
}

/// The fixture of `verify.rs` / `pax_work.rs`: one crate, one unit test with this body.
fn project(tag: &str, test_body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-auto-pax-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"autofixture_{tag}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        format!("#[cfg(test)]\nmod tests {{\n    #[test]\n    fn the_test() {{\n        {test_body}\n    }}\n}}\n"),
    )
    .unwrap();
    dir
}

struct Ran {
    out: Output,
    json: Value,
    stderr: String,
    calls: usize,
}

/// `work --kind verify --json --no-state` on `dir` with this context setting. `None` when PAX is
/// not available here.
async fn verify(dir: &Path, profile: &str) -> Option<Ran> {
    let server = common::start(200, &completion(REQUEST), Duration::ZERO).await;
    let (url, d, p) = (server.url.clone(), dir.to_path_buf(), profile.to_string());
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_stack-agent"))
            .args([
                "work",
                "Verify that the project's tests pass.",
                "--kind",
                "verify",
            ])
            .args(["--json", "--no-state", "--context-profile", &p])
            .current_dir(&d)
            .env("CHIP_PROVIDER", "openai-compatible")
            .env("CHIP_MODEL", "mock-model")
            .env("CHIP_ENDPOINT", url)
            .env("CHIP_API_KEY", "sk-auto-secret-never-printed")
            .env("STACK_AGENT_DATA_DIR", d.join(".runtime"))
            .env_remove("PAX_BIN")
            .env_remove("CHIP_CONTEXT_PROFILE")
            // Cargo's build output goes to the project, as a user's would.
            .env_remove("CARGO_TARGET_DIR")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if out.status.code() == Some(3) && stderr.contains("PAX unavailable") {
        eprintln!("SKIPPED: {}", stderr.trim());
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let json = serde_json::from_str(stdout.lines().last().unwrap_or("null"))
        .unwrap_or_else(|e| panic!("not JSON ({e}): {stdout}{stderr}"));
    let calls = server.captured.lock().await.len();
    Some(Ran {
        out,
        json,
        stderr,
        calls,
    })
}

/// The verdict-bearing parts of a report, which must not depend on the context setting.
fn outcome(j: &Value) -> Value {
    serde_json::json!({
        "terminal_state": j["terminal_state"],
        "verified": j["verified"],
        "goal_satisfied": j["goal_satisfied"],
        "executions_by_capability": j["executions_by_capability"],
        "completion": j["completion"],
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn under_auto_a_passing_project_is_verified_with_one_model_call() {
    let dir = project("pass", "assert_eq!(2 + 2, 4);");
    let Some(auto) = verify(&dir, "auto").await else {
        return;
    };
    let j = &auto.json;
    assert_eq!(auto.out.status.code(), Some(0), "{j}\n{}", auto.stderr);
    assert_eq!(auto.calls, 1, "one model call, no retry");
    assert_eq!(j["goal_kind"], "verify");
    assert_eq!(j["terminal_state"], "completed", "{j}");
    assert_eq!(j["verified"], true, "{j}");
    assert_eq!(j["changed_writes"], 0);
    assert_eq!(j["executions_by_capability"]["pax.test"], 1, "{j}");
    assert_eq!(j["completion"]["tests"]["last"]["status"], "passed", "{j}");
    assert_eq!(j["completion"]["tests"]["last"]["tests"]["passed"], 1);
    assert_eq!(j["completion"]["tests"]["last"]["tests"]["failed"], 0);
    assert!(dir.join("target").exists(), "Cargo really ran, through PAX");
    assert!(!auto.stderr.contains("sk-auto-secret"));
    assert!(!j.to_string().contains("sk-auto-secret"));
    // Tokens come from the provider's own report.
    assert_eq!(
        j["token_usage"]["calls"][0]["usage"]["reported"]["input_tokens"], 11,
        "{j}"
    );
    // Which profile, and why: the request is the one the report says it was.
    let ctx = &j["token_usage"]["calls"][0]["context"];
    assert_eq!(ctx["policy"], "adaptive-v1");
    assert_eq!(ctx["profile"], "light", "{ctx}");
    assert!(
        ctx["reason"]
            .as_str()
            .unwrap()
            .starts_with("verify work with no failure observed"),
        "{ctx}"
    );
    assert_eq!(
        ctx["observations_condensed"], 0,
        "nothing to condense on the first request"
    );

    // The same project under replay reaches the same verdict, the same way.
    let _ = std::fs::remove_dir_all(dir.join("target"));
    let replay = verify(&dir, "replay").await.unwrap();
    assert_eq!(replay.out.status.code(), Some(0), "{}", replay.stderr);
    assert_eq!(replay.calls, 1);
    assert_eq!(outcome(&replay.json), outcome(j));
    assert_eq!(
        replay.json["token_usage"]["calls"][0]["context"]["policy"],
        "dedup-v1"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn under_auto_a_failing_project_is_not_verified() {
    let dir = project("fail", "assert_eq!(2 + 2, 5);");
    let Some(auto) = verify(&dir, "auto").await else {
        return;
    };
    let j = &auto.json;
    assert_ne!(auto.out.status.code(), Some(0), "{j}");
    assert_eq!(j["verified"], false, "{j}");
    assert_ne!(j["terminal_state"], "completed", "{j}");
    assert_eq!(j["completion"]["tests"]["last"]["status"], "failed", "{j}");
    // After the failure the next request is not Light, and the failure is sent in full.
    let calls = j["token_usage"]["calls"].as_array().unwrap();
    for c in &calls[1..] {
        assert_ne!(c["context"]["profile"], "light", "{c}");
        assert_eq!(c["context"]["observations_condensed"], 0, "{c}");
    }
    let replay = verify(&dir, "replay").await.unwrap();
    assert_eq!(outcome(&replay.json), outcome(j));
    assert_eq!(replay.out.status.code(), auto.out.status.code());
}
