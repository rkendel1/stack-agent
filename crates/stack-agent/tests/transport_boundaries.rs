//! Transport boundaries of the real `stack-agent` binary (known issue F), on the one-shot prompt
//! path, against loopback stand-ins. Mocked transport only; no external network.
//!
//! Every run starts from an empty environment (`env_clear`) plus exactly the variables the test
//! sets, so nothing inherited from the shell (a proxy, a CA bundle, a key) can decide the outcome.
//!
//! - Ambient `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY`/`NO_PROXY` (either case) never route a request.
//! - A proxy that refuses authentication (407, echoing the credentials) fails the run once: no
//!   retry, no direct connection, and neither the key nor the proxy password is printed or saved.
//! - An invalid proxy configuration fails before any connection, naming the variable to fix.
//! - `CHIP_NO_PROXY` chooses a direct connection for a listed host and the proxy otherwise.
//! - `SSL_CERT_FILE` is the only way to trust a private CA, directly or through `CHIP_PROXY`;
//!   without it, or with the wrong CA, the endpoint is refused and the key never crosses the
//!   untrusted session; an empty or unreadable bundle is an error naming the variable. These need
//!   `openssl` (test tooling) and follow `fx-native-provider/tests/private_ca.rs`: `SKIPPED`
//!   without it, a failure when `FX_REQUIRE_TLS_TESTS` is set.

#![cfg(unix)]

#[path = "../../fx-native-provider/tests/support/mod.rs"]
mod fxsupport;
#[path = "../../fx-native-provider/tests/support/tls.rs"]
mod tls;

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use fxsupport::proxy::{Proxy, base64};
use fxsupport::{Server, delta, done, finish, sse};
use tls::{Pki, TlsServer, event_stream, openssl_available};

const KEY: &str = "sk-SENTINEL-transport-7c3e9a1f02";
const PROXY_PASSWORD: &str = "pp-SENTINEL-proxy-b81d44e6";

static SANDBOXES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A project directory and a private home, removed afterwards.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "stack-agent-transport-{}-{}-{tag}",
            std::process::id(),
            SANDBOXES.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("project")).unwrap();
        std::fs::create_dir_all(root.join("home/tmp")).unwrap();
        Self { root }
    }

    /// The binary on the prompt path with an empty environment plus the provider selection.
    fn command(&self, endpoint: &str, key: Option<&str>) -> Command {
        let home = self.root.join("home");
        let mut c = Command::new(env!("CARGO_BIN_EXE_stack-agent"));
        c.env_clear()
            .current_dir(self.root.join("project"))
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &home)
            .env("TMPDIR", home.join("tmp"))
            .env("CHIP_PROVIDER", "openai-compatible")
            .env("CHIP_MODEL", "scripted")
            .env("CHIP_ENDPOINT", endpoint)
            .env("CHIP_TIMEOUT_SECS", "20")
            .stdin(Stdio::null())
            .arg("say hello");
        if let Some(key) = key {
            c.env("CHIP_API_KEY", key);
        }
        c
    }

    /// Every file the run left that contains `needle`.
    fn files_containing(&self, needle: &str) -> Vec<PathBuf> {
        let mut hits = Vec::new();
        walk(&self.root, &mut |p| {
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
        let _ = std::fs::remove_dir_all(&self.root);
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

fn run(mut c: Command) -> (Output, String) {
    let out = c.output().expect("run stack-agent");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out, text)
}

/// Neither secret (nor the proxy credential's Basic encoding) is in `text` or any file left behind.
fn assert_no_secret(sandbox: &Sandbox, text: &str) {
    for secret in [
        KEY,
        PROXY_PASSWORD,
        &base64(&format!("chip:{PROXY_PASSWORD}")),
    ] {
        assert!(
            !text.contains(secret),
            "a secret leaked into the output: {text}"
        );
        assert_eq!(sandbox.files_containing(secret), Vec::<PathBuf>::new());
    }
}

/// A loopback listener that records the head of every connection and answers it with `reply`
/// (or closes, when `reply` is empty). As a proxy it never tunnels; as an endpoint it only counts.
struct Listener {
    url: String,
    port: u16,
    heads: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Listener {
    fn start(reply: String) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let heads = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (heads_in, stop_in) = (heads.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !stop_in.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                heads_in.lock().unwrap().push(read_head(&mut stream));
                let _ = stream.write_all(reply.as_bytes());
                let _ = stream.shutdown(Shutdown::Both);
            }
        });
        Self {
            url: format!("http://127.0.0.1:{port}"),
            port,
            heads,
            stop,
            thread: Some(thread),
        }
    }

    fn heads(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_head(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => break,
        }
    }
    String::from_utf8_lossy(&head).into_owned()
}

