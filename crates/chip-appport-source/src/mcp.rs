//! The same source over MCP: what an AppPort application served through `@appport/mcp`, recorded
//! by `recorder/record-mcp.mjs` from a real session (`initialize`, `tools/list`, `tools/call`).
//!
//! Nothing here names a capability. Every descriptor comes from `tools/list`:
//!
//! - a tool is offered only when its `_meta["dev.appport/capability"]` names the AppPort capability
//!   and the scopes it requires, and those scopes are within the operator's grant. A tool without
//!   that metadata is not offered: its authority cannot be read. A tool outside the grant is not
//!   offered even though MCP lists it;
//! - its inputs and required inputs come from its `inputSchema`, with each input's JSON type in its
//!   description;
//! - it is [`Confinement::Unconfined`] and [`Effect::Consequential`], whatever effect the provider
//!   declares: the server is another process that Chip does not confine, so a provider's
//!   "observation" label is reported, never trusted. The source is therefore offered only in replay.
//!
//! Each recorded `tools/call` becomes a [`RecordedExecution`] whose observation says what MCP
//! answered: `success` (a result without `isError`), `refused` (an AppPort error whose category is
//! `unauthenticated` or `forbidden`) or `failed` (any other error, or a JSON-RPC error), with the
//! AppPort error object. Because only tools within the grant are offered, a refusal Chip observes
//! cannot be the grant's: it is the application's own authorization.
//!
//! The bridge stores nothing, so there are no host records to correlate. [`ApplicationAgrees`]
//! checks each observation against the application's own stored state, read independently by the
//! recorder (the `oracle`), and evaluates the state verifications the capability's own contract
//! declares (`integration.verify`) on that state.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use chip_core::{CapabilityDescriptor, CapabilityId, ExecutionStatus, WorkOutcome};
use chip_experiments::{
    CapabilityFixture, CapabilitySource, Confinement, Criterion, CriterionResult, Detection,
    Effect, HostObservation, Mode, RecordedExecution, TaskFixture,
};
use serde::Deserialize;
use serde_json::Value;

use crate::{
    BINDING_FILE, BINDING_SCHEMA, DeclarationsOnly, ID_PREFIX, NotOffered, appport_head,
    schema_inputs, text, valid_selector,
};

pub const MCP_SNAPSHOT_SCHEMA: &str = "chip.appport-mcp-snapshot.v1";
/// Where `@appport/mcp` puts a tool's AppPort capability, and a failed call's AppPort error.
pub const CAPABILITY_META: &str = "dev.appport/capability";
pub const ERROR_META: &str = "dev.appport/error";
pub const TRACE_META: &str = "dev.appport/traceId";
/// Where the AppBoundry host's MCP endpoint (`/apps/<selector>/mcp`, served by `appport host` and
/// `appport dev`) puts the same facts: the capability as `name@version`, its required scopes, a
/// failed call's AppPort error (a `code`, no category) and the execution evidence it records.
pub const HOST_CAPABILITY_META: &str = "appboundry/capability";
pub const HOST_AUTHORIZATION_META: &str = "appboundry/authorization";
pub const HOST_ERROR_META: &str = "appboundry/error";
pub const HOST_EVIDENCE_META: &str = "appboundry/evidence";

/// What a real MCP session served, as `recorder/record-mcp.mjs` wrote it.
#[derive(Debug, Clone, Deserialize)]
pub struct McpSnapshot {
    pub schema: String,
    pub label: String,
    /// The bridge's repository, commit and the packages that served it.
    pub bridge: Value,
    pub application: Value,
    pub grant: McpGrant,
    /// The `initialize` result.
    pub initialize: Value,
    /// The `tools/list` result's tools, as served.
    pub tools: Vec<Value>,
    pub calls: Vec<McpCall>,
    /// The application's stored state after the calls, read directly by the recorder.
    pub oracle: Value,
}

