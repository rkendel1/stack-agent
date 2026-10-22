//! The source-edit boundary. A decision supplies only a body; everything about *where* it goes comes
//! from Chip's own index of the current checkout. An edit is prepared in memory, proven to change
//! nothing but the authorized body, and only then written, through [`write_checked`], which refuses
//! a file that changed since it was read.

use std::fs;
use std::path::{Path, PathBuf};

use crate::symbols::{SymbolRecord, check_relative, index_text, sha256};

/// Why an edit was refused. Each maps to a reported failure class.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum EditError {
    /// The proposed body, or the file with it, is not valid Rust (`no_parse`).
    NoParse(String),
    /// The symbol is not in the current checkout, is not the authorized one, or the path is not a
    /// file inside the worktree (`bad_symbol`).
    BadSymbol(String),
    /// A protected property of the declaration changed (`signature_moved`).
    SignatureMoved(String),
    /// The symbol's source is not what the packet was built from (`stale_source`).
    StaleSource(String),
    /// Bytes outside the authorized body would change (`bad_symbol`).
    UnauthorizedChange(String),
    /// Insertion has no authorized point, or is not supported (`bad_symbol`).
    InsertionRefused(String),
    /// The file could not be read or written: infrastructure, not the model.
    Io(String),
}

impl EditError {
    /// The reporting class (`docs`: no_parse, bad_symbol, signature_moved, stale_source).
    pub fn class(&self) -> &'static str {
        match self {
            EditError::NoParse(_) => "no_parse",
            EditError::SignatureMoved(_) => "signature_moved",
            EditError::StaleSource(_) => "stale_source",
            EditError::Io(_) => "infrastructure_failure",
            EditError::BadSymbol(_)
            | EditError::UnauthorizedChange(_)
            | EditError::InsertionRefused(_) => "bad_symbol",
        }
    }
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EditError::NoParse(m) => write!(f, "no_parse: {m}"),
            EditError::BadSymbol(m) => write!(f, "bad_symbol: {m}"),
            EditError::SignatureMoved(m) => write!(f, "signature_moved: {m}"),
            EditError::StaleSource(m) => write!(f, "stale_source: {m}"),
            EditError::UnauthorizedChange(m) => write!(f, "unauthorized change: {m}"),
            EditError::InsertionRefused(m) => write!(f, "insertion refused: {m}"),
            EditError::Io(m) => write!(f, "io: {m}"),
        }
    }
}

/// An edit proven safe in memory and not yet written.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreparedEdit {
    pub symbol: String,
    pub path: String,
    pub before_sha: String,
    pub after_sha: String,
    pub body_digest: String,
    /// `sha256:` of path, before and after: the patch's identity.
    pub patch_id: String,
    /// The changed lines, `-`/`+` prefixed: a minimal diff of the body region.
    pub diff: String,
    pub new_text: String,
    pub new_fingerprint: String,
}

/// The file `rel` inside `root`, refused if it is not a plain relative path, escapes the root,
/// or is (or passes through) a symbolic link.
pub fn resolve_inside(root: &Path, rel: &str) -> Result<PathBuf, EditError> {
    check_relative(rel).map_err(|e| EditError::BadSymbol(e.to_string()))?;
    let root = fs::canonicalize(root).map_err(|e| EditError::Io(e.to_string()))?;
    let mut at = root.clone();
    for part in rel.split('/') {
        at.push(part);
        let meta = fs::symlink_metadata(&at)
            .map_err(|_| EditError::BadSymbol(format!("{rel} is not in the worktree")))?;
        if meta.file_type().is_symlink() {
            return Err(EditError::BadSymbol(format!(
                "{rel} passes through a symbolic link"
            )));
        }
    }
    let real = fs::canonicalize(&at).map_err(|e| EditError::Io(e.to_string()))?;
    if !real.starts_with(&root) || !real.is_file() {
        return Err(EditError::BadSymbol(format!(
            "{rel} is not a file inside the worktree"
        )));
    }
    Ok(real)
}

fn diff_lines(before: &str, after: &str) -> String {
    let mut out = String::new();
    for l in before.lines() {
        out.push_str(&format!("-{l}\n"));
    }
    for l in after.lines() {
        out.push_str(&format!("+{l}\n"));
    }
    out
}

