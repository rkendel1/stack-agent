//! Opt-in live smoke tests: the real `stack-agent` binary against a real provider, and through a
//! real proxy. Both are `#[ignore]`d and also gated by explicit variables, so neither a plain
//! `cargo test` nor `cargo test -- --ignored` without the variables makes a network request.
//!
//! They make exactly one model request each (no retry). The credential is read by name and only
//! handed to the child process; it is never printed, and every output this test prints has it
//! replaced first. The tests assert it appears in no output and no file the run left behind.
//!
//! ```text
//! # Provider (one request):
//! STACK_AGENT_LIVE_PROVIDER=1 \
//! STACK_AGENT_LIVE_ENDPOINT=https://ai-gateway.vercel.sh/v1/chat/completions \
//! STACK_AGENT_LIVE_MODEL=nvidia/nemotron-3.5-lightning-free \
//! STACK_AGENT_LIVE_KEY_ENV=AI_GATEWAY_API_KEY \
//! cargo test --locked -p stack-agent --test live_smoke -- --ignored live_provider
//!
//! # Proxy (one request through it; the provider variables above are also required):
//! STACK_AGENT_LIVE_PROXY=http://proxy.corp:3128 \
//! [STACK_AGENT_LIVE_PROXY_USERNAME=... STACK_AGENT_LIVE_PROXY_PASSWORD_ENV=NAME_OF_VAR] \
//! [STACK_AGENT_LIVE_NO_PROXY=...] \
//! cargo test --locked -p stack-agent --test live_smoke -- --ignored live_proxy
//! ```
//!
//! Optional: `STACK_AGENT_LIVE_PROVIDER_KIND` (`openai-compatible`, the default, or `anthropic`).
//! `STACK_AGENT_LIVE_KEY_ENV` defaults to `CHIP_API_KEY`. A CA bundle for a TLS-intercepting
//! network is taken from `SSL_CERT_FILE` when it is set, as the product does.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Live {
    endpoint: String,
    model: String,
    kind: String,
    key: String,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The provider settings, or `None` (with a `SKIPPED` line) when the gate is not set.
fn live() -> Option<Live> {
    if var("STACK_AGENT_LIVE_PROVIDER").as_deref() != Some("1") {
        eprintln!("SKIPPED: set STACK_AGENT_LIVE_PROVIDER=1 (and its companions) to run");
        return None;
    }
    let need = |name: &str| var(name).unwrap_or_else(|| panic!("{name} is required"));
    let key_env = var("STACK_AGENT_LIVE_KEY_ENV").unwrap_or_else(|| "CHIP_API_KEY".into());
    // Never print the value: the message names the variable only.
    let key = var(&key_env).unwrap_or_else(|| panic!("{key_env} (the key variable) is not set"));
    Some(Live {
        endpoint: need("STACK_AGENT_LIVE_ENDPOINT"),
        model: need("STACK_AGENT_LIVE_MODEL"),
        kind: var("STACK_AGENT_LIVE_PROVIDER_KIND").unwrap_or_else(|| "openai-compatible".into()),
        key,
    })
}

struct Sandbox(PathBuf);

impl Sandbox {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("stack-agent-live-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("project")).unwrap();
        std::fs::create_dir_all(root.join("home/tmp")).unwrap();
        Self(root)
    }

    fn files_containing(&self, needle: &str) -> Vec<PathBuf> {
        let mut hits = Vec::new();
        walk(&self.0, &mut |p| {
            if std::fs::read(p)
                .is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle.as_bytes()))
            {
                hits.push(p.to_path_buf());
            }
        });
        hits
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk(&p, f);
        } else {
            f(&p);
        }
    }
}

