//! The MCP source against a running server: `initialize` and `tools/list` once, then one `tools/call`
//! per execution, over MCP Streamable HTTP (JSON responses) with the operator's bearer token.
//!
//! Experiment-only, like the rest of this crate. It is a transport and nothing else: which tools are
//! offered is decided exactly as for a recording (`mcp::map_tool`: the tool's own metadata names the
//! AppPort capability and its scopes, and the scopes must be within the operator's grant), every
//! tool is [`Effect::Consequential`], and durability, recovery and completion stay with whoever runs
//! the work (`chip_agent_state::DurableWork` wraps this executor; it knows nothing of MCP).
//!
//! What a call ended as is said in the observation's first line, so a refusal, an application
//! failure, a transport failure and an unknown outcome are never confused:
//!
//! - `success`: a result without `isError`;
//! - `refused`: the endpoint refused the caller (HTTP 401/403), or the AppPort error says the caller
//!   was not authenticated or not permitted;
//! - `failed`: the application or AppPort answered with any other error, or a JSON-RPC error;
//! - the endpoint could not be reached, so nothing was sent: [`ExecutionError::ExecutorUnavailable`];
//! - the request was sent and no well-formed answer came back (timeout, broken connection, a body
//!   that is not a JSON-RPC response): [`ExecutionError::OutcomeUnknown`]. Durable state keeps such
//!   an intent unresolved, so a restart refuses it as uncertain. Nothing here retries.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chip_core::{
    CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityProvider, ExecutionError, ExecutionRequest, ExecutionResult, Executor,
};
use chip_experiments::plugin::recorded_inputs;
use serde_json::Value;

use crate::NotOffered;
use crate::mcp::{HOST_EVIDENCE_META, McpOffered, Outcome, TRACE_META, classify, map_tool};

/// The MCP protocol version this client asks for.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// Why a live session could not be established.
#[derive(Debug)]
pub struct LiveError(pub String);

impl std::fmt::Display for LiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How one HTTP exchange with the endpoint ended.
enum Exchange {
    /// The endpoint answered with this status and body.
    Answered { status: u16, body: String },
    /// Nothing was sent: the endpoint could not be reached.
    Unreachable(String),
    /// The request was sent; no complete answer came back.
    Lost(String),
}

/// One MCP server, live, under the operator's grant.
pub struct LiveMcp {
    endpoint: String,
    bearer: String,
    timeout: Duration,
    server: Value,
    tools: Vec<Value>,
    offered: Vec<McpOffered>,
    not_offered: Vec<NotOffered>,
    next_id: AtomicU64,
}

impl LiveMcp {
    /// `initialize`, then `tools/list`. `grant` is the scopes the operator granted this session.
    pub fn connect(
        endpoint: &str,
        bearer: &str,
        selector: &str,
        grant: &[String],
        timeout: Duration,
    ) -> Result<Self, LiveError> {
        if !crate::valid_selector(selector) {
            return Err(LiveError(format!(
                "selector `{selector}` cannot name a Chip capability"
            )));
        }
        let mut live = Self {
            endpoint: endpoint.to_string(),
            bearer: bearer.to_string(),
            timeout,
            server: Value::Null,
            tools: Vec::new(),
            offered: Vec::new(),
            not_offered: Vec::new(),
            next_id: AtomicU64::new(1),
        };
        let initialize = live.rpc_blocking(
            "initialize",
            serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "chip-appport-source", "version": env!("CARGO_PKG_VERSION")},
            }),
        )?;
        if initialize["capabilities"]["tools"].is_null() {
            return Err(LiveError(
                "the server did not declare the MCP tools capability".into(),
            ));
        }
        live.server = initialize["serverInfo"].clone();
        let listed = live.rpc_blocking("tools/list", serde_json::json!({}))?;
        live.tools = listed["tools"].as_array().cloned().unwrap_or_default();
        for tool in &live.tools {
            match map_tool(selector, tool, grant) {
                Ok(o) => live.offered.push(o),
                Err(n) => live.not_offered.push(n),
            }
        }
        Ok(live)
    }

    pub fn offered(&self) -> &[McpOffered] {
        &self.offered
    }

    pub fn not_offered(&self) -> &[NotOffered] {
        &self.not_offered
    }

    /// The tools the server listed, as served.
    pub fn tools(&self) -> &[Value] {
        &self.tools
    }

    pub fn server(&self) -> &Value {
        &self.server
    }

    fn rpc_blocking(&self, method: &str, params: Value) -> Result<Value, LiveError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        match exchange(
            &self.endpoint,
            &self.bearer,
            self.timeout,
            &request(id, method, params),
        ) {
            Exchange::Answered { status: 200, body } => {
                let response: Value = serde_json::from_str(&body)
                    .map_err(|e| LiveError(format!("{method}: the answer is not JSON: {e}")))?;
                if !response["error"].is_null() {
                    return Err(LiveError(format!("{method}: {}", response["error"])));
                }
                Ok(response["result"].clone())
            }
            Exchange::Answered { status, body } => Err(LiveError(format!(
                "{method}: HTTP {status}: {}",
                body.trim()
            ))),
            Exchange::Unreachable(why) | Exchange::Lost(why) => {
                Err(LiveError(format!("{method}: {why}")))
            }
        }
    }

    /// The observation for one answered call: a JSON first line (capability, transport, request,
    /// outcome, AppPort error, the endpoint's evidence), then the result.
    fn render(
        &self,
        offered: &McpOffered,
        arguments: &BTreeMap<String, Value>,
        outcome: &str,
        error: Value,
        result: &Value,
    ) -> String {
        let mut head = serde_json::json!({
            "capability": offered.descriptor.id.as_str(),
            "transport": "mcp",
            "live": true,
            "tool": offered.tool,
            "appport_capability": offered.capability,
            "request": arguments,
            "outcome": outcome,
            "trace_id": result["_meta"][TRACE_META],
            "evidence": result["_meta"][HOST_EVIDENCE_META],
            "server": self.server,
        });
        if outcome != Outcome::Success.name() {
            head["error"] = error;
        }
        let body = if !result["structuredContent"].is_null() {
            serde_json::to_string(&result["structuredContent"]).expect("JSON serializes")
        } else {
            result["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| c["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        };
        format!("{head}\n--- result ---\n{body}")
    }
}

