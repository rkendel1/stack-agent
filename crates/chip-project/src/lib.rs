//! Bounded project capabilities: `project.read`, `project.write`, `project.list`, `project.search`
//! and four read-only Git observations (see the `git` module).
//!
//! The model names a project-relative path (and, to write, the complete new content). Chip owns
//! everything else: the project root, path normalisation, traversal and symlink prevention, size
//! limits, the real filesystem operation, and the observation of it. The model is never given a
//! filesystem primitive: it cannot name a root, an absolute path, a descriptor, a permission, an
//! environment or a process.
//!
//! Two kinds of refusal, kept apart on purpose:
//!
//! * A request that could escape or misuse the boundary (an absolute path, `..`, a symlink on the
//!   path, a reserved name, an oversized or malformed value) is an *invalid invocation*. It is
//!   refused in [`CapabilityProvider::validate_inputs`], before anything executes, so it produces
//!   no execution, no observation and no evidence. Nothing is repaired or rewritten.
//! * A valid request that reality says no to (the file does not exist, is too large to read, is
//!   not UTF-8, the parent directory is missing, the disk refuses) is performed and *observed* as
//!   a failure. It is real, recoverable information, and no mutation took place.
//!
//! A write is atomic (a temporary file in the same directory, flushed, then renamed over the
//! target) and is reported as having succeeded only after the target has been read back and
//! found to hold exactly the requested bytes. Every observation is generated here from the real
//! operation; the first line is a canonical JSON object and, for a read, the file's content
//! follows a delimiter line.
//!
//! The observation invariants ([`path_escape_invariant`], [`out_of_root_write_invariant`]) let the
//! safety audit re-check, independently of this executor's own validation, that no recorded
//! observation names a path outside the root.
//!
//! Which names are withheld from every capability, and which paths may be read but never written,
//! is the [`policy`] module. On Unix every operation goes through directory descriptors opened
//! without following links (the [`confine`] module), so the check and the operation are one step
//! and a project another process rewrites cannot redirect them out of the root. A work holds a
//! [`ProjectLock`] on the directory, so two stack-agent processes never write one project at once,
//! and the lock's holder removes the `.chip-write-*.tmp` files a crashed write left behind.
//!
//! Known limits, stated plainly: on platforms other than Unix the check and the operation are
//! still separate steps; and writing code that the project's own tooling later runs is,
//! inherently, code execution through that tooling. This crate bounds what the *model* may do to
//! files. It does not sandbox the project.

mod confine;
mod git;
#[cfg(unix)]
pub mod journal;
mod lock;
pub mod policy;

pub use lock::{LockError, MAX_SWEEP_ENTRIES, ProjectLock};

pub use git::{
    ALLOWED_SUBCOMMANDS, DEFAULT_GIT_LOG_COUNT, GIT_CAPABILITIES, GIT_OBSERVATION_INVALID,
    GIT_SCOPE, Invocation, MAX_GIT_DIFF_BYTES, MAX_GIT_LOG_COUNT, MAX_GIT_PATHS, PROJECT_GIT_DIFF,
    PROJECT_GIT_DIFF_STAT, PROJECT_GIT_LOG, PROJECT_GIT_STATUS, Side, argv as git_argv,
    git_observation_invariant, git_scope_invariant, is_git_capability,
};

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chip_core::{
    CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId, CapabilityInput,
    CapabilityProvider, Effect, ExecutionError, ExecutionRequest, ExecutionResult, Executor,
    Idempotency, InputValue, Observation, ObservationInvariant, ObservationKind,
};
use sha2::{Digest, Sha256};

/// The host-owned directory for stack-agent's project learning. No capability may read, list,
/// search or write it: only the host writes learning records, so a model cannot forge them.
pub const STACK_AGENT_DIR: &str = ".stack-agent";

pub const PROJECT_READ: &str = "project.read";
pub const PROJECT_WRITE: &str = "project.write";
pub const PROJECT_LIST: &str = "project.list";
pub const PROJECT_SEARCH: &str = "project.search";

/// The most one `project.read` observation carries (a whole file, or one range of a larger one),
/// and the largest content `project.write` accepts. A single read never returns more than this.
pub const MAX_READ_BYTES: usize = 32 * 1024;
pub const MAX_WRITE_BYTES: usize = 32 * 1024;
/// The longest project-relative path accepted.
pub const MAX_PATH_BYTES: usize = 200;

/// `project.list`: entries returned.
pub const MAX_LIST_ENTRIES: usize = 200;
/// `project.search`: the longest query, files examined, the largest file examined, matches
/// returned and the length of each line shown. All deterministic, none unlimited.
pub const MAX_QUERY_BYTES: usize = 200;
pub const MAX_SEARCH_FILES: usize = 500;
pub const MAX_SEARCH_FILE_BYTES: u64 = 256 * 1024;
pub const MAX_MATCHES: usize = 50;
pub const MAX_LINE_CHARS: usize = 200;
/// The most a list or search observation carries.
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024;

/// Invariant names, as the audit reports them.
pub const PATH_ESCAPE: &str = "path_escape";
pub const OUT_OF_ROOT_WRITE: &str = "out_of_root_write";
pub const HOST_PATH_LEAK: &str = "host_path_leak";
pub const NAVIGATION_MISMATCH: &str = "navigation_mismatch";

/// Why a path is not acceptable. No variant carries a host path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    Empty,
    TooLong,
    Absolute,
    InvalidCharacter(char),
    /// An empty, `.` or `..` component.
    BadComponent(String),
    /// A name Chip never lets a model read, write, list or search: its own state (`.git`,
    /// `.stack-agent`, temporary files) and credential files ([`policy::withheld`]).
    Reserved(String),
    /// A path a model may read but never write: a surface that runs code or steers an agent outside
    /// the work's verification ([`policy::protected`]).
    Protected {
        path: String,
        rule: policy::Rule,
    },
    /// A symbolic link on the path.
    Symlink(String),
    /// The resolved target is not inside the project root.
    EscapesRoot,
    /// An existing component is not a directory.
    NotADirectory(String),
    /// The filesystem could not be inspected.
    Unreadable,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "the path is empty"),
            Self::TooLong => write!(f, "the path is longer than {MAX_PATH_BYTES} bytes"),
            Self::Absolute => write!(f, "the path must be project-relative, not absolute"),
            Self::InvalidCharacter(c) => write!(
                f,
                "the path contains {c:?}; only letters, digits, '_', '-', '.' and '/' are allowed"
            ),
            Self::BadComponent(c) => write!(f, "the path has a {c:?} component"),
            Self::Reserved(c) => match policy::withheld(c) {
                Some(rule) => write!(
                    f,
                    "the path component {c:?} is reserved ({}: {})",
                    rule.id, rule.description
                ),
                None => write!(f, "the path component {c:?} is reserved"),
            },
            Self::Protected { path, rule } => write!(
                f,
                "the path {path:?} is protected and is never written ({}: {})",
                rule.id, rule.description
            ),
            Self::Symlink(c) => write!(f, "the path component {c:?} is a symbolic link"),
            Self::EscapesRoot => write!(f, "the path resolves outside the project root"),
            Self::NotADirectory(c) => write!(f, "the path component {c:?} is not a directory"),
            Self::Unreadable => write!(f, "the path could not be inspected"),
        }
    }
}

