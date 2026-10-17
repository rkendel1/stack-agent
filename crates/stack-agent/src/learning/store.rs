//! Where learning lives, and the only code that reads or writes it.
//!
//! - Project scope: `<project>/.stack-agent/learning/{runs,behaviors}/`, plus the project's stable
//!   identity in `<project>/.stack-agent/project.json`. `.stack-agent` is reserved by the project
//!   path rules, so no capability (and so no model) can read or write it.
//! - Runtime scope: `<data>/runtimes/<installation>/learning/{runs,behaviors}/`, where `<data>` is
//!   `$STACK_AGENT_DATA_DIR`, else `$XDG_DATA_HOME/stack-agent`, else
//!   `$HOME/.local/share/stack-agent`, and `<installation>` is derived from the executable's
//!   canonical path. Never the executable's own directory, never the project.
//!
//! Every write is atomic (temporary file, then rename) and reports failure; nothing claims to be
//! persisted unless the rename succeeded. Every read is bounded, refuses symbolic links and names
//! that are not document ids, and validates what it reads.

use std::path::{Path, PathBuf};

use super::document::{Behavior, Invalid, MAX_DOCUMENT_BYTES, RunDocument, Scope, file_name, sha};

pub const PROJECT_DIR: &str = chip_project::STACK_AGENT_DIR;
/// At most this many behavior files are examined per scope.
pub const MAX_BEHAVIOR_FILES: usize = 64;

#[derive(Debug, Clone)]
pub struct Store {
    pub scope: Scope,
    /// The learning root (contains `runs/` and `behaviors/`).
    pub root: PathBuf,
    /// How the root is shown: relative for the project, `<data>/...` style for the runtime.
    pub shown: String,
}

fn is_plain_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

fn is_plain_file(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file())
}

/// Writes `text` to `path` atomically. Refuses to follow a symbolic link at the destination.
fn atomic_write(path: &Path, text: &str) -> Result<(), String> {
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("the destination is a symbolic link".into());
    }
    let dir = path.parent().ok_or("no parent directory")?;
    let tmp = dir.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&tmp, text).map_err(|e| format!("write failed ({})", e.kind()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename failed ({})", e.kind())
    })
}

/// Creates `dir` (and parents) and checks no component under `base` is a symbolic link.
fn ensure_dir(base: &Path, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("cannot create the learning directory ({})", e.kind()))?;
    let mut p = dir.to_path_buf();
    while p.starts_with(base) && p != base {
        if !is_plain_dir(&p) {
            return Err("a learning directory is a symbolic link or not a directory".into());
        }
        if !p.pop() {
            break;
        }
    }
    Ok(())
}

impl Store {
    pub fn project(project_root: &Path) -> Self {
        Store {
            scope: Scope::Project,
            root: project_root.join(PROJECT_DIR).join("learning"),
            shown: format!("{PROJECT_DIR}/learning"),
        }
    }

    /// The runtime store, from the given environment. `Err` says why there is none.
    pub fn runtime(env: &dyn Fn(&str) -> Option<String>) -> Result<(Self, PathBuf), String> {
        let base = match (
            env("STACK_AGENT_DATA_DIR"),
            env("XDG_DATA_HOME"),
            env("HOME"),
        ) {
            (Some(d), _, _) if !d.is_empty() => PathBuf::from(d),
            (_, Some(x), _) if !x.is_empty() => PathBuf::from(x).join("stack-agent"),
            (_, _, Some(h)) if !h.is_empty() => PathBuf::from(h).join(".local/share/stack-agent"),
            _ => {
                return Err(
                    "no data directory: set STACK_AGENT_DATA_DIR, XDG_DATA_HOME or HOME".into(),
                );
            }
        };
        if !base.is_absolute() {
            return Err("the runtime data directory must be an absolute path".into());
        }
        let exe = std::env::current_exe()
            .and_then(|p| p.canonicalize())
            .map_err(|e| format!("the executable's path is unknown ({})", e.kind()))?;
        let installation =
            sha(&exe.to_string_lossy())["sha256:".len().."sha256:".len() + 16].to_string();
        let install_dir = base.join("runtimes").join(&installation);
        Ok((
            Store {
                scope: Scope::Runtime,
                root: install_dir.join("learning"),
                shown: format!("<runtime data>/runtimes/{installation}/learning"),
            },
            base,
        ))
    }

    fn runs(&self) -> PathBuf {
        self.root.join("runs")
    }

    fn behaviors(&self) -> PathBuf {
        self.root.join("behaviors")
    }

