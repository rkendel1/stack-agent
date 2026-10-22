//! **Experimental.** The native-skill evaluation seam: reviewed text that one arm of a controlled
//! experiment adds to what the model is told, and nothing else. Not a product feature: `stack-agent
//! work`, `verify` and `serve` never construct an [`ArmInstructions`]; only the
//! `skill_experiment` example does (`docs/product/native-skills-experiment.md`).
//!
//! What a skill is (`chip.skill.v1`): a directory holding a strict JSON manifest (`skill.json`), one
//! Markdown instruction file and optional Markdown references. It is inert text. It cannot add,
//! remove or describe a capability, and it does not touch decision validation, authorization,
//! execution, FeltDB, evidence or completion. Its one effect is the product's inert experimental
//! hook: [`ArmInstructions`] is an [`InstructionSource`] whose [`AddedInstructions`] the
//! `WithInstructions` context policy appends to the goal under [`LABEL`], changing nothing else in
//! the request. The dependency runs one way: this module uses `software_work`; `software_work`
//! never names this module.
//!
//! Trust comes from this source file, not from the skill: [`REVIEWED_SKILLS`] and
//! [`LENGTH_CONTROL`] pin each artifact's id, version and digest ([`digest_files`]). A skill whose
//! bytes do not produce its pinned digest is refused before any model call.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::autoconfig::profile::BuildSystem;
use crate::software_work::GoalKind;
pub use crate::software_work::{
    ADDED_INSTRUCTIONS_LABEL as LABEL, AddedInstructions, InstructionSource, WithInstructions,
};

/// The manifest format this reader accepts. Any other value is refused.
pub const SKILL_SCHEMA: &str = "chip.skill.v1";
/// The manifest's file name inside a skill directory.
pub const MANIFEST: &str = "skill.json";
/// The most bytes of instructions and references together (provisional; measured, not tuned).
pub const MAX_TEXT_BYTES: usize = 4096;
/// The most bytes a manifest may hold.
pub const MAX_MANIFEST_BYTES: usize = 4096;
/// The most bytes a description may hold.
pub const MAX_DESCRIPTION_BYTES: usize = 200;
/// The heading the instruction file must carry, on a line of its own.
pub const VERIFICATION_HEADING: &str = "## Verification";

/// Manifest members that are refused by name with their own reason, before the general unknown-field
/// rule: each would be a second capability, permission or execution contract.
const FORBIDDEN_MEMBERS: &[&str] = &[
    "inputs",
    "outputs",
    "permissions",
    "effects",
    "scripts",
    "script",
    "commands",
    "command",
    "executable",
    "run",
    "tools",
    "hooks",
];

/// A skill accepted by review: where it is, and the id, version and digest its bytes must have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewedSkill {
    pub id: &'static str,
    pub version: u32,
    /// Repository-relative directory.
    pub dir: &'static str,
    pub digest: &'static str,
}

/// Reviewed task-irrelevant text of comparable length: the length-control arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewedControl {
    pub id: &'static str,
    /// Repository-relative file.
    pub path: &'static str,
    pub digest: &'static str,
}

/// Every skill an experiment may use. A change to any of a skill's files changes its digest; the
/// new bytes are usable only after this entry is updated in review (and the version bumped).
pub const REVIEWED_SKILLS: &[ReviewedSkill] = &[ReviewedSkill {
    id: "repo.diagnose-failing-test",
    version: 1,
    dir: "docs/product/native-skills/examples/diagnose-failing-test",
    digest: "sha256:1d43d17f58d10ee718f772dfa7cd965c36f23d686f3f5e6c5d2a5749628caa7c",
}];

/// The length control, pinned the same way.
pub const LENGTH_CONTROL: ReviewedControl = ReviewedControl {
    id: "control.length-v1",
    path: "docs/product/native-skills/controls/length-control-v1.md",
    digest: "sha256:ad57e213cedbb2d08d54584f0a45027d54e6fc89e7260b16349526970432dcc1",
};