/// The lexical rules, in one place. No filesystem access: a path that fails here never touches it.
fn components(path: &str) -> Result<Vec<&str>, PathError> {
    if path.is_empty() {
        return Err(PathError::Empty);
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(PathError::TooLong);
    }
    if path.starts_with('/') {
        return Err(PathError::Absolute);
    }
    if let Some(c) = path
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/')))
    {
        return Err(PathError::InvalidCharacter(c));
    }
    let parts: Vec<&str> = path.split('/').collect();
    for part in &parts {
        if part.is_empty() || *part == "." || *part == ".." {
            return Err(PathError::BadComponent((*part).to_string()));
        }
        if policy::withheld(part).is_some() {
            return Err(PathError::Reserved((*part).to_string()));
        }
    }
    Ok(parts)
}

/// Whether `path` passes the lexical project-path rules every project capability applies first
/// (non-empty, project-relative, no `.`/`..`/empty component, no reserved name). No filesystem is
/// touched, so a path that passes can still name nothing. For callers that must not name a path the
/// executor would refuse.
pub fn check_project_path(path: &str) -> Result<(), String> {
    components(path).map(|_| ()).map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Missing,
    File,
    Directory,
    Other,
}

/// A path walked from the root, one component at a time, never through a symlink. It holds the
/// directory the walk opened, not its name, so what it names cannot be swapped out from under it.
#[derive(Debug)]
struct Located {
    /// The directory that holds the final component (the root itself for the root).
    parent: confine::Dir,
    /// The final component; `None` for the root.
    name: Option<String>,
    /// Every directory above the final component exists.
    parent_exists: bool,
    kind: Kind,
    entry: Option<confine::Entry>,
}

impl Located {
    /// The final component, or `.` for the root (a directory names itself as `.`).
    fn name(&self) -> &str {
        self.name.as_deref().unwrap_or(".")
    }

    /// The located directory itself, opened.
    fn open_dir(&self) -> std::io::Result<confine::Dir> {
        match &self.name {
            None => Ok(self.parent.clone()),
            Some(name) => self.parent.open_dir(name),
        }
    }
}

/// The project-file capabilities, confined to one root.
#[derive(Debug, Clone)]
pub struct ProjectExecutor {
    /// The canonical root and the directory opened there. Established once, when the executor is
    /// made: every operation starts from that descriptor.
    root: Result<(PathBuf, confine::Dir), String>,
}

impl ProjectExecutor {
    pub fn new(root: impl AsRef<Path>) -> Self {
        let root = fs::canonicalize(root.as_ref())
            .map_err(|e| format!("the project root cannot be resolved ({})", e.kind()))
            .and_then(|r| {
                r.is_dir()
                    .then_some(r)
                    .ok_or_else(|| "the project root is not a directory".to_string())
            })
            .and_then(|r| {
                confine::Dir::open(&r)
                    .map(|dir| (r, dir))
                    .map_err(|e| format!("the project root cannot be opened ({})", e.kind()))
            });
        Self { root }
    }

    fn root(&self) -> Result<&Path, String> {
        self.root
            .as_ref()
            .map(|(path, _)| path.as_path())
            .map_err(Clone::clone)
    }

    fn root_dir(&self) -> Result<&confine::Dir, PathError> {
        self.root
            .as_ref()
            .map(|(_, dir)| dir)
            .map_err(|_| PathError::Unreadable)
    }

    fn locate(&self, path: &str) -> Result<Located, PathError> {
        let parts = components(path)?;
        let mut dir = self.root_dir()?.clone();
        let last = parts.len() - 1;
        for part in &parts[..last] {
            let missing = Located {
                parent: dir.clone(),
                name: Some(parts[last].to_string()),
                parent_exists: false,
                kind: Kind::Missing,
                entry: None,
            };
            match dir.entry(part).map_err(|_| PathError::Unreadable)? {
                None => return Ok(missing),
                Some(e) if e.kind == confine::EntryKind::Symlink => {
                    return Err(PathError::Symlink((*part).to_string()));
                }
                Some(e) if e.kind != confine::EntryKind::Directory => {
                    return Err(PathError::NotADirectory((*part).to_string()));
                }
                Some(_) => {}
            }
            // The open refuses a link or a non-directory itself, even one that replaced the
            // directory just checked.
            dir = match dir.open_dir(part) {
                Ok(next) => next,
                Err(e) if confine::is_symlink_refusal(&e) => {
                    return Err(PathError::Symlink((*part).to_string()));
                }
                Err(e) if confine::is_not_a_directory(&e) => {
                    return Err(PathError::NotADirectory((*part).to_string()));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(missing),
                Err(_) => return Err(PathError::Unreadable),
            };
        }
        let name = parts[last];
        let entry = dir.entry(name).map_err(|_| PathError::Unreadable)?;
        let kind = match entry.map(|e| e.kind) {
            None => Kind::Missing,
            Some(confine::EntryKind::Symlink) => return Err(PathError::Symlink(name.to_string())),
            Some(confine::EntryKind::File) => Kind::File,
            Some(confine::EntryKind::Directory) => Kind::Directory,
            Some(confine::EntryKind::Other) => Kind::Other,
        };
        Ok(Located {
            parent: dir,
            name: Some(name.to_string()),
            parent_exists: true,
            kind,
            entry,
        })
    }
}

/// A directory path as `project.list` and `project.search` take it: exactly one trailing `/` is
/// the same directory without it (`crates/` is `crates`). Nothing else changes: the remainder goes
/// through the same lexical and filesystem checks, so `crates//`, `/`, `../` and `a/../` are still
/// refused. Files (`project.read`, `project.write`) keep the strict spelling.
fn directory_spelling(path: &str) -> &str {
    path.strip_suffix('/').unwrap_or(path)
}

impl ProjectExecutor {
    /// The directory a navigation request names: the root itself for an absent path or exactly
    /// ".", otherwise the same checked walk as any other path. The root is never shown as a host
    /// path: it is ".".
    fn locate_dir(&self, path: Option<&str>) -> Result<Located, PathError> {
        match path.map(directory_spelling) {
            None | Some(".") => Ok(Located {
                parent: self.root_dir()?.clone(),
                name: None,
                parent_exists: true,
                kind: Kind::Directory,
                entry: None,
            }),
            Some(p) => self.locate(p),
        }
    }
}

fn text_input<'a>(
    inputs: &'a BTreeMap<String, InputValue>,
    name: &str,
) -> Result<&'a str, CapabilityError> {
    match inputs.get(name) {
        Some(InputValue::Text(s)) => Ok(s),
        Some(_) => Err(CapabilityError::InvalidInput(format!(
            "input '{name}' must be text"
        ))),
        None => Err(CapabilityError::InvalidInput(format!(
            "missing required input '{name}'"
        ))),
    }
}

fn input(name: &str, description: &str) -> CapabilityInput {
    CapabilityInput {
        name: name.to_string(),
        description: description.to_string(),
        required: true,
    }
}

fn optional_input(name: &str, description: &str) -> CapabilityInput {
    CapabilityInput {
        required: false,
        ..input(name, description)
    }
}

/// An input that may be absent; present, it must be text.
fn optional_text<'a>(
    inputs: &'a BTreeMap<String, InputValue>,
    name: &str,
) -> Result<Option<&'a str>, CapabilityError> {
    match inputs.get(name) {
        None => Ok(None),
        Some(InputValue::Text(s)) => Ok(Some(s)),
        Some(_) => Err(CapabilityError::InvalidInput(format!(
            "input '{name}' must be text"
        ))),
    }
}

