//! Transport-level boundaries of the explicit proxy and of redaction (known issue F), through the
//! real FX executor against loopback stand-ins. Mocked transport only.
//!
//! - A proxy that refuses authentication and echoes the credentials back (in its status line,
//!   a header and its body) fails the call once: no retry, no direct connection, and neither the
//!   proxy password nor its Basic encoding appears in the report or any event.
//! - A transport diagnostic that quotes the API key (an endpoint answering garbage that contains
//!   it) never carries the key out.
//! - `no_proxy` decides between the proxy and a direct connection exactly as documented: an exact
//!   host or a `.domain` / `*.domain` suffix bypasses the proxy, anything else goes through it.

mod support;

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use fx_core::*;
use fx_native_provider::{OpenAiCompatible, OpenAiConfig, Secret};
use fx_single_attempt::SingleAttemptCapability;
use support::proxy::{Proxy, base64};
use support::*;

const KEY: &str = "sk-transport-boundary-0123456789abcdef";
const PROXY_PASSWORD: &str = "pp-echoed-proxy-secret-5550123";

struct TestHost(Vec<Event>);

impl fx_core::Host for TestHost {
    fn cancel_requested(&mut self) -> bool {
        false
    }
    fn on_event(&mut self, event: &Event) -> Flow {
        self.0.push(event.clone());
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
            request_id: RequestId([8; 16]),
            affinity_key: None,
        },
        budget: None,
    }
}

/// One call; the report and everything a caller could print from it.
fn run(config: OpenAiConfig) -> (ExecutionReport, String) {
    let config = config.with_default_deadline(Duration::from_secs(10));
    let mut host = TestHost(Vec::new());
    let mut capability = SingleAttemptCapability::new(OpenAiCompatible::new(config));
    let report = execute(&mut capability, &request(), &Limits::DEFAULT, &mut host);
    let printed = format!("{report:?} {:?}", host.0);
    (report, printed)
}

fn transport_failure(report: &ExecutionReport) -> &Failure {
    match &report.outcome {
        Outcome::Failed(failure) if failure.kind == ErrorKind::Transport => failure,
        other => panic!("expected a transport failure, got {other:?}"),
    }
}

fn assert_no_secret(printed: &str) {
    for secret in [
        KEY,
        "0123456789abcdef",
        PROXY_PASSWORD,
        &base64(&format!("fx-user:{PROXY_PASSWORD}")),
    ] {
        assert!(!printed.contains(secret), "{secret} leaked: {printed}");
    }
}

/// A proxy stand-in that answers every request with `reply(proxy_authorization)` and records
/// each request line. It never tunnels.
struct EchoProxy {
    url: String,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl EchoProxy {
    fn start(reply: fn(&str) -> String) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        listener.set_nonblocking(true).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen_in, stop_in) = (seen.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !stop_in.load(Ordering::SeqCst) {
                let Ok((mut client, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                let _ = client.set_nonblocking(false);
                let _ = client.set_read_timeout(Some(Duration::from_secs(5)));
                let head = read_head(&mut client);
                let mut lines = head.split("\r\n");
                seen_in
                    .lock()
                    .unwrap()
                    .push(lines.next().unwrap_or_default().to_owned());
                let authorization = lines
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("proxy-authorization")
                            .then(|| value.trim().to_owned())
                    })
                    .unwrap_or_default();
                let _ = client.write_all(reply(&authorization).as_bytes());
                let _ = client.shutdown(Shutdown::Both);
            }
        });
        Self {
            url,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for EchoProxy {
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

/// The decoded `user:password` of a Basic `Proxy-Authorization` value, as a careless proxy might
/// quote it. Only the test's own credentials are expected.
fn decoded(authorization: &str) -> String {
    let expected = base64(&format!("fx-user:{PROXY_PASSWORD}"));
    if authorization.ends_with(&expected) {
        format!("fx-user:{PROXY_PASSWORD}")
    } else {
        "unexpected".to_owned()
    }
}

fn echo_407(authorization: &str) -> String {
    let body = format!(
        "{{\"error\":\"bad proxy credentials {} ({authorization}) for {}\"}}",
        decoded(authorization),
        PROXY_PASSWORD
    );
    format!(
        "HTTP/1.1 407 Rejected {} {authorization}\r\nproxy-authenticate: Basic realm=\"{authorization}\"\r\nx-echo: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        decoded(authorization),
        PROXY_PASSWORD,
        body.len()
    )
}

fn echo_502(authorization: &str) -> String {
    let body = format!("upstream refused for {}", decoded(authorization));
    format!(
        "HTTP/1.1 502 Bad Gateway {authorization}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn echo_garbage(authorization: &str) -> String {
    format!(
        "NOT-HTTP {} {authorization}\r\n\r\n",
        decoded(authorization)
    )
}

#[test]
fn a_proxy_that_echoes_the_credentials_in_its_refusal_fails_once_and_never_leaks_them() {
    for reply in [echo_407 as fn(&str) -> String, echo_502, echo_garbage] {
        let server = Server::start(vec![sse(vec![delta("never"), finish("stop"), done()])]);
        let proxy = EchoProxy::start(reply);
        let config = OpenAiConfig::without_key(&server.base_url)
            .unwrap()
            .with_proxy(&proxy.url)
            .unwrap()
            .with_proxy_credentials("fx-user", Secret::new(PROXY_PASSWORD))
            .unwrap();
        let (report, printed) = run(config);
        transport_failure(&report);
        assert_no_secret(&printed);
        // One CONNECT, nothing retried, and no direct connection as a fallback.
        let seen = proxy.seen();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert!(seen[0].starts_with("CONNECT 127.0.0.1:"), "{seen:?}");
        assert_eq!(server.connections(), 0, "the endpoint was reached directly");
    }
}

#[test]
fn proxy_authentication_failure_is_reported_as_such_without_a_retry() {
    // The standard stand-in: it requires other credentials and answers a plain 407.
    let server = Server::start(vec![sse(vec![delta("never"), finish("stop"), done()])]);
    let proxy = Proxy::start(Some(("fx-user", "the-right-one")));
    let config = OpenAiConfig::without_key(&server.base_url)
        .unwrap()
        .with_proxy(&proxy.url)
        .unwrap()
        .with_proxy_credentials("fx-user", Secret::new(PROXY_PASSWORD))
        .unwrap();
    let (report, printed) = run(config);
    let failure = transport_failure(&report);
    assert!(
        format!("{failure:?}").contains("407"),
        "the 407 is visible to the operator: {failure:?}"
    );
    assert_no_secret(&printed);
    assert_eq!(proxy.seen().len(), 1, "{:?}", proxy.seen());
    assert_eq!(server.connections(), 0);
}

/// A server that answers one connection with `reply` (raw bytes) and records the request head.
fn raw_server(reply: Vec<u8>) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let base_url = format!(
        "http://127.0.0.1:{}/v1",
        listener.local_addr().unwrap().port()
    );
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let head = read_head(&mut stream);
        let _ = stream.write_all(&reply);
        let _ = stream.flush();
        std::thread::sleep(Duration::from_millis(20));
        let _ = stream.shutdown(Shutdown::Both);
        head
    });
    (base_url, thread)
}

