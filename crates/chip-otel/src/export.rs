//! Where an OTLP request goes: a file, a collector on loopback, or nowhere.
//!
//! Export is configured with the standard OpenTelemetry environment variables, read strictly:
//!
//! * `OTEL_SDK_DISABLED=true`, or `OTEL_TRACES_EXPORTER=none`: no export.
//! * `OTEL_TRACES_EXPORTER`: unset or `otlp`; anything else is refused.
//! * `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` (used as given), else `OTEL_EXPORTER_OTLP_ENDPOINT` with
//!   `/v1/traces` appended. It must be `http://` to `127.0.0.1`, `localhost` or `[::1]`: a collector
//!   elsewhere is reached through a local collector, so no work data leaves the machine from here.
//! * `OTEL_EXPORTER_OTLP_PROTOCOL` / `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL`: unset or `http/json`.
//! * `OTEL_EXPORTER_OTLP_HEADERS` / `OTEL_EXPORTER_OTLP_TRACES_HEADERS`: refused. Headers exist to
//!   carry credentials, and a loopback collector needs none; stack-agent never sends one.
//! * `STACK_AGENT_OTLP_FILE`: also (or only) append each request to this file as one JSON line,
//!   created owner-only.
//! * `OTEL_SERVICE_NAME`: the `service.name` resource attribute.
//!
//! A configuration that is refused is an error the caller reports; nothing is sent. Delivery
//! failure is reported the same way and never changes the work's outcome.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

/// One place a request is delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sink {
    /// Append the request as one JSON line.
    File(PathBuf),
    /// POST it to a collector on loopback.
    Http {
        host: String,
        port: u16,
        path: String,
    },
}

impl Sink {
    /// What the sink is, for a report: the file path or the endpoint URL.
    pub fn describe(&self) -> String {
        match self {
            Self::File(path) => path.display().to_string(),
            Self::Http { host, port, path } => format!("http://{host}:{port}{path}"),
        }
    }
}

/// The export configuration: no sinks means export is off.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub sinks: Vec<Sink>,
    pub service_name: Option<String>,
}

impl Config {
    pub fn enabled(&self) -> bool {
        !self.sinks.is_empty()
    }
}

const TIMEOUT: Duration = Duration::from_secs(2);

/// Parses `http://<loopback>[:port][/path]`. Only loopback hosts are accepted.
pub fn loopback_endpoint(url: &str) -> Result<Sink, String> {
    let rest = url.strip_prefix("http://").ok_or_else(|| {
        format!(
            "the OTLP endpoint must be http:// to a collector on loopback, not {}",
            scheme(url)
        )
    })?;
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    if authority.contains('@') {
        return Err("the OTLP endpoint must not carry credentials".into());
    }
    if path.contains(['?', '#']) || !path.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("the OTLP endpoint path must be a plain path".into());
    }
    let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6
            .split_once(']')
            .ok_or("the OTLP endpoint host is malformed")?;
        (format!("[{host}]"), after.strip_prefix(':'))
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (authority.to_string(), None),
        }
    };
    let host = host.to_ascii_lowercase();
    if !matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]") {
        return Err(format!(
            "the OTLP endpoint must be on loopback (127.0.0.1, localhost or [::1]), not {host}; run a local collector to forward elsewhere"
        ));
    }
    let port = match port {
        None => 80,
        Some(p) => p
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or("the OTLP endpoint port is not a port number")?,
    };
    Ok(Sink::Http {
        host,
        port,
        path: path.to_string(),
    })
}

fn scheme(url: &str) -> &str {
    match url.split_once("://") {
        Some((s, _)) if s.len() <= 16 && s.bytes().all(|b| b.is_ascii_alphanumeric()) => s,
        _ => "this value",
    }
}

