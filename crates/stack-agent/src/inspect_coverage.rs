//! Inspection coverage: what an inspection has actually looked at, against what a question needs.
//!
//! A grounded answer cites a file Chip observed. That says nothing about whether enough was
//! observed to answer: an answer about a workspace can cite one manifest and be wrong about the
//! other 27. This module adds a second, separate check for inspections whose submitter declares a
//! coverage contract (`--require-coverage cargo-workspace`). The contract is explicit because a
//! goal's scope is not inferred from its words.
//!
//! Everything here is derived from recorded `project.list` and `project.read` observations and
//! from nothing the model said:
//!
//! * the root `Cargo.toml`, once read completely, is parsed as TOML, and `[workspace].members` and
//!   `exclude` are taken from the parsed arrays, never from comments or prose;
//! * a member counts as *read* only when a complete `project.read` of its `Cargo.toml` succeeded and
//!   parses; a path that was merely listed or derived is *outstanding*, and one that was tried and
//!   failed is *unresolved*;
//! * a glob member (`crates/*`) is expanded only through a recorded `project.list` of its parent,
//!   and only for the one form `parent/prefix*suffix`; any other glob, an exclude pattern that is
//!   not understood, and a path the project rules refuse are recorded as unresolved rather than
//!   guessed.
//!
//! The answer checks that follow are deliberately narrow and mechanical. They are not a general
//! truth engine. They look for exactly two things in the model's text and compare each with parsed
//! manifest data: a glob offered as workspace members that the manifest does not declare, and a
//! "depends on" claim that is true only of dev-dependencies. A claim these cannot check is neither
//! supported nor contradicted by them, and an unread manifest never makes a claim false. Passing
//! them shows coverage and the absence of those two contradictions. It does not show the answer is
//! correct, and an accepted answer is still grounded, not verified.

use std::collections::{BTreeMap, BTreeSet};

use chip_core::{AnswerPredicate, ExecutionStatus, Observation, ObservationKind};
use chip_project::{PROJECT_LIST, PROJECT_READ};

use crate::inspect_context::body;
use crate::software_work::{GroundedAnswer, observation_header};

/// A coverage requirement the submitter declares for an inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageContract {
    /// The root `Cargo.toml` and the `Cargo.toml` of every member `[workspace]` declares, read
    /// completely, before an answer may complete.
    CargoWorkspace,
}

impl CoverageContract {
    pub const CARGO_WORKSPACE: &'static str = "cargo-workspace";

    pub fn name(self) -> &'static str {
        match self {
            Self::CargoWorkspace => Self::CARGO_WORKSPACE,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        (text.trim() == Self::CARGO_WORKSPACE).then_some(Self::CargoWorkspace)
    }
}

/// The size the instructions ask a summary to stay within. The validator's limit
/// ([`chip_core::MAX_TEXT_BYTES`], 1,024 bytes of UTF-8) is the authority; the target leaves room
/// for a model that cannot count bytes exactly and for JSON escapes, which cost more than one byte.
pub const SUMMARY_TARGET_BYTES: usize = 700;

