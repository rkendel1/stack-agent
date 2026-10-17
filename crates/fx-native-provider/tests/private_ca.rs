//! A TLS endpoint signed by a private CA (fx G9, chip-rs#35), through the real FX executor.
//!
//! The endpoint is `openssl s_server` on loopback with a certificate from a CA generated for
//! the test; it answers with a canned event stream, so the call completes only when the TLS
//! session was established and trusted. The `openssl` command is test tooling only: the crate
//! itself stays on rustls. Without `openssl` the tests print `SKIPPED` and pass; with
//! `FX_REQUIRE_TLS_TESTS` set (as on CI) a missing `openssl` is a failure.

mod support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use fx_core::*;
use fx_native_provider::{OpenAiCompatible, OpenAiConfig, Secret};
use fx_single_attempt::SingleAttemptCapability;
use support::proxy::Proxy;
use support::*;

const KEY: &str = "sk-private-ca-0123456789abcdef";

static DIRS: AtomicUsize = AtomicUsize::new(0);

fn openssl_available() -> bool {
    let found = Command::new("openssl")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !found {
        assert!(
            std::env::var_os("FX_REQUIRE_TLS_TESTS").is_none(),
            "FX_REQUIRE_TLS_TESTS is set but openssl is not available"
        );
        eprintln!("SKIPPED: openssl is not available; the private-CA tests did not run");
    }
    found
}