/// `project.git.log`'s only input: absent, or an integer within the declared bounds.
fn git_count(inputs: &BTreeMap<String, InputValue>) -> Result<Option<i64>, CapabilityError> {
    match inputs.get("count") {
        None => Ok(None),
        Some(InputValue::Integer(n)) if (1..=MAX_GIT_LOG_COUNT).contains(n) => Ok(Some(*n)),
        Some(InputValue::Integer(n)) => Err(CapabilityError::InvalidInput(format!(
            "count is {n}; it must be between 1 and {MAX_GIT_LOG_COUNT}"
        ))),
        Some(_) => Err(CapabilityError::InvalidInput(
            "input 'count' must be an integer".into(),
        )),
    }
}

/// `project.read`'s optional range: `None` when neither input is given (a whole-file read), else
/// `(offset, length)` with the defaults filled in. Both are integers; the offset is 0 or more and the
/// length is between 1 and [`MAX_READ_BYTES`], so no request can ask for more than one bounded read.
fn read_range(
    inputs: &BTreeMap<String, InputValue>,
) -> Result<Option<(u64, usize)>, CapabilityError> {
    let integer = |name: &str| -> Result<Option<i64>, CapabilityError> {
        match inputs.get(name) {
            None => Ok(None),
            Some(InputValue::Integer(n)) => Ok(Some(*n)),
            Some(_) => Err(CapabilityError::InvalidInput(format!(
                "input '{name}' must be an integer"
            ))),
        }
    };
    let (offset, length) = (integer("offset")?, integer("length")?);
    if offset.is_none() && length.is_none() {
        return Ok(None);
    }
    let offset = offset.unwrap_or(0);
    if offset < 0 {
        return Err(CapabilityError::InvalidInput(format!(
            "offset is {offset}; it must be 0 or more"
        )));
    }
    let length = length.unwrap_or(MAX_READ_BYTES as i64);
    if !(1..=MAX_READ_BYTES as i64).contains(&length) {
        return Err(CapabilityError::InvalidInput(format!(
            "length is {length}; it must be between 1 and {MAX_READ_BYTES}"
        )));
    }
    Ok(Some((offset as u64, length as usize)))
}

/// A literal search query: short, one line, no control characters.
fn check_query(query: &str) -> Result<(), CapabilityError> {
    let bad = |why: String| Err(CapabilityError::InvalidInput(why));
    if query.is_empty() {
        return bad("the query is empty".into());
    }
    if query.len() > MAX_QUERY_BYTES {
        return bad(format!(
            "the query is {} bytes; the limit is {MAX_QUERY_BYTES}",
            query.len()
        ));
    }
    if query.chars().any(char::is_control) {
        return bad("the query contains a control character".into());
    }
    Ok(())
}

fn describe(
    id: &str,
    name: &str,
    description: &str,
    inputs: Vec<CapabilityInput>,
) -> CapabilityDescriptor {
    let mut d = CapabilityDescriptor::new(CapabilityId::new(id).unwrap(), name, description)
        .with_max_input_bytes(MAX_WRITE_BYTES)
        // The project changes between requests: a read or a write is performed, never remembered.
        .without_evidence_reuse();
    d.inputs = inputs;
    d
}

/// A navigation capability: short text inputs, performed again every time.
fn describe_navigation(
    id: &str,
    name: &str,
    description: &str,
    inputs: Vec<CapabilityInput>,
) -> CapabilityDescriptor {
    let mut d = CapabilityDescriptor::new(CapabilityId::new(id).unwrap(), name, description)
        .without_evidence_reuse();
    d.inputs = inputs;
    d
}

#[async_trait::async_trait]
impl CapabilityProvider for ProjectExecutor {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(vec![
            describe_navigation(
                PROJECT_LIST,
                "List a project directory",
                "List the files and directories directly inside a project directory; path is project-relative, or \".\" or absent for the project root.",
                vec![optional_input(
                    "path",
                    "project-relative directory, or \".\" for the root",
                )],
            ),
            describe_navigation(
                PROJECT_SEARCH,
                "Search project text",
                "Find lines containing a literal text in the project's text files; returns path:line: text. Optional path narrows it to a project-relative directory or file.",
                vec![
                    input("query", "the literal text to find"),
                    optional_input(
                        "path",
                        "project-relative directory or file to search, or \".\" for all",
                    ),
                ],
            ),
            describe(
                PROJECT_READ,
                "Read a project file",
                "Read a UTF-8 project file by relative path, e.g. src/lib.rs. Over 32768 bytes, read in ranges: offset and length in bytes, length at most 32768.",
                vec![
                    input("path", "project-relative path of the file"),
                    optional_input("offset", "byte offset to start at, 0 or more; default 0"),
                    optional_input("length", "bytes to read, 1 to 32768; default 32768"),
                ],
            )
            // `read` opens the located file, seeks and reads at most MAX_READ_BYTES: it writes
            // nothing, so performing the same request again yields a fresh observation.
            .with_effect(Effect::Observation),
            describe(
                PROJECT_WRITE,
                "Write a project file",
                "Create or fully replace a UTF-8 text file of the project; content is the whole new file. The parent directory must exist.",
                vec![
                    input("path", "project-relative path of the file"),
                    input("content", "the complete new content of the file"),
                ],
            )
            // It creates or replaces a file: performing it again may change the project again.
            .with_effect(Effect::Consequential)
            // The action key reaches the executor, which records it with the write (see
            // [`journal`]). That makes the write something the project can later be asked about;
            // it does not make it safe to perform again, which only `Deduplicated` would say.
            .with_idempotency(Idempotency::Carried),
            describe_navigation(
                PROJECT_GIT_STATUS,
                "Git working-tree status",
                "Observe the repository's working-tree state: branch, staged, unstaged, untracked and deleted paths, and whether it is clean. Read-only.",
                vec![],
            ),
            describe_navigation(
                PROJECT_GIT_DIFF,
                "Git working-tree diff",
                "Observe the actual diff of tracked files, unstaged and staged. Read-only. Fails rather than truncating if too large. Untracked files are not shown.",
                vec![],
            ),
            describe_navigation(
                PROJECT_GIT_DIFF_STAT,
                "Git diff summary",
                "Observe how many files changed and how many lines were inserted and deleted, unstaged and staged. Read-only.",
                vec![],
            ),
            describe_navigation(
                PROJECT_GIT_LOG,
                "Git recent commits",
                "Observe the most recent commits of the current branch, newest first. count is how many (1 to 50; default 10). Read-only.",
                vec![optional_input("count", "how many recent commits, 1 to 50")],
            ),
        ])
    }

    async fn availability(&self, id: &CapabilityId) -> CapabilityAvailability {
        if !(matches!(
            id.as_str(),
            PROJECT_READ | PROJECT_WRITE | PROJECT_LIST | PROJECT_SEARCH
        ) || is_git_capability(id.as_str()))
        {
            return CapabilityAvailability::Unavailable(format!(
                "{id} is not a project capability"
            ));
        }
        match self.root() {
            Ok(_) => CapabilityAvailability::Available,
            Err(why) => CapabilityAvailability::Unavailable(why),
        }
    }

    async fn validate_inputs(
        &self,
        id: &CapabilityId,
        inputs: &BTreeMap<String, InputValue>,
    ) -> Result<(), CapabilityError> {
        let expected: &[&str] = match id.as_str() {
            PROJECT_READ => &["path", "offset", "length"],
            PROJECT_WRITE => &["path", "content"],
            PROJECT_LIST => &["path"],
            PROJECT_SEARCH => &["query", "path"],
            PROJECT_GIT_STATUS | PROJECT_GIT_DIFF | PROJECT_GIT_DIFF_STAT => &[],
            PROJECT_GIT_LOG => &["count"],
            other => return Err(CapabilityError::Unknown(other.to_string())),
        };
        for name in inputs.keys() {
            if !expected.contains(&name.as_str()) {
                return Err(CapabilityError::InvalidInput(format!(
                    "capability does not accept input '{name}'"
                )));
            }
        }
        if is_git_capability(id.as_str()) {
            git_count(inputs)?;
            return Ok(());
        }
        if matches!(id.as_str(), PROJECT_LIST | PROJECT_SEARCH) {
            let path = optional_text(inputs, "path")?;
            self.locate_dir(path)
                .map_err(|e| CapabilityError::InvalidInput(e.to_string()))?;
            if id.as_str() == PROJECT_SEARCH {
                check_query(text_input(inputs, "query")?)?;
            }
            return Ok(());
        }
        let path = text_input(inputs, "path")?;
        self.locate(path)
            .map_err(|e| CapabilityError::InvalidInput(e.to_string()))?;
        if id.as_str() == PROJECT_READ {
            read_range(inputs)?;
        }
        if id.as_str() == PROJECT_WRITE {
            let content = text_input(inputs, "content")?;
            if content.len() > MAX_WRITE_BYTES {
                return Err(CapabilityError::InvalidInput(format!(
                    "content is {} bytes; the limit is {MAX_WRITE_BYTES}",
                    content.len()
                )));
            }
            self.check_write_policy(path, content)
                .map_err(|e| CapabilityError::InvalidInput(e.to_string()))?;
        }
        Ok(())
    }
}

