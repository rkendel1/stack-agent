//! An AppPort capability source for `chip-experiments`, built from what a real AppPort host served.
//!
//! The host stays the authority: it discovers, authorizes and executes, and records evidence. This
//! crate holds no WASM, no registry and no contract of its own. It reads a [`Snapshot`] that
//! `recorder/record.mjs` took from a running host (the published `appport` CLI) and turns it into
//! the pieces Chip already has:
//!
//! - the package's contract is used only when its package identity and contract fingerprint are the
//!   ones the host's discovery document advertises;
//! - each request capability whose required scopes are within the operator's grant becomes a
//!   [`CapabilityDescriptor`] whose inputs and required inputs are read from its `inputSchema`; a
//!   capability outside the grant, or whose schema Chip's input contract cannot express, is not
//!   offered, and the reason is kept;
//! - each call the recorder made with the grant becomes a [`RecordedExecution`], so replay answers
//!   exactly what the host answered, with the host's execution unit or error in the observation;
//! - [`EvidenceCorrelated`] checks after a run that every AppPort observation agrees with the host's
//!   durable records; [`Succeeded`] requires, during the run, that a named capability succeeded.
//!
//! Every capability is [`Confinement::Unconfined`] (the host is another process reached over the
//! network: nothing here confines it) and [`Effect::Consequential`] (the host meters every call in
//! its durable state, and some calls change application state). So the source is offered only in
//! replay, which executes nothing. Calling a live host from inside a run would be new authority and
//! is not implemented.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use chip_core::{
    CapabilityAvailability, CapabilityBackend, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, ExecutionError, ExecutionRequest, ExecutionResult,
    ExecutionStatus, Executor, Observation, ObservationKind, ObservationPredicate,
};
use chip_experiments::{
    CapabilityFixture, CapabilitySource, Confinement, Criterion, CriterionResult, Detection,
    Effect, HostObservation, Mode, RecordedExecution, TaskFixture,
};
use serde::Deserialize;
use serde_json::Value;

pub mod mcp;

pub const SNAPSHOT_SCHEMA: &str = "chip.appport-snapshot.v1";
pub const DISCOVERY_PROTOCOL: &str = "AppPort/host-discovery/1";
/// The project file that names the AppPort application a task expects to use.
pub const BINDING_FILE: &str = "appport.binding.json";
pub const BINDING_SCHEMA: &str = "chip.appport-binding.v1";
/// The prefix of every capability id this source provides: `appport.<selector>.<capability>`.
pub const ID_PREFIX: &str = "appport";
/// UTF-8 needs at most four bytes per character, so a `maxLength` in characters bounds the bytes
/// by four times as much. Chip then never refuses what the host would accept.
const BYTES_PER_CHAR: usize = 4;

// ---- the recorded host -------------------------------------------------------------------------------

