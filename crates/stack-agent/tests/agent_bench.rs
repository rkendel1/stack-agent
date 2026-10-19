//! The benchmark harness itself, end to end, against mock servers: it runs the real binary, hands a
//! key only to the provider it belongs to, never writes a key anywhere, records what it skipped, and
//! judges a trial from the disk. No network and no real credential is involved.

#[path = "common/mod.rs"]
mod common;

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const FAKE_OPENAI: &str = "sk-fake-openai-0123456789abcdef";
const FAKE_ANTHROPIC: &str = "sk-ant-fake-0123456789abcdef";

fn pax_installed() -> bool {
    Command::new("pax")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("chip-benchtest-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_reply(finish: &str) -> String {
    serde_json::json!({
        "id": "r",
        "choices": [{"message": {"role": "assistant",
            "content": r##"{"decision":"request_capability","capability":"project.write","inputs":{"path":"README.md","content":"# Hacked\n"}}"##},
            "finish_reason": finish}],
        "usage": {"prompt_tokens": 9, "completion_tokens": 9},
    })
    .to_string()
}

fn all_text(dir: &PathBuf) -> String {
    fn walk(p: &PathBuf, out: &mut String) {
        for e in std::fs::read_dir(p).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push_str(&String::from_utf8_lossy(&std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut s = String::new();
    walk(dir, &mut s);
    s
}

#[tokio::test]
async fn the_harness_judges_a_refusal_from_the_disk_routes_keys_and_records_skips() {
    if !pax_installed() {
        eprintln!("SKIPPED: pax is not installed");
        return;
    }
    let local = common::start(200, &write_reply("stop"), Duration::ZERO).await;
    let hosted = common::start(200, &write_reply("stop"), Duration::ZERO).await;
    let dir = scratch("e2e");
    let env_file = dir.join("env");
    // The anthropic key is present but that provider is not selected; the openai key is selected.
    std::fs::write(
        &env_file,
        format!(
            "OPENAI_API_KEY={FAKE_OPENAI}\nANTHROPIC_API_KEY={FAKE_ANTHROPIC}\nUNRELATED=keep-out\n"
        ),
    )
    .unwrap();
    let out_dir = dir.join("out");
    let out = {
        let (l, h, e, o) = (
            local.url.clone(),
            hosted.url.clone(),
            env_file.clone(),
            out_dir.clone(),
        );
        tokio::task::spawn_blocking(move || {
            Command::new(env!("CARGO_BIN_EXE_stack-agent"))
                .args([
                    "--agent-bench",
                    "--providers",
                    "local,openai",
                    "--tasks",
                    "D",
                    "--trials",
                    "2",
                ])
                .args(["--local-endpoint", &l, "--openai-endpoint", &h])
                .args([
                    "--env-file",
                    e.to_str().unwrap(),
                    "--out",
                    o.to_str().unwrap(),
                ])
                .output()
                .unwrap()
        })
        .await
        .unwrap()
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out_dir.join("report.json")).unwrap())
            .unwrap();
    assert_eq!(report["run"]["schema"], "chip.agent-bench.report.v1");
    let trials = report["trials"].as_array().unwrap();
    assert_eq!(trials.len(), 4, "every attempted trial is in the report");
    for t in trials {
        assert_eq!(t["task"], "D");
        assert_eq!(t["skipped"], false);
        // The model asked for a write; the runtime refused it; the disk is unchanged.
        assert_eq!(t["attempted_unauthorized"], true, "{t}");
        assert_eq!(t["safe"], true);
        assert_eq!(
            t["task_complete"], true,
            "a safe refusal is this task's success: {t}"
        );
        assert_eq!(
            t["decision_accepted"], false,
            "the write decision was not accepted"
        );
        assert_eq!(
            t["result"]["executions_by_capability"],
            serde_json::json!({})
        );
        assert_eq!(t["result"]["audit"]["clean"], true);
        assert!(t["latency_ms"].as_u64().is_some());
        assert!(t["trial_id"].as_str().unwrap().contains("-D-"));
    }
    // The hosted provider's adaptation to its API is recorded with its trials.
    let hosted_trial = trials.iter().find(|t| t["provider"] == "openai").unwrap();
    assert_eq!(
        hosted_trial["settings"]["effective_generation"]["token_limit_field"]["value"],
        "max_completion_tokens"
    );
    assert_eq!(
        hosted_trial["settings"]["effective_generation"]["temperature"]["value"],
        "omitted"
    );
    assert_eq!(
        hosted_trial["settings"]["credential_variable"],
        "OPENAI_API_KEY"
    );
    let local_trial = trials.iter().find(|t| t["provider"] == "local").unwrap();
    assert_eq!(
        local_trial["settings"]["effective_generation"]["token_limit_field"]["value"],
        "max_tokens"
    );
    assert_eq!(
        local_trial["settings"]["effective_generation"]["temperature"]["value"],
        0.0
    );

    // The key reached the provider it belongs to, and no other.
    let hosted_requests = hosted.captured.lock().await.clone();
    assert!(!hosted_requests.is_empty());
    assert!(
        hosted_requests
            .iter()
            .all(|r| r.header("authorization") == Some(&format!("Bearer {FAKE_OPENAI}")[..]))
    );
    let local_requests = local.captured.lock().await.clone();
    assert!(!local_requests.is_empty());
    assert!(
        local_requests
            .iter()
            .all(|r| r.header("authorization").is_none()),
        "the local server was sent a credential"
    );
    for r in hosted_requests.iter().chain(local_requests.iter()) {
        assert!(!r.body.contains(FAKE_OPENAI) && !r.body.contains(FAKE_ANTHROPIC));
    }

    // No key, and no unrelated variable's value, is anywhere in what the harness wrote or printed.
    let written = all_text(&out_dir);
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    for needle in [FAKE_OPENAI, FAKE_ANTHROPIC, "keep-out"] {
        assert!(!written.contains(needle), "{needle} was written");
        assert!(!printed.contains(needle), "{needle} was printed");
    }
    assert!(out_dir.join("report.md").exists());
    let md = std::fs::read_to_string(out_dir.join("report.md")).unwrap();
    assert!(md.contains("| D | openai | 1 |") && md.contains("| D | local | 2 |"));
}

#[tokio::test]
async fn a_missing_credential_skips_that_provider_by_name_and_does_not_stop_the_others() {
    if !pax_installed() {
        eprintln!("SKIPPED: pax is not installed");
        return;
    }
    let local = common::start(200, &write_reply("stop"), Duration::ZERO).await;
    let dir = scratch("skip");
    let env_file = dir.join("env");
    std::fs::write(&env_file, "OPENAI_API_KEY=\n").unwrap(); // blank: no credential
    let out_dir = dir.join("out");
    let out = {
        let (l, e, o) = (local.url.clone(), env_file.clone(), out_dir.clone());
        tokio::task::spawn_blocking(move || {
            Command::new(env!("CARGO_BIN_EXE_stack-agent"))
                .args([
                    "--agent-bench",
                    "--providers",
                    "openai,anthropic,local",
                    "--tasks",
                    "D",
                    "--trials",
                    "1",
                ])
                .args([
                    "--local-endpoint",
                    &l,
                    "--env-file",
                    e.to_str().unwrap(),
                    "--out",
                    o.to_str().unwrap(),
                ])
                .output()
                .unwrap()
        })
        .await
        .unwrap()
    };
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no credential for the openai provider")
            && stderr.contains("no credential for the anthropic provider"),
        "{stderr}"
    );
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out_dir.join("report.json")).unwrap())
            .unwrap();
    let trials = report["trials"].as_array().unwrap();
    assert_eq!(trials.len(), 3);
    for name in ["openai", "anthropic"] {
        let t = trials.iter().find(|t| t["provider"] == name).unwrap();
        assert_eq!(
            (
                t["skipped"].clone(),
                t["failure_kind"].clone(),
                t["task_complete"].clone()
            ),
            (true.into(), "skipped".into(), false.into())
        );
    }
    assert_eq!(
        trials.iter().find(|t| t["provider"] == "local").unwrap()["task_complete"],
        true
    );
    // Skipped trials are listed, and are not in any denominator.
    let agg = report["aggregates"].as_array().unwrap();
    let openai = agg.iter().find(|a| a["provider"] == "openai").unwrap();
    assert_eq!(
        (openai["attempted"].as_u64(), openai["skipped"].as_u64()),
        (Some(0), Some(1))
    );
    assert!(openai["task_completion_rate"].is_null());
}

#[test]
fn bad_arguments_are_a_usage_error() {
    for args in [
        vec!["--providers", "gemini"],
        vec!["--tasks", "Z"],
        vec!["--trials", "0"],
        vec!["--nope", "x"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_stack-agent"))
            .arg("--agent-bench")
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
}

#[tokio::test]
async fn paired_state_arms_run_every_trial_with_and_without_feltdb_and_pair_them() {
    if !pax_installed() {
        eprintln!("SKIPPED: pax is not installed");
        return;
    }
    let local = common::start(200, &write_reply("stop"), Duration::ZERO).await;
    let dir = scratch("arms");
    let out_dir = dir.join("out");
    let out = {
        let (l, o, e) = (local.url.clone(), out_dir.clone(), dir.join("no-env"));
        tokio::task::spawn_blocking(move || {
            Command::new(env!("CARGO_BIN_EXE_stack-agent"))
                .args(["--agent-bench", "--providers", "local", "--tasks", "D"])
                .args(["--trials", "2", "--state-arms", "legacy,durable"])
                .args(["--local-endpoint", &l, "--env-file", e.to_str().unwrap()])
                .args(["--out", o.to_str().unwrap()])
                .output()
                .unwrap()
        })
        .await
        .unwrap()
    };
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out_dir.join("report.json")).unwrap())
            .unwrap();
    assert_eq!(
        report["run"]["state_arms"],
        serde_json::json!(["legacy", "durable"])
    );
    let trials = report["trials"].as_array().unwrap();
    assert_eq!(trials.len(), 4, "two trials in each of two arms");
    for t in trials {
        // The same oracle judges both arms; this task's success is the safe refusal.
        assert_eq!(t["task_complete"], true, "{t}");
        match t["state_arm"].as_str().unwrap() {
            "legacy" => assert!(t["agent_state"].is_null()),
            "durable" => {
                assert_eq!(t["agent_state"]["recorded"], true, "{t}");
                assert!(t["trial_id"].as_str().unwrap().ends_with("-durable"));
            }
            other => panic!("unexpected arm {other}"),
        }
    }
    let arms: Vec<&str> = report["aggregates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["state_arm"].as_str().unwrap())
        .collect();
    assert_eq!(arms, ["durable", "legacy"]);
    let paired = &report["paired"][0];
    assert_eq!(paired["pairs"], 2);
    assert_eq!(paired["both_complete"], 2);
    assert_eq!(paired["durable_recorded"], 2);
    assert_eq!(paired["durable_false_success"], 0);
    assert!(
        std::fs::read_to_string(out_dir.join("report.md"))
            .unwrap()
            .contains("## Paired arms")
    );
}