impl CoverageContract {
    /// What the model is told for an inspection under this contract, in place of the generic
    /// inspection answer instructions. It states the contract, the answer's shape and its size, and
    /// that reaching the answer step does not mean coverage is satisfied: Chip checks that.
    pub fn completion_rule(self) -> String {
        let limit = chip_core::MAX_TEXT_BYTES;
        let target = SUMMARY_TARGET_BYTES;
        format!(
            "This goal asks only for read-only inspection: do not modify any file. Treat successful observations as facts only for what they directly show; do not state unsupported claims; failed operations are not evidence. \
COVERAGE CONTRACT (cargo-workspace): Chip accepts an answer only after the root Cargo.toml and the Cargo.toml of every member that [workspace].members declares have each been read completely with project.read; the context lists what is still unread. Listing crate names does not satisfy it, and reaching the answer step does not mean it is satisfied: Chip checks, and refuses an answer while any required manifest is unread. Read the unread members, one project.read per request, then answer. Use project.read for known paths; do not search for implementation symbols. \
ANSWER (decision complete): the summary is plain prose of at most {target} bytes. The hard limit is {limit} bytes of UTF-8 for the whole summary string; it is bytes, not words or tokens, and a longer summary makes the reply invalid, so stay well under it. In this order: (1) structure: state the declared member count as digits, for example \"28 members\", and the groups the manifests show (crates that share a prefix or a role); (2) responsibilities: what the major crates or groups do, only as far as package descriptions and manifests establish it, otherwise say it is not established; (3) dependencies: the main internal dependencies between groups or key crates, writing dev for a dev-dependency and build for a build-dependency and never calling a dev-dependency a dependency; (4) one short note on what the manifests do not show. Do not list every crate: name a crate only where it explains a responsibility or a dependency. Do not guess a responsibility from a crate name. Cite at least one project-relative file you read, such as Cargo.toml. \
Emit exactly one JSON object, with no Markdown, code fence, wrapper, or extra fields, and nothing before or after it, in one of these shapes: {{\"decision\":\"complete\",\"summary\":\"<the summary>\"}}; {{\"decision\":\"request_capability\",\"capability\":\"project.read\",\"inputs\":{{\"path\":\"crates/x/Cargo.toml\"}}}}; {{\"decision\":\"escalate\",\"reason\":\"<why a person is needed>\"}}; {{\"decision\":\"block\",\"reason\":\"<why work cannot proceed>\"}}. Only request_capability may contain a nested object. Answer as soon as every declared member manifest has been read."
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    Normal,
    Dev,
    Build,
}

/// One dependency a manifest declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    /// The key in the table; the package it names when `package = "..."` renames it.
    pub name: String,
    pub kind: DepKind,
    /// A `path = "..."` declaration, or the path the root's `[workspace.dependencies]` gives it.
    pub path: Option<String>,
    pub inherited: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberState {
    /// Derived from a declaration and not yet read.
    Outstanding,
    /// A complete `project.read` of its manifest succeeded and parsed (observation number).
    Read(usize),
    /// Tried or interpreted, and not usable: the reason.
    Unresolved(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberSource {
    /// A literal entry of `[workspace].members`.
    Declared,
    /// A directory a recorded listing showed matching this glob entry.
    Glob { pattern: String, listing: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub path: String,
    pub source: MemberSource,
    pub state: MemberState,
    pub package: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub dependencies: Vec<Dependency>,
}

impl Member {
    pub fn manifest(&self) -> String {
        format!("{}/Cargo.toml", self.path)
    }

    /// One line that states what this member's parsed manifest establishes: package and version,
    /// a short description, the internal dependencies (a `path` declaration, written or inherited
    /// from the root) by kind, and only a count of the external ones. `#N` is the number of the
    /// observation it was parsed from. It is derived from that observation's TOML and nothing else;
    /// it names no dependency the manifest does not declare. `None` unless the manifest was read
    /// and parsed.
    pub fn compact_line(&self) -> Option<String> {
        let MemberState::Read(number) = self.state else {
            return None;
        };
        let names = |kind: DepKind, internal: bool| -> Vec<&str> {
            let mut v: Vec<&str> = self
                .dependencies
                .iter()
                .filter(|d| d.kind == kind && d.path.is_some() == internal)
                .map(|d| d.name.as_str())
                .collect();
            v.dedup();
            v
        };
        let mut line = format!("#{number} {}", self.path);
        if let Some(package) = &self.package {
            line.push_str(&format!(" [{package}"));
            if let Some(version) = &self.version {
                line.push_str(&format!(" {version}"));
            }
            line.push(']');
        }
        if let Some(text) = &self.description {
            let short: String = text.chars().take(70).collect();
            line.push_str(&format!(
                " \"{short}{}\"",
                if short.len() < text.len() { "…" } else { "" }
            ));
        }
        let (normal, dev, build) = (
            names(DepKind::Normal, true),
            names(DepKind::Dev, true),
            names(DepKind::Build, true),
        );
        if !normal.is_empty() {
            line.push_str(&format!(" -> {}", normal.join(", ")));
        }
        if !dev.is_empty() {
            line.push_str(&format!(" | dev: {}", dev.join(", ")));
        }
        if !build.is_empty() {
            line.push_str(&format!(" | build: {}", build.join(", ")));
        }
        let external = |kind: DepKind| names(kind, false).len();
        let counts: Vec<String> = [
            ("external", external(DepKind::Normal)),
            ("external dev", external(DepKind::Dev)),
            ("external build", external(DepKind::Build)),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(label, n)| format!("{n} {label}"))
        .collect();
        if !counts.is_empty() {
            line.push_str(&format!(" | {}", counts.join(", ")));
        }
        Some(line)
    }
}

/// A candidate or declaration that could not be turned into something to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unresolved {
    pub candidate: String,
    pub reason: String,
    /// More recorded observations (a listing of the glob's parent) could resolve it.
    pub resolvable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageStatus {
    /// The root manifest has not been read completely, so nothing is known to require.
    RootUnread,
    /// The root manifest declares no workspace: it alone is required, and it was read.
    NotAWorkspace,
    /// Declared candidates remain unread, or a glob still needs a listing.
    Incomplete,
    /// Every candidate was tried; some could not be read, parsed or interpreted.
    Unresolved,
    Satisfied,
}

impl CoverageStatus {
    pub fn name(self) -> &'static str {
        match self {
            Self::RootUnread => "root_unread",
            Self::NotAWorkspace => "not_a_workspace",
            Self::Incomplete => "incomplete",
            Self::Unresolved => "unresolved",
            Self::Satisfied => "satisfied",
        }
    }

    pub fn is_satisfied(self) -> bool {
        matches!(self, Self::Satisfied | Self::NotAWorkspace)
    }
}

/// What the recorded observations establish about a Cargo workspace.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inventory {
    /// The observation number of the complete root `Cargo.toml` read.
    pub root_read: Option<usize>,
    pub is_workspace: bool,
    /// The raw `members` strings as the manifest declares them.
    pub declared: Vec<String>,
    pub members: Vec<Member>,
    pub excluded: Vec<String>,
    pub unresolved: Vec<Unresolved>,
    /// A `README*` was read completely.
    pub overview_read: bool,
    /// Entries in the root's `[workspace.dependencies]`.
    pub workspace_dependencies: usize,
}

impl Inventory {
    /// What the root manifest establishes, in one line, with the number of the observation it was
    /// parsed from. `None` unless it was read, parsed and declares a workspace.
    pub fn root_line(&self) -> Option<String> {
        let number = self.root_read?;
        self.is_workspace.then(|| {
            format!(
                "#{number} Cargo.toml [workspace]: {} members declared ({} concrete, {} excluded, {} unresolved declarations); workspace.dependencies: {}",
                self.declared.len(),
                self.members.len(),
                self.excluded.len(),
                self.unresolved.len(),
                self.workspace_dependencies
            )
        })
    }
}

struct Read {
    number: usize,
    text: String,
}

fn succeeded(o: &Observation) -> bool {
    o.kind == ObservationKind::ExecutionCompleted && o.status == ExecutionStatus::Success
}

/// Whether `text` matches `pattern`, where `*` matches any run of characters (including none).
fn wildcard(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || text.len() < first.len() + last.len() || !text.ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for middle in &parts[1..parts.len() - 1] {
        match rest.find(middle) {
            Some(at) => rest = &rest[at + middle.len()..],
            None => return false,
        }
    }
    true
}

fn is_glob(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

fn normalize(path: &str) -> &str {
    path.strip_suffix('/').unwrap_or(path)
}

fn dependencies(manifest: &toml::Table, root: Option<&toml::Table>) -> Vec<Dependency> {
    let inherited_path = |name: &str| -> Option<String> {
        root?
            .get("workspace")?
            .get("dependencies")?
            .get(name)?
            .get("path")?
            .as_str()
            .map(str::to_string)
    };
    let mut out = Vec::new();
    let mut collect = |table: Option<&toml::Value>, kind: DepKind| {
        let Some(table) = table.and_then(toml::Value::as_table) else {
            return;
        };
        for (key, spec) in table {
            let spec_table = spec.as_table();
            let name = spec_table
                .and_then(|t| t.get("package"))
                .and_then(toml::Value::as_str)
                .unwrap_or(key)
                .to_string();
            let inherited = spec_table
                .and_then(|t| t.get("workspace"))
                .and_then(toml::Value::as_bool)
                == Some(true);
            let path = spec_table
                .and_then(|t| t.get("path"))
                .and_then(toml::Value::as_str)
                .map(str::to_string)
                .or_else(|| inherited.then(|| inherited_path(key)).flatten());
            out.push(Dependency {
                name,
                kind,
                path,
                inherited,
            });
        }
    };
    collect(manifest.get("dependencies"), DepKind::Normal);
    collect(manifest.get("dev-dependencies"), DepKind::Dev);
    collect(manifest.get("build-dependencies"), DepKind::Build);
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        for target in targets.values() {
            collect(target.get("dependencies"), DepKind::Normal);
            collect(target.get("dev-dependencies"), DepKind::Dev);
            collect(target.get("build-dependencies"), DepKind::Build);
        }
    }
    out
}

impl Inventory {
    /// Derives the inventory from recorded observations, in order. Nothing else is consulted.
    pub fn derive(observations: &[Observation]) -> Self {
        let mut reads: BTreeMap<String, Read> = BTreeMap::new();
        let mut failed: BTreeMap<String, String> = BTreeMap::new();
        let mut listings: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
        for (i, o) in observations.iter().enumerate() {
            let Some(header) = observation_header(o) else {
                continue;
            };
            let Some(path) = header["path"].as_str().map(normalize).map(str::to_string) else {
                continue;
            };
            match header["capability"].as_str() {
                Some(PROJECT_READ) if succeeded(o) => {
                    if header["complete"].as_bool() == Some(true) {
                        reads.entry(path).or_insert(Read {
                            number: i + 1,
                            text: body(o).to_string(),
                        });
                    } else {
                        failed.insert(path, "partial read".into());
                    }
                }
                Some(PROJECT_READ) => {
                    let why = header["error"].as_str().unwrap_or("read failed");
                    failed.insert(path, format!("read failed ({why})"));
                }
                Some(PROJECT_LIST) if succeeded(o) => {
                    let dirs = body(o)
                        .lines()
                        .filter_map(|l| l.strip_prefix("dir "))
                        .map(str::to_string)
                        .collect();
                    listings.entry(path).or_insert((i + 1, dirs));
                }
                _ => {}
            }
        }
        let mut inv = Inventory {
            overview_read: reads.keys().any(|p| {
                let name = p.rsplit('/').next().unwrap_or(p).to_ascii_uppercase();
                name.starts_with("README")
            }),
            ..Self::default()
        };

        let Some(root) = reads.get("Cargo.toml") else {
            if let Some(why) = failed.get("Cargo.toml") {
                inv.unresolved.push(Unresolved {
                    candidate: "Cargo.toml".into(),
                    reason: why.clone(),
                    resolvable: false,
                });
            }
            return inv;
        };
        let Ok(root_table) = root.text.parse::<toml::Table>() else {
            inv.unresolved.push(Unresolved {
                candidate: "Cargo.toml".into(),
                reason: "did not parse as TOML".into(),
                resolvable: false,
            });
            return inv;
        };
        inv.root_read = Some(root.number);
        let Some(workspace) = root_table.get("workspace").and_then(toml::Value::as_table) else {
            return inv;
        };
        inv.is_workspace = true;
        inv.workspace_dependencies = workspace
            .get("dependencies")
            .and_then(toml::Value::as_table)
            .map_or(0, |t| t.len());
        let strings = |key: &str| -> Vec<String> {
            workspace
                .get(key)
                .and_then(toml::Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        inv.declared = strings("members");
        let exclude = strings("exclude");

        let mut seen: BTreeSet<String> = BTreeSet::new();
        for entry in &inv.declared {
            let entry_path = normalize(entry);
            if is_glob(entry_path) {
                let (parent, last) = entry_path
                    .rsplit_once('/')
                    .map_or((".", entry_path), |(p, l)| (p, l));
                if parent.contains(['*', '?', '[']) || last.contains(['?', '[']) {
                    inv.unresolved.push(Unresolved {
                        candidate: entry.clone(),
                        reason: "glob form not supported; only `parent/prefix*suffix` is expanded"
                            .into(),
                        resolvable: false,
                    });
                    continue;
                }
                let Some((listing, dirs)) = listings.get(parent) else {
                    inv.unresolved.push(Unresolved {
                        candidate: entry.clone(),
                        reason: format!(
                            "glob member; project.list `{parent}` to resolve its matches"
                        ),
                        resolvable: true,
                    });
                    continue;
                };
                for dir in dirs {
                    let name = dir.rsplit('/').next().unwrap_or(dir);
                    if wildcard(last, name) && seen.insert(dir.clone()) {
                        inv.members.push(Member {
                            path: dir.clone(),
                            source: MemberSource::Glob {
                                pattern: entry.clone(),
                                listing: *listing,
                            },
                            state: MemberState::Outstanding,
                            package: None,
                            version: None,
                            description: None,
                            dependencies: Vec::new(),
                        });
                    }
                }
                continue;
            }
            if entry_path == "." {
                continue;
            }
            if let Err(why) = chip_project::check_project_path(entry_path) {
                inv.unresolved.push(Unresolved {
                    candidate: entry.clone(),
                    reason: format!("not an acceptable project path: {why}"),
                    resolvable: false,
                });
                continue;
            }
            if seen.insert(entry_path.to_string()) {
                inv.members.push(Member {
                    path: entry_path.to_string(),
                    source: MemberSource::Declared,
                    state: MemberState::Outstanding,
                    package: None,
                    version: None,
                    description: None,
                    dependencies: Vec::new(),
                });
            }
        }

        for pattern in &exclude {
            let pattern = normalize(pattern);
            if pattern.contains(['?', '[']) {
                inv.unresolved.push(Unresolved {
                    candidate: format!("exclude {pattern}"),
                    reason: "exclude pattern form not evaluated".into(),
                    resolvable: false,
                });
                continue;
            }
            let before = inv.members.len();
            inv.members.retain(|m| !wildcard(pattern, &m.path));
            if inv.members.len() < before || !is_glob(pattern) {
                inv.excluded.push(pattern.to_string());
            }
        }

        for member in &mut inv.members {
            let manifest = member.manifest();
            if let Some(read) = reads.get(&manifest) {
                match read.text.parse::<toml::Table>() {
                    Ok(table) => {
                        member.state = MemberState::Read(read.number);
                        member.package = table
                            .get("package")
                            .and_then(|p| p.get("name"))
                            .and_then(toml::Value::as_str)
                            .map(str::to_string);
                        let package = |key: &str| {
                            table
                                .get("package")
                                .and_then(|p| p.get(key))
                                .and_then(toml::Value::as_str)
                                .map(str::to_string)
                        };
                        member.version = package("version");
                        member.description = package("description");
                        member.dependencies = dependencies(&table, Some(&root_table));
                    }
                    Err(_) => {
                        member.state = MemberState::Unresolved("did not parse as TOML".into());
                    }
                }
            } else if let Some(why) = failed.get(&manifest) {
                member.state = MemberState::Unresolved(why.clone());
            }
        }
        inv
    }

    pub fn outstanding(&self) -> Vec<&Member> {
        self.members
            .iter()
            .filter(|m| m.state == MemberState::Outstanding)
            .collect()
    }

    pub fn read(&self) -> Vec<&Member> {
        self.members
            .iter()
            .filter(|m| matches!(m.state, MemberState::Read(_)))
            .collect()
    }

    /// Every candidate that could not be read or interpreted, with why.
    pub fn all_unresolved(&self) -> Vec<Unresolved> {
        let mut out = self.unresolved.clone();
        for m in &self.members {
            if let MemberState::Unresolved(why) = &m.state {
                out.push(Unresolved {
                    candidate: m.manifest(),
                    reason: why.clone(),
                    resolvable: false,
                });
            }
        }
        out
    }

    pub fn status(&self) -> CoverageStatus {
        if self.root_read.is_none() {
            return CoverageStatus::RootUnread;
        }
        if !self.is_workspace {
            return CoverageStatus::NotAWorkspace;
        }
        let unresolved = self.all_unresolved();
        if !self.outstanding().is_empty() || unresolved.iter().any(|u| u.resolvable) {
            CoverageStatus::Incomplete
        } else if !unresolved.is_empty() {
            CoverageStatus::Unresolved
        } else {
            CoverageStatus::Satisfied
        }
    }

    fn member_named(&self, name: &str) -> Option<&Member> {
        let name =
            name.trim_matches(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '/')));
        self.members.iter().find(|m| {
            m.package.as_deref() == Some(name)
                || m.path == name
                || m.path.rsplit('/').next() == Some(name)
        })
    }
}

// ---- narrow checks of the model's text against parsed manifest data ----------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The text the check looked at.
    pub claim: String,
    pub why: String,
}

fn clean(token: &str) -> &str {
    token
        .trim_matches(|c: char| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '/' | '*' | '.')))
        .trim_end_matches('.')
}