/// What a real host served, as `recorder/record.mjs` wrote it. Unknown fields are ignored, as the
/// discovery contract requires of clients.
#[derive(Debug, Clone, Deserialize)]
pub struct Snapshot {
    pub schema: String,
    pub label: String,
    pub appport_cli: String,
    pub package: Package,
    pub grant: Grant,
    pub discovery: Value,
    pub calls: Vec<Call>,
    pub host_state: HostState,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Package {
    pub identity: String,
    pub wasm_sha256: String,
    pub contract_fingerprint: String,
    pub contract: Value,
}

/// The scopes the operator grants the source; it sends exactly these with every call.
#[derive(Debug, Clone, Deserialize)]
pub struct Grant {
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Call {
    pub label: String,
    /// The operator's setup before any run, made with the operator's own scopes. Never replayed.
    pub operator: bool,
    /// `name@version`.
    pub capability: String,
    pub input: BTreeMap<String, Value>,
    pub scopes: Vec<String>,
    pub http_status: u16,
    /// The host's `x-appboundry-execution-unit` header, when it sent one.
    pub execution_unit: Option<String>,
    pub body: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HostState {
    pub records: Vec<Value>,
    pub usage: Vec<Value>,
}

impl Snapshot {
    pub fn from_json(text: &str) -> Result<Self, String> {
        let snapshot: Snapshot = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if snapshot.schema != SNAPSHOT_SCHEMA {
            return Err(format!(
                "snapshot schema is `{}`; expected `{SNAPSHOT_SCHEMA}`",
                snapshot.schema
            ));
        }
        // A client that does not recognise the protocol must connect to nothing.
        let protocol = snapshot.discovery["protocol"].as_str().unwrap_or_default();
        if protocol != DISCOVERY_PROTOCOL {
            return Err(format!(
                "discovery protocol is `{protocol}`; only `{DISCOVERY_PROTOCOL}` is understood"
            ));
        }
        Ok(snapshot)
    }

    /// The discovery record of `selector`, if the host listed it as running.
    fn application(&self, selector: &str) -> Option<&Value> {
        self.discovery["applications"]
            .as_array()?
            .iter()
            .find(|a| a["selector"] == selector && a["status"] == "running")
    }
}

// ---- contract → descriptor -------------------------------------------------------------------------------

/// One host capability Chip can offer.
#[derive(Debug, Clone)]
pub struct Offered {
    pub descriptor: CapabilityDescriptor,
    /// `name@version` as the host names it.
    pub capability: String,
    pub scopes: Vec<String>,
}

/// A capability the host provides that is not offered, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotOffered {
    pub capability: String,
    pub reason: String,
}

/// Selectors are one safe path segment (the discovery contract). Chip ids are lower case, so a
/// selector with upper case is refused, never folded into another name.
pub(crate) fn valid_selector(selector: &str) -> bool {
    !selector.is_empty()
        && selector.len() <= 48
        && selector
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        && selector
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
}

pub(crate) fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_string()
}

pub(crate) fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The inputs a capability's JSON input schema declares, as Chip inputs, and the most bytes a
/// request may carry when the schema bounds its text. Chip's inputs are text, integers and booleans:
/// a schema with any other input cannot be requested faithfully, so the capability is refused
/// rather than offered with a narrowed meaning. With `typed`, each input's description names its
/// JSON type, which is then the only place the model can learn it.
pub(crate) fn schema_inputs(
    schema: &Value,
    typed: bool,
) -> Result<(Vec<CapabilityInput>, Option<usize>), String> {
    if schema["type"] != "object" {
        return Err("the input schema is not an object".into());
    }
    let required = strings(&schema["required"]);
    let mut inputs = Vec::new();
    let mut max_chars: Option<usize> = None;
    for (input, property) in schema["properties"].as_object().into_iter().flatten() {
        let kind = property["type"].as_str();
        match kind {
            Some("string") => {
                if let Some(n) = property["maxLength"].as_u64() {
                    max_chars = Some(max_chars.map_or(n as usize, |m| m.max(n as usize)));
                }
            }
            Some("integer" | "boolean") => {}
            other => {
                return Err(format!(
                    "input `{input}` has type {other:?}; Chip inputs are text, integers and booleans"
                ));
            }
        }
        let description = text(property, "description");
        let description = match (typed, kind) {
            (true, Some(kind)) if description.is_empty() => format!("JSON {kind}"),
            (true, Some(kind)) => format!("{description} (JSON {kind})"),
            _ => description,
        };
        inputs.push(CapabilityInput {
            name: input.clone(),
            description,
            required: required.contains(input),
        });
    }
    Ok((inputs, max_chars.map(|c| c * BYTES_PER_CHAR)))
}

fn map_capability(
    selector: &str,
    provided: &Value,
    application: &Value,
    grant: &[String],
    package: &str,
) -> Result<Offered, NotOffered> {
    let name = text(provided, "name");
    let capability = format!("{name}@{}", provided["version"]);
    let refuse = |reason: String| NotOffered {
        capability: capability.clone(),
        reason,
    };
    if provided["kind"] != "request" {
        return Err(refuse(format!(
            "kind {} is not a request",
            provided["kind"]
        )));
    }
    if !strings(&application["capabilities"]).contains(&capability) {
        return Err(refuse("the host does not advertise it".into()));
    }
    let scopes = strings(&application["authorization"][&capability]);
    let missing: Vec<&String> = scopes.iter().filter(|s| !grant.contains(s)).collect();
    if !missing.is_empty() {
        return Err(refuse(format!(
            "it requires {missing:?}, which the grant does not hold"
        )));
    }
    let id = format!("{ID_PREFIX}.{selector}.{name}");
    let id = CapabilityId::new(id.clone())
        .map_err(|_| refuse(format!("`{id}` is not a Chip capability id")))?;
    let (inputs, max_bytes) = schema_inputs(&provided["inputSchema"], false).map_err(refuse)?;
    let mut descriptor = CapabilityDescriptor::new(
        id,
        name.clone(),
        format!(
            "{} (AppPort capability {capability}, package {package}; executed by the host, which authorizes it and records evidence)",
            text(provided, "description")
        ),
    )
    .with_effect(Effect::Consequential);
    descriptor.version = Some(capability.clone());
    descriptor.inputs = inputs;
    if let Some(bytes) = max_bytes {
        descriptor = descriptor.with_max_input_bytes(bytes);
    }
    Ok(Offered {
        descriptor,
        capability,
        scopes,
    })
}

// ---- the source ----------------------------------------------------------------------------------------

/// The capabilities one AppPort application offers under the operator's grant, as a Chip
/// [`CapabilitySource`]. Built once, by the host composing an experiment.
pub struct AppPortSource {
    id: &'static str,
    snapshot: Arc<Snapshot>,
    selector: String,
    artifact_hash: String,
    offered: Vec<Offered>,
    not_offered: Vec<NotOffered>,
    /// The offered ids, leaked once: the fixture contract names capabilities statically.
    provides: Vec<&'static str>,
}

impl AppPortSource {
    /// The source for `selector` in `snapshot`. Refused when the host did not list the application
    /// as running, or the recorded contract is not the one it serves.
    pub fn new(snapshot: Snapshot, selector: &str) -> Result<Self, String> {
        if !valid_selector(selector) {
            return Err(format!(
                "selector `{selector}` cannot name a Chip capability"
            ));
        }
        let application = snapshot
            .application(selector)
            .ok_or_else(|| format!("the host did not list `{selector}` as running"))?;
        for (field, recorded) in [
            ("packageIdentity", &snapshot.package.identity),
            (
                "contractFingerprint",
                &snapshot.package.contract_fingerprint,
            ),
        ] {
            if application[field].as_str() != Some(recorded.as_str()) {
                return Err(format!(
                    "the recorded contract is not the one the host serves: discovery {field} is {}, the package's is {recorded}",
                    application[field]
                ));
            }
        }
        let artifact_hash = text(application, "artifactHash");
        if artifact_hash != snapshot.package.wasm_sha256 {
            return Err(format!(
                "the host serves artifact {artifact_hash}; the package holds {}",
                snapshot.package.wasm_sha256
            ));
        }
        let mut offered = Vec::new();
        let mut not_offered = Vec::new();
        for provided in snapshot.package.contract["provides"]
            .as_array()
            .into_iter()
            .flatten()
        {
            match map_capability(
                selector,
                provided,
                application,
                &snapshot.grant.scopes,
                &snapshot.package.identity,
            ) {
                Ok(o) => offered.push(o),
                Err(n) => not_offered.push(n),
            }
        }
        let id: &'static str = Box::leak(format!("{ID_PREFIX}.{selector}").into_boxed_str());
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
            artifact_hash,
            offered,
            not_offered,
            provides,
        })
    }

    pub fn offered(&self) -> &[Offered] {
        &self.offered
    }

    pub fn not_offered(&self) -> &[NotOffered] {
        &self.not_offered
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Every call the recorder made with the grant, of an offered capability, as replay serves it:
    /// the host's answer and nothing substituted. The operator's setup calls and calls of
    /// capabilities that are not offered are left out: Chip can never request them.
    pub fn recorded_executions(&self) -> Vec<RecordedExecution> {
        self.snapshot
            .calls
            .iter()
            .filter(|call| !call.operator)
            .filter_map(|call| {
                let offered = self
                    .offered
                    .iter()
                    .find(|o| o.capability == call.capability)?;
                Some(RecordedExecution {
                    capability: offered.descriptor.id.as_str().to_string(),
                    inputs: call.input.clone(),
                    succeeded: succeeded(call),
                    output: self.render(offered, call),
                })
            })
            .collect()
    }

    /// The recorded call with `label`, for tests that check what the host returned.
    pub fn call(&self, label: &str) -> Option<&Call> {
        self.snapshot.calls.iter().find(|c| c.label == label)
    }

    /// The observation text: a JSON first line (the convention of Chip's project capabilities)
    /// naming the capability, the request, the host's classification and identity, then the result.
    fn render(&self, offered: &Offered, call: &Call) -> String {
        let ok = succeeded(call);
        let mut head = serde_json::json!({
            "capability": offered.descriptor.id.as_str(),
            "appport_capability": offered.capability,
            "request": call.input,
            "outcome": if ok { "success" } else { "failure" },
            "http_status": call.http_status,
            "execution_unit": call.execution_unit,
            "package_identity": self.snapshot.package.identity,
            "artifact_hash": self.artifact_hash,
            "contract_fingerprint": self.snapshot.package.contract_fingerprint,
        });
        if !ok {
            head["error"] = call.body["error"].clone();
        }
        let body = if ok {
            serde_json::to_string(&call.body["output"]).expect("JSON serializes")
        } else {
            serde_json::to_string(&call.body).expect("JSON serializes")
        };
        format!("{head}\n--- result ---\n{body}")
    }
}

