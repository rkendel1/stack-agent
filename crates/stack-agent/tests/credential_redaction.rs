//! Secret-redaction regression: a recognizable sentinel credential must reach the provider and
//! nothing else a caller or operator can read.
//!
//! Every test sets the sentinel as the provider key, points `HOME`, `XDG_*` and `TMPDIR` at a fresh
//! directory, runs the real `stack-agent` binary against a local endpoint, and then checks: the
//! endpoint saw the key (so the sentinel was really in play), and the key is absent from stdout,
//! stderr, the JSON report, the service's work state and event log, and every file the run left in
//! the project or in that home directory. The provider-error cases have the endpoint echo the key
//! back in its error body, as real providers do for a rejected key.
//!
//! Covered paths: `work` (text and `--json`), `serve` (state, events, its own output), and the
//! one-shot prompt path. Not covered: any real provider, `verify` with a real PAX, process crash
//! output, or anything an embedder logs itself. See docs/known-issues.md (F).

#![cfg(unix)]

#[path = "common/mod.rs"]
mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

const SENTINEL: &str = "sk-SENTINEL-stack-agent-4f1c9e2b7d";

fn unique(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "stack-agent-redaction-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A project, a private home for the run and a `pax` stand-in that only answers `--version`.
struct Sandbox {
    project: PathBuf,
    home: PathBuf,
    bin: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let root = unique(tag);
        let (project, home, bin) = (root.join("project"), root.join("home"), root.join("bin"));
        for d in [&project.join("src"), &home.join("tmp"), &bin] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(project.join("src/lib.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
        let _ = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&project)
            .status();
        let pax = bin.join("pax");
        std::fs::write(
            &pax,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'pax 0.4.1'; exit 0; fi\nexit 2\n",
        )
        .unwrap();
        std::fs::set_permissions(&pax, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { project, home, bin }
    }

    /// The binary with the sentinel as every provider key it might read, and nothing inherited
    /// that could carry a different key or redirect output.
    fn command(&self, url: &str) -> Command {
        let mut path = vec![self.bin.clone()];
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let mut c = Command::new(env!("CARGO_BIN_EXE_stack-agent"));
        c.current_dir(&self.project)
            .env("PATH", std::env::join_paths(path).unwrap())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("XDG_STATE_HOME", self.home.join(".local/state"))
            .env("XDG_CACHE_HOME", self.home.join(".cache"))
            .env("TMPDIR", self.home.join("tmp"))
            .env("RUST_BACKTRACE", "1")
            .env_remove("PAX_BIN")
            .env_remove("CHIP_EXECUTOR")
            .env("CHIP_PROVIDER", "openai-compatible")
            .env("CHIP_MODEL", "scripted")
            .env("CHIP_ENDPOINT", url)
            .env("CHIP_API_KEY", SENTINEL)
            .env("OPENAI_API_KEY", SENTINEL)
            .env("ANTHROPIC_API_KEY", SENTINEL)
            .stdin(Stdio::null());
        c
    }

    /// Every file under the project and the private home that contains the sentinel.
    fn files_with_sentinel(&self) -> Vec<PathBuf> {
        let mut hits = Vec::new();
        for root in [&self.project, &self.home] {
            walk(root, &mut |p| {
                if std::fs::read(p).is_ok_and(|b| contains(&b, SENTINEL.as_bytes())) {
                    hits.push(p.to_path_buf());
                }
            });
        }
        hits
    }
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if entry.file_name() != ".git" {
                walk(&p, f);
            }
        } else {
            f(&p);
        }
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn completion(content: &str) -> String {
    serde_json::json!({
        "id": "resp-redaction",
        "choices": [{"message": {"role": "assistant", "content": content}}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1},
    })
    .to_string()
}

/// What a provider says when it rejects a key: the key, quoted back.
fn rejection() -> String {
    serde_json::json!({
        "error": {
            "message": format!("Incorrect API key provided: {SENTINEL}. You can find your API key at https://example.invalid."),
            "type": "invalid_request_error",
            "code": "invalid_api_key",
        }
    })
    .to_string()
}

async fn run(c: Command) -> Output {
    let mut c = c;
    tokio::task::spawn_blocking(move || c.output().unwrap())
        .await
        .unwrap()
}

async fn assert_key_sent(server: &common::MockServer) {
    let captured = server.captured.lock().await;
    assert!(!captured.is_empty(), "the provider was never called");
    assert!(
        captured
            .iter()
            .all(|c| c.header("authorization") == Some(&format!("Bearer {SENTINEL}"))),
        "the sentinel must be the credential the provider received"
    );
}

fn assert_absent(where_: &str, s: &str) {
    assert!(
        !s.contains(SENTINEL),
        "the credential leaked into {where_}:\n{s}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn work_never_shows_the_key_in_its_output_report_or_files() {
    let reply = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
    let server = common::start(200, &completion(reply), Duration::ZERO).await;
    let sandbox = Sandbox::new("work");
    for args in [
        &["--max-turns", "2", "Make the tests pass."][..],
        &["--json", "--max-turns", "2", "Make the tests pass."][..],
        &["--print-reply", "--max-turns", "2", "Make the tests pass."][..],
    ] {
        let mut c = sandbox.command(&server.url);
        c.arg("work").args(args);
        let out = run(c).await;
        assert_eq!(out.status.code(), Some(1), "turn limit: {}", text(&out));
        assert_absent(&format!("`work {args:?}` output"), &text(&out));
    }
    assert_key_sent(&server).await;
    assert_eq!(sandbox.files_with_sentinel(), Vec::<PathBuf>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_provider_that_echoes_the_key_in_its_error_does_not_leak_it_through_work() {
    let server = common::start(401, &rejection(), Duration::ZERO).await;
    let sandbox = Sandbox::new("work-401");
    for json in [false, true] {
        let mut c = sandbox.command(&server.url);
        c.arg("work");
        if json {
            c.arg("--json");
        }
        c.arg("Make the tests pass.");
        let out = run(c).await;
        assert_ne!(out.status.code(), Some(0), "{}", text(&out));
        assert_absent("`work` output after a provider rejection", &text(&out));
    }
    assert_key_sent(&server).await;
    assert_eq!(sandbox.files_with_sentinel(), Vec::<PathBuf>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_one_shot_prompt_path_does_not_leak_the_key() {
    for (status, body) in [
        (200, completion("hello")),
        (401, rejection()),
        (500, rejection()),
    ] {
        let server = common::start(status, &body, Duration::ZERO).await;
        let sandbox = Sandbox::new(&format!("prompt-{status}"));
        let mut c = sandbox.command(&server.url);
        c.arg("say hello");
        let out = run(c).await;
        assert_eq!(out.status.success(), status == 200, "{}", text(&out));
        if status != 200 {
            // The provider's error (with the key quoted in it) really reached the output path.
            assert!(
                text(&out).contains(&format!("HTTP {status}")),
                "{}",
                text(&out)
            );
        }
        assert_absent(
            &format!("the prompt path's output (HTTP {status})"),
            &text(&out),
        );
        assert_key_sent(&server).await;
        assert_eq!(sandbox.files_with_sentinel(), Vec::<PathBuf>::new());
    }
}

fn http(addr: &str, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let body = body.unwrap_or("");
    let mut stream = TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap();
    (
        head.split(' ').nth(1).unwrap().parse().unwrap(),
        body.to_string(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn the_service_never_exposes_the_key_in_state_events_or_its_own_output() {
    for (status, body) in [
        (
            200,
            completion(r#"{"decision":"block","reason":"nothing to do"}"#),
        ),
        (401, rejection()),
    ] {
        let server = common::start(status, &body, Duration::ZERO).await;
        let sandbox = Sandbox::new(&format!("serve-{status}"));
        let stderr_path = sandbox.home.join("serve.stderr");
        let mut c = sandbox.command(&server.url);
        c.args(["serve", "--host", "127.0.0.1", "--port", "0"]);
        let seen = tokio::task::spawn_blocking(move || {
            let mut child = c
                .stdout(Stdio::piped())
                .stderr(std::fs::File::create(&stderr_path).unwrap())
                .spawn()
                .unwrap();
            let mut stdout = BufReader::new(child.stdout.take().unwrap());
            let mut first = String::new();
            stdout.read_line(&mut first).unwrap();
            let addr = first
                .trim()
                .strip_prefix("Chip Runtime Service listening on http://")
                .unwrap_or_else(|| panic!("unexpected first line: {first:?}"))
                .to_string();
            let (code, accepted) = http(
                &addr,
                "POST",
                "/v1/work",
                Some(r#"{"goal":"Make the tests pass."}"#),
            );
            assert_eq!(code, 202, "{accepted}");
            let id: serde_json::Value = serde_json::from_str(&accepted).unwrap();
            let id = id["work_id"].as_str().unwrap().to_string();
            let mut state = String::new();
            for _ in 0..500 {
                let (_, body) = http(&addr, "GET", &format!("/v1/work/{id}"), None);
                if !body.contains("\"status\":\"running\"") {
                    state = body;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(!state.is_empty(), "the work never ended");
            let (_, events) = http(&addr, "GET", &format!("/v1/work/{id}/events"), None);
            let _ = child.kill();
            let _ = child.wait();
            let mut rest = String::new();
            let _ = stdout.read_to_string(&mut rest);
            let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
            vec![
                ("work state", state),
                ("event log", events),
                ("service stdout", format!("{first}{rest}")),
                ("service stderr", stderr),
            ]
        })
        .await
        .unwrap();
        for (where_, s) in seen {
            assert_absent(
                &format!("the service's {where_} (provider HTTP {status})"),
                &s,
            );
        }
        assert_key_sent(&server).await;
        assert_eq!(sandbox.files_with_sentinel(), Vec::<PathBuf>::new());
    }
}
