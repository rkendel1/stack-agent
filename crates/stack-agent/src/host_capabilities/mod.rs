//! Capabilities a **host** supplies to a work, in addition to (or instead of) the built-in project set.
//!
//! The model never supplies a capability, an input schema, an effect or a binding: an operator does,
//! in a file (`--capabilities FILE`, schema `chip.host-capabilities.v1`) or by pointing at an AppPort
//! host (`--appport ORIGIN`). Chip validates every request against what the host declared, builds the
//! execution itself, and the capability's backend performs it. This module only turns a host's
//! declarations into the pieces Chip already has: [`CapabilityDescriptor`]s and an [`Executor`].
//!
//! ## Discovered is not authorized
//!
//! An AppPort host says what it *serves*. That grants nothing. A discovered capability is offered to
//! the model only if the operator **authorized** it (`authorize`: its name, or `*`) and the scopes it
//! declares are within the operator's `grant`. Everything else is *withheld*: it is not in the
//! capabilities the model sees, it cannot be requested, and each withheld capability is recorded as a
//! `CapabilityWithheld` event with the reason. A capability the host serves but whose schema Chip's
//! input contract cannot express is withheld too, never approximated.
//!
//! ## What a host declares, and what Chip does with it
//!
//! - **effect** (`observation` | `consequential`; absent = `unknown`, never treated as an observation);
//! - **idempotency**: Chip assigns and records an idempotency key only for a capability declared
//!   `deduplicated` (AppPort: `"idempotency": "supported"`), and this backend sends a key only to such
//!   a capability. A capability without it never receives one, so a host that rejects keys on
//!   unsupported capabilities is never sent one;
//! - **inputs**: strings, integers and booleans, with the host's `required` and size limits.
//!
//! ## Outcomes are not confused
//!
//! Nothing sent: [`ExecutionError::ExecutorUnavailable`]. Sent and refused (HTTP 401/403, or an
//! `UNAUTHORIZED`/`FORBIDDEN` answer): a failed observation saying `refused`. Sent and the host says
//! it did not run it (validation, not found, rate limit, a conflict that is not an unknown outcome): a
//! failed observation. Sent and the outcome is not known (no complete answer, a 5xx, a timeout, a
//! cancellation, `IDEMPOTENCY_OUTCOME_UNKNOWN`, an answer that is not an AppPort response):
//! [`ExecutionError::OutcomeUnknown`]; durable state keeps that intent unresolved. **Nothing here
//! retries.**
//!
//! Every string a host supplies (descriptions, results) is untrusted data: it is bounded, stripped of
//! control characters, and shown to the model as an observation, never as an instruction.

pub mod appport;
pub mod environment;
pub mod exec;

pub use environment::HostEnvironmentProvider;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chip_core::{
    CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId, CapabilityInput,
    CapabilityProvider, Effect, ExecutionError, ExecutionRequest, ExecutionResult, Executor,
    Idempotency, InputValue, Withheld,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub const SCHEMA: &str = "chip.host-capabilities.v1";
/// Ids a host may not use: they belong to the built-in project capabilities.
pub const RESERVED_PREFIXES: [&str; 2] = ["project.", "pax."];
const MAX_FILE_BYTES: u64 = 1 << 20;
const MAX_CAPABILITIES: usize = 128;
const MAX_INPUTS: usize = 16;
const MAX_NAME: usize = 64;
const MAX_DESCRIPTION: usize = 200;
/// The longest an observation from a host capability is kept, in bytes.
pub const MAX_OBSERVATION_BYTES: usize = 32 * 1024;

// ---- what the operator wrote ------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostFile {
    pub schema: String,
    #[serde(default)]
    pub capabilities: Vec<Declared>,
    #[serde(default)]
    pub appport: Vec<AppPortSource>,
}

