//! AppPort discovery and invocation for host-supplied capabilities.
//!
//! Follows AppPort 1 (`spec/appport-1.0.md` §16-§18, the host client in `packages/client`):
//!
//! - `GET {origin}/.well-known/appport` answers either `appport/1` (one application: `manifest` and
//!   `endpoint` paths) or `AppPort/host-discovery/1` (a desktop host: applications and their
//!   `name@version` references, invoked at `POST {origin}/apps/{id}/capabilities/{name@version}` with
//!   the caller's scopes in `x-appport-scopes`);
//! - the manifest is the authoritative contract (input schema, effect, idempotency, scopes). A desktop
//!   host's discovery document carries none, so the operator names a reviewed `manifest`;
//! - a call is one AppPort request envelope; the answer is one response envelope.
//!
//! Discovery sends **no credential**. Redirects are refused, no proxy is taken from the environment,
//! every body read is bounded, a document may only point at same-origin paths, and `http` is accepted
//! only for a loopback address (a token must not cross the network in the clear).

use std::collections::BTreeSet;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use chip_core::{
    CapabilityDescriptor, CapabilityId, CapabilityInput, ExecutionError, ExecutionRequest,
    ExecutionResult, Idempotency, Withheld,
};
use serde_json::{Value, json};

use super::{
    AppPortSource, Binding, DEFAULT_TIMEOUT, Entry, Kind, MAX_OBSERVATION_BYTES, MAX_TIMEOUT_SECS,
    bounded, clean_text, head, parse_effect, to_json, valid_input_name,
};

const MAX_DOCUMENT_BYTES: u64 = 1 << 20;
const MAX_ANSWER_BYTES: u64 = 4 << 20;
const MAX_INPUTS: usize = 16;
const BYTES_PER_CHAR: usize = 4;
pub const PROTOCOL: &str = "appport/1";
pub const HOST_PROTOCOL: &str = "AppPort/host-discovery/1";

pub(crate) struct Discovered {
    pub(crate) offered: Vec<Entry>,
    pub(crate) withheld: Vec<Withheld>,
    pub(crate) report: Value,
}

/// How to reach one source, shared by its capabilities. The token is held here and nowhere else.
pub struct Conn {
    origin: String,
    token: Option<String>,
    timeout: Duration,
}

enum Route {
    /// `appport/1`: the endpoint the discovery document named.
    Endpoint(String),
    /// A desktop host: the application and the capability reference, plus the scopes to send.
    Host { url: String, scopes: String },
}

pub struct Call {
    conn: Arc<Conn>,
    route: Route,
    name: String,
    version: u64,
    supports_key: bool,
}

// ---- origin and document hygiene ------------------------------------------------------------------

/// `https://host[:port]`, or `http://` for a loopback host only. No userinfo, path, query or fragment.
pub fn check_origin(origin: &str) -> Result<String, String> {
    let origin = origin.trim_end_matches('/');
    let (scheme, rest) = origin
        .split_once("://")
        .ok_or_else(|| format!("`{origin}` is not an http(s) origin"))?;
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', '\\', ' ']) {
        return Err(format!(
            "`{origin}` must be a bare origin (scheme, host, optional port): no path, query, fragment or userinfo"
        ));
    }
    let host = if let Some(v6) = rest.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        rest.rsplit_once(':').map_or(rest, |(h, _)| h)
    };
    match scheme {
        "https" => Ok(origin.to_string()),
        "http" if matches!(host, "localhost" | "127.0.0.1" | "::1") => Ok(origin.to_string()),
        "http" => Err(format!(
            "`{origin}` is plain http to a non-loopback host: a token would cross the network in the clear; use https"
        )),
        _ => Err(format!("`{origin}` is not an http(s) origin")),
    }
}

/// A same-origin, path-absolute reference: what a document may point at.
fn path_ok(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains(['\\', '?', '#', ' '])
        && !path.split('/').any(|s| s == ".." || s == ".")
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .timeout_connect(Some(timeout))
        .timeout_global(Some(timeout))
        .build()
        .into()
}

