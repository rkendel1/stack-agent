//! The one exit contract (`verify::exit_code`), table-driven, through the real binary, for every
//! command that runs a model: `work`, `verify`, `serve` (process codes) and the one-shot prompt.
//!
//! 0 verified · 1 ran correctly, not verified (blocked, limit, cancelled, grounded inspection) ·
//! 2 usage · 3 a required dependency was unavailable and nothing was done · 4 runtime failure
//! (including a model that answered but broke the protocol, and any failure after work began).

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::process::Output;

use gated::{FAILED, Model, PASSED, Pax, Reply, command, project};

const WRITE: &str = r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"src/lib.rs","content":"pub fn one() -> u8 { 1 }\npub fn two() -> u8 { 2 }\n"}}"#;
const TEST: &str = r#"{"decision":"request_capability","capability":"pax.test"}"#;
const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;
const DONE: &str = r#"{"decision":"complete","summary":"`one` is in src/lib.rs."}"#;
const UNREACHABLE: &str = "http://127.0.0.1:1/v1/chat/completions";

struct Case {
    name: &'static str,
    args: &'static [&'static str],
    script: Vec<Reply>,
    pax: &'static str,
    /// Use an endpoint nothing listens on instead of the scripted model.
    unreachable: bool,
    /// Remove the model selection from the environment.
    unconfigured: bool,
    expect: i32,
}

fn case(
    name: &'static str,
    args: &'static [&'static str],
    script: Vec<Reply>,
    expect: i32,
) -> Case {
    Case {
        name,
        args,
        script,
        pax: PASSED,
        unreachable: false,
        unconfigured: false,
        expect,
    }
}

async fn run(c: Case) -> (Output, usize) {
    let model = Model::start(c.script).await;
    let pax = Pax::new(c.name, c.pax, false);
    let dir = project(c.name);
    let url = if c.unreachable {
        UNREACHABLE
    } else {
        model.url.as_str()
    };
    let mut cmd = command(&dir, url, &pax);
    cmd.args(c.args);
    if c.unconfigured {
        cmd.env_remove("CHIP_MODEL").env_remove("CHIP_ENDPOINT");
    }
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    (out, model.requests())
}

async fn check(cases: Vec<Case>) {
    let mut failures = Vec::new();
    for c in cases {
        let (name, expect) = (c.name, c.expect);
        let (out, _) = run(c).await;
        if out.status.code() != Some(expect) {
            failures.push(format!(
                "{name}: expected {expect}, got {:?}\n{}{}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n---\n"));
}

#[tokio::test(flavor = "multi_thread")]
async fn work_follows_the_contract() {
    check(vec![
        case(
            "work-verified",
            &["work", "Add `two`."],
            vec![Reply::Stream(WRITE), Reply::Stream(TEST)],
            0,
        ),
        case(
            "work-blocked",
            &["work", "x"],
            vec![Reply::Stream(BLOCK)],
            1,
        ),
        case(
            "work-limit",
            &["work", "--max-turns", "1", "x"],
            vec![Reply::Stream(READ)],
            1,
        ),
        case(
            "work-grounded-inspection",
            &["work", "--kind", "inspect", "Where is `one`?"],
            vec![Reply::Stream(READ), Reply::Stream(DONE)],
            1,
        ),
        case("work-usage", &["work", "--no-such-flag"], vec![], 2),
        Case {
            unreachable: true,
            ..case("work-unreachable", &["work", "x"], vec![], 3)
        },
        Case {
            unconfigured: true,
            ..case("work-unconfigured", &["work", "x"], vec![], 3)
        },
        case(
            "work-rate-limited-first-call",
            &["work", "x"],
            vec![Reply::Status(429)],
            3,
        ),
        case(
            "work-protocol-violation",
            &["work", "x"],
            vec![Reply::Plain(BLOCK)],
            4,
        ),
        case(
            "work-provider-500-after-work-began",
            &["work", "x"],
            vec![Reply::Stream(READ), Reply::Status(500)],
            4,
        ),
        case(
            "work-invalid-decision",
            &["work", "x"],
            vec![Reply::Stream("not a decision")],
            4,
        ),
    ])
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_follows_the_contract() {
    check(vec![
        case("verify-passed", &["verify"], vec![Reply::Stream(TEST)], 0),
        Case {
            pax: FAILED,
            ..case("verify-failed", &["verify"], vec![Reply::Stream(TEST)], 1)
        },
        case("verify-usage", &["verify", "--nope"], vec![], 2),
        Case {
            unconfigured: true,
            ..case("verify-unconfigured", &["verify"], vec![], 3)
        },
        Case {
            unreachable: true,
            ..case("verify-unreachable", &["verify"], vec![], 3)
        },
        case(
            "verify-protocol-violation",
            &["verify"],
            vec![Reply::Plain(TEST)],
            4,
        ),
    ])
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_one_shot_prompt_follows_the_contract() {
    check(vec![
        case("prompt-ok", &["say hello"], vec![Reply::Stream("hello")], 0),
        Case {
            unconfigured: true,
            ..case("prompt-unconfigured", &["say hello"], vec![], 3)
        },
        Case {
            unreachable: true,
            ..case("prompt-unreachable", &["say hello"], vec![], 3)
        },
        case(
            "prompt-unauthenticated",
            &["say hello"],
            vec![Reply::Status(401)],
            3,
        ),
        case(
            "prompt-protocol-violation",
            &["say hello"],
            vec![Reply::Plain("hello")],
            4,
        ),
    ])
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_process_codes_follow_the_contract() {
    // A running service is not an outcome; its process codes are usage, unavailable and failure.
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let port: &'static str = Box::leak(port.into_boxed_str());
    let busy: &'static [&'static str] =
        Box::leak(vec!["serve", "--host", "127.0.0.1", "--port", port].into_boxed_slice());
    check(vec![
        case("serve-usage", &["serve", "--no-such-flag"], vec![], 2),
        Case {
            unconfigured: true,
            ..case("serve-unconfigured", &["serve"], vec![], 3)
        },
        case("serve-cannot-bind", busy, vec![], 4),
    ])
    .await;
    drop(taken);
}