#[test]
fn a_transport_diagnostic_that_quotes_the_key_never_carries_it_out() {
    for reply in [
        // Not an HTTP response at all, with the key in it.
        format!("{KEY} is not a valid key\r\n\r\n"),
        // A status line the client cannot parse, with the key as its reason.
        format!("HTTP/1.1 4o1 {KEY}\r\ncontent-length: 0\r\n\r\n"),
        // A header block that is cut off after quoting the key.
        format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nx-key: {KEY}"),
        // A 200 stream whose first event quotes the key in malformed JSON.
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {{\"bad\": \"{KEY}\"\n\n"
        ),
    ] {
        let (base_url, server) = raw_server(reply.clone().into_bytes());
        let config = OpenAiConfig::new(&base_url, Secret::new(KEY)).unwrap();
        let (report, printed) = run(config);
        assert!(
            matches!(report.outcome, Outcome::Failed(_)),
            "{reply:?}: {:?}",
            report.outcome
        );
        assert_no_secret(&printed);
        // The key really was in play: it went out in the authorization header.
        let head = server.join().unwrap();
        assert!(head.contains(&format!("Bearer {KEY}")), "{head}");
    }
}

fn reply() -> Reply {
    sse(vec![delta("hi"), finish("stop"), done()])
}

/// A keyless loopback endpoint at `host`, through a tunnelling proxy with `no_proxy` entries.
fn through(server: &Server, host: &str, no_proxy: &[&str], proxy: &Proxy) -> OpenAiConfig {
    let base_url = server.base_url.replace("127.0.0.1", host);
    let mut config = OpenAiConfig::without_key(&base_url)
        .unwrap()
        .with_proxy(&proxy.url)
        .unwrap();
    for entry in no_proxy {
        config = config.with_no_proxy(entry).unwrap();
    }
    config
}

#[test]
fn hosts_not_listed_in_no_proxy_go_through_the_proxy() {
    for (host, entries) in [
        (
            "127.0.0.1",
            &[
                "other.example",
                ".example.com",
                "*.corp.example",
                "127.0.0.10",
            ][..],
        ),
        // A subdomain pattern does not match the bare domain.
        ("localhost", &["*.localhost", ".localhost"][..]),
        // A longer or shorter name is not an exact match.
        ("localhost", &["localhost.example", "local"][..]),
    ] {
        let server = Server::start(vec![reply()]);
        let proxy = Proxy::start(None);
        let (report, _) = run(through(&server, host, entries, &proxy));
        assert!(
            matches!(report.outcome, Outcome::Completed(_)),
            "{host} {entries:?}: {:?}",
            report.outcome
        );
        let seen = proxy.seen();
        assert_eq!(seen.len(), 1, "{host} {entries:?} bypassed the proxy");
        assert!(
            seen[0]
                .request_line
                .starts_with(&format!("CONNECT {host}:")),
            "{seen:?}"
        );
        assert_eq!(server.connections(), 1);
    }
}

#[test]
fn a_listed_host_is_reached_directly_and_the_proxy_credentials_stay_unused() {
    for (host, entries) in [
        ("127.0.0.1", &["other.example", "127.0.0.1"][..]),
        ("localhost", &["localhost"][..]),
        // Matching ignores case.
        ("localhost", &["LOCALHOST"][..]),
    ] {
        let server = Server::start(vec![reply()]);
        let proxy = Proxy::start(Some(("fx-user", PROXY_PASSWORD)));
        let config = through(&server, host, entries, &proxy)
            .with_proxy_credentials("fx-user", Secret::new(PROXY_PASSWORD))
            .unwrap();
        let (report, printed) = run(config);
        assert!(
            matches!(report.outcome, Outcome::Completed(_)),
            "{host} {entries:?}: {:?}",
            report.outcome
        );
        assert!(proxy.seen().is_empty(), "{host} {entries:?} used the proxy");
        assert_eq!(server.connections(), 1);
        let head = server.recorded()[0].head.to_ascii_lowercase();
        assert!(!head.contains("proxy-authorization"), "{head}");
        assert_no_secret(&printed);
    }
}