fn get_json(url: &str, timeout: Duration, limit: u64) -> Result<Value, String> {
    let mut response = agent(timeout)
        .get(url)
        .header("accept", "application/json")
        .call()
        .map_err(|e| format!("{url} could not be fetched ({e})"))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(format!(
            "{url} answered HTTP {status}{}",
            if (300..400).contains(&status) {
                " (a redirect; redirects are refused)"
            } else {
                ""
            }
        ));
    }
    let mut text = String::new();
    response
        .body_mut()
        .as_reader()
        .take(limit + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("{url}: the body could not be read ({e})"))?;
    if text.len() as u64 > limit {
        return Err(format!("{url} is larger than {limit} bytes"));
    }
    serde_json::from_str(&text).map_err(|e| format!("{url} is not JSON ({e})"))
}

fn encode_component(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

// ---- discovery ------------------------------------------------------------------------------------

pub(crate) fn discover(
    source: &AppPortSource,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Discovered, String> {
    let origin = check_origin(&source.origin)?;
    let timeout = match source.timeout_secs {
        None => DEFAULT_TIMEOUT,
        Some(s) if (1..=MAX_TIMEOUT_SECS).contains(&s) => Duration::from_secs(s),
        Some(_) => return Err(format!("`timeout_secs` must be 1 to {MAX_TIMEOUT_SECS}")),
    };
    let token = match &source.token_env {
        None => None,
        Some(name) => Some(
            env(name)
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| format!("the token variable `{name}` is not set"))?,
        ),
    };
    let conn = Arc::new(Conn {
        origin: origin.clone(),
        token,
        timeout,
    });
    // No credential is sent for discovery.
    let doc = get_json(
        &format!("{origin}/.well-known/appport"),
        timeout,
        MAX_DOCUMENT_BYTES,
    )?;
    match doc["protocol"].as_str() {
        Some(PROTOCOL) => single(source, &conn, &doc),
        Some(HOST_PROTOCOL) => host(source, &conn, &doc),
        other => Err(format!(
            "{origin} answered discovery with protocol {other:?}; understood: `{PROTOCOL}`, `{HOST_PROTOCOL}`"
        )),
    }
}

fn load_manifest(
    source: &AppPortSource,
    conn: &Conn,
    default_path: Option<&str>,
) -> Result<Value, String> {
    match source.manifest.as_deref() {
        Some(m) if m.starts_with("http://") || m.starts_with("https://") => {
            if !m.starts_with(&format!("{}/", conn.origin)) {
                return Err(format!(
                    "the manifest URL `{m}` is not on the origin {}",
                    conn.origin
                ));
            }
            get_json(m, conn.timeout, MAX_DOCUMENT_BYTES)
        }
        Some(path) => {
            let meta =
                std::fs::metadata(path).map_err(|e| format!("manifest {path}: {}", e.kind()))?;
            if !meta.is_file() || meta.len() > MAX_DOCUMENT_BYTES {
                return Err(format!(
                    "manifest {path} must be a regular file of at most {MAX_DOCUMENT_BYTES} bytes"
                ));
            }
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("manifest {path}: {}", e.kind()))?;
            serde_json::from_str(&text).map_err(|e| format!("manifest {path} is not JSON ({e})"))
        }
        None => {
            let path = default_path
                .ok_or("this host's discovery document names no manifest; set `manifest`")?;
            if !path_ok(path) {
                return Err(format!(
                    "the discovery document's manifest reference `{path}` is not a same-origin path"
                ));
            }
            get_json(
                &format!("{}{path}", conn.origin),
                conn.timeout,
                MAX_DOCUMENT_BYTES,
            )
        }
    }
}

/// A capability as the manifest declares it for one version.
struct Declared {
    name: String,
    version: u64,
    kind: String,
    description: String,
    schema: Value,
    scopes: Vec<String>,
    effect: Option<String>,
    idempotency_supported: bool,
}