/// One capability the operator declares and binds to a command.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declared {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub inputs: Vec<DeclaredInput>,
    #[serde(default)]
    pub effect: Option<String>,
    #[serde(default)]
    pub idempotency: Option<String>,
    #[serde(default)]
    pub max_input_bytes: Option<usize>,
    pub binding: exec::ExecDecl,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclaredInput {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    /// `string` (default), `integer` or `boolean`.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

/// An AppPort host the operator points the agent at.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppPortSource {
    /// `https://host[:port]`, or `http://` only for a loopback address. No path, query or userinfo.
    pub origin: String,
    /// A desktop host serves several applications: which one. Absent = a single-application server.
    #[serde(default)]
    pub application: Option<String>,
    /// A reviewed manifest to use instead of fetching one: a local file path, or a URL on `origin`.
    /// Required for a desktop host, whose discovery document carries no input schemas.
    #[serde(default)]
    pub manifest: Option<String>,
    /// The environment variable that holds the bearer token. Never read from anywhere else.
    #[serde(default)]
    pub token_env: Option<String>,
    /// The scopes the operator grants this agent. A capability needing others is withheld.
    #[serde(default)]
    pub grant: Vec<String>,
    /// The capability names the operator authorizes, or `["*"]` for every one within the grant.
    /// Empty means none: discovery alone authorizes nothing.
    #[serde(default)]
    pub authorize: Vec<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// What the operator asked for, from flags and the file.
#[derive(Debug, Clone, Default)]
pub struct HostSpec {
    pub file: Option<PathBuf>,
    pub appport: Vec<AppPortSource>,
    /// Keep the built-in project capabilities alongside these. `false` = only these.
    pub keep_builtin: bool,
    /// `--require-success`: each must have succeeded for an `act` work to be verified.
    pub require_success: Vec<String>,
}

impl HostSpec {
    pub fn is_empty(&self) -> bool {
        self.file.is_none() && self.appport.is_empty()
    }
}

// ---- what Chip holds ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Integer,
    Bool,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Text => "string",
            Kind::Integer => "integer",
            Kind::Bool => "boolean",
        }
    }
}

pub(crate) enum Binding {
    Exec(exec::ExecBinding),
    AppPort(appport::Call),
}

pub(crate) struct Entry {
    pub descriptor: CapabilityDescriptor,
    pub kinds: BTreeMap<String, Kind>,
    pub binding: Binding,
    pub source: String,
}

/// Everything the operator supplied, resolved: what is offered, and what was withheld and why.
pub struct HostCapabilities {
    pub(crate) entries: Vec<Entry>,
    pub(crate) withheld: Vec<(String, Withheld)>,
    pub keep_builtin: bool,
    pub required_success: Vec<String>,
    sources: Vec<Value>,
}

impl HostCapabilities {
    /// Reads the operator's file and discovers each AppPort source. Blocks on the network; call it
    /// from a blocking context. `env` is where a token variable is read from, and only there.
    pub fn load(
        spec: &HostSpec,
        project_root: &Path,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let mut file = HostFile {
            schema: SCHEMA.into(),
            capabilities: Vec::new(),
            appport: Vec::new(),
        };
        if let Some(path) = &spec.file {
            file = read_file(path)?;
        }
        file.appport.extend(spec.appport.iter().cloned());

        let mut out = Self {
            entries: Vec::new(),
            withheld: Vec::new(),
            keep_builtin: spec.keep_builtin,
            required_success: spec.require_success.clone(),
            sources: Vec::new(),
        };
        if file.capabilities.len() > MAX_CAPABILITIES {
            return Err(format!(
                "the capability file declares {} capabilities; the most accepted is {MAX_CAPABILITIES}",
                file.capabilities.len()
            ));
        }
        let label = spec
            .file
            .as_ref()
            .map_or("flags".to_string(), |p| p.display().to_string());
        for declared in &file.capabilities {
            let entry = declared_entry(declared, project_root, &label)?;
            out.add(entry)?;
        }
        if !file.capabilities.is_empty() {
            out.sources.push(json!({
                "kind": "declared",
                "file": label,
                "offered": out.entries.iter().map(|e| e.descriptor.id.to_string()).collect::<Vec<_>>(),
            }));
        }
        for source in &file.appport {
            let discovered = appport::discover(source, env)?;
            out.sources.push(discovered.report.clone());
            for entry in discovered.offered {
                out.add(entry)?;
            }
            out.withheld.extend(
                discovered
                    .withheld
                    .into_iter()
                    .map(|w| (source.origin.clone(), w)),
            );
        }
        if out.entries.is_empty() && out.withheld.is_empty() {
            return Err("the host supplied no capabilities".into());
        }
        Ok(out)
    }