/// The operator's configuration of the MCP session: the AppPort scopes it grants, and a
/// description of the application credential it forwards (never the credential).
#[derive(Debug, Clone, Deserialize)]
pub struct McpGrant {
    pub scopes: Vec<String>,
    pub credential: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpCall {
    pub label: String,
    pub tool: String,
    pub arguments: BTreeMap<String, Value>,
    /// Set when the recorder deliberately presented a wrong MCP bearer. Never replayed: Chip
    /// does not choose the session's credential.
    #[serde(default)]
    pub mcp_bearer: Option<String>,
    pub http_status: u16,
    /// The JSON-RPC response.
    pub response: Value,
    /// The application's stored state read right after this call.
    #[serde(default)]
    pub oracle: Option<Value>,
}

impl McpSnapshot {
    pub fn from_json(text: &str) -> Result<Self, String> {
        let snapshot: McpSnapshot = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if snapshot.schema != MCP_SNAPSHOT_SCHEMA {
            return Err(format!(
                "snapshot schema is `{}`; expected `{MCP_SNAPSHOT_SCHEMA}`",
                snapshot.schema
            ));
        }
        if snapshot.initialize["capabilities"]["tools"].is_null() {
            return Err("the server did not declare the MCP tools capability".into());
        }
        Ok(snapshot)
    }
}

/// How a `tools/call` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    /// The application (or AppPort) refused the caller: `unauthenticated` or `forbidden`.
    Refused,
    Failed,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Outcome::Success => "success",
            Outcome::Refused => "refused",
            Outcome::Failed => "failed",
        }
    }
}

/// The outcome of a JSON-RPC `tools/call` response, and the AppPort error when there is one.
pub fn classify(response: &Value) -> (Outcome, Value) {
    if !response["error"].is_null() {
        let error = serde_json::json!({
            "code": "jsonrpc",
            "jsonrpc_code": response["error"]["code"],
            "message": response["error"]["message"],
        });
        return (Outcome::Failed, error);
    }
    let result = &response["result"];
    if result.is_null() {
        return (
            Outcome::Failed,
            serde_json::json!({"code": "jsonrpc", "message": "no result"}),
        );
    }
    if result["isError"] != true {
        return (Outcome::Success, Value::Null);
    }
    let mut error = result["_meta"][ERROR_META].clone();
    let mut outcome = match error["category"].as_str() {
        Some("unauthenticated" | "forbidden") => Outcome::Refused,
        _ => Outcome::Failed,
    };
    if error.is_null() && !result["_meta"][HOST_ERROR_META].is_null() {
        // The host's error carries a code and no category: the refusals are the codes that say the
        // caller was not authenticated or not permitted.
        error = result["_meta"][HOST_ERROR_META].clone();
        outcome = match error["code"].as_str() {
            Some("UNAUTHORIZED" | "AUTHENTICATION_REQUIRED" | "FORBIDDEN") => Outcome::Refused,
            _ => Outcome::Failed,
        };
    }
    let error = if error.is_null() {
        // An error result without AppPort's error object: only its text is known.
        serde_json::json!({"code": "unknown", "message": result["content"][0]["text"]})
    } else {
        error
    };
    (outcome, error)
}

/// One MCP tool Chip can offer.
#[derive(Debug, Clone)]
pub struct McpOffered {
    pub descriptor: CapabilityDescriptor,
    pub tool: String,
    /// The AppPort capability, `name@version`, from the tool's metadata.
    pub capability: String,
    pub scopes: Vec<String>,
    /// The effect the provider declares. Reported; Chip treats every tool as consequential.
    pub declared_effect: Option<String>,
    /// The capability's `integration` metadata, as served (its `verify` declarations among it).
    pub integration: Value,
}

/// A tool's AppPort capability, version and required scopes, from `@appport/mcp`'s metadata or,
/// failing that, the AppBoundry host's. `None` when neither names them.
fn capability_metadata(tool: &Value) -> Option<(String, u64, Option<&Vec<Value>>, &Value)> {
    let meta = &tool["_meta"][CAPABILITY_META];
    if let (Some(capability), Some(version)) = (meta["name"].as_str(), meta["version"].as_u64()) {
        return Some((
            capability.to_string(),
            version,
            meta["authorization"].as_array(),
            meta,
        ));
    }
    let host = tool["_meta"][HOST_CAPABILITY_META].as_str()?;
    let (capability, version) = host.rsplit_once('@')?;
    let version = version.parse().ok()?;
    Some((
        capability.to_string(),
        version,
        tool["_meta"][HOST_AUTHORIZATION_META].as_array(),
        &Value::Null,
    ))
}