fn manifest_capabilities(manifest: &Value) -> Result<Vec<Declared>, String> {
    let list = manifest["capabilities"]
        .as_array()
        .ok_or("the manifest has no `capabilities` array")?;
    let mut out = Vec::new();
    for cap in list {
        let Some(name) = cap["name"].as_str() else {
            continue;
        };
        let latest = cap["latestVersion"].as_u64();
        let mut versions: Vec<(u64, &Value)> = cap["definitions"]
            .as_array()
            .map(|d| {
                d.iter()
                    .filter_map(|d| Some((d["version"].as_u64()?, d)))
                    .collect()
            })
            .unwrap_or_default();
        if versions.is_empty()
            && let Some(v) = latest
        {
            versions.push((v, cap));
        }
        for (version, def) in versions {
            // Fields live on the definition; the entry's top level carries the newest version's.
            let field = |k: &str| {
                if def[k].is_null() && latest == Some(version) {
                    &cap[k]
                } else {
                    &def[k]
                }
            };
            let contract = field("authorizationContract");
            let scopes: Vec<String> = if contract["required"] == json!(false) {
                Vec::new()
            } else if let Some(s) = contract["scopes"].as_array() {
                s.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            } else {
                field("authorization")
                    .as_array()
                    .map(|s| {
                        s.iter()
                            .filter_map(|s| s.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            out.push(Declared {
                name: name.to_string(),
                version,
                kind: field("kind").as_str().unwrap_or("request").to_string(),
                description: field("description")
                    .as_str()
                    .or_else(|| cap["description"].as_str())
                    .unwrap_or("")
                    .to_string(),
                schema: field("inputSchema").clone(),
                scopes,
                effect: field("effect").as_str().map(str::to_string),
                idempotency_supported: field("idempotency").as_str() == Some("supported"),
            });
        }
    }
    Ok(out)
}

fn single(source: &AppPortSource, conn: &Arc<Conn>, doc: &Value) -> Result<Discovered, String> {
    let endpoint = doc["endpoint"].as_str().unwrap_or("/appport");
    if !path_ok(endpoint) {
        return Err(format!(
            "the discovery document's endpoint `{endpoint}` is not a same-origin path"
        ));
    }
    let manifest = load_manifest(source, conn, doc["manifest"].as_str())?;
    let declared = manifest_capabilities(&manifest)?;
    // Only the newest version of each capability is offered.
    let newest: std::collections::BTreeMap<&str, u64> =
        declared.iter().fold(Default::default(), |mut m, d| {
            let e = m.entry(d.name.as_str()).or_insert(0);
            *e = (*e).max(d.version);
            m
        });
    let candidates: Vec<&Declared> = declared
        .iter()
        .filter(|d| newest[d.name.as_str()] == d.version)
        .collect();
    let url = format!("{}{endpoint}", conn.origin);
    build(source, conn, "single", None, &candidates, |_, _| {
        Route::Endpoint(url.clone())
    })
}

fn host(source: &AppPortSource, conn: &Arc<Conn>, doc: &Value) -> Result<Discovered, String> {
    let app_id = source.application.as_deref().ok_or_else(|| {
        format!(
            "{} is a desktop host serving several applications: set `application`",
            conn.origin
        )
    })?;
    let app = doc["applications"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["id"].as_str() == Some(app_id)))
        .ok_or_else(|| format!("the host does not list application `{app_id}`"))?;
    if app["status"].as_str() != Some("running") {
        return Err(format!(
            "application `{app_id}` is not running (status {:?})",
            app["status"]
        ));
    }
    if source.manifest.is_none() {
        return Err(format!(
            "the host's discovery document carries no input schemas: set `manifest` to the reviewed manifest of `{app_id}`"
        ));
    }
    let served: BTreeSet<(String, u64)> = app["capabilities"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let (n, v) = c.as_str()?.rsplit_once('@')?;
                    Some((n.to_string(), v.parse().ok()?))
                })
                .collect()
        })
        .unwrap_or_default();
    let manifest = load_manifest(source, conn, None)?;
    let declared = manifest_capabilities(&manifest)?;
    let (agreed, mut disagreed): (Vec<&Declared>, Vec<&Declared>) = declared
        .iter()
        .partition(|d| served.contains(&(d.name.clone(), d.version)));
    let mut out = build(source, conn, "host", Some(app_id), &agreed, |d, scopes| {
        Route::Host {
            url: format!(
                "{}/apps/{}/capabilities/{}",
                conn.origin,
                encode_component(app_id),
                encode_component(&format!("{}@{}", d.name, d.version))
            ),
            scopes,
        }
    })?;
    // Served but not in the reviewed manifest, and in the manifest but not served: neither is offered.
    for (name, version) in served
        .iter()
        .filter(|(n, v)| !declared.iter().any(|d| &d.name == n && d.version == *v))
    {
        out.withheld.push(Withheld {
            capability: format!("{name}@{version}"),
            reason: "the host serves it but the reviewed manifest does not declare it".into(),
        });
    }
    disagreed.sort_by(|a, b| a.name.cmp(&b.name));
    for d in disagreed {
        out.withheld.push(Withheld {
            capability: format!("{}@{}", d.name, d.version),
            reason: "the reviewed manifest declares it but the host does not serve that version"
                .into(),
        });
    }
    out.report["withheld"] = json!(
        out.withheld
            .iter()
            .map(|w| json!({"capability": w.capability, "reason": w.reason}))
            .collect::<Vec<_>>()
    );
    Ok(out)
}