fn succeeded(call: &Call) -> bool {
    call.http_status == 200 && call.body["ok"] == true && !call.body["output"].is_null()
}

impl CapabilitySource for AppPortSource {
    fn id(&self) -> &'static str {
        self.id
    }

    fn version(&self) -> u32 {
        1
    }

    fn fixture(&self) -> Arc<dyn CapabilityFixture> {
        Arc::new(AppPortFixture {
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

    /// Confirmed only when the project names this application and the package the host served is
    /// the one it names, and the grant offers at least one capability.
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
        if binding["schema"] != BINDING_SCHEMA || binding["selector"] != self.selector.as_str() {
            return Detection::NotDetected {
                evidence: vec![format!(
                    "{BINDING_FILE} does not name the {BINDING_SCHEMA} selector `{}`",
                    self.selector
                )],
            };
        }
        let wanted = text(&binding, "packageIdentity");
        let served = &self.snapshot.package.identity;
        if &wanted != served {
            return Detection::Inconclusive {
                reason: format!(
                    "the project expects package {wanted}; the host serves {served} under `{}`",
                    self.selector
                ),
            };
        }
        if self.offered.is_empty() {
            return Detection::Inconclusive {
                reason: format!(
                    "the host serves `{}` but the grant {:?} offers no capability Chip can request",
                    self.selector, self.snapshot.grant.scopes
                ),
            };
        }
        Detection::Confirmed {
            evidence: vec![
                format!(
                    "discovery ({DISCOVERY_PROTOCOL}) lists `{}` running with package {served}",
                    self.selector
                ),
                format!(
                    "the grant {:?} offers {} of the package's {} capabilities",
                    self.snapshot.grant.scopes,
                    self.offered.len(),
                    self.offered.len() + self.not_offered.len()
                ),
            ],
        }
    }
}

struct AppPortFixture {
    id: &'static str,
    provides: Vec<&'static str>,
    descriptors: Vec<CapabilityDescriptor>,
}

impl CapabilityFixture for AppPortFixture {
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

    fn backend(&self, _root: &Path) -> Arc<dyn CapabilityBackend> {
        Arc::new(DeclarationsOnly(self.descriptors.clone()))
    }
}

/// Declares the host's capabilities and executes nothing. Replay never calls it (it answers from
/// the recording), and resolution keeps an unconfined capability out of every other mode.
pub(crate) struct DeclarationsOnly(pub(crate) Vec<CapabilityDescriptor>);

#[async_trait::async_trait]
impl CapabilityProvider for DeclarationsOnly {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(self.0.clone())
    }

    async fn availability(&self, id: &CapabilityId) -> CapabilityAvailability {
        if self.0.iter().any(|d| &d.id == id) {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable("not granted by the recorded host".into())
        }
    }
}