/// Prepares replacing the body of `target` (the record the packet was built from) with `body`.
pub fn prepare_replace_body(
    root: &Path,
    target: &SymbolRecord,
    body: &str,
    signature_frozen: bool,
) -> Result<PreparedEdit, EditError> {
    let file = resolve_inside(root, &target.path)?;
    let text = fs::read_to_string(&file).map_err(|e| EditError::Io(e.to_string()))?;
    // 1-2. The symbol, as it is now, is the one the packet named.
    let current = index_text(&target.path, &text)
        .map_err(|e| EditError::StaleSource(format!("the file no longer parses: {e}")))?;
    let Some(now) = current.into_iter().find(|s| s.id == target.id) else {
        return Err(EditError::StaleSource(format!(
            "{} is no longer in {}",
            target.id, target.path
        )));
    };
    if now.fingerprint != target.fingerprint {
        return Err(EditError::StaleSource(format!(
            "{} is {}; the packet was built from {}",
            target.id, now.fingerprint, target.fingerprint
        )));
    }
    // 4. The proposal is a block, and only a block.
    let body = body.trim();
    if !(body.starts_with('{') && body.ends_with('}')) {
        return Err(EditError::NoParse(
            "the body must be one `{ ... }` block".into(),
        ));
    }
    syn::parse_str::<syn::Block>(body).map_err(|e| EditError::NoParse(e.to_string()))?;
    // 5. Spliced into the exact span Chip resolved.
    let mut new_text = String::with_capacity(text.len() + body.len());
    new_text.push_str(&text[..now.body.start]);
    new_text.push_str(body);
    new_text.push_str(&text[now.body.end..]);
    // 6. The file still parses, and the symbol is still there, at the same id.
    let after =
        index_text(&target.path, &new_text).map_err(|e| EditError::NoParse(e.to_string()))?;
    let Some(edited) = after.iter().find(|s| s.id == target.id) else {
        return Err(EditError::SignatureMoved(format!(
            "{} is no longer found after the edit",
            target.id
        )));
    };
    // 7. Protected properties, compared structurally.
    if signature_frozen && let Some(what) = now.signature.first_difference(&edited.signature) {
        return Err(EditError::SignatureMoved(format!(
            "the {what} of {} changed",
            target.id
        )));
    }
    // 8. Nothing outside the body changed: same prefix, same suffix, and every other symbol keeps
    //    its fingerprint.
    let new_body_end = now.body.start + body.len();
    if new_text[..now.body.start] != text[..now.body.start]
        || new_text[new_body_end..] != text[now.body.end..]
    {
        return Err(EditError::UnauthorizedChange(
            "bytes outside the body changed".into(),
        ));
    }
    let before_others = index_text(&target.path, &text).unwrap_or_default();
    for other in before_others.iter().filter(|s| s.id != target.id) {
        match after.iter().find(|s| s.id == other.id) {
            Some(a) if a.fingerprint == other.fingerprint => {}
            _ => {
                return Err(EditError::UnauthorizedChange(format!(
                    "{} would change or disappear",
                    other.id
                )));
            }
        }
    }
    if after.len() != before_others.len() {
        return Err(EditError::UnauthorizedChange(
            "the edit adds or removes a symbol".into(),
        ));
    }
    let before_sha = sha256(text.as_bytes());
    let after_sha = sha256(new_text.as_bytes());
    Ok(PreparedEdit {
        symbol: target.id.clone(),
        path: target.path.clone(),
        patch_id: sha256(format!("{}\n{before_sha}\n{after_sha}", target.path).as_bytes()),
        before_sha,
        after_sha,
        body_digest: sha256(body.as_bytes()),
        diff: diff_lines(&text[now.body.start..now.body.end], body),
        new_fingerprint: edited.fingerprint.clone(),
        new_text,
    })
}

/// Insertion needs an authorized point and a reviewed module and visibility rule. This first
/// implementation has neither, so every insertion is refused explicitly, never widened.
pub fn prepare_insert(
    authorized_after: Option<&str>,
    after: &str,
) -> Result<PreparedEdit, EditError> {
    match authorized_after {
        Some(point) if point == after => Err(EditError::InsertionRefused(
            "insert_symbol is not supported by this implementation".into(),
        )),
        _ => Err(EditError::InsertionRefused(format!(
            "{after:?} is not an authorized insertion point"
        ))),
    }
}

/// What a write found: the file already held the intended content, or it was written now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteOutcome {
    Written,
    AlreadyApplied,
}

/// Writes `new_text` to `rel` only if the file holds `expect_sha` now (or already holds
/// `new_sha`, which makes the write idempotent). Written to a temporary file and renamed.
pub fn write_checked(
    root: &Path,
    rel: &str,
    expect_sha: &str,
    new_sha: &str,
    new_text: &str,
) -> Result<WriteOutcome, EditError> {
    if sha256(new_text.as_bytes()) != new_sha {
        return Err(EditError::UnauthorizedChange(
            "the content does not match its digest".into(),
        ));
    }
    let file = resolve_inside(root, rel)?;
    let current = fs::read(&file).map_err(|e| EditError::Io(e.to_string()))?;
    let current = sha256(&current);
    if current == new_sha {
        return Ok(WriteOutcome::AlreadyApplied);
    }
    if current != expect_sha {
        return Err(EditError::StaleSource(format!(
            "{rel} is {current}; the edit was prepared against {expect_sha}"
        )));
    }
    let tmp = file.with_extension("rs.chip-symbol-loop.tmp");
    fs::write(&tmp, new_text).map_err(|e| EditError::Io(e.to_string()))?;
    fs::rename(&tmp, &file).map_err(|e| EditError::Io(e.to_string()))?;
    Ok(WriteOutcome::Written)
}

/// The sha of `rel` now, if it is a file inside the worktree.
pub fn current_sha(root: &Path, rel: &str) -> Result<String, EditError> {
    let file = resolve_inside(root, rel)?;
    fs::read(&file)
        .map(|b| sha256(&b))
        .map_err(|e| EditError::Io(e.to_string()))
}