/// Applies the operator's authorization to what the host serves. Discovery grants nothing.
fn build(
    source: &AppPortSource,
    conn: &Arc<Conn>,
    mode: &str,
    application: Option<&str>,
    candidates: &[&Declared],
    route: impl Fn(&Declared, String) -> Route,
) -> Result<Discovered, String> {
    let grant: BTreeSet<&str> = source.grant.iter().map(String::as_str).collect();
    let all = source.authorize.iter().any(|a| a == "*");
    let mut offered = Vec::new();
    let mut withheld = Vec::new();
    let mut ordered: Vec<&&Declared> = candidates.iter().collect();
    ordered.sort_by(|a, b| a.name.cmp(&b.name));
    for d in ordered {
        let hold = |reason: String| Withheld {
            capability: d.name.clone(),
            reason,
        };
        if d.name.starts_with("appport.") {
            withheld.push(hold("a reserved AppPort protocol capability".into()));
            continue;
        }
        if !all && !source.authorize.iter().any(|a| a == &d.name) {
            withheld.push(hold(
                "discovered, and not authorized by the operator".into(),
            ));
            continue;
        }
        let beyond: Vec<&str> = d
            .scopes
            .iter()
            .map(String::as_str)
            .filter(|s| !grant.contains(s))
            .collect();
        if !beyond.is_empty() {
            withheld.push(hold(format!(
                "needs scopes the operator did not grant: {}",
                beyond.join(", ")
            )));
            continue;
        }
        if d.kind != "request" {
            withheld.push(hold(format!(
                "is a `{}` capability; only request/response capabilities are supported",
                d.kind
            )));
            continue;
        }
        let mapped = match map_schema(&d.schema) {
            Ok(m) => m,
            Err(why) => {
                withheld.push(hold(format!("its input schema cannot be expressed as string, integer and boolean inputs: {why}")));
                continue;
            }
        };
        let effect = match parse_effect(d.effect.as_deref()) {
            Ok(e) => e,
            Err(_) => chip_core::Effect::Unknown,
        };
        let id = match CapabilityId::new(d.name.clone()) {
            Ok(id) => id,
            Err(_) => {
                withheld.push(hold("its name is not a valid Chip capability id (lowercase letters, digits, `.`, `_`, `-`)".into()));
                continue;
            }
        };
        let mut descriptor = CapabilityDescriptor::new(
            id,
            d.name.clone(),
            clean_text(&d.description, super::MAX_DESCRIPTION),
        )
        .with_effect(effect)
        .with_idempotency(if d.idempotency_supported {
            Idempotency::Deduplicated
        } else {
            Idempotency::NotCarried
        });
        if let Some(bytes) = mapped.max_bytes {
            descriptor = descriptor.with_max_input_bytes(bytes);
        }
        let mut kinds = std::collections::BTreeMap::new();
        for (name, kind, required, description) in mapped.inputs {
            kinds.insert(name.clone(), kind);
            descriptor.inputs.push(CapabilityInput {
                name,
                description: clean_text(&description, super::MAX_DESCRIPTION),
                required,
            });
        }
        let scopes = d.scopes.join(",");
        offered.push(Entry {
            descriptor,
            kinds,
            binding: Binding::AppPort(Call {
                conn: conn.clone(),
                route: route(d, scopes),
                name: d.name.clone(),
                version: d.version,
                supports_key: d.idempotency_supported,
            }),
            source: format!("appport:{}", conn.origin),
        });
    }
    let report = json!({
        "kind": "appport",
        "origin": conn.origin,
        "mode": mode,
        "application": application,
        "served": candidates.len(),
        "offered": offered.iter().map(|e| e.descriptor.id.to_string()).collect::<Vec<_>>(),
        "withheld": withheld.iter().map(|w| json!({"capability": w.capability, "reason": w.reason})).collect::<Vec<_>>(),
    });
    Ok(Discovered {
        offered,
        withheld,
        report,
    })
}

