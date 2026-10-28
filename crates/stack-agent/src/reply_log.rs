//! `work --record-replies FILE`: the model's replies, as text, with secrets removed.
//!
//! By default replies are kept only as digests. This writes each reply (the model's raw output, before
//! any interpretation) as one JSON line as it arrives, after redaction:
//!
//! - every secret value the run was given, exactly: the API key variables it can read
//!   (`CHIP_API_KEY`, the one `--api-key-env` names, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`) and any
//!   variable named in `--redact-env`, when 8 or more characters long;
//! - any token that starts like a common credential (`sk-`, `sk_`, `ghp_`, `gho_`, `xoxb-`, `AKIA`)
//!   and runs on for 16 or more key characters;
//! - the token after `Bearer `.
//!
//! This is best effort and says so in every line (`"redaction": "best-effort"`): a reply may still hold
//! something sensitive that looks like none of these (a password in prose, a private path). The file is
//! created with owner-only permissions. Reasoning text the server returns is whatever the model put in
//! its reply content; nothing else is requested or recorded.

use std::io::Write;
use std::sync::Mutex;

use serde_json::json;

pub const SCHEMA: &str = "chip.reply.v1";
const PLACEHOLDER: &str = "[REDACTED]";
const MIN_SECRET: usize = 8;

#[derive(Debug, Clone, Default)]
pub struct Redactor {
    secrets: Vec<String>,
}

fn key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

impl Redactor {
    pub fn new(secrets: impl IntoIterator<Item = String>) -> Self {
        let mut secrets: Vec<String> = secrets
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| s.len() >= MIN_SECRET)
            .collect();
        // Longest first, so one secret that contains another is removed whole.
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Self { secrets }
    }

    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for secret in &self.secrets {
            out = out.replace(secret.as_str(), PLACEHOLDER);
        }
        redact_patterns(&out)
    }
}

fn redact_patterns(text: &str) -> String {
    const PREFIXES: [&str; 6] = ["sk-", "sk_", "ghp_", "gho_", "xoxb-", "AKIA"];
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let start = chars[i].0;
        let rest = &text[start..];
        // Only at the start of a word: a prefix in the middle of a longer word is not a token.
        let at_boundary = i == 0 || !key_char(chars[i - 1].1);
        if at_boundary {
            if let Some(after) = rest.strip_prefix("Bearer ") {
                let len = after
                    .chars()
                    .take_while(|c| key_char(*c) || *c == '.')
                    .count();
                if len > 0 {
                    out.push_str("Bearer ");
                    out.push_str(PLACEHOLDER);
                    i += "Bearer ".len() + len;
                    continue;
                }
            }
            if let Some(prefix) = PREFIXES.iter().find(|p| rest.starts_with(**p)) {
                let body = rest[prefix.len()..]
                    .chars()
                    .take_while(|c| key_char(*c))
                    .count();
                if body >= 16 {
                    out.push_str(PLACEHOLDER);
                    i += prefix.len() + body;
                    continue;
                }
            }
        }
        out.push(chars[i].1);
        i += 1;
    }
    out
}

pub struct ReplyLog {
    out: Mutex<Option<std::fs::File>>,
    redactor: Redactor,
}

impl std::fmt::Debug for ReplyLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReplyLog")
    }
}

impl ReplyLog {
    pub fn create(path: &std::path::Path, redactor: Redactor) -> Result<Self, String> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options
            .open(path)
            .map_err(|e| format!("--record-replies {}: {}", path.display(), e.kind()))?;
        Ok(Self {
            out: Mutex::new(Some(file)),
            redactor,
        })
    }

    /// Never fails the work: a write error stops the log and says so once.
    pub fn record(&self, turn: usize, model: &str, output: &str) {
        let mut guard = self.out.lock().unwrap_or_else(|e| e.into_inner());
        let Some(file) = guard.as_mut() else { return };
        let line = json!({
            "schema": SCHEMA, "turn": turn, "model": model,
            "redaction": "best-effort", "output": self.redactor.redact(output),
        });
        if let Err(e) = writeln!(file, "{line}").and_then(|()| file.flush()) {
            eprintln!(
                "note: the reply log stopped ({}); the work continues",
                e.kind()
            );
            *guard = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_secrets_credential_shapes_and_bearer_tokens_are_removed() {
        let r = Redactor::new(["my-very-secret-key".to_string(), "short".to_string()]);
        let text = "key my-very-secret-key; sk-abcdefghijklmnopqrstuv and ghp_0123456789abcdefABCDEF; \
                    Authorization: Bearer abc.def-123; AKIAIOSFODNN7EXAMPLE; short stays";
        let out = r.redact(text);
        for gone in [
            "my-very-secret-key",
            "sk-abcdef",
            "ghp_0123",
            "abc.def-123",
            "AKIAIOSFODNN7",
        ] {
            assert!(!out.contains(gone), "{gone} survived: {out}");
        }
        assert!(
            out.contains("short stays"),
            "a secret under 8 characters is not treated as one"
        );
        assert!(out.contains("Bearer [REDACTED]"));
    }

    #[test]
    fn ordinary_text_and_short_look_alikes_are_untouched() {
        let r = Redactor::default();
        let text = "the task-list has risk-assessment and sk-1 and a skeleton: sk-short";
        assert_eq!(r.redact(text), text);
    }
}
