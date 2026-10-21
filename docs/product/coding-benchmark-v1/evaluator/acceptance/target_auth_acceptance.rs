//! Benchmark acceptance test (evaluator-held; not given to the agent).
//!
//! The documented target-credential contract (docs/remote-execution.md,
//! "Target credentials"), exercised through the real `compute serve` HTTP
//! path:
//!
//! - every request needs a credential, reads and health included: none, a
//!   malformed one, a wrong secret, or a revoked credential gets 401;
//! - what a request creates belongs to the control plane its credential
//!   names; rotation keeps it, another control plane lists none of it and its
//!   sessions are `unknown_session` to it;
//! - the trust file is re-read whenever it changes: a revocation takes effect
//!   at the next request, without a restart.

use std::io::{Read, Write};
use std::net::{TcpListener as StdTcpListener, TcpStream};
use std::path::Path;
use std::time::{Duration, SystemTime};

use compute_core::{NetworkPolicy, SessionResources, SessionSpec};
use compute_provider::{
    REMOTE_PROTOCOL, ServerConfig, SessionCreateRequest, SessionEnvironmentSpec, TargetAuthorizer,
    TargetCredentials,
};

struct Target {
    runtime: Option<tokio::runtime::Runtime>,
    address: String,
    _state: tempfile::TempDir,
}

impl Target {
    /// A target authenticated by the trust file at `trust`, offering what
    /// `configure` leaves enabled.
    fn start(trust: &Path, configure: impl FnOnce(&mut ServerConfig)) -> Self {
        let state = tempfile::tempdir().unwrap();
        let socket = StdTcpListener::bind("127.0.0.1:0").unwrap();
        socket.set_nonblocking(true).unwrap();
        let address = socket.local_addr().unwrap().to_string();
        let mut config = ServerConfig::local(format!("http://{address}"));
        config.job_store = state.path().join("jobs");
        config.session_store = state.path().join("sessions");
        config.authorizer = std::sync::Arc::new(TargetAuthorizer::from_file(trust));
        configure(&mut config);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.spawn(async move {
            let listener = tokio::net::TcpListener::from_std(socket).unwrap();
            let _ = compute_provider::serve_listener(listener, config).await;
        });
        Self {
            runtime: Some(runtime),
            address,
            _state: state,
        }
    }

