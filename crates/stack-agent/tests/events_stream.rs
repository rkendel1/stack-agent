//! `work --events`: the binary as a host runs it, against a mock OpenAI-compatible server. The
//! stream must show the run while it happens and must equal what the loop recorded.

#[path = "common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-events-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
    dir
}

fn reply() -> String {
    serde_json::json!({
        "id": "r",
        "choices": [{"message": {"role": "assistant",
            "content": "{\"decision\":\"block\",\"reason\":\"nothing to do\"}"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 5, "completion_tokens": 5},
    })
    .to_string()
}

fn command(url: &str, dir: &Path, events: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_stack-agent"));
    c.args([
        "work",
        "Summarize src/lib.rs.",
        "--kind",
        "inspect",
        "--json",
        "--no-state",
    ])
    .args(["--max-turns", "3", "--events"])
    .arg(events)
    .current_dir(dir)
    .env_remove("PAX_BIN")
    .env("CHIP_PROVIDER", "openai-compatible")
    .env("CHIP_MODEL", "mock-model")
    .env("CHIP_ENDPOINT", url)
    .env("CHIP_API_KEY", "sk-events-secret-never-printed");
    c
}

fn lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn events_are_written_as_they_happen_and_equal_the_reports_count() {
    let dir = fixture("live");
    let events = dir.join("events.jsonl");
    // The model call takes 3 s: the stream must already show the start of the work meanwhile.
    let server = common::start(200, &reply(), Duration::from_secs(3)).await;
    let (url, d, e) = (server.url.clone(), dir.clone(), events.clone());
    let out = tokio::task::spawn_blocking(move || {
        let mut child = command(&url, &d, &e)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let begun = Instant::now();
        let mut seen_while_running = Vec::new();
        while begun.elapsed() < Duration::from_secs(2) {
            seen_while_running = lines(&e);
            if !seen_while_running.is_empty() && child.try_wait().unwrap().is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let running = child.try_wait().unwrap().is_none();
        (
            child.wait_with_output().unwrap(),
            seen_while_running,
            running,
        )
    })
    .await
    .unwrap();
    let (out, early, still_running) = out;
    assert!(
        still_running,
        "the run ended before the stream could be observed"
    );
    assert_eq!(early[0]["event"]["kind"], "WorkStarted", "{early:?}");

    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let all = lines(&events);
    for (i, line) in all.iter().enumerate() {
        assert_eq!(line["schema"], "chip.work-event.v1");
        assert_eq!(line["seq"], i as u64, "no gaps");
    }
    assert_eq!(
        all.last().unwrap()["event"]["kind"],
        "WorkBlocked",
        "{all:?}"
    );
    assert_eq!(
        report["event_stream"]["lines"],
        all.len() as u64,
        "{report}"
    );
    assert_eq!(report["event_stream"]["failed"], serde_json::Value::Null);
    // Neither the key nor model prose is in the stream.
    let text = std::fs::read_to_string(&events).unwrap();
    assert!(!text.contains("sk-events-secret"));
}

#[test]
fn an_unwritable_destination_stops_the_run_before_any_model_call() {
    let dir = fixture("bad");
    let out = command(
        "http://127.0.0.1:9/v1/chat/completions",
        &dir,
        Path::new("/nonexistent-dir/e.jsonl"),
    )
    .output()
    .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("--events"));
}
