//! A credential that cannot be printed by accident.

use std::fmt;

/// A secret value supplied by the host. `Debug` never shows it, and nothing in this crate
/// copies it into an error, event or diagnostic.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// Wraps a credential.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value, for building the one authorization header.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    /// Replaces every occurrence of the secret in `text`.
    pub(crate) fn scrub(&self, text: &str) -> String {
        if self.0.is_empty() {
            text.to_owned()
        } else {
            text.replace(&self.0, "[redacted]")
        }
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}
