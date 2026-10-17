//! TLS verification holds on every supported path: direct, and through an explicit CONNECT proxy
//! (known issue F). There is no switch that turns verification off, so the only way to reach an
//! endpoint with a private certificate is to trust its CA; a self-signed certificate, a different
//! CA or a certificate for another host is refused, and the credential never crosses the
//! untrusted session. Failure messages carry neither the API key nor the proxy password.
//!
//! The endpoint is `openssl s_server` on loopback (test tooling only; the crate stays on rustls).
//! Without `openssl` the tests print `SKIPPED`; with `FX_REQUIRE_TLS_TESTS` set (as on CI) a
//! missing `openssl` is a failure.

mod support;
#[path = "support/tls.rs"]
mod tls;

use std::time::Duration;

use fx_core::*;
use fx_native_provider::{OpenAiCompatible, OpenAiConfig, Secret};
use fx_single_attempt::SingleAttemptCapability;
use support::proxy::{Proxy, base64};
use tls::{Pki, TlsServer, event_stream, openssl_available};

const KEY: &str = "sk-tls-boundary-0123456789abcdef";
const PROXY_PASSWORD: &str = "pp-tls-proxy-secret-9876543210";

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
            request_id: RequestId([6; 16]),
            affinity_key: None,
        },
        budget: None,
    }
}

/// Runs one call and returns the report with everything a caller could print from it.
fn run(config: OpenAiConfig) -> (ExecutionReport, String) {
    let config = config.with_default_deadline(Duration::from_secs(20));
    let mut host = TestHost(Vec::new());
    let mut capability = SingleAttemptCapability::new(OpenAiCompatible::new(config));
    let report = execute(&mut capability, &request(), &Limits::DEFAULT, &mut host);
    let printed = format!("{report:?} {:?}", host.0);
    (report, printed)
}

fn keyed(server: &TlsServer, host: &str) -> OpenAiConfig {
    OpenAiConfig::new(&server.base_url_at(host), Secret::new(KEY)).unwrap()
}

fn proxied(config: OpenAiConfig, proxy: &Proxy) -> OpenAiConfig {
    config
        .with_proxy(&proxy.url)
        .unwrap()
        .with_proxy_credentials("fx-user", Secret::new(PROXY_PASSWORD))
        .unwrap()
}

/// The call failed as a transport failure because certificate verification refused the server
/// (`why` is the rustls reason), nothing secret is in any output, and the credential never reached
/// the server (it would only be sent inside a trusted session).
fn refused(server: &TlsServer, (report, printed): (ExecutionReport, String), why: &str) {
    match &report.outcome {
        Outcome::Failed(failure) => {
            assert_eq!(failure.kind, ErrorKind::Transport, "{failure:?}");
            let detail = format!("{failure:?}");
            assert!(
                detail.contains("invalid peer certificate") && detail.contains(why),
                "not refused by certificate verification ({why}): {detail}"
            );
        }
        other => panic!("an untrusted certificate was accepted: {other:?}"),
    }
    assert!(!printed.contains(KEY), "the key leaked: {printed}");
    assert!(
        !printed.contains(PROXY_PASSWORD),
        "the proxy password leaked"
    );
    assert!(!printed.contains(&base64(&format!("fx-user:{PROXY_PASSWORD}"))));
    assert!(
        !server.received().contains(KEY),
        "the credential crossed an untrusted session"
    );
}

fn completed(report: &ExecutionReport) -> String {
    match &report.outcome {
        Outcome::Completed(completion) => format!("{completion:?}"),
        other => panic!("expected a completion over TLS, got {other:?}"),
    }
}

/// The proxy was really on the path: exactly one tunnel to the endpoint, authenticated.
fn tunnelled_once(proxy: &Proxy, host: &str, port: u16) {
    let seen = proxy.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(
        seen[0].request_line,
        format!("CONNECT {host}:{port} HTTP/1.1")
    );
    assert!(seen[0].authorization.is_some());
}