    fn add(&mut self, entry: Entry) -> Result<(), String> {
        let id = entry.descriptor.id.as_str();
        if RESERVED_PREFIXES.iter().any(|p| id.starts_with(p)) {
            return Err(format!(
                "capability `{id}` ({}) uses an id reserved for the built-in project capabilities",
                entry.source
            ));
        }
        if let Some(other) = self
            .entries
            .iter()
            .find(|e| e.descriptor.id == entry.descriptor.id)
        {
            return Err(format!(
                "capability `{id}` is supplied twice ({} and {})",
                other.source, entry.source
            ));
        }
        self.entries.push(entry);
        Ok(())
    }

    pub fn offered_ids(&self) -> Vec<CapabilityId> {
        self.entries
            .iter()
            .map(|e| e.descriptor.id.clone())
            .collect()
    }

    /// The offered capabilities that declare themselves read-only observations: the only ones an
    /// inspection may use.
    pub fn observation_ids(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|e| e.descriptor.effect == Effect::Observation)
            .map(|e| e.descriptor.id.to_string())
            .collect()
    }

    pub fn withheld_list(&self) -> Vec<Withheld> {
        self.withheld.iter().map(|(_, w)| w.clone()).collect()
    }

    /// For the work's report. Contains no token, no URL path and no host response.
    pub fn report(&self) -> Value {
        json!({
            "schema": SCHEMA,
            "keep_builtin": self.keep_builtin,
            "sources": self.sources,
            "offered": self.entries.iter().map(|e| json!({
                "id": e.descriptor.id.to_string(),
                "source": e.source,
                "effect": e.descriptor.effect.name(),
                "idempotency": e.descriptor.idempotency.name(),
            })).collect::<Vec<_>>(),
            "withheld": self.withheld.iter().map(|(origin, w)| json!({
                "capability": w.capability, "reason": w.reason, "source": origin,
            })).collect::<Vec<_>>(),
        })
    }
}

fn read_file(path: &Path) -> Result<HostFile, String> {
    let meta = std::fs::metadata(path)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e.kind()))?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return Err(format!(
            "{} must be a regular file of at most {MAX_FILE_BYTES} bytes",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e.kind()))?;
    let file: HostFile = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not a valid capability file: {e}", path.display()))?;
    if file.schema != SCHEMA {
        return Err(format!(
            "{} has schema `{}`; expected `{SCHEMA}`",
            path.display(),
            file.schema
        ));
    }
    Ok(file)
}

// ---- shared helpers ----------------------------------------------------------------------------------

