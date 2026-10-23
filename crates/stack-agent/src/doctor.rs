//! `stack-agent doctor`: diagnose the environment `work` will run in. It is not an agent run.
//!
//! It decides nothing for the user and repairs nothing. It never selects a model or a provider,
//! never calls the model (no request of any kind reaches the endpoint), never validates a key, never
//! installs or changes anything, and never writes to the project. Its verdicts come from the same
//! code `work` uses to decide whether it can start (`WorkRuntime::prepare_with_generation`,
//! `locate_test_runner`, `ProjectExecutor::availability`, `work_state::default_database`), so the
//! two cannot drift: a check that `work` enforces is a failure here, and a check `work` does not
//! enforce is at most a warning.
//!
//! Every check says whether it is *required* for the selected kind of work (`work` would refuse to
//! start, or could never complete, without it) or *advisory* (`work` starts regardless). A
//! prerequisite the selected kind does not use is `not_required`, never reported as missing.
//!
//! Statuses: `ok`, `fail` (required, and not satisfied), `warn` (advisory, and not satisfied),
//! `unverified` (doctor could not establish it and says why), `not_required`.
//!
//! Exit status follows the one exit contract: 0 when no required check failed, 2 usage, 3 when one
//! did (a required dependency is unavailable and nothing was done).
//!
//! What it deliberately does not do:
//! - It does not run `pax --dry-run`. PAX's project detection spawns `cargo metadata` on the
//!   project, and a project's `rust-toolchain.toml` can make that download and install a toolchain.
//!   Only `pax --version` is run, as `work` does. Whether PAX can verify the project is `unverified`.
//! - It does not connect to a model endpoint through a proxy, or around one. With `CHIP_PROXY` set,
//!   reachability is `unverified`. Without one it opens a TCP connection to the endpoint's host and
//!   port and closes it without sending a byte: that shows something listens, not that the key is
//!   valid, the model exists or the endpoint speaks the protocol.
//! - It does not take the project lock, and does not construct an agent or an executor.
//!
//! The one write: when the state directory already exists, an empty probe file is created in it
//! and removed, to learn whether it is writable. Nothing is created when it does not exist.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chip_core::{CapabilityAvailability, CapabilityId, CapabilityProvider};
use chip_pax::{PaxExecutor, PaxUnavailable};
use chip_project::{PROJECT_LIST, ProjectExecutor};
use serde_json::{Value, json};

use crate::provider_selection::{self, Selection};
use crate::software_work::{MODEL_SETUP, WorkRuntime};
use crate::verify::{EXIT_UNAVAILABLE, EXIT_USAGE, EXIT_VERIFIED};

pub const SCHEMA: &str = "stack-agent.doctor.v1";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const PAX_RELEASES: &str = "https://github.com/rkendel1/pax/releases";
const REDACTED: &str = "[redacted]";
/// Variables whose values must never appear in any output, whatever an error message contains.
const SECRET_VARIABLES: [&str; 4] = [
    "CHIP_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "CHIP_PROXY_PASSWORD",
];

const USAGE: &str = "\
usage: stack-agent doctor [--kind change|verify|inspect] [--dir DIR] [--no-network] [--json]
                          [--provider P] [--model M] [--endpoint URL] [--state-db FILE | --no-state]

Diagnoses the environment `work` would run in. It does not call the model, select a model or
provider, install anything, or change the project. --kind defaults to change, as for `work`.
--no-network skips the endpoint reachability check. Exit: 0 no required check failed, 2 usage,
3 a required prerequisite is unavailable. The model is chosen as for `work` (flags, then
CHIP_PROVIDER / CHIP_MODEL / CHIP_ENDPOINT); doctor never chooses one.
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Change,
    Verify,
    Inspect,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Change => "change",
            Self::Verify => "verify",
            Self::Inspect => "inspect",
        }
    }

    /// Whether the kind completes only through a passing test run (and so needs PAX).
    fn needs_test_runner(self) -> bool {
        self != Self::Inspect
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Fail,
    Warn,
    Unverified,
    NotRequired,
}

impl Status {
    fn name(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Fail => "fail",
            Self::Warn => "warn",
            Self::Unverified => "unverified",
            Self::NotRequired => "not_required",
        }
    }
}

/// What kind of problem a check found. Present on every status except `ok` and `not_required`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    ConfigMissing,
    ConfigInvalid,
    EndpointUnreachable,
    DependencyMissing,
    DependencyUnsuitable,
    ProjectUnsupported,
    Environment,
}

