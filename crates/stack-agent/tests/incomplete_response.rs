//! A model reply that did not finish is never a decision.
//!
//! `chip work --kind inspect`, the binary as a user runs it, against a mock OpenAI-compatible server
//! that streams a scripted reply with a scripted finish reason. Everything between the two is the
//! product's: the native provider, FX's normalization, Chip's decision boundary, the work loop, the
//! audit and the report. Nothing here depends on what a model's repetitive output says.
//!
//! The captured failure these pin (google/gemma-4-26b-a4b-qat behind LM Studio): HTTP 200, a stream
//! of `{"decision": "complete", "summary": "...` that never closes, `finish_reason: "length"` at the
//! 2048-token cap.

#[path = "common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-incomplete-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"incomplete_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
    dir
}

fn pax_installed() -> bool {
    Command::new("pax")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A chat completion whose content and finish reason are as scripted.
fn completion(content: &str, finish_reason: &str) -> String {
    serde_json::json!({
        "id": "resp-incomplete",
        "choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": finish_reason}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 2047},
    })
    .to_string()
}

struct Ran {
    out: Output,
    /// `Null` when the run printed only an error (a provider that could not be reached).
    json: serde_json::Value,
    requests: Vec<serde_json::Value>,
}

async fn work(tag: &str, status: u16, body: &str, extra: &[&str]) -> Ran {
    let dir = fixture(tag);
    let server = common::start(status, body, Duration::ZERO).await;
    let (url, d, extra): (String, PathBuf, Vec<String>) = (
        server.url.clone(),
        dir.clone(),
        extra.iter().map(|s| s.to_string()).collect(),
    );
    let out = tokio::task::spawn_blocking(move || run(&url, &d, &extra))
        .await
        .unwrap();
    let requests = server
        .captured
        .lock()
        .await
        .iter()
        .map(|c| serde_json::from_str(&c.body).unwrap())
        .collect();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    Ran {
        out,
        json,
        requests,
    }
}

fn run(url: &str, dir: &Path, extra: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stack-agent"))
        .args([
            "work",
            "Summarize what src/lib.rs does.",
            "--kind",
            "inspect",
            "--json",
            "--max-turns",
            "3",
        ])
        .args(extra)
        .current_dir(dir)
        .env_remove("PAX_BIN")
        .env("CHIP_PROVIDER", "openai-compatible")
        .env("CHIP_MODEL", "mock-model")
        .env("CHIP_ENDPOINT", url)
        .env("CHIP_API_KEY", "sk-incomplete-secret-never-printed")
        .output()
        .unwrap()
}

/// What must hold after any reply that is not a usable decision or answer: nothing happened.
fn assert_nothing_became_real(r: &Ran, terminal: &str) {
    let j = &r.json;
    assert!(!j.is_null(), "{}", String::from_utf8_lossy(&r.out.stderr));
    assert_eq!(j["executions_by_capability"], serde_json::json!({}), "{j}");
    assert_eq!(j["capability_requests"], 0, "{j}");
    assert_eq!(j["reads"], 0);
    assert_eq!(j["writes"], 0);
    assert_eq!(j["grounded"], false, "an unfinished answer grounds nothing");
    assert_eq!(j["verified"], false);
    assert_ne!(j["goal_satisfied"], true, "{j}");
    assert!(j["answer"].is_null(), "{j}");
    assert_eq!(j["terminal_state"], terminal, "{j}");
    assert_eq!(j["audit"]["clean"], true, "{}", j["audit"]);
    assert_eq!(j["audit"]["false_completions"], 0);
    // It is a model failure, not the context budget.
    assert_eq!(j["context"]["context_limit_rejections"], 0);
}

const PARTIAL_DECISION: &str = r#"{"decision": "request_capability", "capability": "project.read", "inputs": {"path": "src/li"#;
const PARTIAL_ANSWER: &str = r#"{"decision": "complete", "summary": "The crate has multi-group membership in multi-group membership in multi-group membership in multi-group membership in multi-group membersh"#;
const COMPLETE_DECISION: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;

#[tokio::test]
async fn a_reply_cut_off_at_the_output_cap_is_a_typed_failure_and_nothing_executes() {
    if !pax_installed() {
        eprintln!("SKIPPED: pax is not installed");
        return;
    }
    for (tag, content) in [("decision", PARTIAL_DECISION), ("answer", PARTIAL_ANSWER)] {
        let r = work(tag, 200, &completion(content, "length"), &[]).await;
        assert_eq!(r.out.status.code(), Some(4), "{tag}: {}", r.json);
        assert_nothing_became_real(&r, "failed");
        let failures = r.json["model_failures"].as_array().unwrap();
        assert_eq!(failures.len(), 1, "{tag}: {failures:?}");
        assert_eq!(failures[0]["kind"], "length_limited", "{tag}");
        // The provider confirmed it answered; that is not confirmation that the answer is whole.
        assert_eq!(failures[0]["delivery"], "confirmed_response", "{tag}");
        assert_eq!(r.json["delivery"], "confirmed_response", "{tag}");
        assert!(
            r.json["outcome_reason"]
                .as_str()
                .unwrap()
                .contains("length_limited"),
            "{tag}"
        );
        // One request, never replayed with a different prompt or limit.
        assert_eq!(r.requests.len(), 1, "{tag}: no hidden retry");
        assert_eq!(r.json["context"]["provider_failures"], 1, "{tag}");
        assert_eq!(r.json["context"]["requests_sent"], 1, "{tag}");
    }
}

