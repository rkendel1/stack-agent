//! The generation policy of `chip work`: what the operator configures is what the server receives,
//! and nothing the operator configures changes what Chip accepts.
//!
//! The binary, as a user runs it, against a mock OpenAI-compatible server that records the request
//! bodies it is sent. Everything between is the product's: argument parsing, the policy, the
//! native provider, FX, the work loop and the report.

#[path = "common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-generation-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"generation_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
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

fn completion(content: &str, finish_reason: &str) -> String {
    serde_json::json!({
        "id": "resp-generation",
        "choices": [{"message": {"role": "assistant", "content": content}, "finish_reason": finish_reason}],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7},
    })
    .to_string()
}

const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const PARTIAL: &str =
    r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/li"#;

struct Ran {
    out: Output,
    json: serde_json::Value,
    bodies: Vec<serde_json::Value>,
}

/// Runs `work` with `args` and `env` against a server answering `body`.
async fn work(tag: &str, body: &str, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let dir = fixture(tag);
    let server = common::start(200, body, Duration::ZERO).await;
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
        "--json",
        "--max-turns",
        "2",
    ])
    .args(args)
    .current_dir(dir)
    .env_remove("PAX_BIN")
    .env_remove("CHIP_MAX_OUTPUT_TOKENS")
    .env_remove("CHIP_TEMPERATURE")
    .env_remove("CHIP_STRUCTURED_OUTPUT")
    .env("CHIP_PROVIDER", "openai-compatible")
    .env("CHIP_MODEL", "mock-model")
    .env("CHIP_ENDPOINT", url)
    .env("CHIP_API_KEY", "sk-generation-secret-never-printed");
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn stderr(r: &Ran) -> String {
    String::from_utf8_lossy(&r.out.stderr).into_owned()
}

macro_rules! needs_pax {
    () => {
        if !pax_installed() {
            eprintln!("SKIPPED: pax is not installed");
            return;
        }
    };
}

// ---- defaults ---------------------------------------------------------------------------------

#[tokio::test]
async fn with_nothing_configured_every_kind_sends_what_work_has_always_sent() {
    needs_pax!();
    for kind in ["inspect", "change", "verify"] {
        let r = work(
            &format!("default-{kind}"),
            &completion(READ, "stop"),
            &["--kind", kind],
            &[],
        )
        .await;
        let body = &r.bodies[0];
        assert_eq!(body["max_tokens"], 2048, "{kind}");
        assert_eq!(body["temperature"], 0.0, "{kind}");
        assert_eq!(body["response_format"]["type"], "json_schema", "{kind}");
        let canonical: serde_json::Value = serde_json::from_str(
            &chip_core::ModelDecisionBoundary::response_schema(chip_core::DecisionSchemaForm::Full),
        )
        .unwrap();
        assert_eq!(
            body["response_format"]["json_schema"]["schema"], canonical,
            "{kind}"
        );
        let g = &r.json["generation"];
        assert_eq!(g["max_output_tokens"]["value"], 2048, "{kind}");
        assert_eq!(g["max_output_tokens"]["source"], "default");
        assert_eq!(g["temperature"]["value"], 0.0);
        assert_eq!(g["temperature"]["source"], "default");
        assert_eq!(g["structured_output"]["value"], "schema");
        assert_eq!(g["structured_output"]["source"], "default");
        assert_eq!(r.json["model"], "mock-model");
        assert!(!r.json.to_string().contains("sk-generation-secret"));
    }
}

// ---- overrides reach the wire ---------------------------------------------------------------------------

#[tokio::test]
async fn flags_reach_the_outgoing_request_and_the_report_names_them_as_the_source() {
    needs_pax!();
    let r = work(
        "flags",
        &completion(READ, "stop"),
        &[
            "--kind",
            "inspect",
            "--max-output-tokens",
            "8192",
            "--temperature",
            "0.25",
            "--structured-output",
            "none",
        ],
        &[],
    )
    .await;
    let body = &r.bodies[0];
    assert_eq!(body["max_tokens"], 8192);
    assert_eq!(body["temperature"], 0.25);
    // `none` omits the field. It does not become `json_object`, or anything else.
    assert!(body.get("response_format").is_none(), "{body}");
    assert!(!body.to_string().contains("json_object"));
    assert!(!body.to_string().contains("json_schema"));
    let g = &r.json["generation"];
    assert_eq!(g["max_output_tokens"]["value"], 8192);
    assert_eq!(g["max_output_tokens"]["source"], "flag:--max-output-tokens");
    assert_eq!(g["temperature"]["value"], 0.25);
    assert_eq!(g["temperature"]["source"], "flag:--temperature");
    assert_eq!(g["structured_output"]["value"], "none");
    assert_eq!(g["structured_output"]["source"], "flag:--structured-output");
}