fn sentences(answer: &str) -> Vec<&str> {
    answer
        .split(['\n'])
        .flat_map(|line| line.split(". "))
        .collect()
}

/// A glob offered as workspace members that `[workspace].members` does not declare. The answer
/// text is compared with the parsed array only; a comment in the manifest is not an input.
fn membership_findings(answer: &str, inv: &Inventory) -> Vec<Finding> {
    if inv.root_read.is_none() || !inv.is_workspace {
        return Vec::new();
    }
    let mut out = Vec::new();
    for sentence in sentences(answer) {
        if !sentence.to_ascii_lowercase().contains("member") {
            continue;
        }
        for token in sentence.split_whitespace().map(clean) {
            if token.contains('*')
                && token.contains('/')
                && !inv.declared.iter().any(|d| normalize(d) == token)
                && !out.iter().any(|f: &Finding| f.claim == token)
            {
                out.push(Finding {
                    claim: token.to_string(),
                    why: format!(
                        "offered as workspace members, but [workspace].members (observation {}) declares {}",
                        inv.root_read.unwrap_or_default(),
                        if inv.declared.is_empty() {
                            "none".to_string()
                        } else {
                            format!("{} entries and not this pattern", inv.declared.len())
                        }
                    ),
                });
            }
        }
    }
    out
}

