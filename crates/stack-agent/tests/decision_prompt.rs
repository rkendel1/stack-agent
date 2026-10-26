//! **Experimental** `--decision-prompt` / `CHIP_DECISION_PROMPT`: how the decision contract is
//! worded to the model. The binary, as a user runs it, against a mock OpenAI-compatible server that
//! records the request bodies it is sent.
//!
//! Pinned: `full` is the default and is what every release has sent; `compact` is chosen explicitly
//! (flag beats environment, like every generation setting) and an unknown value is refused before
//! anything is sent; the two modes offer the same capabilities and differ in the instructions only;
//! and a reply the contract rejects executes nothing in either mode.
//!
//! Not shown here: that `compact` helps any model. That is measured in `audit/compact-prompt/`.

#[path = "common/mod.rs"]
mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

fn fixture(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("chip-decision-prompt-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"prompt_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
    dir
}

fn completion(content: &str) -> String {
    serde_json::json!({
        "id": "resp-prompt",
        "choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7},
    })
    .to_string()
}

const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
/// The real capture: correct JSON with one closing brace too many.
const EXTRA_BRACE: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}}"#;
const INVENTED: &str = r#"{"decision":"request_capability","capability":"inspect_cargo_toml","inputs":{"file":"Cargo.toml"}}"#;

struct Ran {
    out: Output,
    json: serde_json::Value,
    bodies: Vec<serde_json::Value>,
}

async fn work(tag: &str, reply: &str, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let dir = fixture(tag);
    let server = common::start(200, &completion(reply), Duration::ZERO).await;
    let (url, d) = (server.url.clone(), dir);
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let out = tokio::task::spawn_blocking(move || run(&url, &d, &args, &env))
        .await
        .unwrap();
    let bodies = server
        .captured
        .lock()
        .await
        .iter()
        .map(|c| serde_json::from_str(&c.body).unwrap())
        .collect();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    Ran { out, json, bodies }
}

