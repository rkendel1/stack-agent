//! Which binary produced a report or an event stream.
//!
//! The version and commit are fixed at build time (`build.rs`); the digest is the SHA-256 of the
//! running executable, computed once on first use, so two binaries with the same version and commit
//! but different bytes (a different compiler, a patched build) are told apart. If the executable
//! cannot be read the digest is `null`, never a guess.

use std::sync::OnceLock;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const COMMIT: &str = env!("STACK_AGENT_COMMIT");
pub const DIRTY: &str = env!("STACK_AGENT_DIRTY");

/// `stack-agent 0.1.0+abcdef123456` (or `+abcdef123456.dirty`): short enough for every event line.
pub fn short() -> String {
    if DIRTY == "dirty" {
        format!("stack-agent {VERSION}+{COMMIT}.dirty")
    } else {
        format!("stack-agent {VERSION}+{COMMIT}")
    }
}

fn digest() -> Option<&'static str> {
    static DIGEST: OnceLock<Option<String>> = OnceLock::new();
    DIGEST
        .get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            let mut file = std::fs::File::open(exe).ok()?;
            let mut hasher = Sha256::new();
            std::io::copy(&mut file, &mut hasher).ok()?;
            Some(
                hasher
                    .finalize()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect(),
            )
        })
        .as_deref()
}

/// The full identity, for a report.
pub fn report() -> Value {
    json!({
        "name": "stack-agent",
        "version": VERSION,
        "commit": COMMIT,
        "tree": DIRTY,
        "target": env!("STACK_AGENT_TARGET"),
        "profile": env!("STACK_AGENT_PROFILE"),
        "executable_sha256": digest(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_names_the_version_the_commit_and_the_executable() {
        let r = report();
        assert_eq!(r["version"], VERSION);
        assert!(!r["commit"].as_str().unwrap().is_empty());
        assert_eq!(r["executable_sha256"].as_str().map(str::len), Some(64));
        assert!(short().starts_with(&format!("stack-agent {VERSION}+")));
    }
}