// ---- input schemas ----------------------------------------------------------------------------------

pub struct Mapped {
    pub inputs: Vec<(String, Kind, bool, String)>,
    pub max_bytes: Option<usize>,
}

/// A flat object of string, integer and boolean properties, or an error saying what is not expressible.
pub fn map_schema(schema: &Value) -> Result<Mapped, String> {
    let Some(obj) = schema.as_object() else {
        return Err("the schema is not an object".into());
    };
    for banned in [
        "oneOf",
        "anyOf",
        "allOf",
        "not",
        "$ref",
        "if",
        "patternProperties",
    ] {
        if obj.contains_key(banned) {
            return Err(format!("it uses `{banned}`"));
        }
    }
    if let Some(t) = obj.get("type")
        && t != "object"
    {
        return Err("its type is not `object`".into());
    }
    let empty = serde_json::Map::new();
    let props = obj
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    if props.len() > MAX_INPUTS {
        return Err(format!("it has more than {MAX_INPUTS} properties"));
    }
    let required: BTreeSet<&str> = obj
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if let Some(unknown) = required.iter().find(|r| !props.contains_key(**r)) {
        return Err(format!(
            "it requires `{unknown}`, which it does not declare"
        ));
    }
    let (mut inputs, mut max_chars) = (Vec::new(), None::<usize>);
    for (name, prop) in props {
        if !valid_input_name(name) {
            return Err(format!("property name `{name}` is not representable"));
        }
        for banned in ["oneOf", "anyOf", "allOf", "$ref", "items", "properties"] {
            if prop.get(banned).is_some() {
                return Err(format!("property `{name}` uses `{banned}`"));
            }
        }
        let kind = match prop["type"].as_str() {
            Some("string") => Kind::Text,
            Some("integer") => Kind::Integer,
            Some("boolean") => Kind::Bool,
            other => return Err(format!("property `{name}` has type {other:?}")),
        };
        if kind == Kind::Text
            && let Some(m) = prop["maxLength"].as_u64()
        {
            max_chars = Some(max_chars.map_or(m as usize, |c| c.max(m as usize)));
        }
        inputs.push((
            name.clone(),
            kind,
            required.contains(name.as_str()),
            prop["description"].as_str().unwrap_or("").to_string(),
        ));
    }
    Ok(Mapped {
        inputs,
        max_bytes: max_chars.map(|c| c.saturating_mul(BYTES_PER_CHAR)),
    })
}

// ---- invocation -------------------------------------------------------------------------------------

enum Exchange {
    Answered {
        status: u16,
        body: String,
        unit: Option<String>,
    },
    Unreachable(String),
    Lost(String),
}

fn exchange(url: &str, conn: &Conn, scopes: Option<&str>, body: &str) -> Exchange {
    let mut request = agent(conn.timeout)
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json");
    if let Some(token) = &conn.token {
        request = request.header("authorization", &format!("Bearer {token}"));
    }
    if let Some(scopes) = scopes.filter(|s| !s.is_empty()) {
        request = request.header("x-appport-scopes", scopes);
    }
    let mut response = match request.send(body.as_bytes()) {
        Ok(r) => r,
        Err(error) => {
            return match error {
                ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => {
                    Exchange::Unreachable(format!(
                        "{} could not be reached ({error}); nothing was sent",
                        conn.origin
                    ))
                }
                ureq::Error::Io(ref e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                    Exchange::Unreachable(format!(
                        "{} refused the connection; nothing was sent",
                        conn.origin
                    ))
                }
                ureq::Error::Timeout(ureq::Timeout::Connect | ureq::Timeout::Resolve) => {
                    Exchange::Unreachable(format!(
                        "{} did not accept a connection in time; nothing was sent",
                        conn.origin
                    ))
                }
                other => Exchange::Lost(format!(
                    "the request to {} may have been received, and no answer came back ({other})",
                    conn.origin
                )),
            };
        }
    };
    let status = response.status().as_u16();
    let unit = response
        .headers()
        .get("x-appboundry-execution-unit")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut text = String::new();
    match response
        .body_mut()
        .as_reader()
        .take(MAX_ANSWER_BYTES)
        .read_to_string(&mut text)
    {
        Ok(_) => Exchange::Answered {
            status,
            body: text,
            unit,
        },
        Err(e) => Exchange::Lost(format!(
            "{} answered HTTP {status} but its body could not be read ({e})",
            conn.origin
        )),
    }
}

