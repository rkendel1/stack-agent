//! Multi-tenant `serve`: who may submit, what each tenant may ask for, and where its work runs.
//!
//! Off unless `serve --tenants FILE` is given; without it the service is the local trusted-client
//! interface it has always been. Tenant mode adds authority only on the operator's side of the
//! boundary: a tenant is a token the operator issued, and everything a request may name (a model
//! profile, a project, a budget) is chosen from, or bounded by, what the operator wrote down for that
//! tenant. A request cannot name a provider, endpoint, key, executable or path outside its tenant's
//! workspace root.
//!
//! `tenants.json` (`chip.tenants.v1`, unknown fields refused):
//!
//! ```json
//! {"schema":"chip.tenants.v1","tenants":[{
//!   "id":"acme", "token_sha256":"<64 hex>", "workspace_root":"/srv/ws/acme",
//!   "kinds":["inspect","change"], "profiles":["default"],
//!   "max_concurrent":1, "max_queued":4, "max_turns":20, "max_executions":20, "max_deadline_secs":600}]}
//! ```
//!
//! `profiles.json` (`chip.profiles.v1`): named model settings the operator vetted, each with an
//! optional capability file (`chip.host-capabilities.v1`):
//!
//! ```json
//! {"schema":"chip.profiles.v1","profiles":{"default":{
//!   "provider":"openai-compatible","model":"m","endpoint":"https://.../v1/chat/completions",
//!   "api_key_env":"ACME_KEY","capabilities":"/etc/chip/acme-caps.json"}}}
//! ```
//!
//! What this is not: per-tenant durable state. Tenant works run without FeltDB admission, recovery or
//! persisted reads (`durable: false`), so a restart forgets them and no tenant can read another's
//! record from disk. Workspaces are the isolation boundary; a work never sees another tenant's root.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::host_capabilities::{HostCapabilities, HostSpec};
use crate::software_work::{GoalKind, WorkRuntime};