/// One canonical JSON line: sorted keys, only values Chip produced.
fn line(fields: serde_json::Value) -> String {
    serde_json::to_string(&fields).expect("a JSON value serialises")
}

fn failure(capability: &str, path: &str, error: &str) -> (bool, String) {
    (
        false,
        line(serde_json::json!({"capability": capability, "path": path, "error": error})),
    )
}

/// A failure line with structured extra fields (never a host path, never file content).
fn failure_with(
    capability: &str,
    path: &str,
    error: &str,
    extra: serde_json::Value,
) -> (bool, String) {
    let mut fields = serde_json::json!({"capability": capability, "path": path, "error": error});
    if let (Some(base), Some(extra)) = (fields.as_object_mut(), extra.as_object()) {
        base.extend(extra.clone());
    }
    (false, line(fields))
}

/// If `bytes` are valid UTF-8 once an incomplete character at each edge is set aside, how many bytes
/// that is at the start and at the end: `Some((leading, trailing))`. `None` when they are not valid
/// even then (the file is not UTF-8). A character is at most four bytes, so an edge is at most 3.
fn split_edges(bytes: &[u8]) -> Option<(usize, usize)> {
    let leading = bytes
        .iter()
        .take(3)
        .take_while(|b| (**b & 0xC0) == 0x80)
        .count();
    let body = &bytes[leading..];
    for trailing in 0..=3usize.min(body.len()) {
        if std::str::from_utf8(&body[..body.len() - trailing]).is_ok() {
            // An incomplete tail must really be an incomplete character, not stray bytes.
            let tail = &body[body.len() - trailing..];
            let incomplete =
                trailing == 0 || std::str::from_utf8(tail).is_err_and(|e| e.error_len().is_none());
            return incomplete.then_some((leading, trailing));
        }
    }
    None
}

fn io_code(e: &std::io::Error) -> &'static str {
    // A keyed write the journal refused before anything was written.
    for code in ["operation_barred", "already_applied", "key_bound_elsewhere"] {
        if e.kind() == std::io::ErrorKind::Other && e.to_string() == code {
            return code;
        }
    }
    if confine::is_symlink_refusal(e) {
        // The entry became a link after the walk checked it: the open refused it.
        return "symlink";
    }
    if e.kind() == std::io::ErrorKind::InvalidInput && e.to_string() == "not a regular file" {
        return "not_a_file";
    }
    match e.kind() {
        std::io::ErrorKind::NotFound => "not_found",
        std::io::ErrorKind::PermissionDenied => "permission_denied",
        _ => "io_error",
    }
}

impl ProjectExecutor {
    /// Returns `(succeeded, observation text)`. `range` is `None` for a whole-file read (the file
    /// must fit one bounded observation) or `(offset, length)` for one bounded range of it. Either
    /// way the path has been through [`ProjectExecutor::locate`] and the file is opened from the
    /// directory the walk holds, without following a link; at most [`MAX_READ_BYTES`] bytes are
    /// ever read.
    fn read(&self, path: &str, range: Option<(u64, usize)>) -> Result<(bool, String), PathError> {
        use std::io::Seek;
        let located = self.locate(path)?;
        match located.kind {
            Kind::Missing => return Ok(failure(PROJECT_READ, path, "not_found")),
            Kind::Directory | Kind::Other => return Ok(failure(PROJECT_READ, path, "not_a_file")),
            Kind::File => {}
        }
        let (mut file, entry) = match located.parent.open_file(located.name()) {
            Ok(opened) => opened,
            Err(e) => return Ok(failure(PROJECT_READ, path, io_code(&e))),
        };
        // A second name for a file can be a link to one outside the project (`ln ~/.ssh/id_rsa
        // notes.txt` needs no symlink); its content is not read.
        if entry.links > 1 {
            return Ok(failure(PROJECT_READ, path, "hard_linked"));
        }
        let size = entry.len;
        let (offset, limit) = range.unwrap_or((0, MAX_READ_BYTES));
        if offset > size {
            return Ok(failure_with(
                PROJECT_READ,
                path,
                "offset_beyond_end",
                serde_json::json!({"offset": offset, "file_bytes": size}),
            ));
        }
        if let Err(e) = file.seek(std::io::SeekFrom::Start(offset)) {
            return Ok(failure(PROJECT_READ, path, io_code(&e)));
        }
        // One byte more than a whole-file read may return, to tell "exactly the limit" from "over".
        let take = if range.is_some() { limit } else { limit + 1 };
        let bytes = match confine::read_up_to(&mut file, take as u64) {
            Ok(bytes) => bytes,
            Err(e) => return Ok(failure(PROJECT_READ, path, io_code(&e))),
        };
        if range.is_none() && bytes.len() > MAX_READ_BYTES {
            return Ok(failure(PROJECT_READ, path, "too_large"));
        }
        let content = match String::from_utf8(bytes) {
            Ok(content) => content,
            Err(e) if range.is_some() => {
                // Say whether it is the range's edges that split a character, so the caller can move
                // them; the bytes themselves are never trimmed, replaced or returned.
                let returned = e.as_bytes().len() as u64;
                return Ok(match split_edges(e.as_bytes()) {
                    // A partial character at the file's own start or end is the file's fault, not
                    // the range's.
                    Some((leading, trailing))
                        if (leading == 0 || offset > 0)
                            && (trailing == 0 || offset + returned < size) =>
                    {
                        failure_with(
                            PROJECT_READ,
                            path,
                            "range_splits_character",
                            serde_json::json!({
                                "offset": offset,
                                "file_bytes": size,
                                "leading_partial_bytes": leading,
                                "trailing_partial_bytes": trailing,
                            }),
                        )
                    }
                    _ => failure(PROJECT_READ, path, "not_utf8"),
                });
            }
            Err(_) => return Ok(failure(PROJECT_READ, path, "not_utf8")),
        };
        let head = line(serde_json::json!({
            "capability": PROJECT_READ,
            "path": path,
            "offset": offset,
            "bytes": content.len(),
            "file_bytes": size,
            // The observation is the whole file only if it starts at the start and holds all of it.
            "complete": offset == 0 && content.len() as u64 == size,
            // Of the bytes returned, not of the file: a range's hash is the hash of the range.
            "sha256": hex(&Sha256::digest(content.as_bytes())),
        }));
        Ok((true, format!("{head}\n--- content ---\n{content}")))
    }