    /// The store's base: the project directory for project scope, the data directory for runtime.
    fn base(&self) -> PathBuf {
        match self.scope {
            Scope::Project => self
                .root
                .parent()
                .and_then(Path::parent)
                .unwrap_or(&self.root)
                .to_path_buf(),
            Scope::Runtime => self.root.parent().unwrap_or(&self.root).to_path_buf(),
        }
    }

    /// Writes a run document; its file name is its id. Returns the relative reference.
    pub fn write_run(&self, doc: &RunDocument) -> Result<String, String> {
        doc.validate(self.scope)
            .map_err(|e| format!("refusing an invalid document: {e}"))?;
        let name = file_name(&doc.id).ok_or("the document id is not a digest")?;
        ensure_dir(&self.base(), &self.runs())?;
        let path = self.runs().join(&name);
        let text = doc.to_text();
        if is_plain_file(&path) {
            // Content-addressed: an existing file with this name is this document.
            return match std::fs::read_to_string(&path) {
                Ok(existing) if existing == text => Ok(format!("runs/{name}")),
                _ => Err("a different file already has this document's name".into()),
            };
        }
        atomic_write(&path, &text)?;
        Ok(format!("runs/{name}"))
    }

    pub fn write_behavior(&self, b: &Behavior) -> Result<(), String> {
        let name = file_name(&b.id).ok_or("the behavior id is not a digest")?;
        let text = b.to_text();
        if text.len() > MAX_DOCUMENT_BYTES {
            return Err(format!(
                "the behavior would exceed {MAX_DOCUMENT_BYTES} bytes"
            ));
        }
        ensure_dir(&self.base(), &self.behaviors())?;
        atomic_write(&self.behaviors().join(name), &text)
    }

    /// Reads one run document by id, never outside `runs/`.
    pub fn load_run(&self, id: &str) -> Result<RunDocument, Invalid> {
        let name = file_name(id).ok_or_else(|| Invalid(format!("{id:?} is not a document id")))?;
        if !is_plain_dir(&self.root) || !is_plain_dir(&self.runs()) {
            return Err(Invalid(
                "the runs directory is missing or not a plain directory".into(),
            ));
        }
        let path = self.runs().join(&name);
        if !is_plain_file(&path) {
            return Err(Invalid(format!(
                "runs/{name} is missing or not a plain file"
            )));
        }
        let meta =
            std::fs::metadata(&path).map_err(|e| Invalid(format!("unreadable ({})", e.kind())))?;
        if meta.len() as usize > MAX_DOCUMENT_BYTES {
            return Err(Invalid(format!(
                "runs/{name} is larger than {MAX_DOCUMENT_BYTES} bytes"
            )));
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Invalid(format!("unreadable ({})", e.kind())))?;
        let doc = RunDocument::from_text(&text, self.scope)?;
        if doc.id != id {
            return Err(Invalid(format!("runs/{name} holds a different document")));
        }
        Ok(doc)
    }

    pub fn load_behavior(&self, id: &str) -> Result<Option<Behavior>, Invalid> {
        let name = file_name(id).ok_or_else(|| Invalid(format!("{id:?} is not a behavior id")))?;
        let path = self.behaviors().join(name);
        if !is_plain_dir(&self.root) || !is_plain_dir(&self.behaviors()) || !path.exists() {
            return Ok(None);
        }
        if !is_plain_file(&path) {
            return Err(Invalid("the behavior is not a plain file".into()));
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Invalid(format!("unreadable ({})", e.kind())))?;
        Behavior::from_text(&text, self.scope).map(Some)
    }

    /// Every behavior file, bounded and sorted by name. Invalid ones are reported, not used.
    pub fn load_behaviors(&self) -> (Vec<Behavior>, Vec<String>) {
        let mut diagnostics = Vec::new();
        if !is_plain_dir(&self.root) || !is_plain_dir(&self.behaviors()) {
            return (Vec::new(), diagnostics);
        }
        let mut names: Vec<String> = match std::fs::read_dir(self.behaviors()) {
            Ok(entries) => entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(e) => {
                return (
                    Vec::new(),
                    vec![format!("behaviors unreadable ({})", e.kind())],
                );
            }
        };
        names.retain(|n| !n.starts_with(".tmp-"));
        names.sort();
        if names.len() > MAX_BEHAVIOR_FILES {
            diagnostics.push(format!(
                "{} behavior files; only the first {MAX_BEHAVIOR_FILES} were examined",
                names.len()
            ));
            names.truncate(MAX_BEHAVIOR_FILES);
        }
        let mut out = Vec::new();
        for name in names {
            let path = self.behaviors().join(&name);
            let valid_name = name.len() == 69 && name.ends_with(".json");
            if !valid_name || !is_plain_file(&path) {
                diagnostics.push(format!("ignored behaviors/{name}: not a behavior file"));
                continue;
            }
            match std::fs::metadata(&path) {
                Ok(m) if m.len() as usize > MAX_DOCUMENT_BYTES => {
                    diagnostics.push(format!(
                        "ignored behaviors/{name}: larger than {MAX_DOCUMENT_BYTES} bytes"
                    ));
                    continue;
                }
                Err(e) => {
                    diagnostics.push(format!(
                        "ignored behaviors/{name}: unreadable ({})",
                        e.kind()
                    ));
                    continue;
                }
                _ => {}
            }
            match std::fs::read_to_string(&path)
                .map_err(|e| Invalid(e.kind().to_string()))
                .and_then(|t| Behavior::from_text(&t, self.scope))
            {
                Ok(b) if file_name(&b.id).as_deref() == Some(name.as_str()) => out.push(b),
                Ok(_) => diagnostics.push(format!(
                    "ignored behaviors/{name}: its id does not match its file name"
                )),
                Err(e) => diagnostics.push(format!("ignored behaviors/{name}: {e}")),
            }
        }
        (out, diagnostics)
    }
}