/// The configuration the environment asks for. `Err` says why it is refused.
pub fn config_from_env(env: &dyn Fn(&str) -> Option<String>) -> Result<Config, String> {
    let get = |name: &str| {
        env(name)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    if get("OTEL_SDK_DISABLED").is_some_and(|v| v.eq_ignore_ascii_case("true")) {
        return Ok(Config::default());
    }
    match get("OTEL_TRACES_EXPORTER").as_deref() {
        None | Some("otlp") => {}
        Some("none") => return Ok(Config::default()),
        Some(other) => {
            return Err(format!(
                "OTEL_TRACES_EXPORTER={other} is not supported (only otlp or none)"
            ));
        }
    }
    for name in [
        "OTEL_EXPORTER_OTLP_HEADERS",
        "OTEL_EXPORTER_OTLP_TRACES_HEADERS",
    ] {
        if get(name).is_some() {
            return Err(format!(
                "{name} is set: stack-agent sends no OTLP headers (they carry credentials); export to a local collector that adds them"
            ));
        }
    }
    for name in [
        "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
        "OTEL_EXPORTER_OTLP_PROTOCOL",
    ] {
        if let Some(protocol) = get(name)
            && protocol != "http/json"
        {
            return Err(format!(
                "{name}={protocol} is not supported (only http/json)"
            ));
        }
    }
    let mut sinks = Vec::new();
    let endpoint = get("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").or_else(|| {
        get("OTEL_EXPORTER_OTLP_ENDPOINT")
            .map(|base| format!("{}/v1/traces", base.trim_end_matches('/')))
    });
    if let Some(url) = endpoint {
        sinks.push(loopback_endpoint(&url)?);
    }
    if let Some(file) = get("STACK_AGENT_OTLP_FILE") {
        sinks.push(Sink::File(PathBuf::from(file)));
    }
    Ok(Config {
        sinks,
        service_name: get("OTEL_SERVICE_NAME"),
    })
}

fn write_file(path: &std::path::Path, line: &[u8]) -> Result<(), String> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(path)
        .map_err(|e| format!("the OTLP file cannot be opened ({})", e.kind()))?;
    file.write_all(line)
        .map_err(|e| format!("the OTLP file cannot be written ({})", e.kind()))
}

fn post(host: &str, port: u16, path: &str, body: &[u8]) -> Result<(), String> {
    let connect_host = host.trim_start_matches('[').trim_end_matches(']');
    let addrs: Vec<_> = (connect_host, port)
        .to_socket_addrs()
        .map_err(|e| format!("the collector address cannot be resolved ({})", e.kind()))?
        // `localhost` must resolve to loopback, or nothing is sent.
        .filter(|a| a.ip().is_loopback())
        .collect();
    let addr = addrs.first().ok_or("the collector host is not loopback")?;
    let mut stream = TcpStream::connect_timeout(addr, TIMEOUT)
        .map_err(|e| format!("the collector cannot be reached ({})", e.kind()))?;
    let _ = stream.set_read_timeout(Some(TIMEOUT));
    let _ = stream.set_write_timeout(Some(TIMEOUT));
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(body))
        .map_err(|e| format!("the request cannot be sent ({})", e.kind()))?;
    // The status line is all that is read; a redirect is a failure like any other non-2xx.
    let mut status = Vec::new();
    let mut byte = [0u8; 1];
    while status.len() < 256 {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => status.push(byte[0]),
            Err(e) => return Err(format!("the collector did not answer ({})", e.kind())),
        }
    }
    let status = String::from_utf8_lossy(&status);
    let code = status.split_whitespace().nth(1).unwrap_or_default();
    if status.starts_with("HTTP/1.") && code.len() == 3 && code.starts_with('2') {
        Ok(())
    } else if code.len() == 3 && code.bytes().all(|b| b.is_ascii_digit()) {
        Err(format!("the collector answered {code}"))
    } else {
        Err("the collector did not answer with HTTP".into())
    }
}

/// Delivers the request to every sink; one result per sink, in order.
pub fn export(config: &Config, request: &Value) -> Vec<Result<(), String>> {
    let body = request.to_string();
    config
        .sinks
        .iter()
        .map(|sink| match sink {
            Sink::File(path) => {
                let mut line = body.clone().into_bytes();
                line.push(b'\n');
                write_file(path, &line)
            }
            Sink::Http { host, port, path } => post(host, *port, path, body.as_bytes()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_http_endpoints() {
        assert_eq!(
            loopback_endpoint("http://127.0.0.1:4318/v1/traces"),
            Ok(Sink::Http {
                host: "127.0.0.1".into(),
                port: 4318,
                path: "/v1/traces".into()
            })
        );
        assert!(loopback_endpoint("http://[::1]:4318/v1/traces").is_ok());
        assert!(loopback_endpoint("http://LOCALHOST:4318").is_ok());
        for bad in [
            "https://127.0.0.1:4318/v1/traces",
            "http://collector.example.com:4318/v1/traces",
            "http://127.0.0.2:4318/",
            "http://user:secret@127.0.0.1:4318/",
            "http://127.0.0.1.example.com/",
            "http://127.0.0.1:0/",
            "http://127.0.0.1:99999/",
            "http://127.0.0.1:4318/v1/traces?token=x",
            "grpc://127.0.0.1:4317",
        ] {
            assert!(loopback_endpoint(bad).is_err(), "{bad}");
        }
    }
}
