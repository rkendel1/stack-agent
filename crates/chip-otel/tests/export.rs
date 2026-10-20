//! Delivery: the configuration the environment asks for, a stub collector on loopback that
//! records what it receives, the file sink, and every refusal.

mod common;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;

use chip_otel::export::{Config, Sink, config_from_env, export};
use chip_otel::trace;
use common::*;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |name| map.get(name).cloned()
}

struct Received {
    request_line: String,
    headers: Vec<(String, String)>,
    body: String,
}

/// A one-request collector that answers `status`.
fn collector(status: &'static str) -> (u16, mpsc::Receiver<Received>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut headers = Vec::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            let (k, v) = line.split_once(':').unwrap();
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
        let length: usize = headers
            .iter()
            .find(|(k, _)| k == "content-length")
            .unwrap()
            .1
            .parse()
            .unwrap();
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let mut stream = stream;
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        tx.send(Received {
            request_line: request_line.trim_end().into(),
            headers,
            body: String::from_utf8(body).unwrap(),
        })
        .unwrap();
    });
    (port, rx)
}

fn request() -> serde_json::Value {
    trace(
        &history(
            records(1, &completed_attempt(), 1),
            vec![result(1, "completed", "", Some(verified()))],
        ),
        &context(),
    )
}

#[test]
fn a_loopback_collector_receives_the_request_as_otlp_json_with_no_credentials() {
    let (port, received) = collector("200 OK");
    let config = config_from_env(&env(&[(
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        &format!("http://127.0.0.1:{port}/"),
    )]))
    .unwrap();
    assert_eq!(
        config.sinks,
        [Sink::Http {
            host: "127.0.0.1".into(),
            port,
            path: "/v1/traces".into()
        }]
    );
    let request = request();
    assert_eq!(export(&config, &request), [Ok(())]);
    let got = received.recv().unwrap();
    assert_eq!(got.request_line, "POST /v1/traces HTTP/1.1");
    let names: Vec<&str> = got.headers.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        names,
        ["host", "content-type", "content-length", "connection"]
    );
    assert!(
        got.headers
            .contains(&("content-type".into(), "application/json".into()))
    );
    let body: serde_json::Value = serde_json::from_str(&got.body).unwrap();
    assert_eq!(body, request);
}

#[test]
fn a_collector_that_refuses_or_redirects_is_a_reported_failure() {
    for status in ["500 Internal Server Error", "302 Found", "400 Bad Request"] {
        let (port, received) = collector(status);
        let config = config_from_env(&env(&[(
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            &format!("http://localhost:{port}/v1/traces"),
        )]))
        .unwrap();
        let results = export(&config, &request());
        let code = &status[..3];
        assert_eq!(results, [Err(format!("the collector answered {code}"))]);
        received.recv().unwrap();
    }
}

#[test]
fn no_collector_is_a_reported_failure_not_a_hang() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = Config {
        sinks: vec![Sink::Http {
            host: "127.0.0.1".into(),
            port,
            path: "/v1/traces".into(),
        }],
        service_name: None,
    };
    let started = std::time::Instant::now();
    let results = export(&config, &request());
    assert!(
        results[0]
            .as_ref()
            .unwrap_err()
            .starts_with("the collector cannot be reached")
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn the_file_sink_appends_one_private_json_line_per_request() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("traces.jsonl");
    let config =
        config_from_env(&env(&[("STACK_AGENT_OTLP_FILE", path.to_str().unwrap())])).unwrap();
    let request = request();
    assert_eq!(export(&config, &request), [Ok(())]);
    assert_eq!(export(&config, &request), [Ok(())]);
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines, [request.clone(), request]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let missing = Config {
        sinks: vec![Sink::File(dir.path().join("no/such/dir/t.jsonl"))],
        service_name: None,
    };
    assert!(export(&missing, &serde_json::json!({}))[0].is_err());
}

#[test]
fn export_is_off_unless_asked_for_and_off_when_disabled() {
    assert!(!config_from_env(&env(&[])).unwrap().enabled());
    let endpoint = ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4318");
    assert!(config_from_env(&env(&[endpoint])).unwrap().enabled());
    assert!(
        !config_from_env(&env(&[endpoint, ("OTEL_SDK_DISABLED", "true")]))
            .unwrap()
            .enabled()
    );
    assert!(
        !config_from_env(&env(&[endpoint, ("OTEL_TRACES_EXPORTER", "none")]))
            .unwrap()
            .enabled()
    );
    let named = config_from_env(&env(&[endpoint, ("OTEL_SERVICE_NAME", "ci-agent")])).unwrap();
    assert_eq!(named.service_name.as_deref(), Some("ci-agent"));
}

#[test]
fn configurations_that_could_send_data_off_the_machine_or_carry_credentials_are_refused() {
    let headers = [("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:4318")];
    for refused in [
        vec![(
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "https://otlp.vendor.example:4318",
        )],
        vec![("OTEL_EXPORTER_OTLP_ENDPOINT", "http://10.0.0.5:4318")],
        vec![(
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "http://otel-collector:4318/v1/traces",
        )],
        vec![(
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "http://user:pw@127.0.0.1:4318",
        )],
        vec![
            headers[0],
            ("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer x"),
        ],
        vec![
            headers[0],
            ("OTEL_EXPORTER_OTLP_TRACES_HEADERS", "x-api-key=y"),
        ],
        vec![headers[0], ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc")],
        vec![
            headers[0],
            ("OTEL_EXPORTER_OTLP_TRACES_PROTOCOL", "http/protobuf"),
        ],
        vec![("OTEL_TRACES_EXPORTER", "zipkin")],
        // Headers are refused even with only the file sink: they would be ignored silently.
        vec![
            ("STACK_AGENT_OTLP_FILE", "/tmp/x"),
            ("OTEL_EXPORTER_OTLP_HEADERS", "a=b"),
        ],
    ] {
        let error = config_from_env(&env(&refused)).expect_err(&format!("{refused:?}"));
        // The refusal never repeats a header value or a URL password.
        assert!(
            !error.contains("Bearer") && !error.contains("pw@") && !error.contains("x-api-key"),
            "{error}"
        );
    }
}

#[test]
fn a_refused_endpoint_is_never_contacted() {
    // `localhost` is accepted only if it resolves to loopback; a non-loopback name is refused
    // before any connection is made, so no listener is needed to prove nothing was sent.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let refused = config_from_env(&env(&[(
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        &format!("http://127.0.0.1.nip.io:{port}"),
    )]));
    assert!(refused.is_err());
    assert!(listener.accept().is_err(), "nothing connected");
}