/// The repository this binary was built from: where the reviewed artifacts are read.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The digest of a set of files, as `sha256:<hex>`:
///
/// `SHA-256( domain ‖ for each file, in ascending byte order of its path:
///           path (UTF-8, `/`-separated, relative) ‖ 0x00 ‖ length (u64, big-endian) ‖ bytes )`
///
/// A skill hashes its manifest (as `skill.json`), its instruction file and every declared
/// reference, loaded or not, under the domain `chip.skill.v1\n`. The length control hashes its one
/// file under `chip.skill.v1.control\n`, by file name. Nothing else (timestamps, modes, directory
/// names) is hashed.
pub fn digest_files(domain: &str, files: &BTreeMap<String, Vec<u8>>) -> String {
    let mut h = Sha256::new();
    h.update(domain.as_bytes());
    for (path, bytes) in files {
        h.update(path.as_bytes());
        h.update([0u8]);
        h.update((bytes.len() as u64).to_be_bytes());
        h.update(bytes);
    }
    format!(
        "sha256:{}",
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

const SKILL_DOMAIN: &str = "chip.skill.v1\n";
const CONTROL_DOMAIN: &str = "chip.skill.v1.control\n";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: String,
    id: String,
    version: u32,
    description: String,
    applies_to: AppliesTo,
    capabilities: Vec<String>,
    instructions: String,
    #[serde(default)]
    references: Vec<Reference>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppliesTo {
    kinds: Vec<String>,
    #[serde(default)]
    build_systems: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reference {
    path: String,
    applies_to: AppliesTo,
}

const BUILD_SYSTEMS: &[BuildSystem] = &[
    BuildSystem::Cargo,
    BuildSystem::Npm,
    BuildSystem::Pnpm,
    BuildSystem::Yarn,
    BuildSystem::Bun,
    BuildSystem::Python,
    BuildSystem::Go,
    BuildSystem::Maven,
    BuildSystem::Gradle,
];

impl AppliesTo {
    fn check(&self, what: &str) -> Result<(), String> {
        if self.kinds.is_empty() {
            return Err(format!("{what}: applies_to.kinds is empty"));
        }
        unique(&self.kinds, &format!("{what}: applies_to.kinds"))?;
        for k in &self.kinds {
            if GoalKind::parse(k).is_none() {
                return Err(format!("{what}: unknown kind {k:?}"));
            }
        }
        if let Some(systems) = &self.build_systems {
            if systems.is_empty() {
                return Err(format!("{what}: applies_to.build_systems is empty"));
            }
            unique(systems, &format!("{what}: applies_to.build_systems"))?;
            for s in systems {
                if !BUILD_SYSTEMS.iter().any(|b| b.name() == s) {
                    return Err(format!("{what}: unknown build system {s:?}"));
                }
            }
        }
        Ok(())
    }

    fn matches(&self, kind: GoalKind, build_systems: &[BuildSystem]) -> bool {
        self.kinds.iter().any(|k| k == kind.name())
            && self.build_systems.as_ref().is_none_or(|want| {
                build_systems
                    .iter()
                    .any(|b| want.iter().any(|w| w == b.name()))
            })
    }
}

fn unique(items: &[String], what: &str) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for i in items {
        if !seen.insert(i) {
            return Err(format!("{what} names {i:?} twice"));
        }
    }
    Ok(())
}

/// A path a manifest may name: relative, `/`-separated, no `.` or `..` component, plain characters,
/// a Markdown file.
fn check_relative(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > 128 {
        return Err(format!("path {path:?} is empty or longer than 128 bytes"));
    }
    if path.starts_with('/') || path.contains('\\') || path.contains(':') {
        return Err(format!("path {path:?} is not relative"));
    }
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(format!("path {path:?} has an empty, `.` or `..` component"));
        }
        if !part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(format!(
                "path {path:?} has a character outside A-Z a-z 0-9 - _ ."
            ));
        }
    }
    if !path.ends_with(".md") {
        return Err(format!("path {path:?} is not a Markdown (.md) file"));
    }
    Ok(())
}

