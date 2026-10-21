//! What a project can say about one keyed `project.write` afterwards, from its own files.
//!
//! A write that carries an action key is performed in four steps, each durable before the next:
//!
//! 1. the content is written to a temporary file named for the key
//!    (`.chip-write-<key>.tmp`, created only if absent) and synced;
//! 2. a record is created in `.stack-agent/writes/` naming the key, the target, the content's
//!    SHA-256 and the temporary file's identity (device and inode);
//! 3. the temporary file is renamed over the target;
//! 4. the directory is synced.
//!
//! That is enough to answer, later and from the filesystem alone, what happened to *this* write
//! (not whether the target merely holds the same bytes):
//!
//! * **It happened** if the target is the very file step 1 created: renaming keeps the inode, and no
//!   other write creates a file with it. Matching contents prove nothing (they may predate the
//!   write or come from another process); a later replacement by anyone destroys the identity, and
//!   the answer then is "cannot tell", never "happened".
//! * **It did not, and now cannot** if the project's lock is held by the asker and the key is
//!   *barred*. Only a lock holder writes, and the OS releases the lock when its holder dies, so
//!   whoever holds it knows no write of any earlier holder is still running or can be delivered.
//!   Barring removes the temporary file (so its rename cannot happen) and creates
//!   `<key>.barred`, which a later delivery under the same key finds and refuses.
//!
//! [`inspect`] and [`bar`] take the [`ProjectLock`] as proof. They never perform the write.

use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::confine::Dir;
use crate::lock::ProjectLock;
use crate::{MAX_WRITE_BYTES, STACK_AGENT_DIR, TEMP_PREFIX, confine};

const WRITES_DIR: &str = "writes";

/// Whether `key` has the shape of an action key (`ak1.` and 32 hex digits). Nothing else is ever
/// used to name a file.
pub fn valid_key(key: &str) -> bool {
    key.strip_prefix("ak1.").is_some_and(|hex| {
        hex.len() == 32 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// The temporary file a keyed write stages its content in.
pub fn temp_name(key: &str) -> String {
    format!("{TEMP_PREFIX}{key}.tmp")
}

/// The key a temporary file name stages, if it is a keyed write's.
pub fn key_of_temp(name: &str) -> Option<&str> {
    let key = name.strip_prefix(TEMP_PREFIX)?.strip_suffix(".tmp")?;
    valid_key(key).then_some(key)
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A write's record: written once, before the rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub path: String,
    pub sha256: String,
    pub dev: u64,
    pub ino: u64,
}

impl Prepared {
    fn to_json(&self) -> String {
        serde_json::json!({
            "schema": "chip.project_write_journal.v1",
            "path": self.path, "sha256": self.sha256, "dev": self.dev, "ino": self.ino,
        })
        .to_string()
    }

    fn parse(text: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        if v["schema"] != "chip.project_write_journal.v1" {
            return None;
        }
        Some(Self {
            path: v["path"].as_str()?.to_string(),
            sha256: v["sha256"].as_str()?.to_string(),
            dev: v["dev"].as_u64()?,
            ino: v["ino"].as_u64()?,
        })
    }
}

/// The target as the filesystem holds it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub dev: u64,
    pub ino: u64,
    pub sha256: String,
}

/// What the project holds about one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteState {
    pub prepared: Option<Prepared>,
    pub barred: bool,
    pub target: Option<Target>,
    pub temp_present: bool,
}

fn refusal(code: &str) -> io::Error {
    io::Error::other(code.to_string())
}

/// The journal directory under `root`, created if `create`; `None` when absent and not creating.
fn writes_dir(root: &Path, create: bool) -> io::Result<Option<Dir>> {
    let root = Dir::open(root)?;
    for name in [STACK_AGENT_DIR, WRITES_DIR] {
        // Walked from the handle just opened, never through a link.
        let parent = if name == STACK_AGENT_DIR {
            root.clone()
        } else {
            root.open_dir(STACK_AGENT_DIR)?
        };
        match parent.open_dir(name) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound && create => parent.create_dir(name)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        }
    }
    Ok(Some(root.open_dir(STACK_AGENT_DIR)?.open_dir(WRITES_DIR)?))
}