#[test]
fn a_self_signed_endpoint_is_refused_directly_and_through_a_proxy() {
    if !openssl_available() {
        return;
    }
    let pki = Pki::self_signed("self-signed");

    let server = TlsServer::start(&pki, &event_stream("never"));
    refused(&server, run(keyed(&server, "127.0.0.1")), "UnknownIssuer");
    drop(server);

    let server = TlsServer::start(&pki, &event_stream("never"));
    let proxy = Proxy::start(Some(("fx-user", PROXY_PASSWORD)));
    refused(
        &server,
        run(proxied(keyed(&server, "127.0.0.1"), &proxy)),
        "UnknownIssuer",
    );
    tunnelled_once(&proxy, "127.0.0.1", server.port);
}

#[test]
fn a_different_private_ca_is_refused_through_a_proxy_too() {
    if !openssl_available() {
        return;
    }
    let (pki, other) = (
        Pki::generate("tunnel-server"),
        Pki::generate("tunnel-other"),
    );
    let server = TlsServer::start(&pki, &event_stream("never"));
    let proxy = Proxy::start(Some(("fx-user", PROXY_PASSWORD)));
    let config = proxied(keyed(&server, "127.0.0.1"), &proxy)
        .with_root_certificates_pem(&other.ca())
        .unwrap();
    refused(&server, run(config), "BadSignature");
    tunnelled_once(&proxy, "127.0.0.1", server.port);
}

#[test]
fn the_bundled_roots_do_not_trust_a_private_ca_through_a_proxy() {
    if !openssl_available() {
        return;
    }
    let pki = Pki::generate("tunnel-default");
    let server = TlsServer::start(&pki, &event_stream("never"));
    let proxy = Proxy::start(None);
    let config = keyed(&server, "127.0.0.1").with_proxy(&proxy.url).unwrap();
    refused(&server, run(config), "UnknownIssuer");
    assert_eq!(proxy.seen().len(), 1, "{:?}", proxy.seen());
}

#[test]
fn a_certificate_for_another_host_is_refused_even_when_its_ca_is_trusted() {
    if !openssl_available() {
        return;
    }
    // The certificate names 127.0.0.1 and localhost; 127.0.0.2 reaches the same server.
    let pki = Pki::generate("hostname");

    let server = TlsServer::start(&pki, &event_stream("never"));
    let config = keyed(&server, "127.0.0.2")
        .with_root_certificates_pem(&pki.ca())
        .unwrap();
    refused(&server, run(config), "not valid for name");
    drop(server);

    let server = TlsServer::start(&pki, &event_stream("never"));
    let proxy = Proxy::start(Some(("fx-user", PROXY_PASSWORD)));
    let config = proxied(keyed(&server, "127.0.0.2"), &proxy)
        .with_root_certificates_pem(&pki.ca())
        .unwrap();
    refused(&server, run(config), "not valid for name");
    tunnelled_once(&proxy, "127.0.0.2", server.port);

    // The same server, by a name its certificate carries, with the same trust: accepted. So the
    // refusals above were the host check, not a broken setup.
    let server = TlsServer::start(&pki, &event_stream("by name"));
    let config = keyed(&server, "localhost")
        .with_root_certificates_pem(&pki.ca())
        .unwrap();
    let (report, printed) = run(config);
    assert!(completed(&report).contains("by name"));
    assert!(!printed.contains(KEY));
}

#[test]
fn a_private_ca_endpoint_completes_through_an_authenticating_proxy() {
    if !openssl_available() {
        return;
    }
    let pki = Pki::generate("tunnel-auth");
    let server = TlsServer::start(&pki, &event_stream("tunnelled tls"));
    let proxy = Proxy::start(Some(("fx-user", PROXY_PASSWORD)));
    let config = proxied(keyed(&server, "127.0.0.1"), &proxy)
        .with_root_certificates_pem(&pki.ca())
        .unwrap();
    let (report, printed) = run(config);
    assert!(completed(&report).contains("tunnelled tls"));
    assert!(!printed.contains(KEY) && !printed.contains(PROXY_PASSWORD));
    tunnelled_once(&proxy, "127.0.0.1", server.port);
    let received = server.received_with(&format!("Bearer {KEY}"));
    // The key went to the endpoint inside TLS; the proxy credential did not go to the endpoint.
    assert!(received.contains(&format!("Bearer {KEY}")), "{received}");
    assert!(
        !received
            .to_ascii_lowercase()
            .contains("proxy-authorization")
    );
}