pub(crate) fn map_tool(
    selector: &str,
    tool: &Value,
    grant: &[String],
) -> Result<McpOffered, NotOffered> {
    let name = text(tool, "name");
    let refuse = |reason: String| NotOffered {
        capability: name.clone(),
        reason,
    };
    let Some((capability, version, scopes, meta)) = capability_metadata(tool) else {
        return Err(refuse(format!(
            "the tool carries no {CAPABILITY_META} or {HOST_CAPABILITY_META} metadata: its authority cannot be read"
        )));
    };
    let Some(scopes) = scopes else {
        return Err(refuse(
            "the tool declares no authorization: its authority cannot be read".into(),
        ));
    };
    let scopes: Vec<String> = scopes
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let missing: Vec<&String> = scopes.iter().filter(|s| !grant.contains(s)).collect();
    if !missing.is_empty() {
        return Err(refuse(format!(
            "it requires {missing:?}, which the grant does not hold"
        )));
    }
    let id = format!("{ID_PREFIX}.{selector}.{name}");
    let id = CapabilityId::new(id.clone())
        .map_err(|_| refuse(format!("`{id}` is not a Chip capability id")))?;
    let (inputs, max_bytes) = schema_inputs(&tool["inputSchema"], true).map_err(refuse)?;
    let capability = format!("{capability}@{version}");
    let declared_effect = meta["effect"].as_str().map(str::to_string);
    let mut descriptor = CapabilityDescriptor::new(
        id,
        name.clone(),
        format!(
            "{} (MCP tool {name}: AppPort capability {capability}; the application authorizes and executes it{})",
            text(tool, "description"),
            declared_effect
                .as_deref()
                .map(|e| format!("; declared effect: {e}"))
                .unwrap_or_default()
        ),
    )
    .with_effect(Effect::Consequential);
    descriptor.version = Some(capability.clone());
    descriptor.inputs = inputs;
    if let Some(bytes) = max_bytes {
        descriptor = descriptor.with_max_input_bytes(bytes);
    }
    Ok(McpOffered {
        descriptor,
        tool: name,
        capability,
        scopes,
        declared_effect,
        integration: meta["integration"].clone(),
    })
}

/// The tools one MCP server offers under the operator's grant, as a Chip [`CapabilitySource`].
pub struct McpSource {
    id: &'static str,
    snapshot: Arc<McpSnapshot>,
    selector: String,
    offered: Vec<McpOffered>,
    not_offered: Vec<NotOffered>,
    provides: Vec<&'static str>,
}