/// What an answer means for the action, decided from the host's response and nothing the model said.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Success(Value),
    /// Sent, and the host refused the caller: nothing ran.
    Refused(String),
    /// Sent, and the host says it did not run it.
    NotRun(String),
    /// Sent, and what happened is not known.
    Unknown(String),
}

pub fn classify(status: u16, body: &str) -> Outcome {
    let envelope: Option<Value> = serde_json::from_str(body)
        .ok()
        .filter(|v: &Value| v["type"] == "response" && v["ok"].is_boolean());
    let Some(envelope) = envelope else {
        return match status {
            401 | 403 => Outcome::Refused(format!("HTTP {status}")),
            400 | 404 | 413 | 429 => Outcome::NotRun(format!("HTTP {status}")),
            _ => Outcome::Unknown(format!(
                "HTTP {status} with an answer that is not an AppPort response"
            )),
        };
    };
    if envelope["ok"] == json!(true) {
        return Outcome::Success(envelope["output"].clone());
    }
    let code = envelope["error"]["code"].as_str().unwrap_or("");
    let reason = envelope["error"]["details"]["reason"]
        .as_str()
        .unwrap_or("");
    let message = clean_text(envelope["error"]["message"].as_str().unwrap_or(""), 300);
    let said = if message.is_empty() {
        code.to_string()
    } else {
        format!("{code}: {message}")
    };
    match (code, reason) {
        ("UNAUTHORIZED" | "FORBIDDEN", _) => Outcome::Refused(said),
        ("CONFLICT", "IDEMPOTENCY_OUTCOME_UNKNOWN") => Outcome::Unknown(format!(
            "{said} (a request under this key is in progress or ended unknown)"
        )),
        ("INTERNAL_ERROR" | "TIMEOUT" | "CANCELLED" | "STREAM_ERROR", _) => Outcome::Unknown(said),
        (
            "INVALID_REQUEST"
            | "INVALID_INPUT"
            | "INVALID_PROTOCOL"
            | "UNSUPPORTED_VERSION"
            | "UNKNOWN_CAPABILITY"
            | "UNSUPPORTED_CAPABILITY_VERSION"
            | "NO_COMPATIBLE_MATERIALIZATION"
            | "NOT_FOUND"
            | "CONFLICT"
            | "PAYLOAD_TOO_LARGE"
            | "RATE_LIMITED",
            _,
        ) => Outcome::NotRun(said),
        _ => Outcome::Unknown(format!(
            "the host answered with an unrecognized error code `{code}`"
        )),
    }
}