/// The project's stable identity: a random id kept in `.stack-agent/project.json`, created on the
/// first write. It survives renaming or moving the directory; it is not derived from the
/// directory's name, and nothing about the project leaves the machine.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectIdentity {
    pub schema: String,
    pub id: String,
    pub created_at: u64,
}

pub const PROJECT_SCHEMA: &str = "stack-agent.project.v1";

fn valid_project_id(id: &str) -> bool {
    id.len() == 21
        && id.starts_with("proj_")
        && id[5..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Reads the identity if there is one; `Ok(None)` when the project has none yet.
pub fn read_project_identity(project_root: &Path) -> Result<Option<ProjectIdentity>, String> {
    let dir = project_root.join(PROJECT_DIR);
    let path = dir.join("project.json");
    if !dir.exists() || !path.exists() {
        return Ok(None);
    }
    if !is_plain_dir(&dir) || !is_plain_file(&path) {
        return Err(format!(
            "{PROJECT_DIR}/project.json is not a plain file in a plain directory"
        ));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("{PROJECT_DIR}/project.json is unreadable ({})", e.kind()))?;
    let identity: ProjectIdentity = serde_json::from_str(&text)
        .map_err(|e| format!("{PROJECT_DIR}/project.json is malformed ({e})"))?;
    if identity.schema != PROJECT_SCHEMA || !valid_project_id(&identity.id) {
        return Err(format!(
            "{PROJECT_DIR}/project.json is not a valid project identity"
        ));
    }
    Ok(Some(identity))
}

/// The identity, created if absent. Never overwrites an existing (even invalid) file.
pub fn ensure_project_identity(project_root: &Path, now: u64) -> Result<ProjectIdentity, String> {
    if let Some(existing) = read_project_identity(project_root)? {
        return Ok(existing);
    }
    let dir = project_root.join(PROJECT_DIR);
    ensure_dir(project_root, &dir)?;
    let seed = format!(
        "{}|{}|{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        std::thread::current().id()
    );
    let identity = ProjectIdentity {
        schema: PROJECT_SCHEMA.into(),
        id: format!(
            "proj_{}",
            &sha(&seed)["sha256:".len().."sha256:".len() + 16]
        ),
        created_at: now,
    };
    let path = dir.join("project.json");
    if path.exists() {
        return Err(format!(
            "{PROJECT_DIR}/project.json appeared while creating it"
        ));
    }
    atomic_write(
        &path,
        &(serde_json::to_string_pretty(&identity).expect("identity serializes") + "\n"),
    )?;
    Ok(identity)
}

/// The installation's private salt for project tokens, created on first use under the runtime
/// data directory. Tokens let the runtime scope count distinct projects without naming any.
pub fn runtime_salt(store: &Store) -> Result<String, String> {
    let install = store.root.parent().ok_or("no installation directory")?;
    let base = install
        .parent()
        .and_then(Path::parent)
        .unwrap_or(install)
        .to_path_buf();
    let path = install.join("installation.json");
    if is_plain_file(&path) {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("installation.json unreadable ({})", e.kind()))?;
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|_| "installation.json is malformed".to_string())?;
        return v["salt"]
            .as_str()
            .filter(|s| s.len() == 64)
            .map(String::from)
            .ok_or_else(|| "installation.json has no valid salt".into());
    }
    ensure_dir(&base, install)?;
    let salt = sha(&format!(
        "salt|{}|{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))["sha256:".len()..]
        .to_string();
    atomic_write(
        &path,
        &(serde_json::json!({"schema": "stack-agent.installation.v1", "salt": salt}).to_string()
            + "\n"),
    )?;
    Ok(salt)
}