impl McpSource {
    pub fn new(snapshot: McpSnapshot, selector: &str) -> Result<Self, String> {
        if !valid_selector(selector) {
            return Err(format!(
                "selector `{selector}` cannot name a Chip capability"
            ));
        }
        let mut offered = Vec::new();
        let mut not_offered = Vec::new();
        for tool in &snapshot.tools {
            match map_tool(selector, tool, &snapshot.grant.scopes) {
                Ok(o) => offered.push(o),
                Err(n) => not_offered.push(n),
            }
        }
        let id: &'static str = Box::leak(format!("{ID_PREFIX}.{selector}.mcp").into_boxed_str());
        let provides = offered
            .iter()
            .map(|o| -> &'static str {
                Box::leak(o.descriptor.id.as_str().to_string().into_boxed_str())
            })
            .collect();
        Ok(Self {
            id,
            snapshot: Arc::new(snapshot),
            selector: selector.to_string(),
            offered,
            not_offered,
            provides,
        })
    }

    pub fn offered(&self) -> &[McpOffered] {
        &self.offered
    }

    pub fn not_offered(&self) -> &[NotOffered] {
        &self.not_offered
    }

    pub fn snapshot(&self) -> &McpSnapshot {
        &self.snapshot
    }

    /// The offered capability ids: what an operator's allowlist names to admit this source.
    pub fn ids(&self) -> Vec<String> {
        self.provides.iter().map(|s| s.to_string()).collect()
    }

    pub fn call(&self, label: &str) -> Option<&McpCall> {
        self.snapshot.calls.iter().find(|c| c.label == label)
    }

    /// Every recorded call of an offered tool made with the operator's session, as replay serves it.
    pub fn recorded_executions(&self) -> Vec<RecordedExecution> {
        self.snapshot
            .calls
            .iter()
            .filter(|call| call.mcp_bearer.is_none())
            .filter_map(|call| {
                let offered = self.offered.iter().find(|o| o.tool == call.tool)?;
                let (outcome, _) = classify(&call.response);
                Some(RecordedExecution {
                    capability: offered.descriptor.id.as_str().to_string(),
                    inputs: call.arguments.clone(),
                    succeeded: outcome == Outcome::Success,
                    output: self.render(offered, call),
                })
            })
            .collect()
    }

    /// A JSON first line (capability, transport, request, outcome, AppPort error, trace id), then
    /// the result: the tool's structured content, or its text.
    fn render(&self, offered: &McpOffered, call: &McpCall) -> String {
        let (outcome, error) = classify(&call.response);
        let result = &call.response["result"];
        let mut head = serde_json::json!({
            "capability": offered.descriptor.id.as_str(),
            "transport": "mcp",
            "tool": offered.tool,
            "appport_capability": offered.capability,
            "request": call.arguments,
            "recorded_call": call.label,
            "outcome": outcome.name(),
            "trace_id": result["_meta"][TRACE_META],
            "server": self.snapshot.initialize["serverInfo"],
        });
        if outcome != Outcome::Success {
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

impl CapabilitySource for McpSource {
    fn id(&self) -> &'static str {
        self.id
    }

    fn version(&self) -> u32 {
        1
    }

    fn fixture(&self) -> Arc<dyn CapabilityFixture> {
        Arc::new(McpFixture {
            id: self.id,
            provides: self.provides.clone(),
            descriptors: self.offered.iter().map(|o| o.descriptor.clone()).collect(),
        })
    }

    fn effects(&self) -> Vec<(&'static str, Effect)> {
        self.provides
            .iter()
            .map(|c| (*c, Effect::Consequential))
            .collect()
    }

    /// Confirmed only when the project names this selector and the MCP server it expects is the
    /// one that answered `initialize`, and the grant offers at least one tool.
    fn detect(&self, task: &TaskFixture) -> Detection {
        let Some((_, binding)) = task.files.iter().find(|(p, _)| p == BINDING_FILE) else {
            return Detection::NotDetected {
                evidence: vec![format!("the project has no {BINDING_FILE}")],
            };
        };
        let binding: Value = match serde_json::from_str(binding) {
            Ok(v) => v,
            Err(e) => {
                return Detection::Inconclusive {
                    reason: format!("{BINDING_FILE} is not JSON: {e}"),
                };
            }
        };
        if binding["schema"] != BINDING_SCHEMA
            || binding["selector"] != self.selector.as_str()
            || binding["transport"] != "mcp"
        {
            return Detection::NotDetected {
                evidence: vec![format!(
                    "{BINDING_FILE} does not name the MCP selector `{}`",
                    self.selector
                )],
            };
        }
        let served = &self.snapshot.initialize["serverInfo"];
        if binding["server"] != *served {
            return Detection::Inconclusive {
                reason: format!(
                    "the project expects MCP server {}; {served} answered",
                    binding["server"]
                ),
            };
        }
        if self.offered.is_empty() {
            return Detection::Inconclusive {
                reason: format!(
                    "the server lists {} tools but the grant {:?} offers none Chip can request",
                    self.snapshot.tools.len(),
                    self.snapshot.grant.scopes
                ),
            };
        }
        Detection::Confirmed {
            evidence: vec![
                format!("MCP initialize answered by {served}"),
                format!(
                    "tools/list: {} tools; the grant {:?} offers {}",
                    self.snapshot.tools.len(),
                    self.snapshot.grant.scopes,
                    self.offered.len()
                ),
            ],
        }
    }
}

struct McpFixture {
    id: &'static str,
    provides: Vec<&'static str>,
    descriptors: Vec<CapabilityDescriptor>,
}

impl CapabilityFixture for McpFixture {
    fn id(&self) -> &'static str {
        self.id
    }

    fn version(&self) -> u32 {
        1
    }

    fn provides(&self) -> Vec<(&'static str, Confinement)> {
        self.provides
            .iter()
            .map(|c| (*c, Confinement::Unconfined))
            .collect()
    }

    fn backend(&self, _root: &Path) -> Arc<dyn chip_core::CapabilityBackend> {
        Arc::new(DeclarationsOnly(self.descriptors.clone()))
    }
}