/// A mock chat-completions endpoint that answers one streamed reply saying `text`.
fn endpoint(text: &str) -> (Server, String) {
    let server = Server::start(vec![sse(vec![delta(text), finish("stop"), done()])]);
    let url = format!("{}/chat/completions", server.base_url);
    (server, url)
}

const AMBIENT: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];

#[test]
fn ambient_proxy_variables_never_route_a_request_from_the_real_binary() {
    let ambient = Listener::start(String::new());
    let sandbox = Sandbox::new("ambient");

    // A reachable endpoint: answered directly, the ambient proxy is never contacted.
    let (server, url) = endpoint("direct despite the ambient proxy");
    let mut c = sandbox.command(&url, Some(KEY));
    for name in AMBIENT {
        c.env(name, &ambient.url);
    }
    // A NO_PROXY that lists nothing relevant, so an ambient-proxy reader would use the proxy.
    c.env("NO_PROXY", "nothing.invalid")
        .env("no_proxy", "nothing.invalid");
    let (out, text) = run(c);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("direct despite the ambient proxy"), "{text}");
    assert_eq!(server.connections(), 1);
    assert!(
        server.recorded()[0].head.contains(&format!("Bearer {KEY}")),
        "the request went directly to the endpoint"
    );

    // An https endpoint by a name that does not resolve: an ambient-proxy reader would hand the
    // name to the proxy in a CONNECT; the binary resolves it itself and fails.
    let mut c = sandbox.command(
        "https://stack-agent-ambient-proxy-test.invalid/v1/chat/completions",
        Some(KEY),
    );
    for name in AMBIENT {
        c.env(name, &ambient.url);
    }
    let (out, text) = run(c);
    assert!(!out.status.success(), "{text}");

    assert_eq!(
        ambient.heads(),
        Vec::<String>::new(),
        "an ambient proxy was used"
    );
    assert_no_secret(&sandbox, &text);
}