/// Words that end the list of things a "depends on" clause names.
const CLAUSE_ENDS: [&str; 12] = [
    "but", "while", "which", "whereas", "via", "through", "because", "when", "so", "as", "only",
    "and,",
];

/// A count of workspace members the answer states that is not the count the manifest declares:
/// only the forms `declares N`, `has/contains N members`, and `N declared members` are read.
fn count_findings(answer: &str, inv: &Inventory) -> Vec<Finding> {
    if inv.root_read.is_none() || !inv.is_workspace {
        return Vec::new();
    }
    let ok = [inv.declared.len(), inv.members.len()];
    let words: Vec<String> = answer
        .split_whitespace()
        .map(|w| clean(w).to_ascii_lowercase())
        .collect();
    let mut out = Vec::new();
    for (i, word) in words.iter().enumerate() {
        let Ok(n) = word.parse::<usize>() else {
            continue;
        };
        let before = i.checked_sub(1).map(|j| words[j].as_str()).unwrap_or("");
        let after = words.get(i + 1).map(String::as_str).unwrap_or("");
        let after2 = words.get(i + 2).map(String::as_str).unwrap_or("");
        let stated = before == "declares"
            || (matches!(before, "has" | "contains") && after.starts_with("member"))
            || (after == "declared" && after2.starts_with("member"))
            || (after == "workspace" && after2.starts_with("member"));
        if stated && !ok.contains(&n) {
            out.push(Finding {
                claim: format!("{n} workspace members"),
                why: format!(
                    "[workspace].members (observation {}) declares {}",
                    inv.root_read.unwrap_or_default(),
                    inv.declared.len()
                ),
            });
        }
    }
    out
}