#[tokio::test]
async fn a_cut_off_reply_is_refused_even_when_what_arrived_parses_as_a_decision() {
    if !pax_installed() {
        eprintln!("SKIPPED: pax is not installed");
        return;
    }
    // The cap landed exactly after the closing brace. Chip cannot know that, and does not guess.
    let r = work("whole", 200, &completion(COMPLETE_DECISION, "length"), &[]).await;
    assert_eq!(r.json["model_failures"][0]["kind"], "length_limited");
    assert_nothing_became_real(&r, "failed");
}

#[tokio::test]
async fn a_finished_but_unfinished_looking_reply_is_malformed_not_length_limited() {
    if !pax_installed() {
        eprintln!("SKIPPED: pax is not installed");
        return;
    }
    // The provider says `stop`: the reply is complete, and it is not a decision. This is Chip's
    // verdict on the content, with no model failure recorded, and nothing is repaired or run.
    // It is a malformed reply, so the model is told why and asked again, in recorded turns of
    // their own, up to the limit (`chip_core::DEFAULT_MALFORMED_REPLY_LIMIT`); the mock answers
    // every request the same way, so the work then fails.
    for (tag, content) in [
        ("partial-stop", PARTIAL_DECISION),
        ("prose", "I will now read src/lib.rs."),
    ] {
        let r = work(tag, 200, &completion(content, "stop"), &[]).await;
        assert_nothing_became_real(&r, "failed");
        assert_eq!(r.json["model_failures"], serde_json::json!([]), "{tag}");
        let reason = r.json["outcome_reason"].as_str().unwrap();
        assert!(reason.contains("not a valid decision"), "{tag}: {reason}");
        assert!(
            reason.contains("3 rejected decisions in a row; the limit is 2"),
            "{tag}: {reason}"
        );
        assert_eq!(r.json["malformed_replies"], 3, "{tag}");
        let limit = chip_core::DEFAULT_MALFORMED_REPLY_LIMIT;
        assert_eq!(r.requests.len(), 1 + limit, "{tag}: bounded, never more");
        // Each recovery request says why the last reply was rejected; the first does not.
        for (n, request) in r.requests.iter().enumerate() {
            let said = request.to_string().contains("your reply was rejected");
            assert_eq!(said, n > 0, "{tag}: request {n}");
        }
    }
}

#[tokio::test]
async fn a_provider_error_is_not_mistaken_for_an_incomplete_reply() {
    if !pax_installed() {
        eprintln!("SKIPPED: pax is not installed");
        return;
    }
    let r = work("http500", 500, r#"{"error":{"message":"boom"}}"#, &[]).await;
    // No report is printed for a provider that never produced a reply; the error names the kind.
    let stderr = String::from_utf8_lossy(&r.out.stderr);
    assert!(stderr.contains("provider_unavailable"), "{stderr}");
    assert!(!stderr.contains("length_limited"), "{stderr}");
    assert_eq!(r.requests.len(), 1, "no hidden retry");
}

#[tokio::test]
async fn a_complete_valid_answer_with_no_evidence_is_refused_not_grounded() {
    if !pax_installed() {
        eprintln!("SKIPPED: pax is not installed");
        return;
    }
    let answer =
        r#"{"decision":"complete","summary":"src/lib.rs defines one function returning 1."}"#;
    let r = work("ungrounded", 200, &completion(answer, "stop"), &[]).await;
    assert_nothing_became_real(&r, "blocked");
    assert_eq!(r.json["model_failures"], serde_json::json!([]));
    assert!(
        r.json["outcome_reason"]
            .as_str()
            .unwrap()
            .contains("completion refused"),
        "{}",
        r.json["outcome_reason"]
    );
}

#[tokio::test]
async fn a_complete_valid_decision_is_still_accepted_and_executed() {
    if !pax_installed() {
        eprintln!("SKIPPED: pax is not installed");
        return;
    }
    let r = work(
        "valid",
        200,
        &completion(COMPLETE_DECISION, "stop"),
        &["--max-turns", "2"],
    )
    .await;
    assert_eq!(r.json["model_failures"], serde_json::json!([]));
    assert!(
        r.json["capability_requests"].as_u64().unwrap() >= 1,
        "{}",
        r.json
    );
    assert!(
        r.json["executions_by_capability"]["project.read"]
            .as_u64()
            .unwrap()
            >= 1
    );
    assert_eq!(r.json["audit"]["clean"], true);
}

#[tokio::test]
async fn the_output_cap_and_response_contract_a_request_carries_have_one_documented_source() {
    if !pax_installed() {
        eprintln!("SKIPPED: pax is not installed");
        return;
    }
    let r = work("request", 200, &completion(PARTIAL_DECISION, "length"), &[]).await;
    let body = &r.requests[0];
    // `WORK_MAX_OUTPUT_TOKENS`, through FX, as the OpenAI-compatible `max_tokens`.
    assert_eq!(
        body["max_tokens"],
        chip_cli::software_work::WORK_MAX_OUTPUT_TOKENS
    );
    assert_eq!(body["max_tokens"], 2048);
    assert_eq!(body["temperature"], 0.0);
    assert_eq!(body["stream"], true);
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["model"], "mock-model");
    // The credential never travels in the body or comes back in the report.
    assert!(!body.to_string().contains("sk-incomplete-secret"));
    assert!(!r.json.to_string().contains("sk-incomplete-secret"));
}
