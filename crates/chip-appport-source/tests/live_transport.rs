//! The live MCP transport (`chip_appport_source::live`) against a scripted MCP server in this
//! process: what it offers, and how each way a call can end is reported. The server answers with
//! the AppBoundry host's own message shapes, as `appport dev` served them for the generated
//! invoice-desk bridge (`appboundry/capability`, `appboundry/authorization`, `appboundry/error`,
//! `appboundry/evidence`).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chip_appport_source::live::LiveMcp;
use chip_core::{
    CapabilityId, ExecutionError, ExecutionId, ExecutionRequest, ExecutionStatus, Executor,
    InputValue,
};
use serde_json::{Value, json};

const STATUS: &str = "appport.invoicedesk.invoices_id_status";
const GET: &str = "appport.invoicedesk.invoices_id";

fn tool(name: &str, capability: &str, scope: &str, properties: Value) -> Value {
    json!({
        "name": name,
        "inputSchema": {"type": "object", "properties": properties, "required": ["id"], "additionalProperties": true},
        "_meta": {
            "appboundry/capability": capability,
            "appboundry/authorization": [scope],
            "appboundry/packageIdentity": "unpackaged:com.example.invoice-desk@1.0.0",
            "appboundry/artifactHash": "none:unpackaged",
        }
    })
}

/// How the scripted server answers `tools/call`.
#[derive(Clone, Copy)]
enum Call {
    Success,
    Forbidden,
    NotFound,
    Garbage,
    HangUp,
    /// Answers discovery, then accepts no further connection.
    GoneAfterList,
}

/// A one-connection-at-a-time HTTP server speaking MCP's JSON responses. Records every
/// `tools/call` it received.
struct Server {
    url: String,
    calls: Arc<Mutex<Vec<Value>>>,
}

fn serve(answer: Call) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/apps/com.example.invoice-desk/mcp",
        listener.local_addr().unwrap()
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            if handle(stream, answer, &seen) {
                break; // the listener is dropped: later connections are refused
            }
        }
    });
    Server { url, calls }
}

/// Answers one request; true when the server should stop listening.
fn handle(mut stream: TcpStream, answer: Call, calls: &Arc<Mutex<Vec<Value>>>) -> bool {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return false;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let message: Value = serde_json::from_slice(&body).unwrap();
    let id = message["id"].clone();
    let result = match message["method"].as_str().unwrap() {
        "initialize" => {
            json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {"listChanged": false}}, "serverInfo": {"name": "appboundry-host", "version": "1.0.0"}})
        }
        "tools/list" => json!({"tools": [
            tool("invoices_id", "invoices.id@1", "invoices.read", json!({"id": {"type": "string", "minLength": 1}})),
            tool("invoices_id_status", "invoices.id.status@1", "invoices.write", json!({"id": {"type": "string", "minLength": 1}, "status": {"type": "string"}, "note": {"type": "string"}})),
            json!({"name": "unlabelled", "inputSchema": {"type": "object", "properties": {}}}),
        ]}),
        "tools/call" => {
            calls.lock().unwrap().push(message["params"].clone());
            match answer {
                Call::Success => {
                    json!({"content": [{"type": "text", "text": "{\"id\":2,\"status\":\"approved\"}"}], "structuredContent": {"id": 2, "status": "approved"}, "_meta": {"appboundry/evidence": {"capability": "invoices.id.status@1", "executionMode": "development", "outcome": "success", "executionUnitId": "exec_0123456789abcdef"}}})
                }
                Call::Forbidden => {
                    json!({"isError": true, "content": [{"type": "text", "text": "FORBIDDEN: capability authorization denied"}], "_meta": {"appboundry/error": {"code": "FORBIDDEN", "message": "capability authorization denied", "details": {"missingScopes": ["invoices.write"]}}}})
                }
                Call::NotFound => {
                    json!({"isError": true, "content": [{"type": "text", "text": "NOT_FOUND: Invoice not found"}], "_meta": {"appboundry/error": {"code": "NOT_FOUND", "message": "Invoice not found"}, "appboundry/evidence": {"outcome": "failure", "executionUnitId": "exec_fedcba9876543210"}}})
                }
                Call::Garbage => {
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 9\r\nconnection: close\r\n\r\nnot json!");
                    return false;
                }
                Call::HangUp => return false, // the call was received; no answer is ever sent
                Call::GoneAfterList => {
                    unreachable!("no call reaches a server that stopped listening")
                }
            }
        }
        other => panic!("unexpected method {other}"),
    };
    let text = json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
        text.len()
    );
    matches!(answer, Call::GoneAfterList) && message["method"] == "tools/list"
}

fn connect(server: &Server, grant: &[&str]) -> LiveMcp {
    let grant: Vec<String> = grant.iter().map(|s| s.to_string()).collect();
    LiveMcp::connect(
        &server.url,
        "token",
        "invoicedesk",
        &grant,
        Duration::from_secs(2),
    )
    .unwrap()
}