pub(crate) fn valid_input_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// A host-supplied description as the model may see it: one line, no control characters, bounded.
pub(crate) fn clean_text(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

pub(crate) fn parse_effect(name: Option<&str>) -> Result<Effect, String> {
    match name {
        None | Some("unknown") => Ok(Effect::Unknown),
        Some("observation") => Ok(Effect::Observation),
        Some("consequential") => Ok(Effect::Consequential),
        Some(other) => Err(format!(
            "effect `{other}` is not one of observation, consequential, unknown"
        )),
    }
}

fn parse_idempotency(name: Option<&str>) -> Result<Idempotency, String> {
    match name {
        None | Some("none") => Ok(Idempotency::NotCarried),
        Some("carried") => Ok(Idempotency::Carried),
        Some("deduplicated") => Ok(Idempotency::Deduplicated),
        Some(other) => Err(format!(
            "idempotency `{other}` is not one of none, carried, deduplicated"
        )),
    }
}

fn parse_kind(name: Option<&str>) -> Result<Kind, String> {
    match name {
        None | Some("string") => Ok(Kind::Text),
        Some("integer") => Ok(Kind::Integer),
        Some("boolean") => Ok(Kind::Bool),
        Some(other) => Err(format!(
            "input type `{other}` is not one of string, integer, boolean"
        )),
    }
}

fn declared_entry(d: &Declared, project_root: &Path, label: &str) -> Result<Entry, String> {
    let at = |m: String| format!("capability `{}` in {label}: {m}", d.id);
    let id = CapabilityId::new(d.id.clone()).map_err(|_| {
        at("not a valid capability id (lowercase letters, digits, `.`, `_`, `-`)".into())
    })?;
    if d.inputs.len() > MAX_INPUTS {
        return Err(at(format!("declares more than {MAX_INPUTS} inputs")));
    }
    let mut descriptor = CapabilityDescriptor::new(
        id,
        d.id.clone(),
        clean_text(&d.description, MAX_DESCRIPTION),
    )
    .with_effect(parse_effect(d.effect.as_deref()).map_err(&at)?)
    .with_idempotency(parse_idempotency(d.idempotency.as_deref()).map_err(&at)?);
    if let Some(bytes) = d.max_input_bytes {
        descriptor = descriptor.with_max_input_bytes(bytes);
    }
    let mut kinds = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for input in &d.inputs {
        if !valid_input_name(&input.name) || !seen.insert(input.name.clone()) {
            return Err(at(format!(
                "input name `{}` is invalid or repeated",
                input.name
            )));
        }
        kinds.insert(
            input.name.clone(),
            parse_kind(input.kind.as_deref()).map_err(&at)?,
        );
        descriptor.inputs.push(CapabilityInput {
            name: input.name.clone(),
            description: clean_text(&input.description, MAX_DESCRIPTION),
            required: input.required,
        });
    }
    let required: BTreeSet<&str> = d
        .inputs
        .iter()
        .filter(|i| i.required)
        .map(|i| i.name.as_str())
        .collect();
    let binding = exec::ExecBinding::from_decl(&d.binding, &required, project_root).map_err(&at)?;
    Ok(Entry {
        descriptor,
        kinds,
        binding: Binding::Exec(binding),
        source: format!("declared:{label}"),
    })
}

pub(crate) fn to_json(value: &InputValue) -> Value {
    match value {
        InputValue::Text(s) => Value::String(s.clone()),
        InputValue::Integer(n) => json!(n),
        InputValue::Bool(b) => Value::Bool(*b),
    }
}

/// The first line of every host observation, in the convention the project capabilities and the
/// AppPort source use: one JSON object naming the capability and how it ended. `source: "host"` marks
/// an operator-supplied capability. Chip's predicates read this line and nothing the model wrote.
pub(crate) fn head(capability: &str, outcome: &str, extra: Value) -> String {
    let mut head = json!({"source": "host", "capability": capability, "outcome": outcome});
    if let (Some(h), Some(e)) = (head.as_object_mut(), extra.as_object()) {
        h.extend(e.clone());
    }
    head.to_string()
}

/// "The operator's capabilities that were required succeeded", read from the recorded observations.
///
/// With no names, at least one operator-supplied capability must have succeeded; with names, every
/// one must. An observation counts only if its execution succeeded **and** its head line (written by
/// the backend, not the model) says `outcome: success`. A model's `complete` is accepted only when
/// this holds, so a completion claimed in prose, or after only failures and refusals, is refused.
#[derive(Debug, Clone, Default)]
pub struct ActionsSucceeded {
    pub required: Vec<String>,
}

impl ActionsSucceeded {
    fn succeeded(observations: &[chip_core::Observation]) -> BTreeSet<String> {
        observations
            .iter()
            .filter(|o| o.status == chip_core::ExecutionStatus::Success)
            .filter_map(|o| {
                let first = o.output.as_deref()?.lines().next()?;
                let head: Value = serde_json::from_str(first).ok()?;
                (head["source"] == "host" && head["outcome"] == "success")
                    .then(|| head["capability"].as_str().map(str::to_string))
                    .flatten()
            })
            .collect()
    }

    /// Whether the requirement holds for `observations`.
    pub fn holds(&self, observations: &[chip_core::Observation]) -> bool {
        let done = Self::succeeded(observations);
        if self.required.is_empty() {
            !done.is_empty()
        } else {
            self.required.iter().all(|r| done.contains(r))
        }
    }
}

impl chip_core::ObservationPredicate for ActionsSucceeded {
    fn describe(&self) -> String {
        if self.required.is_empty() {
            "at least one capability the operator supplied succeeded".to_string()
        } else {
            format!(
                "these operator-supplied capabilities each succeeded: {}",
                self.required.join(", ")
            )
        }
    }

    fn satisfied_by(&self, _observation: &chip_core::Observation) -> bool {
        false
    }

    fn satisfied_by_trajectory(&self, observations: &[chip_core::Observation]) -> bool {
        self.holds(observations)
    }
}

/// Bounded, control-free text for an observation, with a note when it was cut.
pub(crate) fn bounded(text: &str, max: usize) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    if cleaned.len() <= max {
        return cleaned;
    }
    let mut end = max;
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[cut: {} of {} bytes shown]",
        &cleaned[..end],
        end,
        cleaned.len()
    )
}

