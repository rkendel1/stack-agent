//! The run interface: where the goal comes from and which directory the work is about. The binary,
//! against a mock model; the reply is a bare `block` so every run ends at once and the report says
//! what the model was sent.

#[path = "common/mod.rs"]
mod common;

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

fn project(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-run-iface-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/marker_file.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
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
    code: Option<i32>,
    stderr: String,
    report: serde_json::Value,
}

async fn run(cwd: PathBuf, args: Vec<String>, stdin: Option<String>) -> (Ran, Vec<String>) {
    let server = common::start(200, &reply(), Duration::ZERO).await;
    let url = server.url.clone();
    let out = tokio::task::spawn_blocking(move || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_stack-agent"))
            .arg("work")
            .args(&args)
            .args([
                "--kind",
                "inspect",
                "--json",
                "--no-state",
                "--max-turns",
                "2",
            ])
            .current_dir(cwd)
            .env_remove("PAX_BIN")
            .env("CHIP_PROVIDER", "openai-compatible")
            .env("CHIP_MODEL", "mock-model")
            .env("CHIP_ENDPOINT", url)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        } else {
            drop(child.stdin.take());
        }
        child.wait_with_output().unwrap()
    })
    .await
    .unwrap();
    let bodies = server
        .captured
        .lock()
        .await
        .iter()
        .map(|c| c.body.clone())
        .collect();
    (
        Ran {
            code: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            report: serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null),
        },
        bodies,
    )
}