impl Category {
    fn name(self) -> &'static str {
        match self {
            Self::ConfigMissing => "config_missing",
            Self::ConfigInvalid => "config_invalid",
            Self::EndpointUnreachable => "endpoint_unreachable",
            Self::DependencyMissing => "dependency_missing",
            Self::DependencyUnsuitable => "dependency_unsuitable",
            Self::ProjectUnsupported => "project_unsupported",
            Self::Environment => "environment",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub id: &'static str,
    pub status: Status,
    pub category: Option<Category>,
    pub summary: String,
    pub detail: Vec<String>,
    pub fix: Option<String>,
}

impl Check {
    fn new(id: &'static str, status: Status, summary: impl Into<String>) -> Self {
        Self {
            id,
            status,
            category: None,
            summary: summary.into(),
            detail: Vec::new(),
            fix: None,
        }
    }

    fn problem(
        id: &'static str,
        status: Status,
        category: Category,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            category: Some(category),
            ..Self::new(id, status, summary)
        }
    }

    fn detail(mut self, line: impl Into<String>) -> Self {
        self.detail.push(line.into());
        self
    }

    fn fix(mut self, text: impl Into<String>) -> Self {
        self.fix = Some(text.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub kind: Kind,
    pub dir: Option<PathBuf>,
    pub network: bool,
    pub json: bool,
    pub selection: Selection,
    pub state_db: Option<PathBuf>,
    pub no_state: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            kind: Kind::Change,
            dir: None,
            network: true,
            json: false,
            selection: Selection::default(),
            state_db: None,
            no_state: false,
        }
    }
}

pub struct Report {
    pub kind: Kind,
    pub project: PathBuf,
    pub checks: Vec<Check>,
}

impl Report {
    /// Whether no required check failed.
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.status != Status::Fail)
    }

    fn redact(&mut self, secrets: &[String]) {
        let scrub = |text: &mut String| {
            for secret in secrets {
                if !secret.is_empty() && text.contains(secret.as_str()) {
                    *text = text.replace(secret.as_str(), REDACTED);
                }
            }
        };
        for check in &mut self.checks {
            scrub(&mut check.summary);
            check.detail.iter_mut().for_each(scrub);
            if let Some(fix) = &mut check.fix {
                scrub(fix);
            }
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "schema": SCHEMA,
            "ok": self.ok(),
            "kind": self.kind.name(),
            "project": self.project.display().to_string(),
            "checks": self.checks.iter().map(|c| json!({
                "id": c.id,
                "status": c.status.name(),
                "category": c.category.map(Category::name),
                "summary": c.summary,
                "detail": c.detail,
                "fix": c.fix,
            })).collect::<Vec<_>>(),
            "not_checked": NOT_CHECKED,
        })
    }

    pub fn to_text(&self) -> String {
        let mut out = format!(
            "stack-agent doctor (kind: {}, project: {})\n\n",
            self.kind.name(),
            self.project.display()
        );
        for c in &self.checks {
            let label = match c.status {
                Status::Ok => "ok",
                Status::Fail => "FAIL",
                Status::Warn => "warn",
                Status::Unverified => "unverified",
                Status::NotRequired => "not needed",
            };
            out.push_str(&format!("  {:<11} {:<13} {}\n", label, c.id, c.summary));
            for line in &c.detail {
                out.push_str(&format!("{:28}{line}\n", ""));
            }
            if let Some(fix) = &c.fix {
                for (i, line) in fix.lines().enumerate() {
                    let lead = if i == 0 { "fix: " } else { "     " };
                    out.push_str(&format!("{:28}{lead}{line}\n", ""));
                }
            }
        }
        let failed = self
            .checks
            .iter()
            .filter(|c| c.status == Status::Fail)
            .count();
        out.push('\n');
        if failed == 0 {
            out.push_str("Result: no required prerequisite is missing.\n");
        } else {
            out.push_str(&format!(
                "Result: {failed} required prerequisite{} unavailable; `work` would not start or could not complete.\n",
                if failed == 1 { " is" } else { "s are" }
            ));
        }
        out.push_str(&format!("Not checked: {NOT_CHECKED}\n"));
        out.push_str("Nothing was run, installed or changed in the project.\n");
        out
    }
}

const NOT_CHECKED: &str = "that the key is accepted, that the model exists, or that the endpoint speaks the protocol (no request is made; `stack-agent \"Say hello\"` makes one call to check)";