fn request(capability: &str, id: &str) -> ExecutionRequest {
    let mut inputs = BTreeMap::new();
    inputs.insert("id".to_string(), InputValue::Text(id.into()));
    inputs.insert("status".to_string(), InputValue::Text("approved".into()));
    let mut request = ExecutionRequest::for_capability(
        ExecutionId::new("e1"),
        &CapabilityId::new(capability).unwrap(),
    );
    request.inputs = inputs;
    request
}

fn head(output: &str) -> Value {
    serde_json::from_str(output.lines().next().unwrap()).unwrap()
}

#[tokio::test]
async fn offers_only_tools_within_the_grant_whose_metadata_names_their_authority() {
    let server = serve(Call::Success);
    let read_only = connect(&server, &["invoices.read"]);
    let offered: Vec<&str> = read_only
        .offered()
        .iter()
        .map(|o| o.descriptor.id.as_str())
        .collect();
    assert_eq!(offered, [GET]);
    let reasons: Vec<&str> = read_only
        .not_offered()
        .iter()
        .map(|n| n.capability.as_str())
        .collect();
    assert_eq!(reasons, ["invoices_id_status", "unlabelled"]);
    let both = connect(&server, &["invoices.read", "invoices.write"]);
    let status = both
        .offered()
        .iter()
        .find(|o| o.descriptor.id.as_str() == STATUS)
        .unwrap();
    assert_eq!(status.capability, "invoices.id.status@1");
    assert_eq!(status.scopes, ["invoices.write"]);
    let inputs: Vec<&str> = status
        .descriptor
        .inputs
        .iter()
        .map(|i| i.name.as_str())
        .collect();
    assert_eq!(inputs, ["id", "note", "status"]);
    assert!(
        status
            .descriptor
            .inputs
            .iter()
            .any(|i| i.name == "id" && i.required)
    );
}

#[tokio::test]
async fn a_success_carries_the_result_and_the_hosts_execution_unit() {
    let server = serve(Call::Success);
    let live = connect(&server, &["invoices.read", "invoices.write"]);
    let result = live.execute(request(STATUS, "2")).await.unwrap();
    assert_eq!(result.status, ExecutionStatus::Success);
    assert_eq!(result.receipt_id.as_deref(), Some("exec_0123456789abcdef"));
    let h = head(&result.output);
    assert_eq!(h["outcome"], "success");
    assert_eq!(h["appport_capability"], "invoices.id.status@1");
    assert!(
        result
            .output
            .ends_with("{\"id\":2,\"status\":\"approved\"}")
    );
    assert_eq!(
        server.calls.lock().unwrap()[0],
        json!({"name": "invoices_id_status", "arguments": {"id": "2", "status": "approved"}})
    );
}

#[tokio::test]
async fn a_refusal_and_an_application_failure_are_failures_with_their_own_codes() {
    let refused = connect(
        &serve(Call::Forbidden),
        &["invoices.read", "invoices.write"],
    )
    .execute(request(STATUS, "2"))
    .await
    .unwrap();
    assert_eq!(refused.status, ExecutionStatus::Failure);
    assert_eq!(head(&refused.output)["outcome"], "refused");
    assert_eq!(head(&refused.output)["error"]["code"], "FORBIDDEN");
    let missing = connect(&serve(Call::NotFound), &["invoices.read"])
        .execute(request(GET, "999"))
        .await
        .unwrap();
    assert_eq!(missing.status, ExecutionStatus::Failure);
    assert_eq!(head(&missing.output)["outcome"], "failed");
    assert_eq!(head(&missing.output)["error"]["code"], "NOT_FOUND");
}

#[tokio::test]
async fn an_answer_that_cannot_be_read_or_never_comes_is_an_unknown_outcome() {
    for answer in [Call::Garbage, Call::HangUp] {
        let server = serve(answer);
        let live = connect(&server, &["invoices.read", "invoices.write"]);
        match live.execute(request(STATUS, "2")).await {
            Err(ExecutionError::OutcomeUnknown(why)) => {
                assert!(why.contains("outcome is unknown"), "{why}")
            }
            other => panic!("expected an unknown outcome, got {other:?}"),
        }
        assert_eq!(
            server.calls.lock().unwrap().len(),
            1,
            "sent once, never retried"
        );
    }
}

#[tokio::test]
async fn an_endpoint_that_cannot_be_reached_ran_nothing() {
    // Not reachable at all: there is no session.
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let grant = ["invoices.read".to_string()];
    assert!(
        LiveMcp::connect(
            &format!("http://127.0.0.1:{port}/apps/x/mcp"),
            "token",
            "invoicedesk",
            &grant,
            Duration::from_secs(2)
        )
        .is_err()
    );
    // Reachable for discovery, gone for the call: nothing was sent, which is not an unknown outcome.
    let server = serve(Call::GoneAfterList);
    let live = connect(&server, &["invoices.read", "invoices.write"]);
    match live.execute(request(STATUS, "2")).await {
        Err(ExecutionError::ExecutorUnavailable(why)) => {
            assert!(why.contains("nothing was sent"), "{why}")
        }
        other => panic!("expected an unavailable executor, got {other:?}"),
    }
    assert!(server.calls.lock().unwrap().is_empty());
}