fn long_goal() -> String {
    format!("Summarize the project. {}", "Be precise. ".repeat(250))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_goal_from_a_file_needs_the_raised_limit_and_then_reaches_the_model_whole() {
    let goal = long_goal();
    assert!(goal.len() > 2000 && goal.len() < 32 * 1024);
    let dir = project("file");
    let file = dir.join("goal.txt");
    std::fs::write(&file, &goal).unwrap();
    let file_arg = file.display().to_string();

    let (refused, bodies) = run(
        dir.clone(),
        vec!["--goal-file".into(), file_arg.clone()],
        None,
    )
    .await;
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(refused.stderr.contains("at most 2000 bytes") && bodies.is_empty());

    let args = vec![
        "--goal-file".into(),
        file_arg,
        "--max-goal-bytes".into(),
        "8000".into(),
    ];
    let (ok, bodies) = run(dir, args, None).await;
    assert_eq!(ok.report["terminal_state"], "blocked", "{}", ok.stderr);
    assert!(
        bodies[0].contains("Be precise. Be precise."),
        "the whole goal was sent"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_goal_may_come_from_standard_input_and_only_from_one_place() {
    let dir = project("stdin");
    let (ok, bodies) = run(
        dir.clone(),
        vec!["--goal-stdin".into()],
        Some("Summarize via stdin.\n".into()),
    )
    .await;
    assert_eq!(ok.report["terminal_state"], "blocked", "{}", ok.stderr);
    assert!(bodies[0].contains("Summarize via stdin."));
    let (both, bodies) = run(
        dir,
        vec!["a goal".into(), "--goal-stdin".into()],
        Some("x".into()),
    )
    .await;
    assert_eq!(both.code, Some(2));
    assert!(bodies.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn project_selects_the_directory_the_work_is_about() {
    let dir = project("elsewhere");
    let elsewhere = std::env::temp_dir();
    let args = vec![
        "Look around.".into(),
        "--project".into(),
        dir.display().to_string(),
    ];
    let (ok, _) = run(elsewhere.clone(), args, None).await;
    assert_eq!(ok.report["terminal_state"], "blocked", "{}", ok.stderr);
    let (bad, _) = run(
        elsewhere,
        vec![
            "Look around.".into(),
            "--project".into(),
            "/definitely/not/here".into(),
        ],
        None,
    )
    .await;
    assert_eq!(bad.code, Some(2), "{}", bad.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_total_token_budget_stops_the_work_before_the_next_call_and_the_report_says_so() {
    let list =
        r#"{"decision":"request_capability","capability":"project.list","inputs":{"path":"."}}"#;
    let body = serde_json::json!({
        "id": "r",
        "choices": [{"message": {"role": "assistant", "content": list}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 3, "completion_tokens": 2},
    })
    .to_string();
    let server = common::start(200, &body, Duration::ZERO).await;
    let (url, dir) = (server.url.clone(), project("budget"));
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_stack-agent"))
            .args([
                "work",
                "Look around.",
                "--kind",
                "inspect",
                "--json",
                "--no-state",
            ])
            .args(["--max-turns", "10", "--max-total-tokens", "5"])
            .current_dir(dir)
            .env_remove("PAX_BIN")
            .env_remove("CHIP_CALL_TIMEOUT_SECS")
            .env("CHIP_PROVIDER", "openai-compatible")
            .env("CHIP_MODEL", "mock-model")
            .env("CHIP_ENDPOINT", url)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        server.captured.lock().await.len(),
        1,
        "no second model call"
    );
    assert_eq!(report["ended_by"], "token_budget", "{report}");
    assert_eq!(report["terminal_state"], "cancelled");
    assert_eq!(report["budget"]["input_tokens"], 3);
    assert_eq!(report["budget"]["output_tokens"], 2);
    assert_eq!(report["budget"]["exhausted"], true);
    assert_eq!(report["verified"], false);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--call-timeout-secs"),
        "the local-model note"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cost_cap_needs_operator_supplied_prices() {
    let (bad, _) = run(
        project("cost"),
        vec!["x".into(), "--max-cost-usd".into(), "1".into()],
        None,
    )
    .await;
    assert_eq!(bad.code, Some(2), "{}", bad.stderr);
    assert!(bad.stderr.contains("price"));
}

async fn calls_for_garbage(extra: &[&str]) -> (usize, serde_json::Value) {
    let body = serde_json::json!({
        "id": "r",
        "choices": [{"message": {"role": "assistant", "content": "this is not a decision"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1},
    })
    .to_string();
    let server = common::start(200, &body, Duration::ZERO).await;
    let (url, dir) = (server.url.clone(), project("garbage"));
    let extra: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_stack-agent"))
            .args([
                "work",
                "Look around.",
                "--kind",
                "inspect",
                "--json",
                "--no-state",
            ])
            .args(["--max-turns", "20"])
            .args(&extra)
            .current_dir(dir)
            .env_remove("PAX_BIN")
            .env("CHIP_PROVIDER", "openai-compatible")
            .env("CHIP_MODEL", "mock-model")
            .env("CHIP_ENDPOINT", url)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let report = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    (server.captured.lock().await.len(), report)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_rejected_reply_limit_is_tunable_and_the_report_shows_it() {
    let (default_calls, default_report) = calls_for_garbage(&[]).await;
    assert_eq!(
        default_calls, 3,
        "two answered with a diagnostic, the third ends the work"
    );
    assert_eq!(default_report["tuning"]["is_default"], true);
    let (more, report) = calls_for_garbage(&["--max-rejected-replies", "4"]).await;
    assert_eq!(more, 5);
    assert_eq!(report["tuning"]["max_rejected_replies"], 4);
    assert_eq!(report["tuning"]["is_default"], false);
    assert_eq!(report["verified"], false);
}

async fn leaves_dot_dir(extra: &[&str], tag: &str) -> bool {
    let dir = project(tag);
    let data =
        std::env::temp_dir().join(format!("chip-run-iface-data-{}-{tag}", std::process::id()));
    let server = common::start(200, &reply(), Duration::ZERO).await;
    let (url, d, dd) = (server.url.clone(), dir.clone(), data.clone());
    let extra: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_stack-agent"))
            .args([
                "work",
                "Look around.",
                "--kind",
                "inspect",
                "--json",
                "--no-state",
            ])
            .args(["--max-turns", "2"])
            .args(&extra)
            .current_dir(d)
            .env_remove("PAX_BIN")
            .env("STACK_AGENT_DATA_DIR", dd)
            .env("CHIP_PROVIDER", "openai-compatible")
            .env("CHIP_MODEL", "mock-model")
            .env("CHIP_ENDPOINT", url)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    dir.join(".stack-agent").exists()
}

#[tokio::test(flavor = "multi_thread")]
async fn no_project_files_keeps_stack_agents_own_files_out_of_the_checkout() {
    assert!(
        leaves_dot_dir(&[], "dot-control").await,
        "the control run must write .stack-agent"
    );
    assert!(!leaves_dot_dir(&["--no-project-files"], "dot-off").await);
}

#[tokio::test(flavor = "multi_thread")]
async fn replies_are_recorded_redacted_and_usage_is_estimated_only_when_the_server_reports_none() {
    // The model echoes a key-shaped string and the run's own key; neither may reach the file.
    let content =
        r#"{"decision":"block","reason":"saw sk-abcdefghijklmnopqrstuv and key-in-the-env-1234"}"#;
    let body = serde_json::json!({
        "id": "r",
        "choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
    })
    .to_string();
    let server = common::start(200, &body, Duration::ZERO).await;
    let (url, dir) = (server.url.clone(), project("replies"));
    let log = dir.join("replies.jsonl");
    let log_arg = log.display().to_string();
    let d = dir.clone();
    let out = tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_stack-agent"))
            .args([
                "work",
                "Look around.",
                "--kind",
                "inspect",
                "--json",
                "--no-state",
            ])
            .args([
                "--max-turns",
                "2",
                "--estimate-usage",
                "--record-replies",
                &log_arg,
            ])
            .current_dir(d)
            .env_remove("PAX_BIN")
            .env("CHIP_API_KEY", "key-in-the-env-1234")
            .env("CHIP_PROVIDER", "openai-compatible")
            .env("CHIP_MODEL", "mock-model")
            .env("CHIP_ENDPOINT", url)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(
        !text.contains("sk-abcdef") && !text.contains("key-in-the-env-1234"),
        "{text}"
    );
    assert!(text.contains("[REDACTED]") && text.contains("chip.reply.v1"));
    let line: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(line["redaction"], "best-effort");
    assert_eq!(report["usage_estimate"]["estimated"], true);
    assert_eq!(report["usage_estimate"]["calls_estimated"], 1);
    assert!(report["usage_estimate"]["output_tokens"].as_u64().unwrap() > 0);
    // The report is not redacted: it carries the model's own `reason`, which this mock made echo the key.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&log).unwrap().permissions().mode() & 0o077,
            0,
            "owner-only"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lease_shorter_than_one_model_call_is_refused_up_front() {
    let args = vec![
        "x".into(),
        "--lease-secs".into(),
        "10".into(),
        "--call-timeout-secs".into(),
        "30".into(),
    ];
    let (ran, bodies) = run(project("lease"), args, None).await;
    assert_eq!(ran.code, Some(2), "{}", ran.stderr);
    assert!(
        ran.stderr.contains("--lease-secs 10") && ran.stderr.contains("30 s"),
        "{}",
        ran.stderr
    );
    assert!(bodies.is_empty(), "nothing was sent");
}