/// Runs every check. `env` is the environment; the process environment is also read by the code
/// shared with `work`, so a caller that passes a different map must expect those parts to differ.
pub async fn diagnose(options: &Options, env: &dyn Fn(&str) -> Option<String>) -> Report {
    let root = match &options.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let mut checks = vec![binary()];
    let (model, resolved_endpoint) = model(options, env);
    let model_ok = model.status == Status::Ok;
    checks.push(model);
    checks.push(endpoint(options, env, model_ok, resolved_endpoint).await);
    checks.push(git());
    checks.push(project(&root).await);
    checks.push(pax(options.kind, &root).await);
    checks.push(verification(options.kind, &root));
    checks.push(state(options, &root, env));

    let mut report = Report {
        kind: options.kind,
        project: root,
        checks,
    };
    let secrets: Vec<String> = SECRET_VARIABLES.iter().filter_map(|n| env(n)).collect();
    report.redact(&secrets);
    report
}

fn binary() -> Check {
    let path = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".into());
    Check::new(
        "binary",
        Status::Ok,
        format!(
            "stack-agent {} ({} {})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    )
    .detail(format!("at {path}"))
}

/// The model selection, judged by exactly what `work` does before it starts. Returns the check and
/// the resolved endpoint (for the reachability check), when selection resolved.
fn model(options: &Options, env: &dyn Fn(&str) -> Option<String>) -> (Check, Option<String>) {
    let endpoint = provider_selection::resolve(&options.selection, env)
        .ok()
        .map(|config| config.endpoint);
    let prepared = WorkRuntime::prepare_with_generation(
        &options.selection,
        None,
        &crate::work_generation::Flags::default(),
    );
    let check = match prepared {
        Ok(runtime) => {
            let id = &runtime.identity;
            let key = match key_source(&id.provider, env) {
                Some(name) => format!("API key: supplied by {name} (value not shown)"),
                None => "API key: none set".to_string(),
            };
            Check::new(
                "model",
                Status::Ok,
                format!(
                    "provider {}, model {}, endpoint {}",
                    id.provider, id.model, id.endpoint
                ),
            )
            .detail(key)
            .detail("chosen by you (flags, then environment); doctor never selects one")
        }
        Err(why) if why.starts_with("no model is selected") => Check::problem(
            "model",
            Status::Fail,
            Category::ConfigMissing,
            why.trim_end_matches("; nothing was run").to_string(),
        )
        .fix(MODEL_SETUP.trim_end()),
        Err(why) => Check::problem(
            "model",
            Status::Fail,
            Category::ConfigInvalid,
            why.trim_end_matches("; nothing was run").to_string(),
        ),
    };
    (check, endpoint)
}

fn key_source(provider: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<&'static str> {
    let set = |name: &str| env(name).is_some_and(|v| !v.is_empty());
    if set("CHIP_API_KEY") {
        return Some("CHIP_API_KEY");
    }
    let own = match provider {
        "anthropic" => "ANTHROPIC_API_KEY",
        _ => "OPENAI_API_KEY",
    };
    set(own).then_some(own)
}

async fn endpoint(
    options: &Options,
    env: &dyn Fn(&str) -> Option<String>,
    model_ok: bool,
    resolved: Option<String>,
) -> Check {
    let unverified = |why: &str| Check::new("endpoint", Status::Unverified, why.to_string());
    if !model_ok {
        return unverified("not checked: no usable model selection");
    }
    if !options.network {
        return unverified("not checked: --no-network");
    }
    if env(provider_selection::PROXY_VARIABLE).is_some_and(|v| !v.trim().is_empty()) {
        return unverified(
            "not checked: CHIP_PROXY is set, so `work` reaches the endpoint through the proxy; doctor does not connect directly or test the proxy path",
        );
    }
    let Some(endpoint) = resolved else {
        return unverified("not checked: the endpoint did not resolve");
    };
    let shown = provider_selection::endpoint_identity(&endpoint);
    let Some((host, port)) = host_port(&endpoint) else {
        return Check::problem(
            "endpoint",
            Status::Unverified,
            Category::ConfigInvalid,
            format!("not checked: no host and port could be read from {shown}"),
        );
    };
    let connect = tokio::net::TcpStream::connect((host.as_str(), port));
    match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Ok(Ok(stream)) => {
            // Closed at once: nothing is sent, so no request reaches the endpoint.
            drop(stream);
            Check::new(
                "endpoint",
                Status::Ok,
                format!("something accepts connections at {host}:{port}"),
            )
            .detail("a TCP connection was opened and closed; no request was sent, so this does not show the key, the model or the protocol work")
        }
        Ok(Err(error)) => unreachable_endpoint(&shown, &host, port, error.kind().to_string()),
        Err(_) => unreachable_endpoint(
            &shown,
            &host,
            port,
            format!("no answer within {} s", CONNECT_TIMEOUT.as_secs()),
        ),
    }
}

fn unreachable_endpoint(shown: &str, host: &str, port: u16, why: String) -> Check {
    Check::problem(
        "endpoint",
        Status::Fail,
        Category::EndpointUnreachable,
        format!("cannot connect to {host}:{port} ({why})"),
    )
    .detail(format!("endpoint: {shown}"))
    .fix("start the server, or correct --endpoint / CHIP_ENDPOINT; use --no-network to skip this check")
}

/// Host and port of an `http` or `https` endpoint. Userinfo, path and query are ignored.
fn host_port(endpoint: &str) -> Option<(String, u16)> {
    let (scheme, rest) = endpoint.split_once("://")?;
    let default = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let hostport = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let (host, port) = if let Some(inner) = hostport.strip_prefix('[') {
        let (host, after) = inner.split_once(']')?;
        match after.strip_prefix(':') {
            Some(port) => (host, port.parse().ok()?),
            None if after.is_empty() => (host, default),
            None => return None,
        }
    } else {
        match hostport.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().ok()?),
            None => (hostport, default),
        }
    };
    (!host.is_empty()).then(|| (host.to_string(), port))
}

