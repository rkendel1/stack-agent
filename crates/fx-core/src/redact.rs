//! Secret masking for diagnostic text.
//!
//! Started as a port of the oracle's `maskSecrets` (src/core/shared/text_utils.zig:430-720) and now
//! follows the redaction rules of Beacon's endpoint telemetry (`pkg/asymptoteobserve/privacy.go` in
//! asymptote-labs/agent-beacon, MIT), which redacts every string an agent runtime reports. The
//! rules, in the order they apply:
//!
//! * a PEM private-key block, whole (to the end of the text when it was cut);
//! * the password in any URL's userinfo (`postgres://user:pw@host`, `https://x:t@github.com`);
//! * a labeled value: any label that *ends* in a secret word (`token`, `secret`, `password`,
//!   `passwd`, `passphrase`, `api_key`, `access_key`, `secret_key`, `private_key`,
//!   `client_secret`, `authorization`, `credential(s)`, `cookie`, `database_url`) followed by `=`
//!   or `:` and a quoted or bare value, in prose, shell, env files, YAML, TOML or JSON. The label
//!   must end in the word, so `max_tokens=4096` and `input_tokens: 812` stay readable. An
//!   authorization scheme (`Bearer`, `Basic`, `Token`, `Digest`) is kept and its credential masked;
//! * a command-line flag whose name ends in `password`, `passwd`, `passphrase`, `token`, `secret`
//!   or `key` (`--password x`, `--token=x`, `--api-key "x"`);
//! * a `Bearer` credential wherever it appears;
//! * credentials recognizable by their shape: OpenAI/Anthropic `sk-`, Stripe `sk_`/`rk_`/`pk_`
//!   live and test keys, GitHub `gh[pousr]_` and `github_pat_`, GitLab `glpat-`, Slack `xox?-`,
//!   Google `AIza`, npm `npm_`, Hugging Face `hf_`, AWS access key ids, and JWTs. Their public type
//!   prefix is kept (`ghp_[redacted]`), the way secret scanners report a finding;
//! * finally, any other copy of a value masked by a label or a flag, so `TOKEN=abc… … abc…` loses
//!   both.
//!
//! Masking runs before diagnostic text is truncated, so a secret that straddles the cut is
//! recognized whole rather than left as a recognizable prefix.

/// What a masked span is replaced with.
pub const REDACTED: &str = "[redacted]";

/// Words a label must end in, compared after removing `_`, `-` and `.` and folding ASCII case.
const LABEL_SUFFIXES: &[&str] = &[
    "apikey",
    "accesskey",
    "secretkey",
    "privatekey",
    "clientsecret",
    "token",
    "secret",
    "password",
    "passwd",
    "passphrase",
    "authorization",
    "credential",
    "credentials",
    "cookie",
    "databaseurl",
];

/// Words a command-line flag's name must end in.
const FLAG_SUFFIXES: &[&str] = &["password", "passwd", "passphrase", "token", "secret", "key"];

const AUTH_SCHEMES: &[&str] = &["bearer", "basic", "token", "digest"];

/// A credential recognizable by its shape: the public prefix, which is kept, the least number of
/// characters after it, the most (`usize::MAX`: unbounded), and which characters it is made of.
struct Shape {
    prefix: &'static str,
    min: usize,
    max: usize,
    chars: fn(u8) -> bool,
}

fn alnum(c: u8) -> bool {
    c.is_ascii_alphanumeric()
}
fn alnum_dash(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_'
}
fn alnum_underscore(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}
fn upper_digit(c: u8) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit()
}

