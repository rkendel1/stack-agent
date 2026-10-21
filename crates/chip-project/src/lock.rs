//! One stack-agent work per project directory, across processes.
//!
//! `Environments` lets one work own a mutable environment, but only within one process: two
//! `stack-agent work` processes on one directory could both write it, and one could report a
//! verified result for a tree the other then changed (audit HA-04). [`ProjectLock`] closes that.
//! It is an exclusive OS lock: `flock` on the project's root directory itself on Unix, so nothing is
//! created and a read-only checkout can still be locked; elsewhere `LockFileEx` on
//! `<project>/.stack-agent/work.lock`, which no capability can address. The OS releases it when
//! the process exits however it exits, so a crashed work never leaves the project locked.
//!
//! Only a lock holder writes temporary files, so whoever acquires the lock knows that every
//! `.chip-write-*.tmp` in the project was left by a write that never finished (a crash between
//! the temporary file and the rename). The lock's holder removes them and reports their paths.

use std::fs::File;
use std::io;
use std::path::Path;

use crate::confine::{Dir, EntryKind, lock_handle};
use crate::{STACK_AGENT_DIR, TEMP_PREFIX};

/// The most directory entries the sweep for stray temporary files examines. A larger project is
/// swept up to this bound and says so (`sweep_complete: false`).
pub const MAX_SWEEP_ENTRIES: usize = 50_000;

/// Why a work cannot have the project.
#[derive(Debug)]
pub enum LockError {
    /// Another process holds the lock: a work is running in this directory.
    Held,
    /// The lock could not be taken (the root cannot be opened, a filesystem without locks; off
    /// Unix also an unwritable directory or a `.stack-agent` that is not a directory). Fails
    /// closed: nothing runs unlocked.
    Unavailable(String),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Held => write!(
                f,
                "another stack-agent work is running in this project directory"
            ),
            Self::Unavailable(why) => write!(f, "the project lock is unavailable ({why})"),
        }
    }
}

impl std::error::Error for LockError {}

/// The exclusive lock a work holds on its project for its whole run. Dropping it releases it.
#[derive(Debug)]
pub struct ProjectLock {
    _file: File,
    /// Project-relative paths of the temporary files an interrupted write left, now removed.
    pub recovered: Vec<String>,
    /// Whether the sweep examined the whole project (false past [`MAX_SWEEP_ENTRIES`]).
    pub sweep_complete: bool,
}

impl ProjectLock {
    /// Takes the lock without waiting. `Held` when another process has it.
    pub fn acquire(root: &Path) -> Result<Self, LockError> {
        let unavailable =
            |what: &str, e: io::Error| LockError::Unavailable(format!("{what}: {}", e.kind()));
        // Never through a link: a repository cannot point the lock somewhere else.
        let file = lock_handle(root).map_err(|e| unavailable("the lock handle", e))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(LockError::Held),
            Err(std::fs::TryLockError::Error(e)) => return Err(unavailable("locking", e)),
        }
        let root_path = root;
        let root = Dir::open(root).map_err(|e| unavailable("the project root", e))?;
        let mut sweep = Sweep {
            root: root_path.to_path_buf(),
            examined: 0,
            recovered: Vec::new(),
            complete: true,
        };
        sweep.directory(&root, None);
        Ok(Self {
            _file: file,
            recovered: sweep.recovered,
            sweep_complete: sweep.complete,
        })
    }
}

struct Sweep {
    root: std::path::PathBuf,
    examined: usize,
    recovered: Vec<String>,
    complete: bool,
}

impl Sweep {
    fn directory(&mut self, dir: &Dir, rel: Option<&str>) {
        let Ok(entries) = dir.entries() else { return };
        for (name, entry) in entries {
            if self.examined >= MAX_SWEEP_ENTRIES {
                self.complete = false;
                return;
            }
            self.examined += 1;
            let Ok(name) = name else { continue };
            let path = match rel {
                Some(r) => format!("{r}/{name}"),
                None => name.clone(),
            };
            match entry.kind {
                EntryKind::File if name.starts_with(TEMP_PREFIX) && name.ends_with(".tmp") => {
                    if dir.remove_file(&name).is_ok() {
                        // A keyed write whose rename can now never happen is barred, so the
                        // project can say so later (see `journal`).
                        #[cfg(unix)]
                        crate::journal::bar_swept(&self.root, &name);
                        self.recovered.push(path);
                    }
                }
                // Never into a link; not into the Git directory, the host's own state or build
                // output, where no capability writes.
                EntryKind::Directory
                    if !matches!(
                        name.as_str(),
                        ".git" | STACK_AGENT_DIR | "target" | "node_modules"
                    ) =>
                {
                    if let Ok(sub) = dir.open_dir(&name) {
                        self.directory(&sub, Some(&path));
                    }
                }
                _ => {}
            }
        }
    }
}