/// Git is used only by the optional `project.git.*` capabilities; `work` starts without it.
fn git() -> Check {
    match std::process::Command::new("git")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => Check::new(
            "git",
            Status::Ok,
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ),
        _ => Check::problem(
            "git",
            Status::Warn,
            Category::DependencyMissing,
            "git was not found; `work` still starts, but its git capabilities fail and you cannot review with `git diff`",
        )
        .fix("install Git (on macOS: `xcode-select --install`)"),
    }
}

/// The project directory, judged by the capability availability `work` checks before it starts.
async fn project(root: &Path) -> Check {
    if !root.is_dir() {
        return Check::problem(
            "project",
            Status::Fail,
            Category::ProjectUnsupported,
            format!("{} is not a directory", root.display()),
        )
        .fix("run doctor from the project directory, or pass --dir");
    }
    let ready = ProjectExecutor::new(root)
        .availability(&CapabilityId::new(PROJECT_LIST).expect("a valid capability id"))
        .await;
    match ready {
        CapabilityAvailability::Available => {
            let mut check = Check::new("project", Status::Ok, format!("{}", root.display()));
            if !root.join(".git").exists() {
                check = check.detail(
                    "not a git repository: `work` edits files in place and does not commit; without git you cannot review changes with `git diff`",
                );
            }
            check
        }
        CapabilityAvailability::Unavailable(why) => Check::problem(
            "project",
            Status::Fail,
            Category::ProjectUnsupported,
            format!("the project capabilities are unavailable: {why}"),
        ),
        _ => Check::problem(
            "project",
            Status::Fail,
            Category::ProjectUnsupported,
            "the project capabilities are not available",
        ),
    }
}

/// PAX, as `work` needs it: located up front for `change` and `verify`, identified and
/// version-checked when `pax.test` is first requested. `inspect` does not use it.
async fn pax(kind: Kind, root: &Path) -> Check {
    if !kind.needs_test_runner() {
        return Check::new(
            "pax",
            Status::NotRequired,
            "`--kind inspect` lists, reads and searches the project without PAX",
        );
    }
    if let Err(why) = crate::local_environment::locate_test_runner(root) {
        return Check::problem(
            "pax",
            Status::Fail,
            Category::DependencyMissing,
            why.trim_end_matches("; nothing was run").to_string(),
        )
        .fix(pax_fix());
    }
    match PaxExecutor::new(root).resolve().await {
        Ok(resolved) => Check::new(
            "pax",
            Status::Ok,
            format!("pax {} at {}", resolved.version, resolved.path.display()),
        ),
        Err(why @ (PaxUnavailable::NotPax { .. } | PaxUnavailable::TooOld { .. })) => {
            let mut check = Check::problem(
                "pax",
                Status::Fail,
                Category::DependencyUnsuitable,
                why.to_string(),
            )
            .detail("`work` starts, but refuses `pax.test` when it first asks for it, so the work cannot be verified");
            if let PaxUnavailable::NotPax { path, .. } = &why
                && matches!(path.to_str(), Some("/usr/bin/pax" | "/bin/pax"))
            {
                check = check.detail(
                    "that file is the operating system's POSIX archive utility, which is unrelated to PAX; put PAX earlier on PATH than /usr/bin",
                );
            }
            check.fix(pax_fix())
        }
        Err(why) => Check::problem(
            "pax",
            Status::Fail,
            Category::DependencyMissing,
            why.to_string(),
        )
        .fix(pax_fix()),
    }
}