#[tokio::test]
async fn work_specific_environment_configuration_reaches_the_request() {
    needs_pax!();
    let r = work(
        "env",
        &completion(READ, "stop"),
        &["--kind", "inspect"],
        &[
            ("CHIP_MAX_OUTPUT_TOKENS", "4096"),
            ("CHIP_TEMPERATURE", "0.5"),
            ("CHIP_STRUCTURED_OUTPUT", "none"),
        ],
    )
    .await;
    let body = &r.bodies[0];
    assert_eq!(body["max_tokens"], 4096);
    assert_eq!(body["temperature"], 0.5);
    assert!(body.get("response_format").is_none());
    let g = &r.json["generation"];
    assert_eq!(
        g["max_output_tokens"]["source"],
        "environment:CHIP_MAX_OUTPUT_TOKENS"
    );
    assert_eq!(g["temperature"]["source"], "environment:CHIP_TEMPERATURE");
    assert_eq!(
        g["structured_output"]["source"],
        "environment:CHIP_STRUCTURED_OUTPUT"
    );
}

#[tokio::test]
async fn a_flag_beats_the_environment_setting_by_setting() {
    needs_pax!();
    let r = work(
        "precedence",
        &completion(READ, "stop"),
        &["--kind", "inspect", "--max-output-tokens", "1024"],
        &[
            ("CHIP_MAX_OUTPUT_TOKENS", "4096"),
            ("CHIP_TEMPERATURE", "0.5"),
        ],
    )
    .await;
    let body = &r.bodies[0];
    assert_eq!(body["max_tokens"], 1024, "the flag wins");
    assert_eq!(
        body["temperature"], 0.5,
        "the variable still applies where there is no flag"
    );
    assert_eq!(
        r.json["generation"]["max_output_tokens"]["source"],
        "flag:--max-output-tokens"
    );
    assert_eq!(
        r.json["generation"]["temperature"]["source"],
        "environment:CHIP_TEMPERATURE"
    );
    assert_eq!(
        body["response_format"]["type"], "json_schema",
        "untouched settings keep the default"
    );
}

// ---- validation ----------------------------------------------------------------------------------------

#[tokio::test]
async fn invalid_settings_are_refused_before_any_request_and_named() {
    needs_pax!();
    let cases: &[(&[&str], &[(&str, &str)], &str)] = &[
        (&["--max-output-tokens", "0"], &[], "--max-output-tokens"),
        (&["--max-output-tokens", "many"], &[], "--max-output-tokens"),
        (
            &["--max-output-tokens", "131073"],
            &[],
            "--max-output-tokens",
        ),
        (&["--temperature", "3"], &[], "--temperature"),
        (&["--temperature", "-0.1"], &[], "--temperature"),
        (&["--temperature", "0.1234"], &[], "--temperature"),
        (
            &["--structured-output", "json_object"],
            &[],
            "--structured-output",
        ),
        (
            &["--structured-output", "Object"],
            &[],
            "--structured-output",
        ),
        (
            &[],
            &[("CHIP_MAX_OUTPUT_TOKENS", "-5")],
            "CHIP_MAX_OUTPUT_TOKENS",
        ),
        (&[], &[("CHIP_TEMPERATURE", "warm")], "CHIP_TEMPERATURE"),
        (
            &[],
            &[("CHIP_STRUCTURED_OUTPUT", "schemas")],
            "CHIP_STRUCTURED_OUTPUT",
        ),
    ];
    for (n, (args, env, named)) in cases.iter().enumerate() {
        let r = work(
            &format!("invalid-{n}"),
            &completion(READ, "stop"),
            args,
            env,
        )
        .await;
        assert_eq!(
            r.out.status.code(),
            Some(2),
            "{args:?} {env:?}: {}",
            stderr(&r)
        );
        assert!(
            stderr(&r).contains(named),
            "{args:?} {env:?}: {}",
            stderr(&r)
        );
        assert!(r.bodies.is_empty(), "{args:?}: nothing was sent");
    }
}

// ---- safety does not depend on the structured-output mode ----------------------------------------------