// ---- the application's own state ----------------------------------------------------------------------

/// How one capability's results and refusals relate to the application's stored state: a
/// collection in the oracle, the request input that is its key, the result fields that must equal
/// stored columns, and the column that names its owner.
#[derive(Debug, Clone)]
pub enum Rule {
    /// The result is one stored row, keyed by `key_input`.
    Row {
        collection: &'static str,
        key_input: &'static str,
        fields: Vec<(&'static str, &'static str)>,
        owner: &'static str,
    },
    /// The result holds the rows at `array` (JSON pointer); each with a non-negative `key_field`
    /// is a stored row of `collection` whose `fields` agree, and every stored row of the caller is
    /// listed.
    Rows {
        collection: &'static str,
        array: &'static str,
        key_field: &'static str,
        fields: Vec<(&'static str, &'static str)>,
        owner: &'static str,
    },
}

/// "Every MCP observation agrees with the application's own stored state", as read right after the
/// recorded call that answered it. A success's result
/// must equal the stored rows its rule names; a refusal must concern a row that exists and that
/// the caller does not own (an application refusal of a missing row would leak nothing, but would
/// not be what was observed); `NOT_FOUND` must concern a row that does not exist; and every state
/// verification the capability's contract declares must hold on the stored state.
pub struct ApplicationAgrees {
    source: Arc<McpSource>,
    /// The application principal whose credential the operator configured.
    principal: String,
    rules: BTreeMap<String, Rule>,
}

impl ApplicationAgrees {
    /// `rules` by AppPort capability name (`vikunja.tasks.get`).
    pub fn new(source: Arc<McpSource>, principal: &str, rules: Vec<(&str, Rule)>) -> Self {
        Self {
            source,
            principal: principal.into(),
            rules: rules.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        }
    }

    /// The stored state an observation is judged by: the state read right after the recorded call
    /// that answered it, or the phase's final state when the call carries none.
    fn state(&self, head: &Value) -> &Value {
        self.source
            .snapshot
            .calls
            .iter()
            .find(|c| c.label == text(head, "recorded_call"))
            .and_then(|c| c.oracle.as_ref())
            .unwrap_or(&self.source.snapshot.oracle)
    }