const SHAPES: &[Shape] = &[
    Shape {
        prefix: "sk-",
        min: 13,
        max: usize::MAX,
        chars: alnum_dash,
    },
    Shape {
        prefix: "sk_live_",
        min: 8,
        max: usize::MAX,
        chars: alnum,
    },
    Shape {
        prefix: "sk_test_",
        min: 8,
        max: usize::MAX,
        chars: alnum,
    },
    Shape {
        prefix: "rk_live_",
        min: 8,
        max: usize::MAX,
        chars: alnum,
    },
    Shape {
        prefix: "rk_test_",
        min: 8,
        max: usize::MAX,
        chars: alnum,
    },
    Shape {
        prefix: "pk_live_",
        min: 8,
        max: usize::MAX,
        chars: alnum,
    },
    Shape {
        prefix: "github_pat_",
        min: 22,
        max: usize::MAX,
        chars: alnum_underscore,
    },
    Shape {
        prefix: "ghp_",
        min: 30,
        max: usize::MAX,
        chars: alnum,
    },
    Shape {
        prefix: "gho_",
        min: 30,
        max: usize::MAX,
        chars: alnum,
    },
    Shape {
        prefix: "ghu_",
        min: 30,
        max: usize::MAX,
        chars: alnum,
    },
    Shape {
        prefix: "ghs_",
        min: 30,
        max: usize::MAX,
        chars: alnum,
    },
    Shape {
        prefix: "ghr_",
        min: 30,
        max: usize::MAX,
        chars: alnum,
    },
    Shape {
        prefix: "glpat-",
        min: 20,
        max: usize::MAX,
        chars: alnum_dash,
    },
    Shape {
        prefix: "xoxa-",
        min: 10,
        max: usize::MAX,
        chars: alnum_dash,
    },
    Shape {
        prefix: "xoxb-",
        min: 10,
        max: usize::MAX,
        chars: alnum_dash,
    },
    Shape {
        prefix: "xoxp-",
        min: 10,
        max: usize::MAX,
        chars: alnum_dash,
    },
    Shape {
        prefix: "xoxo-",
        min: 10,
        max: usize::MAX,
        chars: alnum_dash,
    },
    Shape {
        prefix: "xoxr-",
        min: 10,
        max: usize::MAX,
        chars: alnum_dash,
    },
    Shape {
        prefix: "xoxs-",
        min: 10,
        max: usize::MAX,
        chars: alnum_dash,
    },
    Shape {
        prefix: "AIza",
        min: 35,
        max: 35,
        chars: alnum_dash,
    },
    Shape {
        prefix: "npm_",
        min: 36,
        max: 36,
        chars: alnum,
    },
    Shape {
        prefix: "hf_",
        min: 30,
        max: usize::MAX,
        chars: alnum,
    },
    Shape {
        prefix: "AKIA",
        min: 16,
        max: 16,
        chars: upper_digit,
    },
    Shape {
        prefix: "ASIA",
        min: 16,
        max: 16,
        chars: upper_digit,
    },
    Shape {
        prefix: "ABIA",
        min: 16,
        max: 16,
        chars: upper_digit,
    },
    Shape {
        prefix: "ACCA",
        min: 16,
        max: 16,
        chars: upper_digit,
    },
    Shape {
        prefix: "AIDA",
        min: 16,
        max: 16,
        chars: upper_digit,
    },
    Shape {
        prefix: "AGPA",
        min: 16,
        max: 16,
        chars: upper_digit,
    },
    Shape {
        prefix: "AROA",
        min: 16,
        max: 16,
        chars: upper_digit,
    },
    Shape {
        prefix: "ANPA",
        min: 16,
        max: 16,
        chars: upper_digit,
    },
];

/// A span to replace: bytes `keep_until..end` become [`REDACTED`]. `value` marks a labeled or
/// flagged value, whose other copies are masked too.
struct Span {
    keep_until: usize,
    end: usize,
    value: bool,
}

/// Returns `text` with secret-shaped values replaced by `[redacted]`.
pub fn mask_secrets(text: &str) -> String {
    let text = mask_private_keys(text);
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copies: Vec<&str> = Vec::new();
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if !text.is_char_boundary(i) {
            i += 1;
            continue;
        }
        if let Some(span) = find_span(bytes, i) {
            out.push_str(&text[copied..span.keep_until]);
            out.push_str(REDACTED);
            if span.value && span.end - span.keep_until >= 8 {
                copies.push(&text[span.keep_until..span.end]);
            }
            copied = span.end;
            i = span.end;
        } else {
            i += 1;
        }
    }
    out.push_str(&text[copied..]);
    // Longest first, so a value that contains another is replaced whole.
    copies.sort_by_key(|c| std::cmp::Reverse(c.len()));
    copies.dedup();
    for copy in copies {
        if out.contains(copy) {
            out = out.replace(copy, REDACTED);
        }
    }
    out
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn is_label_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.')
}

fn is_quote(c: u8) -> bool {
    matches!(c, b'"' | b'\'' | b'`')
}

fn starts_word(text: &[u8], start: usize) -> bool {
    start == 0 || !is_word(text[start - 1])
}

fn skip_blanks(text: &[u8], mut i: usize) -> usize {
    while i < text.len() && (text[i] == b' ' || text[i] == b'\t') {
        i += 1;
    }
    i
}

/// Normalised for suffix comparison: ASCII lower case without `_`, `-` and `.`.
fn normalised(label: &[u8]) -> String {
    label
        .iter()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase() as char)
        .collect()
}

fn find_span(text: &[u8], start: usize) -> Option<Span> {
    url_password(text, start)
        .or_else(|| labeled_value(text, start))
        .or_else(|| flag_value(text, start))
        .or_else(|| bearer(text, start))
        .or_else(|| shape(text, start))
        .or_else(|| jwt(text, start))
}