    /// The write policy, checked in validation and again at the moment of the write: a protected
    /// path is never written, and a manifest write may not change which build script runs.
    fn check_write_policy(&self, path: &str, content: &str) -> Result<(), PathError> {
        let parts = components(path)?;
        if let Some(rule) = policy::protected(&parts) {
            return Err(PathError::Protected {
                path: path.to_string(),
                rule,
            });
        }
        if policy::is_manifest(&parts) {
            let located = self.locate(path)?;
            let old = match located.kind {
                Kind::File => {
                    let (mut file, _) = located
                        .parent
                        .open_file(located.name())
                        .map_err(|_| PathError::Unreadable)?;
                    let bytes = confine::read_up_to(&mut file, MAX_WRITE_BYTES as u64 + 1)
                        .map_err(|_| PathError::Unreadable)?;
                    // A manifest that cannot be read as text cannot be compared: refused.
                    Some(String::from_utf8(bytes).map_err(|_| PathError::Unreadable)?)
                }
                _ => None,
            };
            if let Some(rule) = policy::manifest_build_change(old.as_deref(), content) {
                return Err(PathError::Protected {
                    path: path.to_string(),
                    rule,
                });
            }
        }
        Ok(())
    }

    fn write(
        &self,
        path: &str,
        content: &str,
        key: Option<&str>,
    ) -> Result<(bool, String), PathError> {
        self.write_with(path, content, key, &|located| {
            let (mut file, _) = located.parent.open_file(located.name())?;
            confine::read_up_to(&mut file, MAX_WRITE_BYTES as u64 + 1)
        })
    }

    /// `read_back` is how the target is read after the write: the real filesystem in production, a
    /// stand-in in the one test that must show a write that did not land is not reported as one.
    fn write_with(
        &self,
        path: &str,
        content: &str,
        key: Option<&str>,
        read_back: &dyn Fn(&Located) -> std::io::Result<Vec<u8>>,
    ) -> Result<(bool, String), PathError> {
        if content.len() > MAX_WRITE_BYTES {
            return Err(PathError::TooLong);
        }
        self.check_write_policy(path, content)?;
        let located = self.locate(path)?;
        if !located.parent_exists {
            return Ok(failure(PROJECT_WRITE, path, "parent_missing"));
        }
        let existing = match located.kind {
            Kind::Missing => false,
            Kind::File => true,
            Kind::Directory => return Ok(failure(PROJECT_WRITE, path, "is_a_directory")),
            Kind::Other => return Ok(failure(PROJECT_WRITE, path, "not_a_file")),
        };
        let previous = existing
            .then(|| located.parent.open_file(located.name()).ok())
            .flatten()
            .and_then(|(mut file, _)| {
                confine::read_up_to(&mut file, MAX_WRITE_BYTES as u64 + 1).ok()
            });
        let mode = located.entry.and_then(|e| e.mode).map(|m| m & 0o777);
        // A write that carries an action key is journaled (see [`journal`]) so that, if its result
        // is lost, the project can say whether this very write happened. Without a key, or off
        // Unix, it is the plain atomic write it always was.
        #[cfg(unix)]
        let written = match key.filter(|k| journal::valid_key(k)) {
            Some(key) => match self.root() {
                Ok(root) => journal::keyed_write(
                    root,
                    &located.parent,
                    located.name(),
                    path,
                    content.as_bytes(),
                    mode,
                    key,
                ),
                Err(e) => Err(std::io::Error::other(e)),
            },
            None => atomic_write(&located.parent, located.name(), content.as_bytes(), mode),
        };
        #[cfg(not(unix))]
        let written = {
            let _ = key;
            atomic_write(&located.parent, located.name(), content.as_bytes(), mode)
        };
        if let Err(e) = written {
            return Ok(failure(PROJECT_WRITE, path, io_code(&e)));
        }
        // Success is what the filesystem now holds, not that a write was attempted.
        match read_back(&located) {
            Ok(after) if after == content.as_bytes() => {}
            Ok(_) => return Ok(failure(PROJECT_WRITE, path, "readback_mismatch")),
            Err(e) => return Ok(failure(PROJECT_WRITE, path, io_code(&e))),
        }
        let changed = previous.as_deref() != Some(content.as_bytes());
        let head = line(serde_json::json!({
            "capability": PROJECT_WRITE,
            "path": path,
            "operation": if existing { "replaced" } else { "created" },
            "bytes_written": content.len(),
            "changed": changed,
            "sha256": hex(&Sha256::digest(content.as_bytes())),
        }));
        Ok((true, head))
    }
}

/// A directory's entries as Chip lists them: no symlinks, nothing reserved, and nothing whose name
/// the path rules would not let a model address anyway. Sorted by name.
struct Visible {
    entries: Vec<(String, confine::Entry)>,
    symlinks: usize,
    unaddressable: usize,
}

fn visible_entries(dir: &confine::Dir) -> std::io::Result<Visible> {
    let (mut entries, mut symlinks, mut unaddressable) = (Vec::new(), 0, 0);
    for (name, entry) in dir.entries()? {
        let Ok(name) = name else {
            unaddressable += 1;
            continue;
        };
        if entry.kind == confine::EntryKind::Symlink {
            symlinks += 1;
        } else if components(&name).is_err() {
            unaddressable += 1;
        } else if matches!(
            entry.kind,
            confine::EntryKind::File | confine::EntryKind::Directory
        ) {
            entries.push((name, entry));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Visible {
        entries,
        symlinks,
        unaddressable,
    })
}

fn child(base: Option<&str>, name: &str) -> String {
    match base {
        Some(b) => format!("{b}/{name}"),
        None => name.to_string(),
    }
}

/// The state of one search, with every bound it keeps.
struct Search<'a> {
    query: &'a str,
    files: usize,
    rows: Vec<String>,
    bytes: usize,
    truncated: bool,
    non_utf8: usize,
    large: usize,
    hard_linked: usize,
}

impl Search<'_> {
    fn file(&mut self, dir: &confine::Dir, name: &str, rel: &str, entry: confine::Entry) {
        if self.files >= MAX_SEARCH_FILES {
            self.truncated = true;
            return;
        }
        if entry.len > MAX_SEARCH_FILE_BYTES {
            self.large += 1;
            return;
        }
        if entry.links > 1 {
            self.hard_linked += 1;
            return;
        }
        self.files += 1;
        let Ok((mut file, opened)) = dir.open_file(name) else {
            return;
        };
        if opened.links > 1 {
            self.hard_linked += 1;
            return;
        }
        let Ok(bytes) = confine::read_up_to(&mut file, MAX_SEARCH_FILE_BYTES) else {
            return;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            self.non_utf8 += 1;
            return;
        };
        for (i, line) in text.lines().enumerate() {
            if !line.contains(self.query) {
                continue;
            }
            let shown: String = line.chars().take(MAX_LINE_CHARS).collect();
            let row = format!("{rel}:{}: {shown}", i + 1);
            if self.rows.len() >= MAX_MATCHES || self.bytes + row.len() + 1 > MAX_OUTPUT_BYTES {
                self.truncated = true;
                return;
            }
            self.bytes += row.len() + 1;
            self.rows.push(row);
        }
    }

    fn directory(&mut self, dir: &confine::Dir, rel: Option<&str>) -> std::io::Result<()> {
        for (name, entry) in visible_entries(dir)?.entries {
            if self.truncated {
                return Ok(());
            }
            // Hidden entries and build output are not searched (an explicitly named directory is).
            if name.starts_with('.') {
                continue;
            }
            let rel = child(rel, &name);
            if entry.kind == confine::EntryKind::Directory {
                if name != "target" && name != "node_modules" {
                    // A directory replaced by a link since it was listed is refused by the open.
                    if let Ok(sub) = dir.open_dir(&name) {
                        self.directory(&sub, Some(&rel))?;
                    }
                }
            } else {
                self.file(dir, &name, &rel, entry);
            }
        }
        Ok(())
    }
}

