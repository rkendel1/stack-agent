//! Reaching the endpoint through an explicit HTTP CONNECT proxy (fx G9, chip-rs#35), through the
//! real FX executor against a loopback proxy and mock server. The proxy is the host's explicit
//! setting: nothing here, or in the crate, reads `HTTP_PROXY` or similar.

mod support;

use fx_core::*;
use fx_native_provider::{OpenAiCompatible, OpenAiConfig, Secret};
use fx_single_attempt::SingleAttemptCapability;
use support::proxy::{Proxy, base64};
use support::*;

const PROXY_PASSWORD: &str = "pp-proxy-secret-0123456789";

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
            request_id: RequestId([9; 16]),
            affinity_key: None,
        },
        budget: None,
    }
}

fn run(config: OpenAiConfig) -> (ExecutionReport, TestHost) {
    let mut host = TestHost(Vec::new());
    let mut capability = SingleAttemptCapability::new(OpenAiCompatible::new(config));
    let report = execute(&mut capability, &request(), &Limits::DEFAULT, &mut host);
    (report, host)
}

fn reply() -> Reply {
    sse(vec![delta("hi"), finish("stop"), done()])
}

fn target(server: &Server) -> String {
    server
        .base_url
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap()
        .to_owned()
}

#[test]
fn a_call_goes_through_the_proxy_as_a_connect_tunnel() {
    let server = Server::start(vec![reply()]);
    let proxy = Proxy::start(None);
    let config = OpenAiConfig::without_key(&server.base_url)
        .unwrap()
        .with_proxy(&proxy.url)
        .unwrap();
    let (report, _) = run(config);
    assert!(
        matches!(report.outcome, Outcome::Completed(_)),
        "{:?}",
        report.outcome
    );
    let seen = proxy.seen();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(
        seen[0].request_line,
        format!("CONNECT {} HTTP/1.1", target(&server))
    );
    assert_eq!(seen[0].authorization, None);
    assert_eq!(server.connections(), 1);
}

#[test]
fn a_no_proxy_host_is_reached_directly() {
    let server = Server::start(vec![reply()]);
    let proxy = Proxy::start(None);
    let config = OpenAiConfig::without_key(&server.base_url)
        .unwrap()
        .with_proxy(&proxy.url)
        .unwrap()
        .with_no_proxy("127.0.0.1")
        .unwrap();
    let (report, _) = run(config);
    assert!(matches!(report.outcome, Outcome::Completed(_)));
    assert!(proxy.seen().is_empty(), "{:?}", proxy.seen());
    assert_eq!(server.connections(), 1);
}

#[test]
fn proxy_credentials_are_sent_only_to_the_proxy_and_never_reported() {
    let server = Server::start(vec![reply()]);
    let proxy = Proxy::start(Some(("fx-user", PROXY_PASSWORD)));
    let config = OpenAiConfig::without_key(&server.base_url)
        .unwrap()
        .with_proxy(&proxy.url)
        .unwrap()
        .with_proxy_credentials("fx-user", Secret::new(PROXY_PASSWORD))
        .unwrap();
    assert!(!format!("{config:?}").contains(PROXY_PASSWORD));
    let (report, _) = run(config);
    assert!(
        matches!(report.outcome, Outcome::Completed(_)),
        "{:?}",
        report.outcome
    );
    let seen = proxy.seen();
    let expected = format!("Basic {}", base64(&format!("fx-user:{PROXY_PASSWORD}")));
    assert_eq!(
        seen.last().unwrap().authorization.as_deref(),
        Some(&*expected)
    );
    // The tunnelled request carries no proxy credential to the endpoint.
    let head = &server.recorded()[0].head.to_ascii_lowercase();
    assert!(!head.contains("proxy-authorization"), "{head}");
}

#[test]
fn a_refused_proxy_is_the_calls_failure_with_no_direct_fallback() {
    let server = Server::start(vec![reply()]);
    let proxy = Proxy::start(Some(("fx-user", "the-right-one")));
    let config = OpenAiConfig::without_key(&server.base_url)
        .unwrap()
        .with_proxy(&proxy.url)
        .unwrap()
        .with_proxy_credentials("fx-user", Secret::new(PROXY_PASSWORD))
        .unwrap();
    let (report, host) = run(config);
    let Outcome::Failed(failure) = &report.outcome else {
        panic!("expected a failure, got {:?}", report.outcome)
    };
    assert_eq!(failure.kind, ErrorKind::Transport);
    let all = format!("{report:?} {:?}", host.0);
    assert!(
        !all.contains(PROXY_PASSWORD),
        "the proxy password leaked: {all}"
    );
    assert!(!all.contains(&base64(&format!("fx-user:{PROXY_PASSWORD}"))));
    assert_eq!(server.connections(), 0, "the endpoint was reached directly");
}

#[test]
fn an_unreachable_proxy_is_the_calls_failure_with_no_direct_fallback() {
    let server = Server::start(vec![reply()]);
    let dead = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port())
    };
    let config = OpenAiConfig::without_key(&server.base_url)
        .unwrap()
        .with_proxy(&dead)
        .unwrap();
    let (report, _) = run(config);
    let Outcome::Failed(failure) = &report.outcome else {
        panic!("expected a failure, got {:?}", report.outcome)
    };
    assert_eq!(failure.kind, ErrorKind::Transport);
    assert_eq!(server.connections(), 0, "the endpoint was reached directly");
}

#[test]
fn proxy_settings_are_explicit_and_validated() {
    let base = "https://api.example.com/v1";
    let keyed = || OpenAiConfig::new(base, Secret::new("sk-x")).unwrap();
    for bad in [
        "https://proxy:8080",
        "socks5://proxy:1080",
        "http://user:pw@proxy:8080",
        "http://proxy",
        "http://proxy:0",
        "http://proxy:8080/path",
    ] {
        assert!(keyed().with_proxy(bad).is_err(), "{bad} was accepted");
    }
    assert!(keyed().with_proxy("http://proxy.corp:3128").is_ok());
    // Credentials and exceptions need a proxy first.
    assert!(
        keyed()
            .with_proxy_credentials("u", Secret::new("p"))
            .is_err()
    );
    assert!(keyed().with_no_proxy("example.com").is_err());
    // A keyed plain-http endpoint is never reached through a proxy.
    let keyed_loopback = OpenAiConfig::new("http://127.0.0.1:1/v1", Secret::new("sk-x")).unwrap();
    assert!(keyed_loopback.with_proxy("http://proxy:8080").is_err());
    // A password that would change how the proxy URL parses is refused, and not echoed.
    let error = keyed()
        .with_proxy("http://proxy:8080")
        .unwrap()
        .with_proxy_credentials("u", Secret::new("p@ss/word"))
        .unwrap_err();
    assert!(!format!("{error:?}").contains("p@ss/word"));
}