fn check_id(id: &str) -> Result<(), String> {
    let parts: Vec<&str> = id.split('.').collect();
    let ok = id.len() <= 64
        && parts.len() >= 2
        && parts[0]
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        });
    if ok {
        Ok(())
    } else {
        Err(format!(
            "id {id:?} is not of the form `segment.segment[...]` (a-z 0-9 -, at most 64 bytes)"
        ))
    }
}

/// Every regular file under `dir`, by `/`-separated relative path. A symbolic link anywhere (the
/// directory itself included), or anything that is not a regular file or directory, is refused.
fn files_under(dir: &Path) -> Result<BTreeSet<String>, String> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeSet<String>) -> Result<(), String> {
        let entries =
            std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
            let path = entry.path();
            let rel = path
                .strip_prefix(base)
                .map_err(|_| "a listed entry is outside the skill".to_string())?
                .to_str()
                .ok_or_else(|| format!("{} is not a UTF-8 path", path.display()))?
                .replace(std::path::MAIN_SEPARATOR, "/");
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|e| format!("cannot inspect {rel}: {e}"))?;
            if meta.file_type().is_symlink() {
                return Err(format!("{rel} is a symbolic link"));
            }
            if meta.is_dir() {
                walk(base, &path, out)?;
            } else if meta.is_file() {
                out.insert(rel);
            } else {
                return Err(format!("{rel} is not a regular file"));
            }
        }
        Ok(())
    }
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("cannot inspect {}: {e}", dir.display()))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(format!("{} is not a plain directory", dir.display()));
    }
    let mut out = BTreeSet::new();
    walk(dir, dir, &mut out)?;
    Ok(out)
}