fn echo_407() -> String {
    let encoded = base64(&format!("chip:{PROXY_PASSWORD}"));
    let body = format!("credentials chip:{PROXY_PASSWORD} ({encoded}) rejected");
    format!(
        "HTTP/1.1 407 Rejected chip:{PROXY_PASSWORD}\r\nproxy-authenticate: Basic realm=\"{encoded}\"\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[test]
fn a_proxy_refusing_authentication_fails_once_with_no_direct_fallback_and_no_secret() {
    let proxy = Listener::start(echo_407());
    let target = Listener::start(String::new());
    let sandbox = Sandbox::new("407");
    let mut c = sandbox.command(
        &format!("https://127.0.0.1:{}/v1/chat/completions", target.port),
        Some(KEY),
    );
    c.env("CHIP_PROXY", &proxy.url)
        .env("CHIP_PROXY_USERNAME", "chip")
        .env("CHIP_PROXY_PASSWORD", PROXY_PASSWORD);
    let (out, text) = run(c);
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("407"), "the refusal is explicit: {text}");
    assert_no_secret(&sandbox, &text);
    let heads = proxy.heads();
    assert_eq!(heads.len(), 1, "exactly one CONNECT, no retry: {heads:?}");
    assert!(
        heads[0].starts_with(&format!("CONNECT 127.0.0.1:{} ", target.port)),
        "{heads:?}"
    );
    // The key travels only inside the tunnel, so the proxy never saw it.
    assert!(!heads[0].contains(KEY));
    assert_eq!(
        target.heads(),
        Vec::<String>::new(),
        "the endpoint was reached directly"
    );
}

#[test]
fn an_invalid_proxy_configuration_fails_before_any_request_naming_the_variable() {
    let proxy = Listener::start(String::new());
    let target = Listener::start(String::new());
    let endpoint = format!("https://127.0.0.1:{}/v1/chat/completions", target.port);
    let sandbox = Sandbox::new("invalid-proxy");
    let embedded = format!("http://chip:{PROXY_PASSWORD}@127.0.0.1:{}", proxy.port);
    let socks = format!("socks5://127.0.0.1:{}", proxy.port);
    let https = format!("https://127.0.0.1:{}", proxy.port);
    let bare = format!("127.0.0.1:{}", proxy.port);
    let cases: Vec<(Vec<(&str, &str)>, &str)> = vec![
        (vec![("CHIP_PROXY", &https)], "CHIP_PROXY"),
        (vec![("CHIP_PROXY", &socks)], "CHIP_PROXY"),
        (vec![("CHIP_PROXY", &bare)], "CHIP_PROXY"),
        (vec![("CHIP_PROXY", "http://127.0.0.1")], "CHIP_PROXY"),
        (vec![("CHIP_PROXY", "::not a url::")], "CHIP_PROXY"),
        (vec![("CHIP_PROXY", &embedded)], "CHIP_PROXY"),
        (
            vec![("CHIP_PROXY", &proxy.url), ("CHIP_PROXY_USERNAME", "chip")],
            "CHIP_PROXY_PASSWORD",
        ),
        (
            vec![
                ("CHIP_PROXY", &proxy.url),
                ("CHIP_PROXY_PASSWORD", PROXY_PASSWORD),
            ],
            "CHIP_PROXY_USERNAME",
        ),
        (
            vec![
                ("CHIP_PROXY", &proxy.url),
                ("CHIP_PROXY_USERNAME", "chip"),
                ("CHIP_PROXY_PASSWORD", "pp-SENTINEL-proxy-b81d44e6/slash"),
            ],
            "CHIP_PROXY_PASSWORD",
        ),
        (
            vec![
                ("CHIP_PROXY", &proxy.url),
                ("CHIP_NO_PROXY", "ok.example, not a host"),
            ],
            "CHIP_NO_PROXY",
        ),
        (vec![("CHIP_PROXY_PASSWORD", PROXY_PASSWORD)], "CHIP_PROXY"),
    ];
    for (pairs, variable) in cases {
        let mut c = sandbox.command(&endpoint, Some(KEY));
        for (name, value) in &pairs {
            c.env(name, value);
        }
        let (out, text) = run(c);
        assert!(!out.status.success(), "{pairs:?} was accepted: {text}");
        assert!(
            text.contains(variable) && text.contains("nothing was run"),
            "{pairs:?}: the error does not name {variable}: {text}"
        );
        assert_no_secret(&sandbox, &text);
    }
    assert_eq!(
        proxy.heads(),
        Vec::<String>::new(),
        "the proxy was contacted"
    );
    assert_eq!(
        target.heads(),
        Vec::<String>::new(),
        "the endpoint was contacted"
    );
}

#[test]
fn chip_no_proxy_sends_a_listed_host_direct_and_everything_else_through_the_proxy() {
    let sandbox = Sandbox::new("no-proxy");
    // Keyless: a credential is never sent over plain http through a proxy, and the loopback mock
    // is plain http.
    for (no_proxy, proxied) in [
        ("127.0.0.1", false),
        ("other.example, 127.0.0.1", false),
        ("other.example, .corp.example, *.internal.example", true),
        ("127.0.0.10", true),
    ] {
        let (server, url) = endpoint("routed");
        let proxy = Proxy::start(Some(("chip", PROXY_PASSWORD)));
        let mut c = sandbox.command(&url, None);
        c.env("CHIP_PROXY", &proxy.url)
            .env("CHIP_PROXY_USERNAME", "chip")
            .env("CHIP_PROXY_PASSWORD", PROXY_PASSWORD)
            .env("CHIP_NO_PROXY", no_proxy);
        let (out, text) = run(c);
        assert!(out.status.success(), "{no_proxy}: {text}");
        assert!(text.contains("routed"), "{text}");
        assert_eq!(server.connections(), 1, "{no_proxy}");
        let seen = proxy.seen();
        if proxied {
            assert_eq!(seen.len(), 1, "{no_proxy}: the proxy was bypassed");
            assert!(
                seen[0].request_line.starts_with("CONNECT 127.0.0.1:"),
                "{seen:?}"
            );
        } else {
            assert!(seen.is_empty(), "{no_proxy}: the proxy was used: {seen:?}");
        }
        let head = server.recorded()[0].head.to_ascii_lowercase();
        assert!(!head.contains("proxy-authorization"), "{head}");
        assert_no_secret(&sandbox, &text);
    }
}

/// Variables other tools read to weaken TLS. None of them means anything to `stack-agent`.
const INSECURE_KNOBS: [(&str, &str); 6] = [
    ("NODE_TLS_REJECT_UNAUTHORIZED", "0"),
    ("PYTHONHTTPSVERIFY", "0"),
    ("CURL_INSECURE", "1"),
    ("GIT_SSL_NO_VERIFY", "1"),
    ("CHIP_INSECURE", "1"),
    ("CHIP_TLS_VERIFY", "0"),
];

#[test]
fn ssl_cert_file_is_the_only_way_to_trust_a_private_ca_directly_or_through_chip_proxy() {
    if !openssl_available() {
        return;
    }
    let (pki, other) = (Pki::generate("binary"), Pki::generate("binary-other"));
    let sandbox = Sandbox::new("tls");
    let ca = pki.ca_path().display().to_string();
    let wrong = other.ca_path().display().to_string();
    let url = |server: &TlsServer| format!("{}/chat/completions", server.base_url());

    // Trusted with SSL_CERT_FILE: completes, and the key went to the endpoint inside TLS.
    let server = TlsServer::start(&pki, &event_stream("private ca ok"));
    let mut c = sandbox.command(&url(&server), Some(KEY));
    c.env("SSL_CERT_FILE", &ca);
    let (out, text) = run(c);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("private ca ok"), "{text}");
    assert!(
        server
            .received_with(&format!("Bearer {KEY}"))
            .contains(&format!("Bearer {KEY}"))
    );
    assert_no_secret(&sandbox, &text);
    drop(server);

    // The same through CHIP_PROXY (authenticating), end to end.
    let server = TlsServer::start(&pki, &event_stream("tunnelled ok"));
    let proxy = Proxy::start(Some(("chip", PROXY_PASSWORD)));
    let mut c = sandbox.command(&url(&server), Some(KEY));
    c.env("SSL_CERT_FILE", &ca)
        .env("CHIP_PROXY", &proxy.url)
        .env("CHIP_PROXY_USERNAME", "chip")
        .env("CHIP_PROXY_PASSWORD", PROXY_PASSWORD);
    let (out, text) = run(c);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("tunnelled ok"), "{text}");
    assert_eq!(proxy.seen().len(), 1);
    assert_no_secret(&sandbox, &text);
    drop((server, proxy));

    // Refused: no bundle (the bundled public roots), the wrong CA, each directly and through the
    // proxy, with every "insecure" knob other tools know set. The key never reaches the server.
    for (bundle, through_proxy) in [
        (None, false),
        (None, true),
        (Some(wrong.as_str()), false),
        (Some(wrong.as_str()), true),
    ] {
        let server = TlsServer::start(&pki, &event_stream("never"));
        let proxy = Proxy::start(None);
        let mut c = sandbox.command(&url(&server), Some(KEY));
        for (name, value) in INSECURE_KNOBS {
            c.env(name, value);
        }
        if let Some(bundle) = bundle {
            c.env("SSL_CERT_FILE", bundle);
        }
        if through_proxy {
            c.env("CHIP_PROXY", &proxy.url);
        }
        let (out, text) = run(c);
        let case = format!("bundle {bundle:?}, proxy {through_proxy}");
        assert!(
            !out.status.success(),
            "{case}: an untrusted server was accepted: {text}"
        );
        assert!(text.contains("certificate"), "{case}: {text}");
        assert!(
            !server.received().contains(KEY),
            "{case}: the key crossed an untrusted session"
        );
        assert_eq!(proxy.seen().len(), usize::from(through_proxy), "{case}");
        assert_no_secret(&sandbox, &text);
    }
}

#[test]
fn an_empty_or_unreadable_ssl_cert_file_fails_before_any_request_naming_the_variable() {
    let target = Listener::start(String::new());
    let sandbox = Sandbox::new("bad-bundle");
    let empty = sandbox.root.join("empty.pem");
    std::fs::write(&empty, "").unwrap();
    let garbage = sandbox.root.join("garbage.pem");
    std::fs::write(&garbage, "not a certificate\n").unwrap();
    let missing = sandbox.root.join("missing.pem");
    let endpoint = format!("https://127.0.0.1:{}/v1/chat/completions", target.port);
    for bundle in [&missing, &empty, &garbage] {
        let mut c = sandbox.command(&endpoint, Some(KEY));
        c.env("SSL_CERT_FILE", bundle);
        let (out, text) = run(c);
        assert!(!out.status.success(), "{bundle:?} was accepted: {text}");
        assert!(
            text.contains("SSL_CERT_FILE") && text.contains("nothing was run"),
            "{bundle:?}: {text}"
        );
        assert_no_secret(&sandbox, &text);
    }
    assert_eq!(
        target.heads(),
        Vec::<String>::new(),
        "the endpoint was contacted"
    );
}