/// The parts of the requested answer a mechanical check can look for. These are not judgements of
/// quality: the answer must state the declared member count, and must say something about
/// dependencies when the manifests that were read declare internal ones. Nothing else is required of
/// its wording, and an answer is never required to list the members.
fn omissions(answer: &str, inv: &Inventory) -> Vec<String> {
    let mut out = Vec::new();
    let lower = answer.to_ascii_lowercase();
    let counts = [inv.declared.len(), inv.members.len()];
    let states_count = answer
        .split_whitespace()
        .filter_map(|w| clean(w).parse::<usize>().ok())
        .any(|n| counts.contains(&n));
    if inv.is_workspace && !states_count {
        out.push(format!(
            "it does not state the declared member count ({}) as a number",
            inv.declared.len()
        ));
    }
    let has_internal_edge = inv
        .read()
        .iter()
        .any(|m| m.dependencies.iter().any(|d| d.path.is_some()));
    if has_internal_edge && !lower.contains("depend") {
        out.push("it says nothing about dependencies between the crates".to_string());
    }
    out
}

/// "X depends on Y" where X's manifest was read and Y matches only dev-dependencies there. Each
/// occurrence is its own clause: the names it lists end at `;`, `:`, a parenthesis or a connective
/// word. A sentence that says "dev" is read as having made the distinction and is not checked.
fn dependency_findings(answer: &str, inv: &Inventory) -> Vec<Finding> {
    let mut out = Vec::new();
    for sentence in sentences(answer) {
        let lower = sentence.to_ascii_lowercase();
        if lower.contains("dev-dep") || lower.contains("dev dep") || lower.contains("dev_dep") {
            continue;
        }
        for verb in [" depends on ", " depend on "] {
            for (at, _) in lower.match_indices(verb) {
                let subject = sentence[..at]
                    .split_whitespace()
                    .last()
                    .map(clean)
                    .unwrap_or("");
                let Some(member) = inv.member_named(subject) else {
                    continue;
                };
                if !matches!(member.state, MemberState::Read(_)) {
                    continue;
                }
                for raw in sentence[at + verb.len()..].split_whitespace() {
                    if raw.starts_with('(')
                        || CLAUSE_ENDS.contains(&raw.to_ascii_lowercase().as_str())
                    {
                        break;
                    }
                    let token = clean(raw);
                    let names_a_dependency =
                        token.contains('*') || member.dependencies.iter().any(|d| d.name == token);
                    if !token.is_empty() && names_a_dependency {
                        let matching: Vec<&Dependency> = member
                            .dependencies
                            .iter()
                            .filter(|d| wildcard(token, &d.name))
                            .collect();
                        if !matching.is_empty() && matching.iter().all(|d| d.kind == DepKind::Dev) {
                            out.push(Finding {
                                claim: format!("{subject} depends on {token}"),
                                why: format!(
                                    "in {} every dependency matching `{token}` is a dev-dependency",
                                    member.manifest()
                                ),
                            });
                        }
                    }
                    if raw.ends_with([';', ':']) {
                        break;
                    }
                }
            }
        }
    }
    out
}