#[async_trait::async_trait]
impl Executor for DeclarationsOnly {
    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        Err(ExecutionError::ExecutionFailed(format!(
            "{} would call a live AppPort host; this source only replays what a host served",
            request.intent
        )))
    }
}

// ---- observations ------------------------------------------------------------------------------------

/// The JSON first line of an AppPort observation (either transport), if `observation` is one.
pub(crate) fn appport_head(observation: &Observation) -> Option<Value> {
    if observation.kind == ObservationKind::ExecutionCancelled {
        return None;
    }
    let first = observation.output.as_deref()?.lines().next()?;
    let head: Value = serde_json::from_str(first).ok()?;
    text(&head, "capability")
        .starts_with(&format!("{ID_PREFIX}."))
        .then_some(head)
}

/// "`capability` succeeded at least once." Enforced in the loop, so a completion claimed without it
/// is refused.
pub struct Succeeded {
    capability: &'static str,
}

impl Succeeded {
    pub fn new(capability: &str) -> Self {
        Self {
            capability: Box::leak(capability.to_string().into_boxed_str()),
        }
    }
}

#[derive(Debug)]
struct SucceededPredicate(&'static str);

impl ObservationPredicate for SucceededPredicate {
    fn describe(&self) -> String {
        format!("{} succeeded", self.0)
    }

    fn satisfied_by(&self, observation: &Observation) -> bool {
        observation.status == ExecutionStatus::Success
            && appport_head(observation)
                .is_some_and(|h| h["capability"] == self.0 && h["outcome"] == "success")
    }
}

impl Criterion for Succeeded {
    fn id(&self) -> &'static str {
        "appport.succeeded"
    }

    fn describe(&self) -> String {
        format!("{} succeeded at least once", self.capability)
    }

    fn supports(&self, _mode: Mode) -> bool {
        true
    }

    fn in_loop(&self, _task: &TaskFixture) -> Option<Arc<dyn ObservationPredicate>> {
        Some(Arc::new(SucceededPredicate(self.capability)))
    }

    fn evaluate(&self, host: &HostObservation<'_>) -> CriterionResult {
        if host
            .report
            .observations
            .iter()
            .any(|o| SucceededPredicate(self.capability).satisfied_by(o))
        {
            CriterionResult::Met
        } else {
            CriterionResult::NotMet(format!("no successful {} observation", self.capability))
        }
    }
}

