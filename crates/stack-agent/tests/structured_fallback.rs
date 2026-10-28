//! `--structured-output-fallback` and `--streaming off`, as a user runs the binary, against a plain
//! OpenAI-compatible server that refuses the response formats it does not support (as
//! llama-cpp-python does for `json_schema`). Mocked transport: it shows what the product sends and
//! what it does with each answer, not how any live server behaves.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-ladder-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
    dir
}

const OK: &str = r#"{"id":"r","model":"mock-model","choices":[{"index":0,"message":{"role":"assistant","content":"{\"decision\":\"block\",\"reason\":\"nothing to do\"}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":5}}"#;

/// Answers a request carrying a response format in `refused` with HTTP `status`, anything else with a
/// complete (non-streamed) reply. Returns the endpoint and every request body, in order.
fn server(refused: &'static [&'static str], status: u16) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let (head_end, length) = loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break (0, 0);
                }
                raw.extend_from_slice(&buf[..n]);
                if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&raw[..end]).to_lowercase();
                    let length = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    break (end + 4, length);
                }
            };
            if head_end == 0 {
                continue;
            }
            while raw.len() < head_end + length {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
            }
            let body = String::from_utf8_lossy(&raw[head_end..]).to_string();
            let asks_for = |kind: &str| body.contains(&format!("\"type\":\"{kind}\""));
            let reject = refused.iter().any(|r| asks_for(r));
            log.lock().unwrap().push(body);
            let (code, answer) = if reject {
                (
                    status,
                    r#"{"error":{"message":"response_format is not supported"}}"#,
                )
            } else {
                (200, OK)
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                answer.len()
            );
        }
    });
    (url, seen)
}

fn run(tag: &str, url: &str, extra: &[&str]) -> (serde_json::Value, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_stack-agent"))
        .args([
            "work",
            "Summarize src/lib.rs.",
            "--kind",
            "inspect",
            "--json",
            "--no-state",
        ])
        .args(["--max-turns", "2", "--streaming", "off"])
        .args(extra)
        .current_dir(fixture(tag))
        .env_remove("PAX_BIN")
        .env_remove("OPENAI_API_KEY")
        .env("CHIP_PROVIDER", "openai-compatible")
        .env("CHIP_MODEL", "mock-model")
        .env("CHIP_ENDPOINT", url)
        .output()
        .unwrap();
    (
        serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

fn formats(seen: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|b| {
            let v: serde_json::Value = serde_json::from_str(b).unwrap();
            assert_eq!(v["stream"], false, "--streaming off sends stream:false");
            v.get("response_format").map_or("none".to_string(), |f| {
                f["type"].as_str().unwrap().to_string()
            })
        })
        .collect()
}

#[test]
fn without_the_ladder_a_refused_format_is_a_failure_and_nothing_is_substituted() {
    let (url, seen) = server(&["json_schema"], 500);
    let (report, stderr) = run("off", &url, &[]);
    assert_eq!(
        formats(&seen),
        ["json_schema"],
        "one request, no second attempt: {stderr}"
    );
    assert_ne!(report["terminal_state"], "blocked", "{report}");
    assert!(
        report
            .get("structured_output_fallback")
            .is_none_or(|v| v.is_null())
    );
}

#[test]
fn with_the_ladder_a_refused_schema_steps_down_to_json_object_and_says_so() {
    let (url, seen) = server(&["json_schema"], 500);
    let (report, stderr) = run("on", &url, &["--structured-output-fallback", "on"]);
    assert_eq!(formats(&seen), ["json_schema", "json_object"], "{stderr}");
    assert_eq!(
        report["terminal_state"], "blocked",
        "the reply was a usable decision: {report}"
    );
    let ladder = &report["structured_output_fallback"];
    assert_eq!(ladder["used"], "json-object");
    assert_eq!(ladder["steps"][0]["from"], "schema");
    assert_eq!(ladder["steps"][0]["status"], 500);
    assert_eq!(
        report["generation"]["structured_output_fallback"]["value"],
        true
    );
    assert!(
        stderr.contains("refused the `schema` structured-output request"),
        "{stderr}"
    );
}

#[test]
fn the_ladder_ends_at_no_response_format_and_never_loops() {
    let (url, seen) = server(&["json_schema", "json_object"], 400);
    let (report, _) = run("none", &url, &["--structured-output-fallback", "on"]);
    assert_eq!(formats(&seen), ["json_schema", "json_object", "none"]);
    assert_eq!(report["structured_output_fallback"]["used"], "none");
    assert_eq!(
        report["structured_output_fallback"]["steps"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn a_server_error_that_is_not_a_refusal_of_the_request_does_not_step_down() {
    let (url, seen) = server(&["json_schema"], 503);
    let (_, _) = run("503", &url, &["--structured-output-fallback", "on"]);
    assert_eq!(formats(&seen), ["json_schema"]);
}