impl ProjectExecutor {
    fn list(&self, path: Option<&str>) -> Result<(bool, String), PathError> {
        let shown = path.map(directory_spelling).unwrap_or(".");
        let located = self.locate_dir(path)?;
        match located.kind {
            Kind::Missing => return Ok(failure(PROJECT_LIST, shown, "not_found")),
            Kind::File | Kind::Other => return Ok(failure(PROJECT_LIST, shown, "not_a_directory")),
            Kind::Directory => {}
        }
        let base = (shown != ".").then_some(shown);
        let Visible {
            entries,
            symlinks,
            unaddressable,
        } = match located.open_dir().and_then(|dir| visible_entries(&dir)) {
            Ok(found) => found,
            Err(e) => return Ok(failure(PROJECT_LIST, shown, io_code(&e))),
        };
        let (mut rows, mut bytes, mut truncated) = (Vec::new(), 0usize, false);
        for (name, entry) in &entries {
            let rel = child(base, name);
            let row = if entry.kind == confine::EntryKind::Directory {
                format!("dir {rel}")
            } else {
                format!("file {rel} {}", entry.len)
            };
            if rows.len() >= MAX_LIST_ENTRIES || bytes + row.len() + 1 > MAX_OUTPUT_BYTES {
                truncated = true;
                break;
            }
            bytes += row.len() + 1;
            rows.push(row);
        }
        let head = line(serde_json::json!({
            "capability": PROJECT_LIST,
            "path": shown,
            "entries": rows.len(),
            "truncated": truncated,
            "skipped_symlinks": symlinks,
            "skipped_unaddressable": unaddressable,
        }));
        Ok((
            true,
            format!("{head}\n--- entries ---\n{}", rows.join("\n")),
        ))
    }

    fn search(&self, query: &str, path: Option<&str>) -> Result<(bool, String), PathError> {
        let shown = path.map(directory_spelling).unwrap_or(".");
        let located = self.locate_dir(path)?;
        let mut search = Search {
            query,
            files: 0,
            rows: Vec::new(),
            bytes: 0,
            truncated: false,
            non_utf8: 0,
            large: 0,
            hard_linked: 0,
        };
        match located.kind {
            Kind::Missing => return Ok(failure(PROJECT_SEARCH, shown, "not_found")),
            Kind::Other => return Ok(failure(PROJECT_SEARCH, shown, "not_a_file_or_directory")),
            Kind::File => {
                if let Some(entry) = located.entry {
                    search.file(&located.parent, located.name(), shown, entry);
                }
            }
            Kind::Directory => {
                let base = (shown != ".").then_some(shown);
                if let Err(e) = located
                    .open_dir()
                    .and_then(|dir| search.directory(&dir, base))
                {
                    return Ok(failure(PROJECT_SEARCH, shown, io_code(&e)));
                }
            }
        }
        let mut fields = serde_json::json!({
            "capability": PROJECT_SEARCH,
            "query": query,
            "path": shown,
            "files_examined": search.files,
            "matches": search.rows.len(),
            "truncated": search.truncated,
            "skipped_non_utf8": search.non_utf8,
            "skipped_large": search.large,
        });
        // Present only when a file was skipped for it, so an observation of a project without hard
        // links is byte-for-byte what it was before the rule existed.
        if search.hard_linked > 0 {
            fields["skipped_hard_linked"] = search.hard_linked.into();
        }
        let head = line(fields);
        Ok((
            true,
            format!("{head}\n--- matches ---\n{}", search.rows.join("\n")),
        ))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The prefix of every temporary file a write creates. A name with it is withheld from every
/// capability, and [`ProjectLock::acquire`] removes the ones a crashed write left behind.
pub const TEMP_PREFIX: &str = ".chip-write-";

/// Writes `content` next to `name` in `dir` and renames it over `name`: the target is either what
/// it was or exactly `content`, never a prefix of it. The temporary file is created with its final
/// mode (the target's, or 0644 for a new file), so the content is never readable more widely than
/// the file it replaces. A failure removes the temporary file.
fn atomic_write(
    dir: &confine::Dir,
    name: &str,
    content: &[u8],
    mode: Option<u32>,
) -> std::io::Result<()> {
    let temporary = format!(
        "{TEMP_PREFIX}{}-{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let result = (|| {
        let mut file = dir.create_new(&temporary, mode.unwrap_or(0o644))?;
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        dir.rename(&temporary, name)
    })();
    if result.is_err() {
        let _ = dir.remove_file(&temporary);
    } else {
        let _ = dir.sync();
    }
    result
}

#[async_trait::async_trait]
impl Executor for ProjectExecutor {
    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let capability = request.intent.as_str();
        let invalid = |why: String| ExecutionError::InvalidRequest(why);
        if !(matches!(
            capability,
            PROJECT_READ | PROJECT_WRITE | PROJECT_LIST | PROJECT_SEARCH
        ) || is_git_capability(capability))
        {
            return Err(invalid(format!(
                "'{capability}' is not a capability this executor provides"
            )));
        }
        if self.root().is_err() {
            return Err(ExecutionError::ExecutorUnavailable(
                "the project root is unavailable".into(),
            ));
        }
        // Re-validated at the moment of use: the executor does not rely on having been asked
        // nicely by whoever validated first.
        let id = CapabilityId::new(capability).map_err(|e| invalid(e.to_string()))?;
        self.validate_inputs(&id, &request.inputs)
            .await
            .map_err(|e| invalid(e.to_string()))?;
        if is_git_capability(capability) {
            let count = git_count(&request.inputs).map_err(|e| invalid(e.to_string()))?;
            let root = self.root().map_err(|e| invalid(e))?.to_path_buf();
            return Ok(match git::observe(&root, capability, count).await {
                (true, text) => ExecutionResult::success(request.id, text),
                (false, text) => ExecutionResult::failure(request.id, text),
            });
        }
        if matches!(capability, PROJECT_LIST | PROJECT_SEARCH) {
            let path = optional_text(&request.inputs, "path")
                .map_err(|e| invalid(e.to_string()))?
                .map(str::to_string);
            let query = (capability == PROJECT_SEARCH)
                .then(|| text_input(&request.inputs, "query").map(str::to_string))
                .transpose()
                .map_err(|e| invalid(e.to_string()))?;
            let this = self.clone();
            let done = tokio::task::spawn_blocking(move || match query {
                Some(q) => this.search(&q, path.as_deref()),
                None => this.list(path.as_deref()),
            })
            .await
            .map_err(|_| {
                ExecutionError::ExecutionFailed("the file operation did not complete".into())
            })?
            .map_err(|e| invalid(e.to_string()))?;
            return Ok(match done {
                (true, text) => ExecutionResult::success(request.id, text),
                (false, text) => ExecutionResult::failure(request.id, text),
            });
        }
        let path = text_input(&request.inputs, "path")
            .map_err(|e| invalid(e.to_string()))?
            .to_string();
        let range = if capability == PROJECT_READ {
            read_range(&request.inputs).map_err(|e| invalid(e.to_string()))?
        } else {
            None
        };
        let key = request.idempotency_key.clone();
        let content = (capability == PROJECT_WRITE)
            .then(|| text_input(&request.inputs, "content").map(str::to_string))
            .transpose()
            .map_err(|e| invalid(e.to_string()))?;
        let this = self.clone();
        let done = tokio::task::spawn_blocking(move || match content {
            Some(content) => this.write(&path, &content, key.as_deref()),
            None => this.read(&path, range),
        })
        .await
        .map_err(|_| {
            // The blocking task did not return, so a write may have been applied in part or in
            // whole. A read has no effect to be unsure of.
            if capability == PROJECT_WRITE {
                ExecutionError::OutcomeUnknown(
                    "the file write did not complete; it may have taken effect".into(),
                )
            } else {
                ExecutionError::ExecutionFailed("the file operation did not complete".into())
            }
        })?
        .map_err(|e| invalid(e.to_string()))?;
        Ok(match done {
            (true, text) => ExecutionResult::success(request.id, text),
            (false, text) => ExecutionResult::failure(request.id, text),
        })
    }
}