/// The end of a value starting at `i`: quoted (to the closing quote, never past a line), or bare
/// (to whitespace or punctuation that ends a value). `(content_start, content_end)`.
fn value_at(text: &[u8], i: usize) -> Option<(usize, usize)> {
    // Already masked: masking is idempotent.
    if i >= text.len() || text[i..].starts_with(REDACTED.as_bytes()) {
        return None;
    }
    let (start, end) = if is_quote(text[i]) {
        let quote = text[i];
        let start = i + 1;
        let mut end = start;
        while end < text.len() && text[end] != quote && text[end] != b'\n' && text[end] != b'\r' {
            end += 1;
        }
        (start, end)
    } else {
        let mut end = i;
        while end < text.len()
            && !text[end].is_ascii_whitespace()
            && !is_quote(text[end])
            && !matches!(
                text[end],
                b',' | b';' | b')' | b'}' | b']' | b'&' | b'<' | b'>'
            )
        {
            end += 1;
        }
        (i, end)
    };
    (end > start).then_some((start, end))
}

/// Whether a value is a literal that holds no secret.
fn is_literal(value: &[u8]) -> bool {
    let lower = value.to_ascii_lowercase();
    matches!(lower.as_slice(), b"null" | b"true" | b"false" | b"none")
        || value.starts_with(REDACTED.as_bytes())
}

fn is_auth_scheme(value: &[u8]) -> bool {
    let lower = value.to_ascii_lowercase();
    AUTH_SCHEMES.iter().any(|s| lower == s.as_bytes())
}

/// `label=value`, `label: value`, `"label": "value"`. The label starts a word and ends in a secret
/// word; the value is quoted or bare.
fn labeled_value(text: &[u8], start: usize) -> Option<Span> {
    if !is_label_char(text[start]) || (start > 0 && is_label_char(text[start - 1])) {
        return None;
    }
    let mut end = start;
    while end < text.len() && is_label_char(text[end]) {
        end += 1;
    }
    let label = normalised(&text[start..end]);
    if !LABEL_SUFFIXES.iter().any(|s| label.ends_with(s)) {
        return None;
    }
    let mut i = end;
    // The closing quote of a quoted label.
    if i < text.len() && is_quote(text[i]) {
        i += 1;
    }
    i = skip_blanks(text, i);
    if i >= text.len() || !matches!(text[i], b'=' | b':') {
        return None;
    }
    // `token::Kind`, `token == x`, `token => x` are code, not assignments.
    if text
        .get(i + 1)
        .is_some_and(|c| matches!(c, b':' | b'=' | b'>'))
    {
        return None;
    }
    i = skip_blanks(text, i + 1);
    let (value_start, value_end) = value_at(text, i)?;
    let value = &text[value_start..value_end];
    if is_literal(value) {
        return None;
    }
    if is_auth_scheme(value) {
        // `Authorization: Bearer x`: the scheme stays, the credential after it goes.
        let next = skip_blanks(text, value_end);
        if next == value_end {
            return None;
        }
        let (credential_start, credential_end) = value_at(text, next)?;
        return Some(Span {
            keep_until: credential_start,
            end: credential_end,
            value: true,
        });
    }
    Some(Span {
        keep_until: value_start,
        end: value_end,
        value: true,
    })
}

/// `--password x`, `-token=x`, `--api-key "x"`, at the start of the text or after whitespace.
fn flag_value(text: &[u8], start: usize) -> Option<Span> {
    if text[start] != b'-' || (start > 0 && !text[start - 1].is_ascii_whitespace()) {
        return None;
    }
    let mut i = start;
    while i < text.len() && text[i] == b'-' && i - start < 2 {
        i += 1;
    }
    let name_start = i;
    while i < text.len() && (text[i].is_ascii_alphanumeric() || text[i] == b'-' || text[i] == b'_')
    {
        i += 1;
    }
    let name = normalised(&text[name_start..i]);
    if name.is_empty() || !FLAG_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        return None;
    }
    if i < text.len() && text[i] == b'=' {
        i += 1;
    } else {
        let after = skip_blanks(text, i);
        if after == i {
            return None;
        }
        i = after;
    }
    // A flag followed by another flag has no value.
    if i >= text.len() || text[i] == b'-' {
        return None;
    }
    let (value_start, value_end) = value_at(text, i)?;
    Some(Span {
        keep_until: value_start,
        end: value_end,
        value: true,
    })
}

