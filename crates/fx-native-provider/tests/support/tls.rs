//! Test-only TLS tooling: generated PKI and an `openssl s_server` endpoint on loopback, as in
//! `tests/private_ca.rs`. The `openssl` command is test tooling only; the crate stays on rustls.
//!
//! Without `openssl`, [`openssl_available`] prints `SKIPPED` and the caller returns; with
//! `FX_REQUIRE_TLS_TESTS` set (as on CI) a missing `openssl` is a failure, never a skip.
//!
//! This file is standalone (std only) so other crates' tests can include it with `#[path]`.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static DIRS: AtomicUsize = AtomicUsize::new(0);

/// Whether `openssl` can run. Fails the test when it cannot and `FX_REQUIRE_TLS_TESTS` is set.
pub fn openssl_available() -> bool {
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
        eprintln!("SKIPPED: openssl is not available; the TLS tests did not run");
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

const SERVER_CNF: &str = "[req]\nprompt = no\ndistinguished_name = dn\n\
     [dn]\nCN = localhost\n\
     [server]\nbasicConstraints = critical, CA:FALSE\n\
     keyUsage = critical, digitalSignature, keyEncipherment\n\
     extendedKeyUsage = serverAuth\nsubjectAltName = IP:127.0.0.1, DNS:localhost\n";

/// Certificates for one test, in a private temporary directory. The server certificate names
/// `127.0.0.1` and `localhost` only.
pub struct Pki {
    pub dir: PathBuf,
}

impl Pki {
    fn dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fx-tls-{}-{}-{name}",
            std::process::id(),
            DIRS.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("server.cnf"), SERVER_CNF).unwrap();
        dir
    }

    /// A CA (`ca.pem`) and a server certificate signed by it, written with explicit config files
    /// so OpenSSL and LibreSSL produce the same certificates.
    pub fn generate(name: &str) -> Self {
        let dir = Self::dir(name);
        std::fs::write(
            dir.join("ca.cnf"),
            "[req]\nprompt = no\ndistinguished_name = dn\nx509_extensions = ca\n\
             [dn]\nCN = fx test CA\n\
             [ca]\nbasicConstraints = critical, CA:TRUE\nkeyUsage = critical, keyCertSign, cRLSign\n\
             subjectKeyIdentifier = hash\n",
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

    /// A self-signed server certificate with no CA at all (`ca.pem` is absent).
    pub fn self_signed(name: &str) -> Self {
        let dir = Self::dir(name);
        openssl(
            &dir,
            &[
                "req",
                "-x509",
                "-new",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "2",
                "-config",
                "server.cnf",
                "-extensions",
                "server",
                "-keyout",
                "server.key",
                "-out",
                "server.pem",
            ],
        );
        Self { dir }
    }

    /// The CA certificate (PEM).
    pub fn ca(&self) -> Vec<u8> {
        std::fs::read(self.dir.join("ca.pem")).unwrap()
    }

    pub fn ca_path(&self) -> PathBuf {
        self.dir.join("ca.pem")
    }
}

impl Drop for Pki {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// `openssl s_server` presenting the PKI's server certificate on every local address. Whatever it
/// is sent on stdin goes to the first client that completes a handshake, and what that client
/// sends is captured.
pub struct TlsServer {
    child: Child,
    pub port: u16,
    received: PathBuf,
}

impl TlsServer {
    pub fn start(pki: &Pki, response: &[u8]) -> Self {
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

    /// The API root at `host` (which must reach this machine).
    pub fn base_url_at(&self, host: &str) -> String {
        format!("https://{host}:{}/v1", self.port)
    }

    pub fn base_url(&self) -> String {
        self.base_url_at("127.0.0.1")
    }

    /// Everything s_server wrote: its status lines and what a trusting client sent.
    pub fn received(&self) -> String {
        let mut text = String::new();
        if let Ok(mut file) = std::fs::File::open(&self.received) {
            let _ = file.read_to_string(&mut text);
        }
        text
    }

    /// What s_server has written once it contains `needle`, waiting up to 10 s. s_server sends the
    /// queued response as soon as it accepts a client, so the client can finish before s_server
    /// has written the request it received to its output: reading at once can miss it.
    pub fn received_with(&self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let text = self.received();
            if text.contains(needle) || Instant::now() >= deadline {
                return text;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for TlsServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A complete OpenAI-style event stream response saying `text`.
pub fn event_stream(text: &str) -> Vec<u8> {
    let mut response =
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n".to_vec();
    let quoted = format!("{text:?}");
    response.extend(
        format!(
            "data: {{\"model\":\"mock-1\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":{quoted}}},\"finish_reason\":null}}]}}\n\n"
        )
        .bytes(),
    );
    response.extend(
        b"data: {\"model\":\"mock-1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    );
    response.extend(b"data: [DONE]\n\n");
    response
}