fn pax_fix() -> String {
    format!(
        "install PAX 0.4.1 or newer and put it on PATH: {PAX_RELEASES}\nverify the download against that release's SHA256SUMS; stack-agent never installs it\n(`--kind inspect` does not need it)"
    )
}

/// Whether the project looks verifiable. PAX owns that decision and asking it (`--dry-run`) runs
/// `cargo` on the project, so doctor only looks for the manifest and says that it did.
fn verification(kind: Kind, root: &Path) -> Check {
    if !kind.needs_test_runner() {
        return Check::new(
            "verification",
            Status::NotRequired,
            "`--kind inspect` completes from read-only observations, not a test run",
        );
    }
    if root.join("Cargo.toml").is_file() {
        Check::new("verification", Status::Ok, "Cargo.toml found").detail(
            "doctor did not ask PAX whether it can run this project's tests: PAX's own dry run executes cargo on the project",
        )
    } else {
        Check::problem(
            "verification",
            Status::Unverified,
            Category::ProjectUnsupported,
            "no Cargo.toml here; whether PAX can verify this project is not checked",
        )
        .detail("`work` still starts; it completes only if PAX recognises the project and its tests pass")
    }
}

/// The durable state directory. The default database is a record: `work` runs and says it was not
/// persisted if it cannot be used, so that is a warning. `--state-db` is required to work.
fn state(options: &Options, root: &Path, env: &dyn Fn(&str) -> Option<String>) -> Check {
    if options.no_state {
        return Check::new(
            "state",
            Status::NotRequired,
            "--no-state: nothing is persisted",
        );
    }
    let (required, path) = match &options.state_db {
        Some(file) => (true, Ok(file.clone())),
        None => (false, crate::work_state::default_database(root, env)),
    };
    let bad = |category, why: String, fix: &str| {
        let status = if required { Status::Fail } else { Status::Warn };
        let mut check = Check::problem("state", status, category, why).fix(fix);
        if !required {
            check = check.detail("`work` still runs, and says it was not persisted");
        }
        check
    };
    let file = match path {
        Ok(file) => file,
        Err(why) => {
            return bad(
                Category::Environment,
                why,
                "set STACK_AGENT_DATA_DIR to an absolute, writable directory, or pass --no-state",
            );
        }
    };
    let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return bad(
            Category::Environment,
            format!("{} has no directory", file.display()),
            "name a file in a writable directory",
        );
    };
    let describe = |note: &str| format!("{} ({note})", dir.display());
    if dir.is_dir() {
        return match probe_writable(dir) {
            Ok(()) => Check::new("state", Status::Ok, describe("writable")),
            Err(why) => bad(
                Category::Environment,
                format!("{} is not writable ({why})", dir.display()),
                "make the directory writable, set STACK_AGENT_DATA_DIR elsewhere, or pass --no-state",
            ),
        };
    }
    // Not there yet: nothing is created. Look at the nearest parent that exists.
    match dir.ancestors().find(|a| a.is_dir()) {
        Some(parent)
            if std::fs::metadata(parent).is_ok_and(|m| !m.permissions().readonly()) =>
        {
            Check::new(
                "state",
                Status::Ok,
                describe("will be created on first use"),
            )
            .detail(format!(
                "not created by doctor; the nearest existing parent {} is not read-only (permission bits only)",
                parent.display()
            ))
        }
        _ => bad(
            Category::Environment,
            format!("{} cannot be created here", dir.display()),
            "set STACK_AGENT_DATA_DIR to a writable directory, or pass --no-state",
        ),
    }
}