// ---- the assessment ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerStatus {
    GroundedComplete,
    GroundedIncomplete,
    /// The answer cites nothing Chip observed.
    Unsupported,
    /// A checked claim conflicts with parsed manifest data.
    Contradicted,
    /// Nothing was observed from which a requirement could be derived.
    InsufficientEvidence,
}

impl AnswerStatus {
    pub fn name(self) -> &'static str {
        match self {
            Self::GroundedComplete => "grounded_complete",
            Self::GroundedIncomplete => "grounded_incomplete",
            Self::Unsupported => "unsupported_claim",
            Self::Contradicted => "contradicted_claim",
            Self::InsufficientEvidence => "insufficient_evidence",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub status: AnswerStatus,
    pub reason: String,
    pub findings: Vec<Finding>,
    /// Parts of the requested answer that are missing though coverage is satisfied.
    pub omissions: Vec<String>,
}

/// Classifies a proposed answer against the recorded observations, in this order: no root manifest
/// observed (insufficient evidence), cites nothing observed (unsupported), a checked claim
/// contradicted, coverage not satisfied (grounded but incomplete), otherwise grounded and complete.
pub fn assess(answer: &str, observations: &[Observation]) -> Assessment {
    let inv = Inventory::derive(observations);
    let grounded = GroundedAnswer.accepts(answer, observations);
    let mut findings = membership_findings(answer, &inv);
    findings.extend(count_findings(answer, &inv));
    findings.extend(dependency_findings(answer, &inv));
    let missing = if status_satisfied(&inv) {
        omissions(answer, &inv)
    } else {
        Vec::new()
    };
    let status = inv.status();
    let (kind, reason) = if inv.root_read.is_none() {
        (
            AnswerStatus::InsufficientEvidence,
            "the root Cargo.toml has not been read completely, so what the inspection requires is unknown".to_string(),
        )
    } else if !grounded {
        (
            AnswerStatus::Unsupported,
            "the answer cites no file Chip observed".to_string(),
        )
    } else if !findings.is_empty() {
        (
            AnswerStatus::Contradicted,
            format!(
                "{} claim(s) conflict with parsed manifest data: {}",
                findings.len(),
                findings
                    .iter()
                    .map(|f| f.claim.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        )
    } else if !status.is_satisfied() {
        (AnswerStatus::GroundedIncomplete, coverage_reason(&inv))
    } else if !missing.is_empty() {
        (
            AnswerStatus::GroundedIncomplete,
            format!(
                "coverage is satisfied but the answer is incomplete: {}",
                missing.join("; ")
            ),
        )
    } else {
        (
            AnswerStatus::GroundedComplete,
            "every required manifest was read and no checked claim is contradicted".to_string(),
        )
    };
    Assessment {
        status: kind,
        reason,
        findings,
        omissions: missing,
    }
}

fn status_satisfied(inv: &Inventory) -> bool {
    inv.status().is_satisfied()
}

fn coverage_reason(inv: &Inventory) -> String {
    let required = inv.members.len();
    let unresolved = inv.all_unresolved();
    format!(
        "coverage {}: {} of {} declared member manifests read, {} outstanding, {} unresolved",
        inv.status().name(),
        inv.read().len(),
        required,
        inv.outstanding().len(),
        unresolved.len()
    )
}

/// The completion check for a declared coverage contract: the answer must be grounded, complete
/// and uncontradicted. It sits beside [`GroundedAnswer`], which still applies unchanged.
#[derive(Debug, Clone, Copy)]
pub struct CoveredAnswer(pub CoverageContract);

impl AnswerPredicate for CoveredAnswer {
    fn describe(&self) -> String {
        format!(
            "the inspection covers the {} contract: the root manifest and every declared member manifest were read, and no checked claim is contradicted",
            self.0.name()
        )
    }

    fn accepts(&self, answer: &str, observations: &[Observation]) -> bool {
        assess(answer, observations).status == AnswerStatus::GroundedComplete
    }
}

// ---- the report ---------------------------------------------------------------------------------------

const LISTED: usize = 25;

/// The coverage section of the work report. `proposed` is the last answer the model proposed with
/// `complete`, as recorded; `enforced` says whether a contract made it a condition of completion.
pub fn report(
    contract: Option<CoverageContract>,
    observations: &[Observation],
    proposed: Option<&str>,
) -> serde_json::Value {
    let inv = Inventory::derive(observations);
    let unresolved = inv.all_unresolved();
    let outstanding: Vec<String> = inv.outstanding().iter().map(|m| m.manifest()).collect();
    let edges: usize = inv.read().iter().map(|m| m.dependencies.len()).sum();
    let answer = proposed.map(|text| {
        let a = assess(text, observations);
        serde_json::json!({
            "status": a.status.name(),
            "reason": a.reason,
            "enforced": contract.is_some(),
            "contradicted_claims": a.findings.iter().map(|f| serde_json::json!({"claim": f.claim, "why": f.why})).collect::<Vec<_>>(),
            "omissions": a.omissions,
            // The summary as recorded, in bytes of UTF-8, against the validator's limit.
            "proposed_answer_bytes": text.len(),
            "summary_limit_bytes": chip_core::MAX_TEXT_BYTES,
            "proposed_answer": text,
        })
    });
    serde_json::json!({
        "contract": contract.map_or("none", CoverageContract::name),
        "status": inv.status().name(),
        "root_manifest_read": inv.root_read.is_some(),
        "overview_read": inv.overview_read,
        "workspace_members_declared": inv.declared.len(),
        "required_candidates": inv.members.len(),
        "read_candidates": inv.read().len(),
        "outstanding_candidates": outstanding.len(),
        "unresolved_candidates": unresolved.len(),
        "outstanding": outstanding.iter().take(LISTED).collect::<Vec<_>>(),
        "unresolved": unresolved.iter().take(LISTED).map(|u| serde_json::json!({"candidate": u.candidate, "reason": u.reason, "resolvable": u.resolvable})).collect::<Vec<_>>(),
        "excluded": inv.excluded,
        "dependency_declarations_read": edges,
        // Recorded observations the runtime validates from, and the one-line facts parsed from them
        // (the root's and each read member's). The model is sent those lines, not the manifests.
        "source_observations_retained": inv.read().len() + usize::from(inv.root_read.is_some()),
        "structured_facts_available": inv.read().iter().filter(|m| m.compact_line().is_some()).count()
            + usize::from(inv.root_line().is_some()),
        "answer": answer,
        "factual_validation": "limited: only a glob offered as workspace members and a `depends on` claim true of dev-dependencies alone are checked against parsed manifests; coverage is not correctness, and an accepted answer is grounded, not verified",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_match_runs_and_nothing_else() {
        assert!(wildcard("chip-*", "chip-core"));
        assert!(wildcard("*", "anything"));
        assert!(wildcard("a*c", "abc") && wildcard("a*c", "ac"));
        assert!(wildcard("a*b*c", "aXbYc"));
        assert!(!wildcard("chip-*", "fx-core"));
        assert!(!wildcard("a*c", "ab"));
        assert!(wildcard("exact", "exact") && !wildcard("exact", "exactly"));
    }
}