fn openssl(dir: &Path, args: &[&str]) {
    let output = Command::new("openssl")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run openssl");
    assert!(
        output.status.success(),
        "openssl {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A CA and a server certificate for `127.0.0.1` and `localhost` signed by it, written with
/// explicit config files so OpenSSL and LibreSSL produce the same certificates.
struct Pki {
    dir: PathBuf,
}

impl Pki {
    fn generate(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "fx-private-ca-{}-{}-{name}",
            std::process::id(),
            DIRS.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("ca.cnf"),
            "[req]\nprompt = no\ndistinguished_name = dn\nx509_extensions = ca\n\
             [dn]\nCN = fx test CA\n\
             [ca]\nbasicConstraints = critical, CA:TRUE\nkeyUsage = critical, keyCertSign, cRLSign\n\
             subjectKeyIdentifier = hash\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("server.cnf"),
            "[req]\nprompt = no\ndistinguished_name = dn\n\
             [dn]\nCN = localhost\n\
             [server]\nbasicConstraints = critical, CA:FALSE\n\
             keyUsage = critical, digitalSignature, keyEncipherment\n\
             extendedKeyUsage = serverAuth\nsubjectAltName = IP:127.0.0.1, DNS:localhost\n",
        )
        .unwrap();
        openssl(
            &dir,
            &[
                "req", "-x509", "-new", "-newkey", "rsa:2048", "-nodes", "-days", "2", "-config",
                "ca.cnf", "-keyout", "ca.key", "-out", "ca.pem",
            ],
        );
        openssl(
            &dir,
            &[
                "req",
                "-new",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-config",
                "server.cnf",
                "-keyout",
                "server.key",
                "-out",
                "server.csr",
            ],
        );
        openssl(
            &dir,
            &[
                "x509",
                "-req",
                "-in",
                "server.csr",
                "-CA",
                "ca.pem",
                "-CAkey",
                "ca.key",
                "-set_serial",
                "2",
                "-days",
                "2",
                "-extfile",
                "server.cnf",
                "-extensions",
                "server",
                "-out",
                "server.pem",
            ],
        );
        Self { dir }
    }

    fn ca(&self) -> Vec<u8> {
        std::fs::read(self.dir.join("ca.pem")).unwrap()
    }
}

impl Drop for Pki {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `openssl s_server` presenting the PKI's server certificate. Whatever it is sent on stdin goes
/// to the first client that completes a handshake, and what that client sends is captured.
struct TlsServer {
    child: Child,
    port: u16,
    received: PathBuf,
}

impl TlsServer {
    fn start(pki: &Pki, response: &[u8]) -> Self {
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
        };
        let received = pki.dir.join(format!("received-{port}"));
        let child = Command::new("openssl")
            .args(["s_server", "-cert", "server.pem", "-key", "server.key"])
            .args(["-accept", &port.to_string()])
            .current_dir(&pki.dir)
            .stdin(Stdio::piped())
            .stdout(std::fs::File::create(&received).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .expect("start openssl s_server");
        let mut server = Self {
            child,
            port,
            received,
        };
        // s_server reads no input until a client is accepted, so the response waits in the pipe.
        // The pipe is held open: EOF would end the session.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !server.received().contains("ACCEPT") {
            assert!(Instant::now() < deadline, "s_server did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        let stdin = server.child.stdin.as_mut().unwrap();
        stdin.write_all(response).unwrap();
        server
    }

    fn base_url(&self) -> String {
        format!("https://127.0.0.1:{}/v1", self.port)
    }

    fn received(&self) -> String {
        let mut text = String::new();
        if let Ok(mut file) = std::fs::File::open(&self.received) {
            let _ = file.read_to_string(&mut text);
        }
        text
    }
}

impl Drop for TlsServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn event_stream() -> Vec<u8> {
    let mut response =
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n".to_vec();
    for chunk in [delta("over tls"), finish("stop"), done()] {
        response.extend(chunk);
    }
    response
}

struct TestHost;

impl fx_core::Host for TestHost {
    fn cancel_requested(&mut self) -> bool {
        false
    }
    fn on_event(&mut self, _: &Event) -> Flow {
        Flow::Continue
    }
}

fn request() -> Request {
    Request {
        model: ModelRef("mock-1".to_owned()),
        messages: vec![Message {
            role: Role::User,
            parts: vec![Part::Text("say hi".to_owned())],
            cache: CacheHint::Default,
        }],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        options: GenerationOptions::default(),
        correlation: Correlation {
            request_id: RequestId([5; 16]),
            affinity_key: None,
        },
        budget: None,
    }
}

fn run(config: OpenAiConfig) -> ExecutionReport {
    let config = config.with_default_deadline(Duration::from_secs(20));
    let mut capability = SingleAttemptCapability::new(OpenAiCompatible::new(config));
    execute(&mut capability, &request(), &Limits::DEFAULT, &mut TestHost)
}

fn completed_text(report: &ExecutionReport) -> String {
    match &report.outcome {
        Outcome::Completed(completion) => format!("{completion:?}"),
        other => panic!("expected a completion over TLS, got {other:?}"),
    }
}

fn transport_failure(report: &ExecutionReport) {
    match &report.outcome {
        Outcome::Failed(failure) => assert_eq!(failure.kind, ErrorKind::Transport, "{failure:?}"),
        other => panic!("an untrusted certificate was accepted: {other:?}"),
    }
}

#[test]
fn a_private_ca_endpoint_is_refused_by_default_and_trusted_with_its_ca() {
    if !openssl_available() {
        return;
    }
    let pki = Pki::generate("default");

    // The built-in roots do not trust the private CA: the handshake fails and nothing is sent.
    let untrusted = TlsServer::start(&pki, &event_stream());
    let config = OpenAiConfig::new(&untrusted.base_url(), Secret::new(KEY)).unwrap();
    transport_failure(&run(config));
    assert!(
        !untrusted.received().contains(KEY),
        "the credential crossed an untrusted session"
    );
    drop(untrusted);

    // With the CA supplied, the same endpoint completes, and the credential went inside TLS.
    let trusted = TlsServer::start(&pki, &event_stream());
    let config = OpenAiConfig::new(&trusted.base_url(), Secret::new(KEY))
        .unwrap()
        .with_root_certificates_pem(&pki.ca())
        .unwrap();
    assert!(completed_text(&run(config)).contains("over tls"));
    let received = trusted.received();
    assert!(
        received.contains("POST /v1/chat/completions "),
        "{received}"
    );
    assert!(received.contains(&format!("Bearer {KEY}")), "{received}");
}

#[test]
fn a_different_private_ca_is_not_trusted() {
    if !openssl_available() {
        return;
    }
    let (pki, other) = (Pki::generate("server"), Pki::generate("other"));
    let server = TlsServer::start(&pki, &event_stream());
    let config = OpenAiConfig::new(&server.base_url(), Secret::new(KEY))
        .unwrap()
        .with_root_certificates_pem(&other.ca())
        .unwrap();
    transport_failure(&run(config));
    assert!(!server.received().contains(KEY));
}

#[test]
fn a_private_ca_endpoint_is_reached_through_a_proxy_end_to_end() {
    if !openssl_available() {
        return;
    }
    let pki = Pki::generate("proxy");
    let server = TlsServer::start(&pki, &event_stream());
    let proxy = Proxy::start(None);
    let config = OpenAiConfig::new(&server.base_url(), Secret::new(KEY))
        .unwrap()
        .with_root_certificates_pem(&pki.ca())
        .unwrap()
        .with_proxy(&proxy.url)
        .unwrap();
    assert!(completed_text(&run(config)).contains("over tls"));
    let seen = proxy.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(
        seen[0].request_line,
        format!("CONNECT 127.0.0.1:{} HTTP/1.1", server.port)
    );
    assert!(server.received().contains(&format!("Bearer {KEY}")));
}