/// Reads one file, at most `max` bytes, as text: UTF-8 with no control character but newline and
/// tab.
fn read_text(path: &Path, rel: &str, max: usize) -> Result<(Vec<u8>, String), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("cannot read {rel}: {e}"))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(format!("{rel} is not a regular file"));
    }
    if meta.len() > max as u64 {
        return Err(format!(
            "{rel} is {} bytes; at most {max} are accepted",
            meta.len()
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {rel}: {e}"))?;
    if bytes.len() > max {
        return Err(format!(
            "{rel} is {} bytes; at most {max} are accepted",
            bytes.len()
        ));
    }
    let text = String::from_utf8(bytes.clone()).map_err(|_| format!("{rel} is not UTF-8"))?;
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(format!("{rel} contains a control character"));
    }
    Ok((bytes, text))
}

/// A skill directory that passed every check of `chip.skill.v1`. It has not been compared with a
/// review pin; [`load_reviewed`] does that.
#[derive(Debug, Clone)]
pub struct Skill {
    manifest: Manifest,
    /// Instruction and reference texts by relative path.
    texts: BTreeMap<String, String>,
    digest: String,
}

impl Skill {
    pub fn id(&self) -> &str {
        &self.manifest.id
    }
    pub fn version(&self) -> u32 {
        self.manifest.version
    }
    /// The digest of the skill's bytes ([`digest_files`]).
    pub fn digest(&self) -> &str {
        &self.digest
    }
    /// The capability ids the skill names: preconditions it requires to be offered, never grants.
    pub fn capabilities(&self) -> &[String] {
        &self.manifest.capabilities
    }
}

/// Reads and validates the skill in `dir`. Fails closed on the first problem, with the reason:
/// malformed or oversized manifest, unknown or forbidden member, wrong schema, bad id or version,
/// unknown kind, build system or capability, a bad or duplicated path, a missing, undeclared,
/// non-regular or symbolically linked file, text that is not UTF-8, too much text, or an
/// instruction file without its verification section.
pub fn load_skill_dir(dir: &Path) -> Result<Skill, String> {
    let refuse = |why: String| format!("skill {} refused: {why}", dir.display());
    let found = files_under(dir).map_err(refuse)?;
    let (manifest_bytes, manifest_text) =
        read_text(&dir.join(MANIFEST), MANIFEST, MAX_MANIFEST_BYTES).map_err(refuse)?;
    let raw: Value = serde_json::from_str(&manifest_text)
        .map_err(|e| refuse(format!("{MANIFEST} is not strict JSON: {e}")))?;
    let Some(object) = raw.as_object() else {
        return Err(refuse(format!("{MANIFEST} is not a JSON object")));
    };
    for member in FORBIDDEN_MEMBERS {
        if object.contains_key(*member) {
            return Err(refuse(format!(
                "{MANIFEST} declares `{member}`; a skill declares no inputs, outputs, permissions, effects or executable content"
            )));
        }
    }
    // Typed parse of the same text: an unknown member, a duplicated member, a wrong type or a missing
    // field is refused here.
    let manifest: Manifest = serde_json::from_str(&manifest_text)
        .map_err(|e| refuse(format!("{MANIFEST} does not match {SKILL_SCHEMA}: {e}")))?;
    if manifest.schema != SKILL_SCHEMA {
        return Err(refuse(format!(
            "schema is {:?}; only {SKILL_SCHEMA:?} is accepted",
            manifest.schema
        )));
    }
    check_id(&manifest.id).map_err(refuse)?;
    if manifest.version == 0 {
        return Err(refuse("version must be 1 or more".into()));
    }
    let description = manifest.description.trim();
    if description.is_empty()
        || manifest.description.len() > MAX_DESCRIPTION_BYTES
        || manifest.description.contains('\n')
    {
        return Err(refuse(format!(
            "description must be one non-empty line of at most {MAX_DESCRIPTION_BYTES} bytes"
        )));
    }
    manifest.applies_to.check("skill").map_err(refuse)?;
    unique(&manifest.capabilities, "capabilities").map_err(refuse)?;
    let declared: Vec<String> = crate::software_work::declared()
        .iter()
        .map(|c| c.to_string())
        .collect();
    for c in &manifest.capabilities {
        if !declared.contains(c) {
            return Err(refuse(format!(
                "capability {c:?} is not one any stack-agent capability declares"
            )));
        }
    }
    let mut paths = vec![manifest.instructions.clone()];
    for r in &manifest.references {
        r.applies_to
            .check(&format!("reference {}", r.path))
            .map_err(refuse)?;
        if !r
            .applies_to
            .kinds
            .iter()
            .all(|k| manifest.applies_to.kinds.contains(k))
        {
            return Err(refuse(format!(
                "reference {} applies to a kind the skill does not",
                r.path
            )));
        }
        paths.push(r.path.clone());
    }
    for p in &paths {
        check_relative(p).map_err(refuse)?;
    }
    unique(&paths, "instructions and references").map_err(refuse)?;
    let mut expected: BTreeSet<String> = paths.iter().cloned().collect();
    expected.insert(MANIFEST.to_string());
    if let Some(extra) = found.difference(&expected).next() {
        return Err(refuse(format!(
            "{extra} is not declared by the manifest; a skill holds only its manifest, instructions and references"
        )));
    }
    if let Some(missing) = expected.difference(&found).next() {
        return Err(refuse(format!("{missing} is declared but missing")));
    }
    let mut hashed: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    hashed.insert(MANIFEST.to_string(), manifest_bytes);
    let mut texts = BTreeMap::new();
    let mut total = 0usize;
    for p in &paths {
        let (bytes, text) = read_text(&dir.join(p), p, MAX_TEXT_BYTES).map_err(refuse)?;
        total += bytes.len();
        hashed.insert(p.clone(), bytes);
        texts.insert(p.clone(), text);
    }
    if total > MAX_TEXT_BYTES {
        return Err(refuse(format!(
            "instructions and references are {total} bytes; at most {MAX_TEXT_BYTES} are accepted"
        )));
    }
    if !texts[&manifest.instructions]
        .lines()
        .any(|l| l.trim_end() == VERIFICATION_HEADING)
    {
        return Err(refuse(format!(
            "{} has no `{VERIFICATION_HEADING}` section",
            manifest.instructions
        )));
    }
    Ok(Skill {
        manifest,
        texts,
        digest: digest_files(SKILL_DOMAIN, &hashed),
    })
}

/// The reviewed registry is well formed: no id twice.
pub fn check_registry(entries: &[ReviewedSkill]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for e in entries {
        if !seen.insert(e.id) {
            return Err(format!("the reviewed registry names skill {} twice", e.id));
        }
    }
    Ok(())
}

/// Loads `entry` from `root` and refuses it unless its id, version and digest are the reviewed ones.
pub fn load_reviewed(entry: &ReviewedSkill, root: &Path) -> Result<Skill, String> {
    check_registry(REVIEWED_SKILLS)?;
    let skill = load_skill_dir(&root.join(entry.dir))?;
    if skill.id() != entry.id || skill.version() != entry.version {
        return Err(format!(
            "skill refused: {} is {}@{}; review accepted {}@{}",
            entry.dir,
            skill.id(),
            skill.version(),
            entry.id,
            entry.version
        ));
    }
    if skill.digest() != entry.digest {
        return Err(format!(
            "skill refused: {}@{} is {}; review pinned {}",
            entry.id,
            entry.version,
            skill.digest(),
            entry.digest
        ));
    }
    Ok(skill)
}

/// The length control from `root`, refused unless it is the reviewed file with the pinned digest.
pub fn load_control(control: &ReviewedControl, root: &Path) -> Result<(String, String), String> {
    check_relative(control.path).map_err(|why| format!("length control refused: {why}"))?;
    let path = root.join(control.path);
    let (bytes, text) = read_text(&path, control.path, MAX_TEXT_BYTES)
        .map_err(|why| format!("length control refused: {why}"))?;
    let name = control.path.rsplit('/').next().unwrap_or(control.path);
    let digest = digest_files(CONTROL_DOMAIN, &BTreeMap::from([(name.to_string(), bytes)]));
    if digest != control.digest {
        return Err(format!(
            "length control refused: {} is {digest}; review pinned {}",
            control.path, control.digest
        ));
    }
    Ok((text, digest))
}

/// One arm of the experiment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Arm {
    /// Exactly what the model is told today.
    Baseline,
    /// The same, plus one reviewed skill's instructions.
    Skill,
    /// The same, plus reviewed task-irrelevant text of comparable length.
    LengthControl,
}