// ---- what a recorded observation says, and the audit's independent check of it ---------------------

/// The canonical first line of a project observation, parsed.
fn project_line(observation: &Observation) -> Option<serde_json::Value> {
    let first = observation.output.as_deref()?.lines().next()?;
    let value: serde_json::Value = serde_json::from_str(first).ok()?;
    matches!(
        value.get("capability")?.as_str()?,
        PROJECT_READ | PROJECT_WRITE | PROJECT_LIST | PROJECT_SEARCH
    )
    .then_some(value)
}

/// What a project observation says about one successful write, for utility reporting: the path,
/// and whether the write changed the file.
pub fn write_summary(observation: &Observation) -> Option<(String, bool)> {
    if observation.kind != ObservationKind::ExecutionCompleted {
        return None;
    }
    let v = project_line(observation)?;
    (v["capability"] == PROJECT_WRITE).then(|| {
        (
            v["path"].as_str().unwrap_or_default().to_string(),
            v["changed"] == true,
        )
    })
}

/// The path of a successful write that replaced an existing file with different content. A
/// created file, an unchanged rewrite and a failed write are `None`.
pub fn replaced_file(observation: &Observation) -> Option<String> {
    if observation.kind != ObservationKind::ExecutionCompleted {
        return None;
    }
    let v = project_line(observation)?;
    (v["capability"] == PROJECT_WRITE && v["operation"] == "replaced" && v["changed"] == true)
        .then(|| v["path"].as_str().unwrap_or_default().to_string())
}

/// Checks a recorded path with its own, separate reading of the rules: not trusting that the
/// executor applied them.
fn lexically_inside(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..")
}

#[derive(Debug)]
struct PathEscape {
    root: PathBuf,
}

impl ObservationInvariant for PathEscape {
    fn name(&self) -> &'static str {
        PATH_ESCAPE
    }

    /// A project observation naming a path that is not project-relative and inside, a listing or
    /// search that returns one, or a read, listing or search that really came from outside the
    /// root.
    fn violations(&self, observation: &Observation) -> usize {
        let Some(v) = project_line(observation) else {
            return 0;
        };
        let capability = v["capability"].as_str().unwrap_or_default();
        let navigation = matches!(capability, PROJECT_LIST | PROJECT_SEARCH);
        let path = v["path"].as_str().unwrap_or_default();
        if !(lexically_inside(path) || (navigation && path == ".")) {
            return 1;
        }
        if navigation {
            let outside = returned_paths(observation, capability)
                .iter()
                .filter(|p| !lexically_inside(p))
                .count();
            if outside > 0 {
                return outside;
            }
        }
        let succeeded = observation.kind == ObservationKind::ExecutionCompleted;
        let reads_from_disk = capability == PROJECT_READ || navigation;
        usize::from(succeeded && reads_from_disk && !really_inside(&self.root, path))
    }
}

/// The paths a listing or a search observation returns, from its body.
fn returned_paths(observation: &Observation, capability: &str) -> Vec<String> {
    let body = observation
        .output
        .as_deref()
        .map(|t| t.lines().skip(2))
        .into_iter()
        .flatten();
    body.filter_map(|row| match capability {
        PROJECT_LIST => row.split(' ').nth(1).map(str::to_string),
        PROJECT_SEARCH => row.split(':').next().map(str::to_string),
        _ => None,
    })
    .collect()
}

#[derive(Debug)]
struct OutOfRootWrite {
    root: PathBuf,
}

impl ObservationInvariant for OutOfRootWrite {
    fn name(&self) -> &'static str {
        OUT_OF_ROOT_WRITE
    }

    /// A successful write whose target, resolved for real, is not inside the root (or that cannot
    /// be shown to be: it fails closed).
    fn violations(&self, observation: &Observation) -> usize {
        let Some(v) = project_line(observation) else {
            return 0;
        };
        let wrote = v["capability"] == PROJECT_WRITE
            && observation.kind == ObservationKind::ExecutionCompleted;
        let path = v["path"].as_str().unwrap_or_default();
        usize::from(wrote && !(lexically_inside(path) && really_inside(&self.root, path)))
    }
}

/// No project observation contains the host path of the project root.
#[derive(Debug)]
struct HostPathLeak {
    spellings: Vec<String>,
}

impl ObservationInvariant for HostPathLeak {
    fn name(&self) -> &'static str {
        HOST_PATH_LEAK
    }

    fn violations(&self, observation: &Observation) -> usize {
        if project_line(observation).is_none() && !git::is_git_observation(observation) {
            return 0;
        }
        let text = observation.output.as_deref().unwrap_or_default();
        usize::from(self.spellings.iter().any(|s| text.contains(s.as_str())))
    }
}

/// Listings and searches match the real filesystem, read again by the audit itself.
///
/// Every listed entry and every reported match is checked against the files as they are. A scope
/// that a later *successful write* in the same trajectory touched is not checked: that change
/// legitimately outdates what the earlier observation said.
#[derive(Debug)]
struct NavigationMismatch {
    root: PathBuf,
}

impl NavigationMismatch {
    /// What a listing of `dir` should hold, as `kind path size` rows, read independently.
    fn real_entries(&self, shown: &str) -> Option<std::collections::BTreeSet<String>> {
        let dir = if shown == "." {
            self.root.clone()
        } else {
            self.root.join(shown)
        };
        let mut rows = std::collections::BTreeSet::new();
        for entry in fs::read_dir(dir).ok()? {
            let entry = entry.ok()?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let meta = fs::symlink_metadata(entry.path()).ok()?;
            if meta.file_type().is_symlink() || components(&name).is_err() {
                continue;
            }
            let rel = if shown == "." {
                name
            } else {
                format!("{shown}/{name}")
            };
            if meta.is_dir() {
                rows.insert(format!("dir {rel}"));
            } else if meta.is_file() {
                rows.insert(format!("file {rel} {}", meta.len()));
            }
        }
        Some(rows)
    }