    fn stored<'a>(state: &'a Value, collection: &str) -> Vec<&'a Value> {
        state[collection]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default()
    }

    fn row<'a>(state: &'a Value, collection: &str, key: &Value) -> Option<&'a Value> {
        Self::stored(state, collection)
            .into_iter()
            .find(|r| r["id"] == *key)
    }

    fn rule_for(&self, capability: &str) -> Option<&Rule> {
        self.rules
            .get(capability.split('@').next().unwrap_or_default())
    }

    /// Checks one observation (its head and result text) against the stored state.
    pub fn check(&self, head: &Value, result: &str) -> Result<(), String> {
        let id = text(head, "capability");
        let capability = text(head, "appport_capability");
        let offered = self
            .source
            .offered
            .iter()
            .find(|o| o.descriptor.id.as_str() == id)
            .ok_or_else(|| format!("{id} is not a tool this source offered"))?;
        let rule = self
            .rule_for(&capability)
            .ok_or_else(|| format!("{id}: no rule relates {capability} to stored state"))?;
        let request = &head["request"];
        let state = self.state(head);
        match head["outcome"].as_str() {
            Some("success") => {
                let output: Value = serde_json::from_str(result)
                    .map_err(|e| format!("{id}: the result is not JSON: {e}"))?;
                self.agrees(state, &id, rule, request, &output)?;
                self.verified(state, &id, offered, request)
            }
            Some("refused") => {
                let (collection, key, owner) = match rule {
                    Rule::Row {
                        collection,
                        key_input,
                        owner,
                        ..
                    } => (*collection, &request[*key_input], *owner),
                    Rule::Rows { .. } => {
                        return Err(format!("{id}: a listing was refused to its own caller"));
                    }
                };
                let row = Self::row(state, collection, key)
                    .ok_or_else(|| format!("{id}: refused for {key}, which is not stored"))?;
                if row[owner] == self.principal.as_str() {
                    return Err(format!(
                        "{id}: refused for {key}, which {} owns",
                        self.principal
                    ));
                }
                Ok(())
            }
            Some("failed") => match (text(&head["error"], "code").as_str(), rule) {
                (
                    "NOT_FOUND",
                    Rule::Row {
                        collection,
                        key_input,
                        ..
                    },
                ) => match Self::row(state, collection, &request[*key_input]) {
                    None => Ok(()),
                    Some(_) => Err(format!(
                        "{id}: NOT_FOUND for {}, which is stored",
                        request[*key_input]
                    )),
                },
                // Nothing reached stored state, so there is nothing in it to agree with.
                ("INVALID_INPUT", _) => Ok(()),
                (code, _) => Err(format!("{id}: failed with {code}, which no rule explains")),
            },
            other => Err(format!("{id}: unknown outcome {other:?}")),
        }
    }

    fn agrees(
        &self,
        state: &Value,
        id: &str,
        rule: &Rule,
        request: &Value,
        output: &Value,
    ) -> Result<(), String> {
        let same = |pointer: &str, row: &Value, column: &str| -> Result<(), String> {
            let got = output.pointer(pointer).unwrap_or(&Value::Null);
            if got == &row[column] {
                Ok(())
            } else {
                Err(format!(
                    "{id}: result {pointer} is {got}; the application stored {}",
                    row[column]
                ))
            }
        };
        match rule {
            Rule::Row {
                collection,
                key_input,
                fields,
                ..
            } => {
                let key = &request[*key_input];
                let row = Self::row(state, collection, key)
                    .ok_or_else(|| format!("{id}: {key} is not stored"))?;
                fields.iter().try_for_each(|(p, c)| same(p, row, c))
            }
            Rule::Rows {
                collection,
                array,
                key_field,
                fields,
                owner,
            } => {
                let listed = output
                    .pointer(array)
                    .and_then(Value::as_array)
                    .ok_or_else(|| format!("{id}: the result has no {array}"))?;
                let mut seen = Vec::new();
                for item in listed {
                    let key = &item[*key_field];
                    if key.as_i64().is_some_and(|k| k < 0) {
                        continue; // virtual rows the application computes, not stored ones
                    }
                    let row = Self::row(state, collection, key)
                        .ok_or_else(|| format!("{id}: listed {key}, which is not stored"))?;
                    for (p, c) in fields {
                        let got = item.pointer(p).unwrap_or(&Value::Null);
                        if got != &row[*c] {
                            return Err(format!(
                                "{id}: listed {key} has {p} {got}; the application stored {}",
                                row[*c]
                            ));
                        }
                    }
                    seen.push(key.clone());
                }
                for row in Self::stored(state, collection) {
                    if row[*owner] == self.principal.as_str() && !seen.contains(&row["id"]) {
                        return Err(format!(
                            "{id}: {} stores {} for {}, which the result does not list",
                            collection, row["id"], self.principal
                        ));
                    }
                }
                Ok(())
            }
        }
    }

    /// The capability's declared state verifications (`integration.verify`, kind `state`), each
    /// evaluated on stored state: the verifying capability's rule says which row it reads and
    /// which column each result field is.
    fn verified(
        &self,
        state: &Value,
        id: &str,
        offered: &McpOffered,
        request: &Value,
    ) -> Result<(), String> {
        for verify in offered.integration["verify"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if verify["kind"] != "state" {
                continue;
            }
            let via = text(&verify["via"], "capability");
            let Some(Rule::Row {
                collection,
                key_input,
                fields,
                ..
            }) = self.rules.get(&via)
            else {
                return Err(format!(
                    "{id}: its contract verifies through {via}, which no row rule relates to stored state"
                ));
            };
            let resolve = |v: &Value| -> Value {
                match v["$input"].as_str() {
                    Some(pointer) => request.pointer(pointer).cloned().unwrap_or(Value::Null),
                    None => v.clone(),
                }
            };
            let key = resolve(&verify["via"]["input"][*key_input]);
            let row = Self::row(state, collection, &key).ok_or_else(|| {
                format!(
                    "{id}: verification {} reads {key}, which is not stored",
                    verify["id"]
                )
            })?;
            for expect in verify["expect"].as_array().into_iter().flatten() {
                let at = text(expect, "at");
                let column = fields
                    .iter()
                    .find(|(p, _)| *p == at)
                    .map(|(_, c)| *c)
                    .ok_or_else(|| {
                        format!(
                            "{id}: verification reads {at}, which the rule for {via} does not map"
                        )
                    })?;
                let wanted = resolve(&expect["equals"]);
                if row[column] != wanted {
                    return Err(format!(
                        "{id}: verification {} failed: stored {column} is {}, the contract expects {wanted}",
                        verify["id"], row[column]
                    ));
                }
            }
        }
        Ok(())
    }
}