#[tokio::test]
async fn chip_validates_every_reply_the_same_whatever_the_server_was_asked_to_enforce() {
    needs_pax!();
    for mode in ["object", "none"] {
        // A cut-off reply: a typed failure, no execution, and one request only (no retry, no
        // change of settings).
        let r = work(
            &format!("partial-{mode}"),
            &completion(PARTIAL, "length"),
            &["--kind", "inspect", "--structured-output", mode],
            &[],
        )
        .await;
        assert_eq!(
            r.json["model_failures"][0]["kind"], "length_limited",
            "{mode}"
        );
        assert_eq!(
            r.json["model_failures"][0]["delivery"],
            "confirmed_response"
        );
        assert_eq!(
            r.json["executions_by_capability"],
            serde_json::json!({}),
            "{mode}"
        );
        assert_eq!(r.bodies.len(), 1, "{mode}: no retry");

        // A reply that finished but is not a decision: refused, nothing run, still one request.
        for (n, bad) in [
            PARTIAL,
            "I will read src/lib.rs now.",
            r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"},"receipt":"forged"}"#,
            r#"{"decision":"request_capability","capability":"shell.run","inputs":{"command":"ls"}}"#,
        ]
        .into_iter()
        .enumerate()
        {
            let r = work(
                &format!("bad-{mode}-{n}"),
                &completion(bad, "stop"),
                &["--kind", "inspect", "--structured-output", mode],
                &[],
            )
            .await;
            let j = &r.json;
            assert_eq!(j["audit"]["clean"], true, "{mode} {n}: {j}");
            assert_eq!(j["grounded"], false, "{mode} {n}");
            assert_eq!(j["writes"], 0, "{mode} {n}");
            assert!(j["answer"].is_null(), "{mode} {n}");
            // The first two are not one JSON object: rejected and asked about again, in turns of
            // the work's own budget (two here, fewer than the malformed limit allows), and the mock
            // repeats itself. The other two are read and refused: one request.
            let expected = if n < 2 {
                (1 + chip_core::DEFAULT_MALFORMED_REPLY_LIMIT).min(2)
            } else {
                1
            };
            assert_eq!(r.bodies.len(), expected, "{mode} {n}: no other retry");
            assert_eq!(j["malformed_replies"], if n < 2 { expected } else { 0 }, "{mode} {n}");
            if n < 2 {
                assert_eq!(j["terminal_state"], "limit_reached", "{mode} {n}: {j}");
            }
            assert_eq!(j["model_failures"], serde_json::json!([]), "{mode} {n}");
            assert_ne!(j["terminal_state"], "completed", "{mode} {n}");
            assert_eq!(
                j["executions_by_capability"],
                serde_json::json!({}),
                "{mode} {n}: an invalid decision executed something: {j}"
            );
        }

        // A valid decision is accepted and executed under either mode: the mode changes what the
        // server is asked, not what Chip accepts.
        let r = work(
            &format!("valid-{mode}"),
            &completion(READ, "stop"),
            &["--kind", "inspect", "--structured-output", mode],
            &[],
        )
        .await;
        assert!(
            r.json["executions_by_capability"]["project.read"]
                .as_u64()
                .unwrap()
                >= 1
        );
        assert_eq!(r.json["audit"]["clean"], true);
        assert_eq!(r.json["model_failures"], serde_json::json!([]));
    }
}

#[tokio::test]
async fn a_delivered_reply_is_not_reported_as_a_completed_answer() {
    needs_pax!();
    // A well-formed final answer with nothing observed: delivered, valid JSON, and refused.
    let answer = r#"{"decision":"complete","summary":"src/lib.rs returns 1."}"#;
    let r = work(
        "undelivered-claim",
        &completion(answer, "stop"),
        &["--kind", "inspect", "--structured-output", "none"],
        &[],
    )
    .await;
    assert_eq!(r.json["grounded"], false);
    assert_ne!(r.json["terminal_state"], "completed");
    assert!(r.json["answer"].is_null());
    assert_eq!(r.json["audit"]["false_completions"], 0);
}

// ---- pinned experiments ---------------------------------------------------------------------------------