// ---- evidence ----------------------------------------------------------------------------------------

/// "Every AppPort observation agrees with the host's durable records." A success must name an
/// execution unit whose invocation record is completed, for the same capability, input, artifact and
/// contract, with a success usage record. A failure must have no completed invocation record for its
/// request; when the host named an execution unit, that unit's usage record must be a failure. The
/// published host names no unit on a failed call, so a failure is then checked by absence only, and
/// the host's failure usage records that no response names are what remain to account for it.
pub struct EvidenceCorrelated {
    source: Arc<AppPortSource>,
}

impl EvidenceCorrelated {
    pub fn new(source: Arc<AppPortSource>) -> Self {
        Self { source }
    }

    /// Checks one observation's first line against the host's records.
    pub fn check(&self, head: &Value, succeeded: bool) -> Result<(), String> {
        let id = text(head, "capability");
        let offered = self
            .source
            .offered
            .iter()
            .find(|o| o.descriptor.id.as_str() == id)
            .ok_or_else(|| format!("{id} is not a capability this source offered"))?;
        for (field, declared) in [
            ("package_identity", &self.source.snapshot.package.identity),
            ("artifact_hash", &self.source.artifact_hash),
            (
                "contract_fingerprint",
                &self.source.snapshot.package.contract_fingerprint,
            ),
        ] {
            if head[field].as_str() != Some(declared.as_str()) {
                return Err(format!(
                    "{id}: {field} {} differs from the host's {declared}",
                    head[field]
                ));
            }
        }
        let (name, version) = offered.capability.split_once('@').expect("name@version");
        let state = &self.source.snapshot.host_state;
        let same_call = |r: &&Value| {
            r["capability"]["name"] == name
                && r["capability"]["version"].to_string() == version
                && r["input"] == head["request"]
        };
        let unit = head["execution_unit"].as_str();
        let usage = |u: &str| state.usage.iter().find(|r| r["executionId"] == u);
        if succeeded {
            let unit = unit.ok_or_else(|| format!("{id}: a success names no execution unit"))?;
            let record = state
                .records
                .iter()
                .find(|r| r["executionUnitId"] == unit)
                .ok_or_else(|| format!("{id}: no invocation record for {unit}"))?;
            if record["status"] != "completed" || !same_call(&record) {
                return Err(format!(
                    "{id}: the invocation record for {unit} is not this completed call"
                ));
            }
            if record["artifactHash"].as_str() != Some(self.source.artifact_hash.as_str())
                || record["contractFingerprint"].as_str()
                    != Some(self.source.snapshot.package.contract_fingerprint.as_str())
            {
                return Err(format!(
                    "{id}: the invocation record for {unit} names another artifact or contract"
                ));
            }
            match usage(unit) {
                Some(u) if u["status"] == "success" => Ok(()),
                _ => Err(format!("{id}: no success usage record for {unit}")),
            }
        } else {
            if state
                .records
                .iter()
                .any(|r| r["status"] == "completed" && same_call(&r))
            {
                return Err(format!(
                    "{id}: the observation says failure but the host completed this call"
                ));
            }
            match unit {
                Some(unit) => match usage(unit) {
                    Some(u) if u["status"] == "failure" => Ok(()),
                    _ => Err(format!("{id}: no failure usage record for {unit}")),
                },
                None => Ok(()),
            }
        }
    }
}

impl Criterion for EvidenceCorrelated {
    fn id(&self) -> &'static str {
        "appport.evidence-correlated"
    }

    fn describe(&self) -> String {
        "every AppPort observation agrees with the host's durable invocation and usage records"
            .into()
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
            // MCP observations have no durable host records; `mcp::ApplicationAgrees` checks them.
            if head["transport"] == "mcp" {
                continue;
            }
            let succeeded = observation.status == ExecutionStatus::Success;
            if let Err(why) = self.check(&head, succeeded) {
                return CriterionResult::NotMet(why);
            }
            checked += 1;
        }
        if checked == 0 {
            CriterionResult::Unavailable("no AppPort capability was executed".into())
        } else {
            CriterionResult::Met
        }
    }
}
