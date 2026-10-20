//! Controllable stand-ins for product-level tests: a chat-completions endpoint that streams (and
//! can stall mid-stream until released) and a `pax` that can block while "running tests" until
//! released. Both record what they saw. Tests synchronize on these gates, not on sleeps.
#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::Notify;

pub const PASSED: &str = r#"{"schema":"pax.execution-result.v1","operation":"test","status":"passed","reason":"tests-passed","tool":"cargo","exit_code":0,"tests":{"passed":3,"failed":0,"ignored":0,"measured":0}}"#;
pub const FAILED: &str = r#"{"schema":"pax.execution-result.v1","operation":"test","status":"failed","reason":"tests-failed","tool":"cargo","exit_code":101,"tests":{"passed":1,"failed":2,"ignored":0,"measured":0}}"#;

/// One reply of the scripted model, chosen by how many requests came before it.
#[derive(Clone, Debug)]
pub enum Reply {
    /// A complete event stream carrying this decision text.
    Stream(&'static str),
    /// Headers and a first chunk of this text, then nothing until [`Model::release`]; then the
    /// rest. The connection stays open meanwhile.
    Stall(&'static str),
    /// An HTTP error status with a small JSON body.
    Status(u16),
    /// A 200 that is one JSON body, not an event stream.
    Plain(&'static str),
    /// A complete event stream carrying this decision text, served as `model`, with this exact
    /// usage object on its last chunk, or no usage object at all for `None`.
    StreamUsage {
        text: &'static str,
        model: &'static str,
        usage: Option<&'static str>,
    },
}

pub struct Model {
    pub url: String,
    requests: Arc<AtomicUsize>,
    release: Arc<Notify>,
}

impl Model {
    /// Starts the endpoint. Request `n` (0-based) gets `script[n]`, the last entry once exhausted.
    pub async fn start(script: Vec<Reply>) -> Model {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let requests = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Notify::new());
        let (count, gate) = (requests.clone(), release.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    continue;
                };
                let (count, gate, script) = (count.clone(), gate.clone(), script.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 16384];
                    loop {
                        let Ok(n) = stream.read(&mut chunk).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                            let len = head
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:"))
                                .and_then(|v| v.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            if buf.len() >= end + 4 + len {
                                break;
                            }
                        }
                    }
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    let reply = script.get(n).or(script.last()).cloned().unwrap();
                    let sse_head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
                    match reply {
                        Reply::Stream(text) => {
                            let body = format!("{sse_head}{}{}", delta(text), finish());
                            let _ = stream.write_all(body.as_bytes()).await;
                        }
                        Reply::StreamUsage { text, model, usage } => {
                            let event = serde_json::json!({"id": "m", "model": model, "choices": [{"index": 0, "delta": {"role": "assistant", "content": text}}]});
                            let stop = serde_json::json!({"id": "m", "model": model, "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]});
                            let mut body = format!("{sse_head}data: {event}\n\ndata: {stop}\n\n");
                            if let Some(usage) = usage {
                                body.push_str(&format!(
                                    "data: {{\"id\":\"m\",\"model\":\"{model}\",\"choices\":[],\"usage\":{usage}}}\n\n"
                                ));
                            }
                            body.push_str("data: [DONE]\n\n");
                            let _ = stream.write_all(body.as_bytes()).await;
                        }
                        Reply::Stall(text) => {
                            let (first, rest) = text.split_at(text.len() / 2);
                            let _ = stream
                                .write_all(format!("{sse_head}{}", delta(first)).as_bytes())
                                .await;
                            let _ = stream.flush().await;
                            gate.notified().await;
                            let _ = stream
                                .write_all(format!("{}{}", delta(rest), finish()).as_bytes())
                                .await;
                        }
                        Reply::Status(code) => {
                            let body = r#"{"error":{"message":"scripted failure"}}"#;
                            let _ = stream
                                .write_all(
                                    format!(
                                        "HTTP/1.1 {code} Scripted\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                        body.len()
                                    )
                                    .as_bytes(),
                                )
                                .await;
                        }
                        Reply::Plain(text) => {
                            let body = serde_json::json!({
                                "id": "m",
                                "choices": [{"message": {"role": "assistant", "content": text}}],
                                "usage": {"prompt_tokens": 1, "completion_tokens": 1},
                            })
                            .to_string();
                            let _ = stream
                                .write_all(
                                    format!(
                                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                                        body.len()
                                    )
                                    .as_bytes(),
                                )
                                .await;
                        }
                    }
                    let _ = stream.shutdown().await;
                });
            }
        });
        Model {
            url,
            requests,
            release,
        }
    }

    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    /// Lets every stalled stream finish (now and later).
    pub fn release(&self) {
        self.release.notify_waiters();
        self.release.notify_one();
    }

    /// Waits (bounded) until at least `n` requests have arrived.
    pub async fn wait_requests(&self, n: usize) {
        wait_until(&format!("{n} model request(s)"), || self.requests() >= n).await;
    }
}