pub const TENANTS_SCHEMA: &str = "chip.tenants.v1";
pub const PROFILES_SCHEMA: &str = "chip.profiles.v1";
const MAX_TENANTS: usize = 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TenantsFile {
    schema: String,
    tenants: Vec<TenantDecl>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TenantDecl {
    id: String,
    token_sha256: String,
    workspace_root: PathBuf,
    #[serde(default)]
    kinds: Vec<String>,
    #[serde(default)]
    profiles: Vec<String>,
    #[serde(default = "one")]
    max_concurrent: usize,
    #[serde(default = "eight")]
    max_queued: usize,
    #[serde(default = "twenty")]
    max_turns: usize,
    #[serde(default = "twenty")]
    max_executions: usize,
    #[serde(default = "ten_minutes")]
    max_deadline_secs: u64,
}

fn one() -> usize {
    1
}
fn eight() -> usize {
    8
}
fn twenty() -> usize {
    20
}
fn ten_minutes() -> u64 {
    600
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfilesFile {
    schema: String,
    profiles: BTreeMap<String, ProfileDecl>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileDecl {
    provider: Option<String>,
    model: String,
    endpoint: Option<String>,
    api_key_env: Option<String>,
    capabilities: Option<PathBuf>,
    #[serde(default)]
    keep_builtin_capabilities: Option<bool>,
    streaming: Option<String>,
    structured_output: Option<String>,
    structured_output_fallback: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Tenant {
    pub id: String,
    token: [u8; 32],
    pub workspace_root: PathBuf,
    pub kinds: Vec<GoalKind>,
    pub profiles: Vec<String>,
    pub max_concurrent: usize,
    pub max_queued: usize,
    pub max_turns: usize,
    pub max_executions: usize,
    pub max_deadline_secs: u64,
}

/// One vetted model setting: the runtime is built once, at startup, so a bad profile stops the
/// service before it accepts anything.
pub struct Profile {
    pub runtime: WorkRuntime,
    pub host: Option<Arc<HostCapabilities>>,
}

pub struct Tenancy {
    tenants: Vec<Tenant>,
    pub profiles: HashMap<String, Arc<Profile>>,
}

fn hex32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn constant_time_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, what: &str) -> Result<T, String> {
    let meta =
        std::fs::metadata(path).map_err(|e| format!("{what} {}: {}", path.display(), e.kind()))?;
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        return Err(format!(
            "{what} {} must be a regular file of at most 1 MiB",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{what} {}: {}", path.display(), e.kind()))?;
    serde_json::from_str(&text).map_err(|e| format!("{what} {} is not valid ({e})", path.display()))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}

impl Tenancy {
    /// Reads and validates both files. Blocks (profiles may discover AppPort hosts); call it before
    /// the service listens. Any problem refuses the whole configuration.
    pub fn load(
        tenants: &Path,
        profiles: Option<&Path>,
        context_budget: Option<usize>,
    ) -> Result<Self, String> {
        let file: TenantsFile = read_json(tenants, "tenants file")?;
        if file.schema != TENANTS_SCHEMA {
            return Err(format!(
                "the tenants file's schema must be `{TENANTS_SCHEMA}`"
            ));
        }
        if file.tenants.is_empty() || file.tenants.len() > MAX_TENANTS {
            return Err(format!(
                "the tenants file must list 1 to {MAX_TENANTS} tenants"
            ));
        }
        let mut declared = Vec::new();
        for t in file.tenants {
            if !valid_id(&t.id) {
                return Err(format!(
                    "tenant id `{}` must be lowercase letters, digits, `-` or `_`",
                    t.id
                ));
            }
            if declared.iter().any(|d: &Tenant| d.id == t.id) {
                return Err(format!("tenant `{}` is listed twice", t.id));
            }
            let token = hex32(&t.token_sha256)
                .ok_or_else(|| format!("tenant `{}`: token_sha256 must be 64 hex digits (the SHA-256 of the token; the token itself is never stored)", t.id))?;
            if declared
                .iter()
                .any(|d: &Tenant| constant_time_eq(&d.token, &token))
            {
                return Err(format!(
                    "tenant `{}` has the same token as another tenant",
                    t.id
                ));
            }
            let root = std::fs::canonicalize(&t.workspace_root).map_err(|e| {
                format!(
                    "tenant `{}`: workspace_root {}: {}",
                    t.id,
                    t.workspace_root.display(),
                    e.kind()
                )
            })?;
            if !root.is_dir() {
                return Err(format!(
                    "tenant `{}`: workspace_root is not a directory",
                    t.id
                ));
            }
            if declared.iter().any(|d: &Tenant| {
                root.starts_with(&d.workspace_root) || d.workspace_root.starts_with(&root)
            }) {
                return Err(format!(
                    "tenant `{}`: workspace_root overlaps another tenant's; roots must be disjoint",
                    t.id
                ));
            }
            let kinds = if t.kinds.is_empty() {
                vec![GoalKind::Inspect]
            } else {
                t.kinds
                    .iter()
                    .map(|k| {
                        GoalKind::parse(k)
                            .filter(|k| *k != GoalKind::Act)
                            .ok_or_else(|| format!("tenant `{}`: `{k}` is not a kind a tenant may be given (change, verify, inspect)", t.id))
                    })
                    .collect::<Result<_, _>>()?
            };
            let in_range = |n: usize, max: usize| (1..=max).contains(&n);
            if !in_range(t.max_concurrent, 64)
                || t.max_queued > 1024
                || !in_range(t.max_turns, crate::software_work::LIMIT_CEILING)
                || !in_range(t.max_executions, crate::software_work::LIMIT_CEILING)
                || !(1..=86_400).contains(&t.max_deadline_secs)
            {
                return Err(format!("tenant `{}`: a limit is out of range", t.id));
            }
            declared.push(Tenant {
                id: t.id,
                token,
                workspace_root: root,
                kinds,
                profiles: t.profiles,
                max_concurrent: t.max_concurrent,
                max_queued: t.max_queued,
                max_turns: t.max_turns,
                max_executions: t.max_executions,
                max_deadline_secs: t.max_deadline_secs,
            });
        }

        let mut built = HashMap::new();
        if let Some(path) = profiles {
            let file: ProfilesFile = read_json(path, "profiles file")?;
            if file.schema != PROFILES_SCHEMA {
                return Err(format!(
                    "the profiles file's schema must be `{PROFILES_SCHEMA}`"
                ));
            }
            for (name, p) in file.profiles {
                if !valid_id(&name) {
                    return Err(format!(
                        "profile name `{name}` must be lowercase letters, digits, `-` or `_`"
                    ));
                }
                let selection = crate::provider_selection::Selection {
                    provider: p.provider,
                    model: Some(p.model),
                    endpoint: p.endpoint,
                    api_key_env: p.api_key_env,
                    no_ambient_keys: true,
                    ..Default::default()
                };
                let flags = crate::work_generation::Flags {
                    streaming: p.streaming,
                    structured_output: p.structured_output,
                    structured_output_fallback: p.structured_output_fallback,
                    ..Default::default()
                };
                let runtime =
                    WorkRuntime::prepare_with_generation(&selection, context_budget, &flags)
                        .map_err(|e| format!("profile `{name}`: {e}"))?;
                let host = match &p.capabilities {
                    None => None,
                    Some(file) => {
                        let spec = HostSpec {
                            file: Some(file.clone()),
                            keep_builtin: p.keep_builtin_capabilities.unwrap_or(false),
                            ..HostSpec::default()
                        };
                        Some(Arc::new(
                            HostCapabilities::load(&spec, Path::new("."), &|n| {
                                std::env::var(n).ok()
                            })
                            .map_err(|e| format!("profile `{name}`: {e}"))?,
                        ))
                    }
                };
                built.insert(name, Arc::new(Profile { runtime, host }));
            }
        }
        for t in &declared {
            if let Some(missing) = t.profiles.iter().find(|p| !built.contains_key(*p)) {
                return Err(format!(
                    "tenant `{}` names profile `{missing}`, which the profiles file does not define",
                    t.id
                ));
            }
        }
        Ok(Self {
            tenants: declared,
            profiles: built,
        })
    }

    /// The tenant whose token this `Authorization` header carries. Compares SHA-256 digests in
    /// constant time over every tenant, so neither a token's content nor which tenant matched is
    /// visible in timing.
    pub fn authenticate(&self, header: Option<&str>) -> Option<&Tenant> {
        let token = header?.strip_prefix("Bearer ")?.trim();
        if token.is_empty() || token.len() > 512 {
            return None;
        }
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let mut found = None;
        for t in &self.tenants {
            if constant_time_eq(&t.token, &digest) {
                found = Some(t);
            }
        }
        found
    }

    pub fn tenant(&self, id: &str) -> Option<&Tenant> {
        self.tenants.iter().find(|t| t.id == id)
    }
}

impl Tenant {
    /// The project directory a request names, always inside this tenant's workspace root: a relative
    /// path of plain components, which must exist, be a directory, and still be inside the root after
    /// links are resolved. `None` is the root itself.
    pub fn project(&self, relative: Option<&str>) -> Result<PathBuf, String> {
        let Some(relative) = relative.filter(|r| !r.is_empty() && *r != ".") else {
            return Ok(self.workspace_root.clone());
        };
        let path = Path::new(relative);
        if relative.len() > 200
            || path.is_absolute()
            || !path.components().all(|c| matches!(c, Component::Normal(_)))
        {
            return Err("the project must be a relative path inside the tenant's workspace".into());
        }
        let joined = self.workspace_root.join(path);
        let real = std::fs::canonicalize(&joined)
            .map_err(|_| "the project does not exist in the tenant's workspace".to_string())?;
        if !real.starts_with(&self.workspace_root) || !real.is_dir() {
            return Err("the project must be a directory inside the tenant's workspace".into());
        }
        Ok(real)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant(root: &Path) -> Tenant {
        Tenant {
            id: "a".into(),
            token: Sha256::digest(b"tok").into(),
            workspace_root: std::fs::canonicalize(root).unwrap(),
            kinds: vec![GoalKind::Inspect],
            profiles: vec![],
            max_concurrent: 1,
            max_queued: 1,
            max_turns: 5,
            max_executions: 5,
            max_deadline_secs: 60,
        }
    }

    #[test]
    fn a_project_cannot_leave_the_tenants_workspace() {
        let base = std::env::temp_dir().join(format!("tenancy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("a/proj")).unwrap();
        std::fs::create_dir_all(base.join("b")).unwrap();
        let t = tenant(&base.join("a"));
        assert!(t.project(Some("proj")).is_ok());
        assert_eq!(t.project(None).unwrap(), t.workspace_root);
        for bad in ["../b", "/etc", "proj/../../b", "missing", "proj/.."] {
            assert!(t.project(Some(bad)).is_err(), "{bad}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(base.join("b"), base.join("a/link")).unwrap();
            assert!(
                t.project(Some("link")).is_err(),
                "a link out of the root is refused"
            );
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_token_is_matched_by_digest_and_nothing_else_authenticates() {
        let t = tenant(&std::env::temp_dir());
        let tenancy = Tenancy {
            tenants: vec![t],
            profiles: HashMap::new(),
        };
        assert!(tenancy.authenticate(Some("Bearer tok")).is_some());
        for bad in [
            None,
            Some("tok"),
            Some("Bearer "),
            Some("Bearer nope"),
            Some("Basic tok"),
        ] {
            assert!(tenancy.authenticate(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn hex_digests_are_exactly_64_hex_digits() {
        assert!(hex32(&"ab".repeat(32)).is_some());
        for bad in ["", "zz", &"ab".repeat(31), &"gg".repeat(32)] {
            assert!(hex32(bad).is_none());
        }
    }
}