fn run(url: &str, dir: &Path, args: &[String], env: &[(String, String)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_stack-agent"));
    c.args([
        "work",
        "Summarize what src/lib.rs does.",
        "--kind",
        "inspect",
        "--json",
        "--max-turns",
        "2",
    ])
    .args(args)
    .current_dir(dir)
    .env_remove("PAX_BIN")
    .env_remove("CHIP_DECISION_PROMPT")
    .env("CHIP_PROVIDER", "openai-compatible")
    .env("CHIP_MODEL", "mock-model")
    .env("CHIP_ENDPOINT", url)
    .env("CHIP_API_KEY", "sk-prompt-secret-never-printed");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn stderr(r: &Ran) -> String {
    String::from_utf8_lossy(&r.out.stderr).into_owned()
}

/// The first system message: the goal's guidance and the question.
fn instructions(r: &Ran) -> String {
    r.bodies[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The capability ids the instructions offer, in the order listed: `project.xxx` tokens that are
/// followed by ` (` or ` -`, which is how both wordings list an available capability.
fn offered(text: &str) -> BTreeSet<String> {
    let listed = text.split("apabilities:").last().unwrap();
    listed
        .split(['\n', ';'])
        .filter_map(|e| {
            let id = e.trim().trim_start_matches("- ").split([' ', ':']).next()?;
            (id.starts_with("project.") && !id.contains('<')).then(|| id.to_string())
        })
        .collect()
}

// ---- configuration ------------------------------------------------------------------------------

#[tokio::test]
async fn full_is_the_default_and_is_reported_as_such() {
    let r = work("default", READ, &[], &[]).await;
    let g = &r.json["generation"]["decision_prompt"];
    assert_eq!(
        (g["value"].as_str(), g["source"].as_str()),
        (Some("full"), Some("default")),
        "{}",
        stderr(&r)
    );
    let text = instructions(&r);
    assert!(
        text.contains("Available capabilities:")
            && text.contains("Use only these existing decision shapes")
    );
}

#[tokio::test]
async fn compact_is_chosen_explicitly_by_environment_or_flag_and_the_flag_wins() {
    let r = work("env", READ, &[], &[("CHIP_DECISION_PROMPT", "compact")]).await;
    let g = &r.json["generation"]["decision_prompt"];
    assert_eq!(g["value"], "compact");
    assert_eq!(g["source"], "environment:CHIP_DECISION_PROMPT");

    let r = work("flag", READ, &["--decision-prompt", "compact"], &[]).await;
    let g = &r.json["generation"]["decision_prompt"];
    assert_eq!(
        (g["value"].as_str(), g["source"].as_str()),
        (Some("compact"), Some("flag:--decision-prompt"))
    );

    let r = work(
        "flagwins",
        READ,
        &["--decision-prompt", "full"],
        &[("CHIP_DECISION_PROMPT", "compact")],
    )
    .await;
    assert_eq!(r.json["generation"]["decision_prompt"]["value"], "full");
    assert!(instructions(&r).contains("Available capabilities:"));
}

#[tokio::test]
async fn an_unknown_value_is_refused_by_name_before_anything_is_sent() {
    for (args, env, names) in [
        (
            vec![],
            vec![("CHIP_DECISION_PROMPT", "tiny")],
            "CHIP_DECISION_PROMPT",
        ),
        (
            vec!["--decision-prompt", "Compact"],
            vec![],
            "--decision-prompt",
        ),
    ] {
        let r = work("bad", READ, &args, &env).await;
        let err = stderr(&r);
        // An invalid generation setting is a usage error, like every other one.
        assert_eq!(r.out.status.code(), Some(2), "{args:?} {env:?}: {err}");
        assert!(
            err.contains(names) && err.contains("`full`") && err.contains("`compact`"),
            "{err}"
        );
        assert!(r.bodies.is_empty(), "nothing was sent: {args:?} {env:?}");
    }
}

#[tokio::test]
async fn an_empty_variable_is_unset_as_it_is_for_every_setting() {
    let r = work("empty", READ, &[], &[("CHIP_DECISION_PROMPT", "")]).await;
    let g = &r.json["generation"]["decision_prompt"];
    assert_eq!(
        (g["value"].as_str(), g["source"].as_str()),
        (Some("full"), Some("default"))
    );
}

// ---- what the model is told ---------------------------------------------------------------------

#[tokio::test]
async fn compact_offers_the_same_capabilities_and_keeps_the_rules_of_evidence() {
    let full = work("same-full", READ, &[], &[]).await;
    let compact = work(
        "same-compact",
        READ,
        &[],
        &[("CHIP_DECISION_PROMPT", "compact")],
    )
    .await;
    let (f, c) = (instructions(&full), instructions(&compact));

    // The same capabilities, from the same offered set.
    let (fo, co) = (offered(&f), offered(&c));
    assert!(
        fo.contains("project.read") && fo.contains("project.list") && fo.contains("project.search"),
        "{fo:?}"
    );
    assert_eq!(fo, co, "the two wordings list the same capabilities");

    // Shorter, and the duplicated reply-format passage with its hard-coded example is gone.
    assert!(c.len() < f.len(), "compact {} vs full {}", c.len(), f.len());
    assert!(!c.contains("Use only these existing decision shapes"));
    assert!(!c.contains(r#"{"decision":"complete","summary":"<the answer"#));

    // Everything about evidence, grounding and the answer is kept.
    for kept in [
        "Failed operations are not successful evidence",
        "A path citation alone does not prove a claim",
        "Chip currently accepts completion when the summary cites a project-relative path",
        "read relevant source files before drawing conclusions about them",
        "do not treat your own statements as evidence",
        "rejected, not repaired",
        "at most 1024 bytes of UTF-8",
    ] {
        assert!(c.contains(kept), "compact lost: {kept}");
    }
}

#[tokio::test]
async fn nothing_but_the_instructions_differs_in_the_request() {
    let full = work("req-full", READ, &[], &[]).await;
    let compact = work(
        "req-compact",
        READ,
        &[],
        &[("CHIP_DECISION_PROMPT", "compact")],
    )
    .await;
    let (mut f, mut c) = (full.bodies[0].clone(), compact.bodies[0].clone());
    f["messages"][0]["content"] = serde_json::Value::Null;
    c["messages"][0]["content"] = serde_json::Value::Null;
    assert_eq!(
        f, c,
        "model, sampling, response schema, stream flag, observations and state are identical"
    );
}

// ---- the control boundary is the same -----------------------------------------------------------

#[tokio::test]
async fn a_valid_decision_reads_the_same_file_under_both_modes() {
    for env in [vec![], vec![("CHIP_DECISION_PROMPT", "compact")]] {
        let r = work("valid", READ, &[], &env).await;
        assert!(
            r.json["reads"].as_u64().unwrap() >= 1,
            "{env:?}: {}",
            stderr(&r)
        );
        assert_eq!(r.json["malformed_replies"], 0, "{env:?}");
    }
}

#[tokio::test]
async fn a_rejected_reply_executes_nothing_and_is_not_repaired_under_either_mode() {
    for reply in [EXTRA_BRACE, INVENTED] {
        for env in [vec![], vec![("CHIP_DECISION_PROMPT", "compact")]] {
            let r = work("reject", reply, &[], &env).await;
            assert_eq!(r.json["reads"], 0, "{reply} {env:?}: nothing was read");
            assert_eq!(r.json["lists"], 0, "{reply} {env:?}");
            assert_eq!(
                r.json["capability_requests"], 0,
                "{reply} {env:?}: no request was authorized"
            );
            assert_ne!(r.out.status.code(), Some(0), "{reply} {env:?}");
            assert!(
                r.json["malformed_replies"].as_u64().unwrap()
                    + r.json["invalid_decisions"].as_u64().unwrap()
                    >= 1,
                "{reply} {env:?}: it was counted as rejected"
            );
            // With the default recovery the work is told why and decides again; it never completes.
            assert!(
                matches!(
                    r.json["terminal_state"].as_str(),
                    Some("failed" | "limit_reached")
                ),
                "{reply} {env:?}: {}",
                r.json["terminal_state"]
            );
        }
    }
}