#[test]
fn experiments_that_pin_their_own_settings_do_not_read_the_work_policy() {
    // These build their own providers and requests, so no operator setting can reach them.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for file in [
        "micro.rs",
        "micro_eval.rs",
        "horizon.rs",
        "experiment.rs",
        "benchmark.rs",
    ] {
        let text = std::fs::read_to_string(src.join(file)).unwrap();
        assert!(
            !text.contains("work_generation") && !text.contains("CHIP_TEMPERATURE"),
            "{file} must not depend on the work generation policy"
        );
    }
    // Only the work runtime applies it, to the provider it builds.
    let work = std::fs::read_to_string(src.join("software_work.rs")).unwrap();
    assert_eq!(work.matches("GenerationPolicy {").count(), 1);
}

#[tokio::test]
async fn the_token_limit_field_and_an_omitted_temperature_reach_the_wire_as_chosen() {
    needs_pax!();
    // Default: `max_tokens` and an explicit temperature, as always.
    let d = work(
        "field-default",
        &completion(READ, "stop"),
        &["--kind", "inspect"],
        &[],
    )
    .await;
    assert_eq!(d.bodies[0]["max_tokens"], 2048);
    assert!(d.bodies[0].get("max_completion_tokens").is_none());
    assert_eq!(d.bodies[0]["temperature"], 0.0);
    assert_eq!(
        d.json["generation"]["token_limit_field"]["value"],
        "max_tokens"
    );

    // Chosen by the operator: the cap is renamed and the temperature is not sent at all.
    let r = work(
        "field-chosen",
        &completion(READ, "stop"),
        &[
            "--kind",
            "inspect",
            "--token-limit-field",
            "max_completion_tokens",
            "--temperature",
            "omit",
        ],
        &[],
    )
    .await;
    let body = &r.bodies[0];
    assert_eq!(body["max_completion_tokens"], 2048);
    assert!(body.get("max_tokens").is_none(), "{body}");
    assert!(body.get("temperature").is_none(), "{body}");
    let g = &r.json["generation"];
    assert_eq!(g["token_limit_field"]["value"], "max_completion_tokens");
    assert_eq!(g["token_limit_field"]["source"], "flag:--token-limit-field");
    assert_eq!(g["temperature"]["value"], "omitted");
    // Everything else about the run is unchanged.
    assert_eq!(r.json["audit"]["clean"], true);
    assert!(
        r.json["executions_by_capability"]["project.read"]
            .as_u64()
            .unwrap()
            >= 1
    );

    // A value that is not one of the two field names is refused before any request.
    let bad = work(
        "field-bad",
        &completion(READ, "stop"),
        &["--token-limit-field", "max_output_tokens"],
        &[],
    )
    .await;
    assert_eq!(bad.out.status.code(), Some(2));
    assert!(bad.bodies.is_empty());
}

// ---- provider profiles, on the wire --------------------------------------------------------------------

fn canonical(form: chip_core::DecisionSchemaForm) -> serde_json::Value {
    serde_json::from_str(&chip_core::ModelDecisionBoundary::response_schema(form)).unwrap()
}

#[tokio::test]
async fn each_profile_writes_the_request_it_describes_and_nothing_else_changes() {
    needs_pax!();
    let reply = completion(READ, "stop");
    // The default and the explicit local profile send the same request.
    let plain = work("prof-none", &reply, &["--kind", "inspect"], &[]).await;
    let local = work(
        "prof-local",
        &reply,
        &["--kind", "inspect", "--provider-profile", "local"],
        &[],
    )
    .await;
    let (p, l) = (&plain.bodies[0], &local.bodies[0]);
    assert_eq!(p["max_tokens"], 2048);
    assert_eq!(p["temperature"], 0.0);
    assert_eq!(
        p["response_format"]["json_schema"]["schema"],
        canonical(chip_core::DecisionSchemaForm::Full)
    );
    assert_eq!(
        (&p["max_tokens"], &p["temperature"], &p["response_format"]),
        (&l["max_tokens"], &l["temperature"], &l["response_format"])
    );
    assert_eq!(
        local.json["generation"]["provider_profile"]["value"],
        "local"
    );
    assert_eq!(
        local.json["generation"]["token_limit_field"]["source"],
        "profile:local"
    );

    // The openai profile: the other token field, no temperature, the flat projection.
    let openai = work(
        "prof-openai",
        &reply,
        &["--kind", "inspect", "--provider-profile", "openai"],
        &[],
    )
    .await;
    let o = &openai.bodies[0];
    assert_eq!(o["max_completion_tokens"], 2048);
    assert!(
        o.get("max_tokens").is_none() && o.get("temperature").is_none(),
        "{o}"
    );
    assert_eq!(
        o["response_format"]["json_schema"]["schema"],
        canonical(chip_core::DecisionSchemaForm::Flat)
    );
    assert!(
        o["response_format"]["json_schema"]["schema"]
            .get("anyOf")
            .is_none()
    );
    let g = &openai.json["generation"];
    assert_eq!(g["provider_profile"]["source"], "flag:--provider-profile");
    assert_eq!(g["structured_output"]["value"], "schema-flat");
    assert_eq!(g["structured_output"]["source"], "profile:openai");
    assert_eq!(g["temperature"]["value"], "omitted");
    // The task, the prompt, the capability set and the decision handling are the profile's no-business:
    // the messages are identical across profiles.
    assert_eq!(o["messages"], p["messages"]);
    assert_eq!(openai.json["audit"]["clean"], true);
    assert!(
        openai.json["executions_by_capability"]["project.read"]
            .as_u64()
            .unwrap()
            >= 1
    );
}