fn read_text(dir: &Dir, name: &str) -> io::Result<Option<String>> {
    match dir.open_file(name) {
        Ok((mut file, _)) => {
            let mut text = String::new();
            (&mut file).take(64 * 1024).read_to_string(&mut text)?;
            Ok(Some(text))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn create_file(dir: &Dir, name: &str, content: &str) -> io::Result<()> {
    let mut file = dir.create_new(name, 0o600)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    dir.sync()
}

/// Whether `key` has been barred.
fn is_barred(root: &Path, key: &str) -> io::Result<bool> {
    Ok(match writes_dir(root, false)? {
        Some(dir) => dir.entry(&format!("{key}.barred"))?.is_some(),
        None => false,
    })
}

/// Records that `key` can never apply. Idempotent.
fn mark_barred(root: &Path, key: &str) -> io::Result<()> {
    let Some(dir) = writes_dir(root, true)? else {
        return Err(refusal("the write journal is unavailable"));
    };
    match create_file(&dir, &format!("{key}.barred"), "barred\n") {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Used by [`ProjectLock`]'s sweep, which holds the lock: a temporary file it removes can never be
/// renamed, so its key is barred.
pub(crate) fn bar_swept(root: &Path, temp_name: &str) {
    if let Some(key) = key_of_temp(temp_name) {
        let _ = mark_barred(root, key);
    }
}

/// The keyed write: see the module documentation. Errors before the rename leave the target as it
/// was and remove the temporary file; the codes are `operation_barred` (the key was barred: this
/// logical operation can never apply), `already_applied` (this very write is already in place) and
/// `key_bound_elsewhere` (the key was used for another target or content).
pub(crate) fn keyed_write(
    root: &Path,
    parent: &Dir,
    name: &str,
    path: &str,
    content: &[u8],
    mode: Option<u32>,
    key: &str,
) -> io::Result<()> {
    if is_barred(root, key)? {
        return Err(refusal("operation_barred"));
    }
    let digest = sha256(content);
    let journal =
        writes_dir(root, true)?.ok_or_else(|| refusal("the write journal is unavailable"))?;
    let record_name = format!("{key}.prepared");
    // An earlier delivery under this key left a record: this one is the same write only if it
    // asks for the same thing.
    let earlier = read_text(&journal, &record_name)?;
    if let Some(earlier) = &earlier {
        match Prepared::parse(earlier) {
            Some(p) if p.path == path && p.sha256 == digest => {
                // Already applied: the target is still the file this write staged. A second
                // delivery (a late worker, a settled-then-repeated request) does nothing.
                if let Ok((file, _)) = parent.open_file(name) {
                    let meta = file.metadata()?;
                    if (meta.dev(), meta.ino()) == (p.dev, p.ino) {
                        return Err(refusal("already_applied"));
                    }
                }
            }
            _ => return Err(refusal("key_bound_elsewhere")),
        }
    }
    let temporary = temp_name(key);
    let result = (|| {
        let mut file = parent.create_new(&temporary, mode.unwrap_or(0o644))?;
        file.write_all(content)?;
        file.sync_all()?;
        let meta = file.metadata()?;
        drop(file);
        let prepared = Prepared {
            path: path.into(),
            sha256: digest.clone(),
            dev: meta.dev(),
            ino: meta.ino(),
        };
        if earlier.is_some() {
            // The same write asked for again (an operator settled the first as not executed): the
            // record is replaced by one naming this file.
            journal.remove_file(&record_name)?;
        }
        create_file(&journal, &record_name, &prepared.to_json())?;
        parent.rename(&temporary, name)
    })();
    if result.is_err() {
        let _ = parent.remove_file(&temporary);
    } else {
        let _ = parent.sync();
    }
    result
}

fn parent_of(root: &Path, path: &str) -> io::Result<(Dir, String)> {
    let mut parts: Vec<&str> = path.split('/').collect();
    let name = parts.pop().filter(|n| !n.is_empty());
    let Some(name) = name else {
        return Err(refusal("the path names no file"));
    };
    let mut dir = Dir::open(root)?;
    for part in parts {
        dir = dir.open_dir(part)?;
    }
    Ok((dir, name.to_string()))
}

/// What the project holds about `key`, for a write to `path`. The caller holds the project lock.
pub fn inspect(_lock: &ProjectLock, root: &Path, key: &str, path: &str) -> io::Result<WriteState> {
    if !valid_key(key) {
        return Err(refusal("not an action key"));
    }
    let journal = writes_dir(root, false)?;
    let prepared = match &journal {
        Some(dir) => read_text(dir, &format!("{key}.prepared"))?.and_then(|t| Prepared::parse(&t)),
        None => None,
    };
    let barred = is_barred(root, key)?;
    let (target, temp_present) = match parent_of(root, path) {
        Ok((parent, name)) => {
            let target = match parent.open_file(&name) {
                Ok((mut file, _)) => {
                    let meta = file.metadata()?;
                    let mut bytes = Vec::new();
                    (&mut file)
                        .take(MAX_WRITE_BYTES as u64 + 1)
                        .read_to_end(&mut bytes)?;
                    Some(Target {
                        dev: meta.dev(),
                        ino: meta.ino(),
                        sha256: sha256(&bytes),
                    })
                }
                Err(e)
                    if e.kind() == io::ErrorKind::NotFound
                        || confine::is_symlink_refusal(&e)
                        || e.to_string() == "not a regular file" =>
                {
                    None
                }
                Err(e) => return Err(e),
            };
            (target, parent.entry(&temp_name(key))?.is_some())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (None, false),
        Err(e) => return Err(e),
    };
    Ok(WriteState {
        prepared,
        barred,
        target,
        temp_present,
    })
}

/// Bars `key` for a write to `path`: removes its temporary file, if any, so its rename cannot
/// happen, and records the bar. The caller holds the project lock, so no writer is running.
/// Idempotent. It performs nothing the write would have.
pub fn bar(_lock: &ProjectLock, root: &Path, key: &str, path: &str) -> io::Result<()> {
    if !valid_key(key) {
        return Err(refusal("not an action key"));
    }
    match parent_of(root, path) {
        Ok((parent, _)) => match parent.remove_file(&temp_name(key)) {
            Ok(()) => {
                let _ = parent.sync();
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    mark_barred(root, key)
}