impl Arm {
    pub const ALL: [Arm; 3] = [Arm::Baseline, Arm::Skill, Arm::LengthControl];

    pub fn name(self) -> &'static str {
        match self {
            Arm::Baseline => "baseline",
            Arm::Skill => "skill",
            Arm::LengthControl => "length-control",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.name() == s)
    }
}

#[derive(Debug, Clone)]
enum Source {
    Skill(Skill),
    Control {
        id: &'static str,
        path: &'static str,
        text: String,
        digest: String,
    },
}

/// The reviewed artifact an arm adds, loaded and checked against its pin. Constructed only by
/// [`prepare`]; nothing else can put text into a work's context.
#[derive(Debug, Clone)]
pub struct ArmInstructions {
    arm: Arm,
    source: Source,
}

/// The arm's reviewed artifact from `root`, or `None` for the baseline. Fails closed.
pub fn prepare(arm: Arm, root: &Path) -> Result<Option<ArmInstructions>, String> {
    let source = match arm {
        Arm::Baseline => return Ok(None),
        Arm::Skill => {
            let [entry] = REVIEWED_SKILLS else {
                return Err("the experiment uses exactly one reviewed skill".into());
            };
            Source::Skill(load_reviewed(entry, root)?)
        }
        Arm::LengthControl => {
            let (text, digest) = load_control(&LENGTH_CONTROL, root)?;
            Source::Control {
                id: LENGTH_CONTROL.id,
                path: LENGTH_CONTROL.path,
                text,
                digest,
            }
        }
    };
    Ok(Some(ArmInstructions { arm, source }))
}