#[tokio::test]
async fn explicit_settings_beat_the_profile_and_contradictions_are_refused_before_any_request() {
    needs_pax!();
    let reply = completion(READ, "stop");
    // A flag overrides the profile's value for that setting only.
    let r = work(
        "prof-override",
        &reply,
        &[
            "--kind",
            "inspect",
            "--provider-profile",
            "openai",
            "--temperature",
            "0.5",
            "--structured-output",
            "object",
        ],
        &[("CHIP_TOKEN_LIMIT_FIELD", "max_tokens")],
    )
    .await;
    let b = &r.bodies[0];
    assert_eq!(b["temperature"], 0.5);
    assert_eq!(b["max_tokens"], 2048, "the variable beat the profile");
    assert!(b.get("max_completion_tokens").is_none());
    assert_eq!(
        b["response_format"]["json_schema"]["schema"],
        serde_json::json!({"type": "object"})
    );
    let g = &r.json["generation"];
    assert_eq!(g["temperature"]["source"], "flag:--temperature");
    assert_eq!(
        g["token_limit_field"]["source"],
        "environment:CHIP_TOKEN_LIMIT_FIELD"
    );
    assert_eq!(g["structured_output"]["source"], "flag:--structured-output");
    assert_eq!(g["provider_profile"]["value"], "openai");

    // Refused before any request: an unknown profile, and a profile for the other provider.
    for (tag, args) in [
        ("unknown", vec!["--provider-profile", "gpt-5"]),
        ("wrong-wire", vec!["--provider-profile", "anthropic"]),
    ] {
        let r = work(&format!("prof-bad-{tag}"), &reply, &args, &[]).await;
        assert!(!r.out.status.success(), "{tag}");
        let stderr = String::from_utf8_lossy(&r.out.stderr);
        assert!(stderr.contains("profile"), "{tag}: {stderr}");
        assert!(r.bodies.is_empty(), "{tag}: a request was sent");
    }
    // The variable selects a profile too.
    let r = work(
        "prof-env",
        &reply,
        &["--kind", "inspect"],
        &[("CHIP_PROVIDER_PROFILE", "openai")],
    )
    .await;
    assert_eq!(r.bodies[0]["max_completion_tokens"], 2048);
    assert_eq!(
        r.json["generation"]["provider_profile"]["source"],
        "environment:CHIP_PROVIDER_PROFILE"
    );
}

#[tokio::test]
async fn no_error_changes_the_profile_and_nothing_is_retried() {
    needs_pax!();
    // The server refuses the request the way OpenAI refuses an unsupported parameter.
    let refusal = r#"{"error":{"message":"Unsupported parameter: 'max_tokens' is not supported with this model.","type":"invalid_request_error"}}"#;
    let dir = fixture("prof-400");
    let server = common::start(400, refusal, Duration::ZERO).await;
    let (url, d) = (server.url.clone(), dir);
    let out = tokio::task::spawn_blocking(move || {
        run(
            &url,
            &d,
            &["--kind".to_string(), "inspect".to_string()],
            &[],
        )
    })
    .await
    .unwrap();
    assert!(!out.status.success());
    let sent = server.captured.lock().await.clone();
    assert_eq!(
        sent.len(),
        1,
        "no retry, with the same or any other settings"
    );
    let body: serde_json::Value = serde_json::from_str(&sent[0].body).unwrap();
    assert!(
        body.get("max_tokens").is_some() && body.get("max_completion_tokens").is_none(),
        "no silent switch of the token field"
    );
    assert!(
        body.get("response_format").is_some(),
        "no silent fall back to weaker structured output"
    );
}