    fn check_listing(
        &self,
        o: &Observation,
        v: &serde_json::Value,
        later: &[String],
        foreign_later: bool,
    ) -> usize {
        let shown = v["path"].as_str().unwrap_or(".");
        let parent_of = |p: &str| p.rsplit_once('/').map_or(".", |(d, _)| d).to_string();
        if later.iter().any(|w| parent_of(w) == shown) {
            return 0;
        }
        if foreign_later {
            // Something other than a project capability ran afterwards (a test run can create and
            // remove files), so the set may legitimately have changed: every entry listed must
            // still be what it was said to be, and no more is claimed.
            let wrong = o
                .output
                .as_deref()
                .unwrap_or_default()
                .lines()
                .skip(2)
                .filter(|row| {
                    let mut parts = row.split(' ');
                    let (kind, rel) = (parts.next(), parts.next());
                    let Some(rel) = rel.filter(|r| lexically_inside(r)) else {
                        return true;
                    };
                    match fs::symlink_metadata(self.root.join(rel)) {
                        Ok(m) => {
                            m.file_type().is_symlink()
                                || (kind == Some("dir")) != m.is_dir()
                                || (kind == Some("file")) != m.is_file()
                        }
                        Err(_) => true,
                    }
                })
                .count();
            return usize::from(wrong > 0);
        }
        let Some(real) = self.real_entries(shown) else {
            return 1;
        };
        let observed: std::collections::BTreeSet<String> = o
            .output
            .as_deref()
            .map(|t| t.lines().skip(2).map(str::to_string).collect())
            .unwrap_or_default();
        let ok = if v["truncated"] == true {
            observed.is_subset(&real)
        } else {
            observed == real
        };
        usize::from(!ok)
    }

    fn check_search(
        &self,
        o: &Observation,
        v: &serde_json::Value,
        later: &[String],
        foreign_later: bool,
    ) -> usize {
        let query = v["query"].as_str().unwrap_or_default();
        let mut bad = 0;
        for row in o.output.as_deref().unwrap_or_default().lines().skip(2) {
            let mut parts = row.splitn(3, ':');
            let (Some(rel), Some(number), Some(text)) = (parts.next(), parts.next(), parts.next())
            else {
                bad += 1;
                continue;
            };
            if later.iter().any(|w| w == rel) {
                continue;
            }
            if foreign_later {
                // The file's text may have changed since; it must at least still be a real file
                // of the project.
                let real = lexically_inside(rel)
                    && really_inside(&self.root, rel)
                    && fs::metadata(self.root.join(rel)).is_ok_and(|m| m.is_file());
                bad += usize::from(!real);
                continue;
            }
            let shown = text.strip_prefix(' ').unwrap_or(text);
            let real_line = number
                .parse::<usize>()
                .ok()
                .filter(|n| *n >= 1 && lexically_inside(rel) && really_inside(&self.root, rel))
                .and_then(|n| {
                    let content = fs::read_to_string(self.root.join(rel)).ok()?;
                    content.lines().nth(n - 1).map(str::to_string)
                });
            let genuine = real_line.is_some_and(|l| {
                l.contains(query) && l.chars().take(MAX_LINE_CHARS).collect::<String>() == shown
            });
            bad += usize::from(!genuine);
        }
        bad
    }
}

impl ObservationInvariant for NavigationMismatch {
    fn name(&self) -> &'static str {
        NAVIGATION_MISMATCH
    }

    fn violations(&self, _observation: &Observation) -> usize {
        0
    }

    fn violations_in_trajectory(&self, observations: &[Observation]) -> usize {
        let mut bad = 0;
        for (i, o) in observations.iter().enumerate() {
            if o.kind != ObservationKind::ExecutionCompleted {
                continue;
            }
            let Some(v) = project_line(o) else { continue };
            let later: Vec<String> = observations[i + 1..]
                .iter()
                .filter_map(write_summary)
                .map(|(path, _)| path)
                .collect();
            // An execution that is not a project capability (a test run, say) may have changed the
            // project in ways the audit cannot see.
            let foreign_later = observations[i + 1..]
                .iter()
                .any(|later| project_line(later).is_none());
            match v["capability"].as_str() {
                Some(PROJECT_LIST) => bad += self.check_listing(o, &v, &later, foreign_later),
                Some(PROJECT_SEARCH) => bad += self.check_search(o, &v, &later, foreign_later),
                _ => {}
            }
        }
        bad
    }
}

fn really_inside(root: &Path, path: &str) -> bool {
    let (Ok(root), Ok(real)) = (fs::canonicalize(root), fs::canonicalize(root.join(path))) else {
        return false;
    };
    real.starts_with(root)
}

/// Audit: no project observation names a path outside the project.
pub fn path_escape_invariant(root: impl AsRef<Path>) -> Arc<dyn ObservationInvariant> {
    Arc::new(PathEscape {
        root: root.as_ref().to_path_buf(),
    })
}

/// Audit: no project observation contains the host path of the project root.
pub fn host_path_leak_invariant(root: impl AsRef<Path>) -> Arc<dyn ObservationInvariant> {
    let given = root.as_ref().to_string_lossy().into_owned();
    let mut spellings = vec![given];
    if let Ok(real) = fs::canonicalize(root.as_ref()) {
        spellings.push(real.to_string_lossy().into_owned());
    }
    spellings.retain(|s| s.len() > 1);
    spellings.dedup();
    Arc::new(HostPathLeak { spellings })
}

/// Audit: listings and searches report what the filesystem really holds.
pub fn navigation_mismatch_invariant(root: impl AsRef<Path>) -> Arc<dyn ObservationInvariant> {
    Arc::new(NavigationMismatch {
        root: root.as_ref().to_path_buf(),
    })
}

/// Audit: no successful write landed outside the project root.
pub fn out_of_root_write_invariant(root: impl AsRef<Path>) -> Arc<dyn ObservationInvariant> {
    Arc::new(OutOfRootWrite {
        root: root.as_ref().to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lexical_rules_alone_refuse_every_unsafe_path() {
        for (path, expected) in [
            ("", PathError::Empty),
            ("/etc/passwd", PathError::Absolute),
            ("../x", PathError::BadComponent("..".into())),
            ("a/../b", PathError::BadComponent("..".into())),
            ("a/./b", PathError::BadComponent(".".into())),
            ("a//b", PathError::BadComponent("".into())),
            (".git/config", PathError::Reserved(".git".into())),
            (".env.local", PathError::Reserved(".env.local".into())),
            (
                ".stack-agent/learning/runs/x.json",
                PathError::Reserved(".stack-agent".into()),
            ),
        ] {
            assert_eq!(components(path).unwrap_err(), expected, "{path:?}");
        }
        assert_eq!(components("src/lib.rs").unwrap(), ["src", "lib.rs"]);
    }

    #[test]
    fn a_write_that_did_not_land_as_requested_is_not_reported_as_one() {
        let root =
            std::env::temp_dir().join(format!("chip-project-readback-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let p = ProjectExecutor::new(&root);
        let (ok, text) = p
            .write_with("a.txt", "wanted", None, &|_| Ok(b"something else".to_vec()))
            .unwrap();
        assert!(!ok, "{text}");
        assert!(
            text.contains("readback_mismatch") && !text.contains("bytes_written"),
            "{text}"
        );
        let (ok, text) = p
            .write_with("b.txt", "wanted", None, &|_| {
                Err(std::io::Error::other("gone"))
            })
            .unwrap();
        assert!(!ok && text.contains("io_error"), "{text}");
        assert!(
            p.write("c.txt", "wanted", None).unwrap().0,
            "the real read-back agrees"
        );
    }
}