impl ArmInstructions {
    pub fn arm(&self) -> Arm {
        self.arm
    }

    /// The text this work is given, checked against the work itself: a skill must apply to the
    /// work's kind and project, and every capability it names must be among those the work offers
    /// (`offered`). A skill can only fail this check; it can never add to `offered`.
    pub fn select(
        &self,
        kind: GoalKind,
        build_systems: &[BuildSystem],
        offered: &[String],
    ) -> Result<AddedInstructions, String> {
        match &self.source {
            Source::Skill(skill) => {
                let m = &skill.manifest;
                if !m.applies_to.matches(kind, build_systems) {
                    return Err(format!(
                        "skill {}@{} does not apply to this {} work on this project",
                        m.id,
                        m.version,
                        kind.name()
                    ));
                }
                let missing: Vec<&String> = m
                    .capabilities
                    .iter()
                    .filter(|c| !offered.contains(c))
                    .collect();
                if !missing.is_empty() {
                    return Err(format!(
                        "skill {}@{} names capabilities this work does not offer: {missing:?}",
                        m.id, m.version
                    ));
                }
                let mut text = skill.texts[&m.instructions].clone();
                let mut files = vec![m.instructions.clone()];
                for r in &m.references {
                    if r.applies_to.matches(kind, build_systems) {
                        text.push('\n');
                        text.push_str(&skill.texts[&r.path]);
                        files.push(r.path.clone());
                    }
                }
                let provenance = json!({
                    "arm": self.arm.name(),
                    "id": m.id,
                    "version": m.version,
                    "digest": skill.digest,
                    "files": files,
                    "text_bytes": text.len(),
                    "label_bytes": LABEL.len(),
                });
                Ok(AddedInstructions::new(text, provenance))
            }
            Source::Control {
                id,
                path,
                text,
                digest,
            } => Ok(AddedInstructions::new(
                text.clone(),
                json!({
                    "arm": self.arm.name(),
                    "id": id,
                    "digest": digest,
                    "files": [path],
                    "text_bytes": text.len(),
                    "label_bytes": LABEL.len(),
                }),
            )),
        }
    }
}

impl InstructionSource for ArmInstructions {
    fn select(
        &self,
        kind: GoalKind,
        build_systems: &[BuildSystem],
        offered: &[String],
    ) -> Result<AddedInstructions, String> {
        ArmInstructions::select(self, kind, build_systems, offered)
    }
}

// ---- the trial plan -------------------------------------------------------------------------------------------------

/// The arms of `trials` rounds, in the pre-registered order: round `r` (1-based) runs the arms in
/// the rotation `r - 1` of baseline, skill, length control, so over every three rounds each arm
/// takes each position once.
pub fn schedule(trials: u32) -> Vec<(u32, Arm)> {
    (1..=trials)
        .flat_map(|r| {
            let shift = ((r - 1) % 3) as usize;
            (0..3).map(move |i| (r, Arm::ALL[(i + shift) % 3]))
        })
        .collect()
}

/// The `work` arguments of a task-E trial with `provider`. They do not depend on the arm: every arm
/// runs the same command line with the same limits and generation settings, and only the context
/// text differs.
pub fn trial_work_args(provider: &crate::agent_bench::Provider) -> Vec<String> {
    crate::agent_bench::work_args(crate::agent_bench::TaskId::E, provider)
}

/// Every capability id a stack-agent capability declares: the most a skill may name.
pub fn declared_capabilities() -> Vec<String> {
    crate::software_work::declared()
        .iter()
        .map(|c| c.to_string())
        .collect()
}