// ---- the backend --------------------------------------------------------------------------------------

/// The capability backend over a resolved [`HostCapabilities`].
pub struct HostBackend {
    host: Arc<HostCapabilities>,
}

impl HostBackend {
    pub fn new(host: Arc<HostCapabilities>) -> Self {
        Self { host }
    }

    fn entry(&self, id: &str) -> Option<&Entry> {
        self.host
            .entries
            .iter()
            .find(|e| e.descriptor.id.as_str() == id)
    }
}

#[async_trait::async_trait]
impl CapabilityProvider for HostBackend {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(self
            .host
            .entries
            .iter()
            .map(|e| e.descriptor.clone())
            .collect())
    }

    async fn availability(&self, id: &CapabilityId) -> CapabilityAvailability {
        match self.entry(id.as_str()) {
            Some(_) => CapabilityAvailability::Available,
            None => {
                CapabilityAvailability::Unavailable(format!("{id} is not supplied by the host"))
            }
        }
    }

    fn withheld(&self) -> Vec<Withheld> {
        self.host.withheld_list()
    }

    /// The host's declared types: a value of another type is refused here, never coerced.
    async fn validate_inputs(
        &self,
        id: &CapabilityId,
        inputs: &BTreeMap<String, InputValue>,
    ) -> Result<(), CapabilityError> {
        let entry = self
            .entry(id.as_str())
            .ok_or_else(|| CapabilityError::Unknown(id.to_string()))?;
        for (name, value) in inputs {
            let expected = entry.kinds.get(name).copied().unwrap_or(Kind::Text);
            let got = match value {
                InputValue::Text(_) => Kind::Text,
                InputValue::Integer(_) => Kind::Integer,
                InputValue::Bool(_) => Kind::Bool,
            };
            if got != expected {
                return Err(CapabilityError::InvalidInput(format!(
                    "input `{name}` must be a {}, not a {}",
                    expected.name(),
                    got.name()
                )));
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl Executor for HostBackend {
    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let Some(entry) = self.entry(&request.intent) else {
            return Err(ExecutionError::InvalidRequest(format!(
                "{} is not supplied by the host",
                request.intent
            )));
        };
        // A key goes only to a capability that declared it can use one.
        let key = match entry.descriptor.idempotency {
            Idempotency::NotCarried => None,
            _ => request.idempotency_key.clone(),
        };
        match &entry.binding {
            Binding::Exec(binding) => {
                binding
                    .run(&request, key.as_deref(), &entry.descriptor)
                    .await
            }
            Binding::AppPort(call) => call.run(&request, key.as_deref(), &entry.descriptor).await,
        }
    }
}

/// How long a source may take to answer, by default.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const MAX_TIMEOUT_SECS: u64 = 600;

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, text: &str) -> PathBuf {
        let path = dir.join("caps.json");
        std::fs::write(&path, text).unwrap();
        path
    }

    fn load(text: &str) -> Result<HostCapabilities, String> {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("hostcaps-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let spec = HostSpec {
            file: Some(write(&dir, text)),
            appport: vec![],
            keep_builtin: true,
            ..HostSpec::default()
        };
        HostCapabilities::load(&spec, Path::new("/tmp"), &|_| None)
    }

    const OK: &str = r#"{"schema":"chip.host-capabilities.v1","capabilities":[
      {"id":"notes.add","description":"Add a note","effect":"consequential","idempotency":"deduplicated",
       "inputs":[{"name":"text","required":true},{"name":"pinned","type":"boolean"}],
       "binding":{"type":"exec","argv":["echo","${text}"]}},
      {"id":"notes.count","description":"Count notes","effect":"observation",
       "binding":{"type":"exec","argv":["echo","3"]}}]}"#;

    #[test]
    fn a_valid_file_declares_effects_idempotency_and_typed_inputs() {
        let host = load(OK).unwrap();
        let add = host
            .entries
            .iter()
            .find(|e| e.descriptor.id.as_str() == "notes.add")
            .unwrap();
        assert_eq!(add.descriptor.effect, Effect::Consequential);
        assert_eq!(add.descriptor.idempotency, Idempotency::Deduplicated);
        assert_eq!(add.kinds["pinned"], Kind::Bool);
        assert!(
            add.descriptor
                .inputs
                .iter()
                .any(|i| i.name == "text" && i.required)
        );
        let count = host
            .entries
            .iter()
            .find(|e| e.descriptor.id.as_str() == "notes.count")
            .unwrap();
        assert_eq!(
            count.descriptor.idempotency,
            Idempotency::NotCarried,
            "undeclared means no key"
        );
        assert_eq!(host.observation_ids(), vec!["notes.count".to_string()]);
    }

    #[test]
    fn an_invalid_file_is_refused_never_repaired() {
        for (why, text) in [
            ("wrong schema", OK.replace("v1", "v9")),
            (
                "an unknown field",
                OK.replace(
                    "\"description\":\"Add a note\"",
                    "\"description\":\"x\",\"surprise\":1",
                ),
            ),
            ("a reserved id", OK.replace("notes.add", "project.write")),
            ("a duplicate id", OK.replace("notes.count", "notes.add")),
            ("an uppercase id", OK.replace("notes.add", "Notes.Add")),
            ("an unknown effect", OK.replace("consequential", "harmless")),
            ("an unknown idempotency", OK.replace("deduplicated", "yes")),
            ("not JSON", "{".to_string()),
            (
                "nothing supplied",
                r#"{"schema":"chip.host-capabilities.v1"}"#.to_string(),
            ),
        ] {
            assert!(load(&text).is_err(), "{why}");
        }
    }

    #[test]
    fn host_text_is_one_bounded_line_without_control_characters() {
        assert_eq!(clean_text("a\u{0}b\n\n  c\t\u{7}d", 50), "ab c d");
        assert_eq!(clean_text(&"x".repeat(500), 10), "x".repeat(10));
        assert_eq!(bounded("ok\u{0}\u{1b}[31m", 100), "ok[31m");
        assert!(
            bounded(&"é".repeat(100), 51).contains("[cut"),
            "cut on a character boundary, with a note"
        );
    }

    #[tokio::test]
    async fn a_value_of_the_wrong_type_is_refused_not_coerced() {
        let host = Arc::new(load(OK).unwrap());
        let backend = HostBackend::new(host);
        let id = CapabilityId::new("notes.add").unwrap();
        let ok = BTreeMap::from([
            ("text".into(), InputValue::Text("x".into())),
            ("pinned".into(), InputValue::Bool(true)),
        ]);
        assert!(backend.validate_inputs(&id, &ok).await.is_ok());
        let bad = BTreeMap::from([
            ("text".into(), InputValue::Text("x".into())),
            ("pinned".into(), InputValue::Text("true".into())),
        ]);
        assert!(
            matches!(backend.validate_inputs(&id, &bad).await, Err(CapabilityError::InvalidInput(m)) if m.contains("boolean"))
        );
        let int = BTreeMap::from([("text".into(), InputValue::Integer(5))]);
        assert!(backend.validate_inputs(&id, &int).await.is_err());
    }
}