fn delta(text: &str) -> String {
    let event = serde_json::json!({"id": "m", "choices": [{"index": 0, "delta": {"role": "assistant", "content": text}}]});
    format!("data: {event}\n\n")
}

fn finish() -> String {
    let stop = serde_json::json!({"id": "m", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]});
    let usage = serde_json::json!({"id": "m", "choices": [], "usage": {"prompt_tokens": 1, "completion_tokens": 1}});
    format!("data: {stop}\n\ndata: {usage}\n\ndata: [DONE]\n\n")
}

/// Polls `condition` every 10 ms for up to 30 s; panics naming `what` if it never holds.
pub async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub fn unique(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("stack-agent-gated-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub const LIB: &str = "pub fn one() -> u8 { 1 }\n";

pub fn project(tag: &str) -> PathBuf {
    let dir = unique(&format!("project-{tag}"));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), LIB).unwrap();
    let _ = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&dir)
        .status();
    dir
}

/// A `pax` stand-in. `--version` answers at once. A test run writes `started`, then, if gated,
/// waits until `release` exists (at most 60 s), then prints `result` and records `finished`.
pub struct Pax {
    pub dir: PathBuf,
}

impl Pax {
    pub fn new(tag: &str, result: &str, gated: bool) -> Pax {
        let dir = unique(&format!("pax-{tag}"));
        let script = format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'pax 0.4.1'; exit 0; fi\ntouch '{d}/started'\nif [ {gated} = 1 ]; then i=0; while [ ! -e '{d}/release' ] && [ $i -lt 6000 ]; do sleep 0.01; i=$((i+1)); done; fi\necho '{result}'\ntouch '{d}/finished'\nexit 0\n",
            d = dir.display(),
            gated = u8::from(gated),
        );
        let path = dir.join("pax");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Pax { dir }
    }

    pub fn started(&self) -> bool {
        self.dir.join("started").exists()
    }

    pub fn finished(&self) -> bool {
        self.dir.join("finished").exists()
    }

    pub fn release(&self) {
        std::fs::write(self.dir.join("release"), "").unwrap();
    }

    pub fn path_env(&self) -> std::ffi::OsString {
        let mut paths = vec![self.dir.clone()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        std::env::join_paths(paths).unwrap()
    }
}

/// The binary with a scripted model and a stand-in `pax`; nothing inherited that selects another
/// provider or carries another key.
pub fn command(dir: &Path, url: &str, pax: &Pax) -> std::process::Command {
    let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_stack-agent"));
    c.current_dir(dir)
        .env_remove("PAX_BIN")
        .env_remove("CHIP_EXECUTOR")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env("PATH", pax.path_env())
        .env("CHIP_PROVIDER", "openai-compatible")
        .env("CHIP_MODEL", "scripted")
        .env("CHIP_ENDPOINT", url)
        .env("CHIP_API_KEY", "sk-gated-test-key")
        // Runtime learning goes to a private directory per command, so no test learns from
        // another; a test that needs a shared runtime store sets it again after this.
        .env("STACK_AGENT_DATA_DIR", private_runtime_dir())
        .stdin(std::process::Stdio::null());
    c
}

/// A fresh, empty runtime data directory.
pub fn private_runtime_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("stack-agent-runtime-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}