/// `Bearer <credential>`, wherever it appears.
fn bearer(text: &[u8], start: usize) -> Option<Span> {
    let word = b"bearer";
    if !starts_word(text, start)
        || start + word.len() >= text.len()
        || !text[start..start + word.len()].eq_ignore_ascii_case(word)
    {
        return None;
    }
    let value_start = skip_blanks(text, start + word.len());
    if value_start == start + word.len() {
        return None;
    }
    let mut end = value_start;
    while end < text.len()
        && (text[end].is_ascii_alphanumeric()
            || matches!(text[end], b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-'))
    {
        end += 1;
    }
    (end - value_start >= 8 && !text[value_start..end].starts_with(REDACTED.as_bytes())).then_some(
        Span {
            keep_until: value_start,
            end,
            value: false,
        },
    )
}

/// The password in a URL's userinfo, any scheme: `scheme://user:password@host`.
fn url_password(text: &[u8], start: usize) -> Option<Span> {
    // `start` is the first character of the scheme.
    if !text[start].is_ascii_alphabetic() || (start > 0 && text[start - 1].is_ascii_alphanumeric())
    {
        return None;
    }
    let mut i = start;
    while i < text.len()
        && (text[i].is_ascii_alphanumeric() || matches!(text[i], b'+' | b'.' | b'-'))
    {
        i += 1;
    }
    if !text[i..].starts_with(b"://") {
        return None;
    }
    let user_start = i + 3;
    let mut colon = None;
    let mut j = user_start;
    while j < text.len() {
        match text[j] {
            b'/' | b'?' | b'#' | b'"' | b'\'' | b'<' | b'>' => return None,
            c if c.is_ascii_whitespace() => return None,
            b':' if colon.is_none() => colon = Some(j),
            b'@' => {
                let colon = colon?;
                return (colon > user_start && j > colon + 1).then_some(Span {
                    keep_until: colon + 1,
                    end: j,
                    value: false,
                });
            }
            _ => {}
        }
        j += 1;
    }
    None
}

fn shape(text: &[u8], start: usize) -> Option<Span> {
    if !starts_word(text, start) || (start > 0 && matches!(text[start - 1], b'-' | b'.')) {
        return None;
    }
    for shape in SHAPES {
        let prefix = shape.prefix.as_bytes();
        if !text[start..].starts_with(prefix) {
            continue;
        }
        let body = start + prefix.len();
        let mut end = body;
        while end < text.len() && (shape.chars)(text[end]) && end - body < shape.max {
            end += 1;
        }
        let len = end - body;
        let bounded = shape.max == usize::MAX || end == text.len() || !(shape.chars)(text[end]);
        if len >= shape.min && bounded && !(end < text.len() && is_word(text[end])) {
            return Some(Span {
                keep_until: body,
                end,
                value: false,
            });
        }
    }
    None
}

/// `eyJ<header>.eyJ<payload>.<signature>`: the public `eyJ` stays.
fn jwt(text: &[u8], start: usize) -> Option<Span> {
    if !starts_word(text, start) || !text[start..].starts_with(b"eyJ") {
        return None;
    }
    let part = |from: usize| {
        let mut end = from;
        while end < text.len() && alnum_dash(text[end]) {
            end += 1;
        }
        (end - from >= 8).then_some(end)
    };
    let header = part(start + 3)?;
    if !text[header..].starts_with(b".eyJ") {
        return None;
    }
    let payload = part(header + 4)?;
    if text.get(payload) != Some(&b'.') {
        return None;
    }
    let signature = part(payload + 1)?;
    Some(Span {
        keep_until: start + 3,
        end: signature,
        value: false,
    })
}

/// Replaces every PEM private-key block that carries key material, from its BEGIN line to its END
/// line (or to the end of the text, when the block was cut). A line that only names the header, as
/// source code or a diff does, is left alone.
fn mask_private_keys(text: &str) -> std::borrow::Cow<'_, str> {
    const BEGIN: &str = "-----BEGIN ";
    if !text.contains(BEGIN) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(BEGIN) {
        let header_end = rest[at + BEGIN.len()..]
            .find("-----")
            .map(|e| at + BEGIN.len() + e + 5);
        let is_private = header_end.is_some_and(|e| rest[at..e].contains("PRIVATE KEY"));
        let Some(header_end) = header_end.filter(|_| is_private) else {
            out.push_str(&rest[..at + BEGIN.len()]);
            rest = &rest[at + BEGIN.len()..];
            continue;
        };
        let body = &rest[header_end..];
        let end_marker = body.find("-----END ");
        let material = &body[..end_marker.unwrap_or(body.len())];
        let has_key = material
            .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')))
            .any(|run| run.len() >= 16);
        if !has_key {
            out.push_str(&rest[..header_end]);
            rest = &rest[header_end..];
            continue;
        }
        let block_end = match end_marker {
            Some(e) => {
                let after = header_end + e + "-----END ".len();
                rest[after..]
                    .find("-----")
                    .map(|f| after + f + 5)
                    .unwrap_or(rest.len())
            }
            None => rest.len(),
        };
        out.push_str(&rest[..at]);
        out.push_str(REDACTED);
        rest = &rest[block_end..];
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}