fn request(id: u64, method: &str, params: Value) -> String {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

/// One POST. Redirects are not followed, and no proxy is taken from the environment.
fn exchange(endpoint: &str, bearer: &str, timeout: Duration, body: &str) -> Exchange {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .timeout_connect(Some(timeout))
        .timeout_global(Some(timeout))
        .build()
        .into();
    let sent = agent
        .post(endpoint)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", PROTOCOL_VERSION)
        .header("authorization", &format!("Bearer {bearer}"))
        .send(body.as_bytes());
    let mut response = match sent {
        Ok(response) => response,
        Err(error) => {
            return match error {
                ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => Exchange::Unreachable(
                    format!("{endpoint} could not be reached ({error}); nothing was sent"),
                ),
                ureq::Error::Io(ref e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    Exchange::Unreachable(format!(
                        "{endpoint} refused the connection; nothing was sent"
                    ))
                }
                ureq::Error::Timeout(ureq::Timeout::Connect | ureq::Timeout::Resolve) => {
                    Exchange::Unreachable(format!(
                        "{endpoint} did not accept a connection in time; nothing was sent"
                    ))
                }
                other => Exchange::Lost(format!(
                    "the request to {endpoint} may have been received, and no answer came back ({other})"
                )),
            };
        }
    };
    let status = response.status().as_u16();
    let mut text = String::new();
    match response
        .body_mut()
        .as_reader()
        .take(MAX_RESPONSE_BYTES)
        .read_to_string(&mut text)
    {
        Ok(_) => Exchange::Answered { status, body: text },
        Err(e) => Exchange::Lost(format!(
            "{endpoint} answered HTTP {status} but its body could not be read ({e})"
        )),
    }
}

#[async_trait::async_trait]
impl CapabilityProvider for LiveMcp {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(self.offered.iter().map(|o| o.descriptor.clone()).collect())
    }

    async fn availability(&self, id: &CapabilityId) -> CapabilityAvailability {
        if self.offered.iter().any(|o| &o.descriptor.id == id) {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable("not offered by this MCP session".into())
        }
    }
}

#[async_trait::async_trait]
impl Executor for LiveMcp {
    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let Some(offered) = self
            .offered
            .iter()
            .find(|o| o.descriptor.id.as_str() == request.intent)
        else {
            return Err(ExecutionError::InvalidRequest(format!(
                "{} is not offered by this MCP session",
                request.intent
            )));
        };
        let arguments = recorded_inputs(&request.inputs);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let body = self::request(
            id,
            "tools/call",
            serde_json::json!({"name": offered.tool, "arguments": arguments}),
        );
        let (endpoint, bearer, timeout) =
            (self.endpoint.clone(), self.bearer.clone(), self.timeout);
        let exchanged =
            tokio::task::spawn_blocking(move || exchange(&endpoint, &bearer, timeout, &body))
                .await
                .map_err(|e| {
                    ExecutionError::OutcomeUnknown(format!(
                        "the MCP call did not complete ({e}); its outcome is unknown"
                    ))
                })?;
        let (status, text) = match exchanged {
            Exchange::Unreachable(why) => return Err(ExecutionError::ExecutorUnavailable(why)),
            Exchange::Lost(why) => {
                return Err(ExecutionError::OutcomeUnknown(format!(
                    "{}: {why}; its outcome is unknown",
                    request.intent
                )));
            }
            Exchange::Answered { status, body } => (status, body),
        };
        if status != 200 {
            // The endpoint answered, so it was reached; it did not run the tool for this caller.
            let outcome = if matches!(status, 401 | 403) {
                "refused"
            } else {
                "failed"
            };
            let error =
                serde_json::json!({"code": format!("http.{status}"), "message": text.trim()});
            let output = self.render(offered, &arguments, outcome, error, &Value::Null);
            return Ok(ExecutionResult::failure(request.id, output));
        }
        let response: Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                return Err(ExecutionError::OutcomeUnknown(format!(
                    "{}: the endpoint answered with a body that is not JSON ({e}); its outcome is unknown",
                    request.intent
                )));
            }
        };
        if response["error"].is_null() && !response["result"].is_object() {
            return Err(ExecutionError::OutcomeUnknown(format!(
                "{}: the endpoint answered with no JSON-RPC result or error; its outcome is unknown",
                request.intent
            )));
        }
        let (outcome, error) = classify(&response);
        let result = &response["result"];
        let output = self.render(offered, &arguments, outcome.name(), error, result);
        let mut executed = if outcome == Outcome::Success {
            ExecutionResult::success(request.id, output)
        } else {
            ExecutionResult::failure(request.id, output)
        };
        executed.receipt_id = result["_meta"][HOST_EVIDENCE_META]["executionUnitId"]
            .as_str()
            .map(str::to_string);
        Ok(executed)
    }
}