/// Runs the prompt path once with an empty environment plus the selection and `extra`, and returns
/// (success, combined output with every secret replaced, whether a secret was in the raw output).
fn run_once(
    live: &Live,
    sandbox: &Sandbox,
    extra: &[(&str, String)],
    secrets: &[&str],
) -> (bool, String, bool) {
    let home = sandbox.0.join("home");
    let mut c = Command::new(env!("CARGO_BIN_EXE_stack-agent"));
    c.env_clear()
        .current_dir(sandbox.0.join("project"))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("TMPDIR", home.join("tmp"))
        .env("CHIP_PROVIDER", &live.kind)
        .env("CHIP_MODEL", &live.model)
        .env("CHIP_ENDPOINT", &live.endpoint)
        .env("CHIP_API_KEY", &live.key)
        .stdin(Stdio::null())
        .arg("Reply with the single word: pong");
    if let Some(bundle) = std::env::var_os("SSL_CERT_FILE") {
        c.env("SSL_CERT_FILE", bundle);
    }
    for (name, value) in extra {
        c.env(name, value);
    }
    let out = c.output().expect("run stack-agent");
    let raw = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let leaked = secrets.iter().any(|s| raw.contains(s));
    let mut shown = raw;
    for secret in secrets {
        shown = shown.replace(secret, "[redacted-by-test]");
    }
    (out.status.success(), shown, leaked)
}

fn assert_clean(sandbox: &Sandbox, ok: bool, shown: &str, leaked: bool, secrets: &[&str]) {
    assert!(
        !leaked,
        "a secret appeared in the output (shown redacted): {shown}"
    );
    for secret in secrets {
        assert!(
            sandbox.files_containing(secret).is_empty(),
            "a secret was written to a file"
        );
    }
    assert!(ok, "the live call failed: {shown}");
    assert!(
        shown.contains("Turn completed"),
        "no completed turn: {shown}"
    );
    eprintln!("live output (secrets redacted):\n{shown}");
}

#[test]
#[ignore = "live: makes one real provider request; needs STACK_AGENT_LIVE_PROVIDER=1"]
fn live_provider_prompt_path_succeeds_and_never_shows_the_key() {
    let Some(live) = live() else { return };
    let sandbox = Sandbox::new("provider");
    let secrets = [live.key.as_str()];
    let (ok, shown, leaked) = run_once(&live, &sandbox, &[], &secrets);
    assert_clean(&sandbox, ok, &shown, leaked, &secrets);
}

#[test]
#[ignore = "live: makes one real request through a real proxy; needs STACK_AGENT_LIVE_PROXY"]
fn live_proxy_prompt_path_succeeds_and_never_shows_the_key_or_proxy_password() {
    let Some(proxy) = var("STACK_AGENT_LIVE_PROXY") else {
        eprintln!("SKIPPED: set STACK_AGENT_LIVE_PROXY=http://host:port to run");
        return;
    };
    let Some(live) = live() else { return };
    let sandbox = Sandbox::new("proxy");
    let mut extra = vec![("CHIP_PROXY", proxy)];
    let password = match var("STACK_AGENT_LIVE_PROXY_USERNAME") {
        Some(username) => {
            let password_env = var("STACK_AGENT_LIVE_PROXY_PASSWORD_ENV").unwrap_or_else(|| {
                panic!("STACK_AGENT_LIVE_PROXY_PASSWORD_ENV must name the password variable")
            });
            let password = std::env::var(&password_env)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| panic!("{password_env} (the proxy password) is not set"));
            extra.push(("CHIP_PROXY_USERNAME", username));
            extra.push(("CHIP_PROXY_PASSWORD", password.clone()));
            Some(password)
        }
        None => None,
    };
    if let Some(no_proxy) = var("STACK_AGENT_LIVE_NO_PROXY") {
        extra.push(("CHIP_NO_PROXY", no_proxy));
    }
    let mut secrets = vec![live.key.as_str()];
    if let Some(password) = &password {
        secrets.push(password);
    }
    let (ok, shown, leaked) = run_once(&live, &sandbox, &extra, &secrets);
    assert_clean(&sandbox, ok, &shown, leaked, &secrets);
}
