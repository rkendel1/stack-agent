//! What `project.write` can be asked afterwards, answered from the project's own files.
//!
//! The destination is the project directory, and its contract is in `chip_project::journal`: a
//! keyed write records which file it staged (device and inode) before renaming it into place, and
//! whoever holds the project lock knows no earlier writer is still running. So, with the lock held:
//!
//! * the target **is the write's own staged file** -> it happened (`Committed`). Matching contents
//!   alone never says so;
//! * the write's staged file is gone or never existed and the key is barred -> it did not, and
//!   cannot (`NotApplied { barred }`);
//! * anything else, including a target replaced since -> `Unknown`.
//!
//! The business effect is read separately: whether the target holds the content that was asked for.
//! Neither answer dispatches the write. Barring is the project refusing the original write for
//! good; it is what makes the answer `NotExecuted` true, and a later delivery under the same key is
//! refused (`operation_barred`).
//!
//! Unix only: elsewhere the journal does not exist and every answer is `Unknown`.

use std::path::{Path, PathBuf};

use chip_agent_state::{BusinessCheck, DestinationStatus, ReconcileQuery, Reconciler};
use chip_core::InputValue;
use chip_project::PROJECT_WRITE;

pub struct ProjectWriteReconciler {
    root: PathBuf,
}

impl ProjectWriteReconciler {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

/// The write's target and content as the query carries them.
fn request_of(q: &ReconcileQuery) -> Option<(String, String)> {
    match (q.inputs.get("path"), q.inputs.get("content")) {
        (Some(InputValue::Text(path)), Some(InputValue::Text(content))) => {
            Some((path.clone(), content.clone()))
        }
        _ => None,
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use chip_project::journal;
    use chip_project::{LockError, ProjectLock};
    use sha2::{Digest, Sha256};

    fn sha256(text: &str) -> String {
        Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn locked(root: &Path) -> Result<ProjectLock, String> {
        ProjectLock::acquire(root).map_err(|e| match e {
            LockError::Held => "a work holds the project lock, so a writer that is still running cannot be ruled out".into(),
            LockError::Unavailable(why) => format!("the project lock is unavailable ({why})"),
        })
    }

    pub(super) fn status(root: &Path, q: &ReconcileQuery) -> DestinationStatus {
        let unknown = |reason: String| DestinationStatus::Unknown { reason };
        let Some((path, content)) = request_of(q) else {
            return unknown("the recorded request has no path and content to ask about".into());
        };
        let lock = match locked(root) {
            Ok(lock) => lock,
            Err(why) => return unknown(why),
        };
        let key = &q.operation.key;
        let state = match journal::inspect(&lock, root, key, &path) {
            Ok(state) => state,
            Err(e) => return unknown(format!("the project's write record could not be read: {e}")),
        };
        let wanted = sha256(&content);
        if let Some(p) = &state.prepared {
            if p.path != path || p.sha256 != wanted {
                return unknown(
                    "the project's record binds this key to another target or content".into(),
                );
            }
            if let Some(t) = &state.target
                && (t.dev, t.ino) == (p.dev, p.ino)
            {
                return DestinationStatus::Committed {
                    operation: q.operation.clone(),
                    receipt: Some(format!("file-id:{}:{}", p.dev, p.ino)),
                    reported: format!(
                        "the target is the file this write staged (content sha256 {})",
                        &p.sha256[..12]
                    ),
                };
            }
            if !state.barred && !state.temp_present {
                return unknown("the write staged a file and the target is no longer it, and nothing records that the staged file was removed unrenamed: it may have been renamed and replaced since, or never renamed".into());
            }
        }
        // No record of the write's rename, and the lock is ours: no writer is running. Barring
        // removes the staged file, if any, so the rename cannot happen after this answer.
        match journal::bar(&lock, root, key, &path) {
            Ok(()) => DestinationStatus::NotApplied {
                barred: true,
                basis: "the project is locked by the asker and the write's key is barred: its staged file is removed, so its rename can never happen".into(),
            },
            Err(e) => unknown(format!("the key could not be barred: {e}")),
        }
    }

    pub(super) fn verify(root: &Path, q: &ReconcileQuery) -> BusinessCheck {
        let unavailable = |reason: String| BusinessCheck::Unavailable { reason };
        let Some((path, content)) = request_of(q) else {
            return unavailable("the recorded request has no path and content".into());
        };
        let lock = match locked(root) {
            Ok(lock) => lock,
            Err(why) => return unavailable(why),
        };
        match journal::inspect(&lock, root, &q.operation.key, &path) {
            Ok(state) => match state.target {
                Some(t) if t.sha256 == sha256(&content) => BusinessCheck::Present {
                    evidence: "the target holds the content that was asked for".into(),
                },
                Some(_) => BusinessCheck::Absent {
                    evidence: "the target holds other content than was asked for".into(),
                },
                None => BusinessCheck::Absent {
                    evidence: "the target does not exist".into(),
                },
            },
            Err(e) => unavailable(format!("the target could not be read: {e}")),
        }
    }
}

#[async_trait::async_trait]
impl Reconciler for ProjectWriteReconciler {
    fn handles(&self, capability: &str) -> bool {
        capability == PROJECT_WRITE
    }

    async fn status(&self, q: &ReconcileQuery) -> DestinationStatus {
        #[cfg(unix)]
        {
            let (root, q) = (self.root.clone(), q.clone());
            return tokio::task::spawn_blocking(move || unix::status(&root, &q))
                .await
                .unwrap_or(DestinationStatus::Unknown {
                    reason: "the query did not complete".into(),
                });
        }
        #[cfg(not(unix))]
        {
            let _ = q;
            DestinationStatus::Unknown {
                reason: "the write journal exists only on Unix".into(),
            }
        }
    }

    async fn verify(&self, q: &ReconcileQuery) -> BusinessCheck {
        #[cfg(unix)]
        {
            let (root, q) = (self.root.clone(), q.clone());
            return tokio::task::spawn_blocking(move || unix::verify(&root, &q))
                .await
                .unwrap_or(BusinessCheck::Unavailable {
                    reason: "the query did not complete".into(),
                });
        }
        #[cfg(not(unix))]
        {
            let _ = q;
            BusinessCheck::Unavailable {
                reason: "the write journal exists only on Unix".into(),
            }
        }
    }
}