    /// One raw HTTP request: (status, body).
    fn call(&self, method: &str, path: &str, token: Option<&str>, body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nX-Compute-Protocol: {REMOTE_PROTOCOL}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.address,
            body.len()
        );
        if let Some(token) = token {
            request.push_str(&format!("Authorization: {token}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(body);
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or_else(|| panic!("no HTTP status in {response:?}"));
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_owned())
            .unwrap_or_default();
        (status, body)
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// Every protected endpoint of `compute.remote@1`, with a syntactically valid
/// body where one is needed.
fn protected_endpoints() -> Vec<(&'static str, String)> {
    let job = "job_0000000000000000000000000000000000000000000000000000000000000000";
    let session = "ses_0000000000000000000000000000000000000000000000000000000000000000";
    let mut endpoints = vec![
        ("GET", "/compute/health".to_owned()),
        ("GET", "/compute/capabilities".to_owned()),
        ("GET", "/compute/capacity".to_owned()),
        ("GET", "/compute/inspect".to_owned()),
        ("POST", "/compute/execute".to_owned()),
        ("POST", "/compute/admission".to_owned()),
        ("POST", "/compute/jobs".to_owned()),
        ("GET", "/compute/jobs".to_owned()),
        ("POST", "/compute/sessions".to_owned()),
        ("GET", "/compute/sessions".to_owned()),
    ];
    for tail in ["", "/result", "/receipt", "/artifacts", "/events", "/logs"] {
        endpoints.push(("GET", format!("/compute/jobs/{job}{tail}")));
    }
    endpoints.push(("POST", format!("/compute/jobs/{job}/cancel")));
    for tail in ["", "/events", "/logs"] {
        endpoints.push(("GET", format!("/compute/sessions/{session}{tail}")));
    }
    for action in ["connect", "exec", "stop", "resume", "claim", "destroy"] {
        endpoints.push(("POST", format!("/compute/sessions/{session}/{action}")));
    }
    endpoints
}

fn issue(trust: &Path, control_planes: &[&str]) -> Vec<(String, String)> {
    let mut credentials = TargetCredentials::load_or_default(trust).unwrap();
    let issued = control_planes
        .iter()
        .map(|control_plane| {
            let (record, token) = credentials.issue(control_plane).unwrap();
            (record.credential_id, token)
        })
        .collect();
    credentials.save(trust).unwrap();
    issued
}

/// Rewrite the trust file as an operator's tool would, then give it the
/// modification time it had before: two writes inside one filesystem
/// timestamp tick are indistinguishable by time alone.
fn rewrite_within_one_tick(trust: &Path, change: impl FnOnce(&mut TargetCredentials)) {
    let before = std::fs::metadata(trust).unwrap().modified().unwrap();
    let mut credentials = TargetCredentials::load(trust).unwrap();
    change(&mut credentials);
    credentials.save(trust).unwrap();
    std::fs::File::options()
        .append(true)
        .open(trust)
        .unwrap()
        .set_modified(before)
        .unwrap();
    assert_eq!(
        std::fs::metadata(trust).unwrap().modified().unwrap(),
        before
    );
}

fn session_request() -> String {
    let request = SessionCreateRequest::new(
        &SessionEnvironmentSpec {
            resources: SessionResources {
                cpu_count: Some(1),
                memory_bytes: Some(64 * 1024 * 1024),
                disk_bytes: None,
            },
            network: NetworkPolicy::Network,
            isolation: Default::default(),
            architecture: None,
        },
        SessionSpec {
            ttl_seconds: Some(3600),
            ..Default::default()
        },
    )
    .unwrap();
    serde_json::to_string(&request).unwrap()
}

fn kind(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("kind").and_then(|kind| kind.as_str()).map(str::to_owned))
        .unwrap_or_default()
}

/// A1. No credential, a malformed one, an unknown one and a wrong secret are
/// all refused with 401 on every protected endpoint, whatever the target
/// offers.
#[test]
fn every_endpoint_refuses_requests_without_a_valid_credential() {
    let directory = tempfile::tempdir().unwrap();
    let trust = directory.path().join("target-credentials.json");
    let issued = issue(&trust, &["control-plane-a"]);
    let token = &issued[0].1;
    let wrong_secret = format!(
        "{}{}",
        &token[..token.len() - 1],
        if token.ends_with('0') { '1' } else { '0' }
    );
    let refused = [
        None,
        Some("Basic dXNlcjpwYXNz".to_owned()),
        Some("Bearer".to_owned()),
        Some("Bearer not-a-target-credential".to_owned()),
        Some(bearer("cmpt_tcred_0000000000000000_00")),
        Some(bearer(&wrong_secret)),
    ];
    // Everything offered, then a target that offers only on-request runs.
    let everything = Target::start(&trust, |config| config.execution.sessions = true);
    let little = Target::start(&trust, |config| {
        config.execution.jobs = false;
        config.execution.sessions = false;
    });
    let mut failures = vec![];
    for target in [&everything, &little] {
        for (method, path) in protected_endpoints() {
            for credential in &refused {
                let (status, body) = target.call(method, &path, credential.as_deref(), "{}");
                if status != 401 || kind(&body) != "unauthorized" {
                    failures.push(format!(
                        "{method} {path} with {credential:?}: {status} {body}"
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "not refused:\n{}", failures.join("\n"));
    // The valid credential is accepted.
    let (status, body) = everything.call("GET", "/compute/health", Some(&bearer(token)), "");
    assert_eq!(status, 200, "{body}");
    let (status, body) = little.call("GET", "/compute/capabilities", Some(&bearer(token)), "");
    assert_eq!(status, 200, "{body}");
}

/// A2. A revocation takes effect at the next request without a restart, even
/// when it is written within the same timestamp tick as the previous write.
#[test]
fn a_revocation_takes_effect_at_the_next_request() {
    let directory = tempfile::tempdir().unwrap();
    let trust = directory.path().join("target-credentials.json");
    let issued = issue(&trust, &["control-plane-a", "control-plane-b"]);
    let (first_id, first) = issued[0].clone();
    let (_, other) = issued[1].clone();
    let target = Target::start(&trust, |_| {});
    assert_eq!(target.call("GET", "/compute/health", Some(&bearer(&first)), "").0, 200);
    assert_eq!(target.call("GET", "/compute/health", Some(&bearer(&other)), "").0, 200);

    rewrite_within_one_tick(&trust, |credentials| {
        credentials.revoke(&first_id).unwrap();
    });
    let (status, body) = target.call("GET", "/compute/health", Some(&bearer(&first)), "");
    assert_eq!(status, 401, "a revoked credential was still accepted: {body}");
    assert!(body.contains("revoked"), "{body}");
    // The other control plane is unaffected.
    assert_eq!(target.call("GET", "/compute/health", Some(&bearer(&other)), "").0, 200);

    // Deleting a credential from the file ends its access the same way.
    rewrite_within_one_tick(&trust, |credentials| {
        credentials
            .credentials
            .retain(|record| record.control_plane != "control-plane-b");
    });
    let (status, body) = target.call("GET", "/compute/health", Some(&bearer(&other)), "");
    assert_eq!(status, 401, "a deleted credential was still accepted: {body}");
}

/// A3. Sessions belong to the control plane, not the token: a rotated
/// credential keeps them, the revoked one and another control plane reach
/// none of them, and the other control plane learns nothing about them.
#[test]
fn sessions_belong_to_the_control_plane_across_rotation_and_revocation() {
    let directory = tempfile::tempdir().unwrap();
    let trust = directory.path().join("target-credentials.json");
    let issued = issue(&trust, &["control-plane-a", "control-plane-b"]);
    let (first_id, first) = issued[0].clone();
    let (_, intruder) = issued[1].clone();
    let target = Target::start(&trust, |config| config.execution.sessions = true);

    let (status, body) = target.call(
        "POST",
        "/compute/sessions",
        Some(&bearer(&first)),
        &session_request(),
    );
    assert_eq!(status, 200, "{body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = created["session_id"].as_str().unwrap().to_owned();
    assert_eq!(created["owner"], "control-plane:control-plane-a");

    // Another control plane lists none of it and cannot tell it exists.
    let (status, body) = target.call("GET", "/compute/sessions", Some(&bearer(&intruder)), "");
    assert_eq!(status, 200, "{body}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&body).unwrap(), serde_json::json!([]));
    let unknown = "ses_0000000000000000000000000000000000000000000000000000000000000000";
    for (method, tail) in [
        ("GET", ""),
        ("GET", "/events"),
        ("GET", "/logs"),
        ("POST", "/stop"),
        ("POST", "/claim"),
        ("POST", "/destroy"),
    ] {
        let foreign = target.call(
            method,
            &format!("/compute/sessions/{id}{tail}"),
            Some(&bearer(&intruder)),
            "{}",
        );
        let missing = target.call(
            method,
            &format!("/compute/sessions/{unknown}{tail}"),
            Some(&bearer(&intruder)),
            "{}",
        );
        assert_eq!(foreign.0, 404, "{method} {tail}: {foreign:?}");
        assert_eq!(kind(&foreign.1), "unknown_session", "{method} {tail}: {foreign:?}");
        assert_eq!(foreign.0, missing.0, "{method} {tail}");
        assert_eq!(kind(&foreign.1), kind(&missing.1), "{method} {tail}");
        assert!(!foreign.1.contains("control-plane-a"), "{foreign:?}");
    }

    // Rotate: issue a new credential for the same control plane and revoke
    // the old one, in one write.
    let mut rotated = String::new();
    rewrite_within_one_tick(&trust, |credentials| {
        rotated = credentials.issue("control-plane-a").unwrap().1;
        credentials.revoke(&first_id).unwrap();
    });
    let (status, body) = target.call(
        "GET",
        &format!("/compute/sessions/{id}"),
        Some(&bearer(&rotated)),
        "",
    );
    assert_eq!(status, 200, "the rotated credential lost its session: {body}");
    let (status, body) = target.call(
        "GET",
        &format!("/compute/sessions/{id}"),
        Some(&bearer(&first)),
        "",
    );
    assert_eq!(status, 401, "the replaced credential still works: {body}");
    // The session is still intact for its owner, and its owner can end it.
    let (status, body) = target.call(
        "POST",
        &format!("/compute/sessions/{id}/destroy"),
        Some(&bearer(&rotated)),
        "{}",
    );
    assert_eq!(status, 200, "{body}");
}

/// A4. The modification time alone never decides: a target that has already
/// read its trust file sees a credential issued in the same tick.
#[test]
fn a_credential_issued_in_the_same_tick_is_admitted() {
    let directory = tempfile::tempdir().unwrap();
    let trust = directory.path().join("target-credentials.json");
    let issued = issue(&trust, &["control-plane-a"]);
    let target = Target::start(&trust, |_| {});
    assert_eq!(target.call("GET", "/compute/health", Some(&bearer(&issued[0].1)), "").0, 200);
    let mut added = String::new();
    rewrite_within_one_tick(&trust, |credentials| {
        added = credentials.issue("control-plane-c").unwrap().1;
    });
    let (status, body) = target.call("GET", "/compute/health", Some(&bearer(&added)), "");
    assert_eq!(status, 200, "a newly issued credential was refused: {body}");
}