impl Call {
    pub async fn run(
        &self,
        request: &ExecutionRequest,
        key: Option<&str>,
        descriptor: &CapabilityDescriptor,
    ) -> Result<ExecutionResult, ExecutionError> {
        let input: serde_json::Map<String, Value> = request
            .inputs
            .iter()
            .map(|(k, v)| (k.clone(), to_json(v)))
            .collect();
        let request_id: String = format!("req_{}", request.id.0)
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let mut envelope = json!({
            "protocol": PROTOCOL,
            "type": "request",
            "requestId": request_id,
            "capability": {"name": self.name, "version": self.version},
            "input": input,
            "timeoutMs": self.conn.timeout.as_millis() as u64,
        });
        // A key reaches only a capability the manifest says supports one (and Chip assigned one only
        // because the descriptor said so).
        if let (true, Some(key)) = (self.supports_key, key) {
            envelope["idempotencyKey"] = json!(key);
        }
        let (url, scopes) = match &self.route {
            Route::Endpoint(url) => (url.clone(), None),
            Route::Host { url, scopes } => (url.clone(), Some(scopes.clone())),
        };
        let (conn, body) = (self.conn.clone(), envelope.to_string());
        let label = format!("{}@{}", self.name, self.version);
        let exchanged =
            tokio::task::spawn_blocking(move || exchange(&url, &conn, scopes.as_deref(), &body))
                .await
                .map_err(|e| {
                    ExecutionError::OutcomeUnknown(format!(
                        "{label}: the call did not complete ({e}); its outcome is unknown"
                    ))
                })?;
        let (status, text, unit) = match exchanged {
            Exchange::Unreachable(why) => return Err(ExecutionError::ExecutorUnavailable(why)),
            Exchange::Lost(why) => {
                return Err(ExecutionError::OutcomeUnknown(format!(
                    "{label}: {why}; its outcome is unknown"
                )));
            }
            Exchange::Answered { status, body, unit } => (status, body, unit),
        };
        let line = |outcome: &str, detail: &str| {
            bounded(
                &format!(
                    "{}\n{detail}",
                    head(descriptor.id.as_str(), outcome, json!({"appport": label}))
                ),
                MAX_OBSERVATION_BYTES,
            )
        };
        match classify(status, &text) {
            Outcome::Success(output) => {
                let mut r = ExecutionResult::success(
                    request.id.clone(),
                    line("success", &output.to_string()),
                );
                r.receipt_id = unit;
                Ok(r)
            }
            Outcome::Refused(why) => Ok(ExecutionResult::failure(
                request.id.clone(),
                line("refused", &why),
            )),
            Outcome::NotRun(why) => Ok(ExecutionResult::failure(
                request.id.clone(),
                line("failed", &why),
            )),
            Outcome::Unknown(why) => Err(ExecutionError::OutcomeUnknown(format!(
                "{label}: {why}; its outcome is unknown"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_bare_https_origin_or_a_loopback_http_origin_is_accepted() {
        for ok in [
            "https://app.example.com",
            "https://app.example.com:8443/",
            "http://127.0.0.1:8080",
            "http://localhost:9",
            "http://[::1]:7",
        ] {
            assert!(check_origin(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://app.example.com",
            "http://10.0.0.5:80",
            "https://user:pw@app.example.com",
            "https://app.example.com/path",
            "https://app.example.com?x=1",
            "ftp://app.example.com",
            "app.example.com",
            "https://",
            "https://a b",
        ] {
            assert!(check_origin(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_document_may_only_point_at_same_origin_paths() {
        for ok in ["/appport", "/appport/manifest", "/a/b-c_d"] {
            assert!(path_ok(ok), "{ok}");
        }
        for bad in [
            "appport",
            "//evil.example.com/x",
            "http://evil.example.com/x",
            "/a/../b",
            "/a/./b",
            "/a\\b",
            "/a?x=1",
            "/a#f",
            "/a b",
            "",
        ] {
            assert!(!path_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_flat_schema_maps_and_everything_else_is_refused_not_approximated() {
        let m = map_schema(&json!({
            "type": "object",
            "properties": {
                "title": {"type": "string", "maxLength": 100, "description": "the title"},
                "count": {"type": "integer"},
                "done": {"type": "boolean"}
            },
            "required": ["title"]
        }))
        .unwrap();
        let by: std::collections::BTreeMap<_, _> = m
            .inputs
            .iter()
            .map(|i| (i.0.as_str(), (i.1, i.2)))
            .collect();
        assert_eq!(by["title"], (Kind::Text, true));
        assert_eq!(by["count"], (Kind::Integer, false));
        assert_eq!(by["done"], (Kind::Bool, false));
        assert_eq!(
            m.max_bytes,
            Some(400),
            "maxLength is in characters; bytes are at most 4 each"
        );
        // An empty schema and a bare object take no inputs.
        assert!(map_schema(&json!({})).unwrap().inputs.is_empty());
        assert!(
            map_schema(&json!({"type": "object"}))
                .unwrap()
                .inputs
                .is_empty()
        );
        for (why, schema) in [
            (
                "a number",
                json!({"type":"object","properties":{"x":{"type":"number"}}}),
            ),
            (
                "an array",
                json!({"type":"object","properties":{"x":{"type":"array","items":{"type":"string"}}}}),
            ),
            (
                "a nested object",
                json!({"type":"object","properties":{"x":{"type":"object","properties":{}}}}),
            ),
            ("oneOf", json!({"oneOf":[{"type":"object"}]})),
            (
                "a $ref",
                json!({"type":"object","properties":{"x":{"$ref":"#/y"}}}),
            ),
            ("a non-object", json!({"type":"string"})),
            (
                "an untyped property",
                json!({"type":"object","properties":{"x":{}}}),
            ),
            (
                "an undeclared required",
                json!({"type":"object","properties":{},"required":["x"]}),
            ),
            (
                "a bad name",
                json!({"type":"object","properties":{"a b":{"type":"string"}}}),
            ),
            ("a non-schema", json!("object")),
        ] {
            assert!(map_schema(&schema).is_err(), "{why}");
        }
        let many: serde_json::Map<String, Value> = (0..17)
            .map(|i| (format!("p{i}"), json!({"type":"string"})))
            .collect();
        assert!(
            map_schema(&json!({"type":"object","properties":many})).is_err(),
            "more than 16 inputs"
        );
    }

    fn envelope(ok: bool, code: &str, reason: &str) -> String {
        if ok {
            json!({"protocol":"appport/1","type":"response","requestId":"r","ok":true,"output":{"id":"t1"}}).to_string()
        } else {
            json!({"protocol":"appport/1","type":"response","requestId":"r","ok":false,
                   "error":{"code":code,"message":"m","details":{"reason":reason}}})
            .to_string()
        }
    }

    #[test]
    fn an_answer_is_classified_from_the_hosts_response_alone() {
        assert_eq!(
            classify(200, &envelope(true, "", "")),
            Outcome::Success(json!({"id":"t1"}))
        );
        // Refused: nothing ran.
        assert!(matches!(
            classify(403, &envelope(false, "FORBIDDEN", "")),
            Outcome::Refused(_)
        ));
        assert!(matches!(
            classify(401, &envelope(false, "UNAUTHORIZED", "")),
            Outcome::Refused(_)
        ));
        // The republished idempotency rule: an in-progress or unknown claim is an unknown outcome.
        assert!(matches!(
            classify(
                409,
                &envelope(false, "CONFLICT", "IDEMPOTENCY_OUTCOME_UNKNOWN")
            ),
            Outcome::Unknown(_)
        ));
        // A reused key, or any other conflict, did not run the handler.
        assert!(matches!(
            classify(409, &envelope(false, "CONFLICT", "IDEMPOTENCY_KEY_REUSED")),
            Outcome::NotRun(_)
        ));
        assert!(matches!(
            classify(
                400,
                &envelope(false, "INVALID_REQUEST", "IDEMPOTENCY_KEY_UNSUPPORTED")
            ),
            Outcome::NotRun(_)
        ));
        for code in [
            "INVALID_INPUT",
            "UNKNOWN_CAPABILITY",
            "NOT_FOUND",
            "PAYLOAD_TOO_LARGE",
            "RATE_LIMITED",
        ] {
            assert!(
                matches!(
                    classify(400, &envelope(false, code, "")),
                    Outcome::NotRun(_)
                ),
                "{code}"
            );
        }
        // The handler may have run: unknown, never a clean failure.
        for code in ["INTERNAL_ERROR", "TIMEOUT", "CANCELLED", "STREAM_ERROR"] {
            assert!(
                matches!(
                    classify(500, &envelope(false, code, "")),
                    Outcome::Unknown(_)
                ),
                "{code}"
            );
        }
        assert!(
            matches!(
                classify(200, &envelope(false, "SOMETHING_NEW", "")),
                Outcome::Unknown(_)
            ),
            "an unrecognized code is unknown"
        );
        // No usable envelope: decided by the status, conservatively.
        assert!(matches!(classify(200, "not json"), Outcome::Unknown(_)));
        assert!(
            matches!(classify(200, "{\"ok\":true}"), Outcome::Unknown(_)),
            "not a response envelope"
        );
        assert!(matches!(classify(502, "<html>"), Outcome::Unknown(_)));
        assert!(matches!(classify(403, "denied"), Outcome::Refused(_)));
        assert!(matches!(classify(404, ""), Outcome::NotRun(_)));
    }

    #[test]
    fn a_path_component_is_encoded_so_it_cannot_change_the_route() {
        assert_eq!(encode_component("todo.create@1"), "todo.create%401");
        assert_eq!(encode_component("a/b?c d"), "a%2Fb%3Fc%20d");
        assert_eq!(encode_component("../x"), "..%2Fx");
    }
}