fn result_text(output: &str) -> &str {
    output
        .split_once("\n--- result ---\n")
        .map(|(_, r)| r)
        .unwrap_or_default()
}

impl Criterion for ApplicationAgrees {
    fn id(&self) -> &'static str {
        "appport.mcp.application-agrees"
    }

    fn describe(&self) -> String {
        "every MCP observation agrees with the application's own stored state, and the contract's declared verifications hold on it".into()
    }

    fn supports(&self, _mode: Mode) -> bool {
        true
    }

    fn evaluate(&self, host: &HostObservation<'_>) -> CriterionResult {
        let mut checked = 0;
        for observation in &host.report.observations {
            let Some(head) = appport_head(observation) else {
                continue;
            };
            if head["transport"] != "mcp" {
                continue;
            }
            let output = observation.output.as_deref().unwrap_or_default();
            if let Err(why) = self.check(&head, result_text(output)) {
                return CriterionResult::NotMet(why);
            }
            checked += 1;
        }
        if checked == 0 {
            CriterionResult::Unavailable("no MCP tool was executed".into())
        } else {
            CriterionResult::Met
        }
    }
}

/// "The work's completion summary states `expected`": each `key=value` the host requires, read
/// from the application's stored state by whoever composes the experiment. The model is asked to
/// end with these pairs; any other value for a key, or a missing key, is not met.
pub struct AnswerStates {
    expected: Vec<(String, String)>,
}

impl AnswerStates {
    pub fn new(expected: Vec<(&str, String)>) -> Self {
        Self {
            expected: expected
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        }
    }

    /// The `key=value` pairs in `summary`. A key is the run of letters, digits and `_` just
    /// before `=`; its value runs to the next `;`, new line, `,` or `)`, or a `. ` that ends a
    /// sentence, trimmed of quotes and a final period. A later pair for a key wins, so prose may
    /// precede the pairs the goal asks to end with.
    pub fn pairs(summary: &str) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        for segment in summary.split([';', '\n']) {
            for (at, _) in segment.match_indices('=') {
                let before = &segment[..at];
                let key: String = before
                    .chars()
                    .rev()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                if key.is_empty() {
                    continue;
                }
                let rest = &segment[at + 1..];
                let end = [rest.find(','), rest.find(')'), rest.find(". ")]
                    .into_iter()
                    .flatten()
                    .min()
                    .unwrap_or(rest.len());
                let value = rest[..end]
                    .trim()
                    .trim_end_matches('.')
                    .trim_matches(['"', '\'', '`'])
                    .to_string();
                out.insert(key.to_ascii_lowercase(), value);
            }
        }
        out
    }
}

impl Criterion for AnswerStates {
    fn id(&self) -> &'static str {
        "appport.mcp.answer-states"
    }

    fn describe(&self) -> String {
        format!("the completion summary states {:?}", self.expected)
    }

    fn supports(&self, _mode: Mode) -> bool {
        true
    }

    fn evaluate(&self, host: &HostObservation<'_>) -> CriterionResult {
        let WorkOutcome::Completed { summary } = &host.report.outcome else {
            return CriterionResult::NotMet("the work did not complete".into());
        };
        let stated = Self::pairs(summary);
        for (key, value) in &self.expected {
            match stated.get(key) {
                Some(v) if v.eq_ignore_ascii_case(value) => {}
                Some(v) => {
                    return CriterionResult::NotMet(format!(
                        "the summary states {key}={v}; the application stores {value}"
                    ));
                }
                None => {
                    return CriterionResult::NotMet(format!("the summary does not state {key}"));
                }
            }
        }
        CriterionResult::Met
    }
}

/// Whether an observation is a successful MCP call of `capability` (for tests and drivers).
pub fn succeeded(observation: &chip_core::Observation, capability: &str) -> bool {
    observation.status == ExecutionStatus::Success
        && appport_head(observation)
            .is_some_and(|h| h["capability"] == capability && h["outcome"] == "success")
}