/// Creates and removes one empty file in an existing directory. The only write doctor makes.
fn probe_writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(format!(".stack-agent-doctor-{}", std::process::id()));
    let created = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|e| e.kind().to_string())?;
    drop(created);
    std::fs::remove_file(&probe).map_err(|e| e.kind().to_string())
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut i = 0;
    let value = |i: &mut usize, flag: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i)
            .filter(|v| !v.starts_with("--"))
            .cloned()
            .ok_or_else(|| format!("{flag} needs a value"))
    };
    while i < args.len() {
        match args[i].as_str() {
            "--json" => options.json = true,
            "--no-network" => options.network = false,
            "--no-state" => options.no_state = true,
            "--kind" => {
                options.kind = match value(&mut i, "--kind")?.as_str() {
                    "change" => Kind::Change,
                    "verify" => Kind::Verify,
                    "inspect" => Kind::Inspect,
                    other => {
                        return Err(format!(
                            "--kind must be change, verify or inspect, not {other}"
                        ));
                    }
                }
            }
            "--dir" => options.dir = Some(value(&mut i, "--dir")?.into()),
            "--state-db" => options.state_db = Some(value(&mut i, "--state-db")?.into()),
            "--provider" => options.selection.provider = Some(value(&mut i, "--provider")?),
            "--model" => options.selection.model = Some(value(&mut i, "--model")?),
            "--endpoint" => options.selection.endpoint = Some(value(&mut i, "--endpoint")?),
            other => return Err(format!("unknown argument {other}")),
        }
        i += 1;
    }
    if options.no_state && options.state_db.is_some() {
        return Err("--state-db and --no-state cannot be combined".into());
    }
    Ok(options)
}

/// `stack-agent doctor ...`; the process exit status.
pub async fn doctor(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{USAGE}");
        return EXIT_VERIFIED;
    }
    let options = match parse(args) {
        Ok(options) => options,
        Err(why) => {
            eprintln!("error: {why}");
            eprint!("{USAGE}");
            return EXIT_USAGE;
        }
    };
    let env = |name: &str| std::env::var(name).ok();
    let report = diagnose(&options, &env).await;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report.to_json()).unwrap_or_default()
        );
    } else {
        print!("{}", report.to_text());
    }
    if report.ok() {
        EXIT_VERIFIED
    } else {
        EXIT_UNAVAILABLE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_and_port_are_read_from_the_endpoint_without_userinfo_or_path() {
        let cases = [
            (
                "http://localhost:11434/v1/chat/completions",
                Some(("localhost", 11434)),
            ),
            (
                "https://api.example.com/v1/messages",
                Some(("api.example.com", 443)),
            ),
            ("http://api.example.com/x", Some(("api.example.com", 80))),
            (
                "https://user:pw@host.example:8443/p?q=tok",
                Some(("host.example", 8443)),
            ),
            ("http://[::1]:8080/v1", Some(("::1", 8080))),
            ("http://[::1]/v1", Some(("::1", 80))),
            ("ftp://host/x", None),
            ("http://:80/x", None),
            ("http://host:notaport/x", None),
            ("no-scheme", None),
        ];
        for (endpoint, expected) in cases {
            assert_eq!(
                host_port(endpoint),
                expected.map(|(h, p)| (h.to_string(), p)),
                "{endpoint}"
            );
        }
    }

    #[test]
    fn arguments_are_parsed_and_unknown_ones_are_a_usage_error() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let o = parse(&args(&["--kind", "inspect", "--no-network", "--json"])).unwrap();
        assert_eq!(o.kind, Kind::Inspect);
        assert!(!o.network && o.json);
        assert!(parse(&args(&["--kind", "other"])).is_err());
        assert!(parse(&args(&["--model"])).is_err());
        assert!(parse(&args(&["--surprise"])).is_err());
        assert!(parse(&args(&["--no-state", "--state-db", "f"])).is_err());
    }

    #[test]
    fn a_secret_value_is_removed_from_every_field() {
        let mut report = Report {
            kind: Kind::Change,
            project: PathBuf::from("/p"),
            checks: vec![
                Check::new("a", Status::Ok, "key sk-secret-1 here")
                    .detail("also sk-secret-1")
                    .fix("and sk-secret-1"),
            ],
        };
        report.redact(&["sk-secret-1".to_string()]);
        let text = report.to_text() + &report.to_json().to_string();
        assert!(!text.contains("sk-secret-1"), "{text}");
        assert!(text.contains(REDACTED));
    }

    #[test]
    fn only_a_failed_required_check_makes_the_report_not_ok() {
        let mk = |status| Report {
            kind: Kind::Change,
            project: PathBuf::from("/p"),
            checks: vec![Check::new("x", status, "s")],
        };
        for status in [
            Status::Ok,
            Status::Warn,
            Status::Unverified,
            Status::NotRequired,
        ] {
            assert!(mk(status).ok(), "{status:?}");
        }
        assert!(!mk(Status::Fail).ok());
    }
}
