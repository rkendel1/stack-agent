//! Secret masking for diagnostic text.
//!
//! Ports the oracle's `maskSecrets` patterns (src/core/shared/text_utils.zig:430-720):
//! inline keys (`sk-`, `sk_live_`, `pk_live_`, `github_pat_`, `xoxb-`, `xoxp-`,
//! `Bearer `), GitHub tokens (`gh[pousr]_`), AWS access key ids, credentials in
//! `https://user:pass@` URLs, and named assignments (`API_KEY=...` and friends).
//! The oracle's generic `*_KEY=` heuristic (`matchesSensitiveAssignment`) is not
//! ported yet; diagnostic detail is also length-bounded, which limits exposure.

const REDACTED: &str = "[redacted]";

const ASSIGNMENT_PREFIXES: &[&str] = &[
    "OPENAI_API_KEY=",
    "ANTHROPIC_API_KEY=",
    "AI_GATEWAY_API_KEY=",
    "VERCEL_OIDC_TOKEN=",
    "GITHUB_TOKEN=",
    "AWS_SECRET_ACCESS_KEY=",
    "DATABASE_URL=",
    "SECRET_KEY=",
    "API_KEY=",
    "TOKEN=",
    "PASSWORD=",
    "PRIVATE_KEY=",
];

const INLINE_PREFIXES: &[&str] = &[
    "sk-",
    "sk_live_",
    "pk_live_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "Bearer ",
];

const GITHUB_PREFIXES: &[&str] = &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"];
const AWS_PREFIXES: &[&str] = &["AKIA", "ASIA", "AIDA", "AGPA", "AROA", "ANPA"];

/// A span to replace: bytes `keep_until..end` become `[redacted]`.
struct Span {
    keep_until: usize,
    end: usize,
}

/// Returns `text` with secret-shaped values replaced by `[redacted]`.
pub fn mask_secrets(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
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
            copied = span.end;
            i = span.end;
        } else {
            i += 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

fn is_token_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'.'
}

fn find_span(text: &[u8], start: usize) -> Option<Span> {
    basic_auth_url(text, start)
        .or_else(|| aws_access_key(text, start))
        .or_else(|| assignment(text, start))
        .or_else(|| inline_key(text, start))
        .or_else(|| github_token(text, start))
}

fn basic_auth_url(text: &[u8], start: usize) -> Option<Span> {
    let rest = &text[start..];
    if !rest.starts_with(b"https://") {
        return None;
    }
    let credential_start = start + "https://".len();
    let mut colon = None;
    let mut i = credential_start;
    while i < text.len() && !text[i].is_ascii_whitespace() {
        match text[i] {
            b'/' | b'?' | b'#' => return None,
            b':' if colon.is_none() => colon = Some(i),
            b'@' => {
                return (colon.is_some() && i > credential_start).then_some(Span {
                    keep_until: credential_start,
                    end: i,
                });
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn aws_access_key(text: &[u8], start: usize) -> Option<Span> {
    if start + 20 > text.len() || (start > 0 && is_token_char(text[start - 1])) {
        return None;
    }
    let candidate = &text[start..start + 20];
    if !AWS_PREFIXES
        .iter()
        .any(|p| candidate.starts_with(p.as_bytes()))
    {
        return None;
    }
    if !candidate
        .iter()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return None;
    }
    if start + 20 < text.len() && is_token_char(text[start + 20]) {
        return None;
    }
    Some(Span {
        keep_until: start,
        end: start + 20,
    })
}

fn assignment(text: &[u8], start: usize) -> Option<Span> {
    for prefix in ASSIGNMENT_PREFIXES {
        let end = start + prefix.len();
        if end < text.len() && text[start..end].eq_ignore_ascii_case(prefix.as_bytes()) {
            let quote = text[end];
            if quote == b'"' || quote == b'\'' {
                let content = end + 1;
                let mut stop = content;
                while stop < text.len()
                    && text[stop] != quote
                    && text[stop] != b'\n'
                    && text[stop] != b'\r'
                {
                    stop += 1;
                }
                if stop > content {
                    return Some(Span {
                        keep_until: content,
                        end: stop,
                    });
                }
                return None;
            }
            let mut stop = end;
            while stop < text.len()
                && !text[stop].is_ascii_whitespace()
                && text[stop] != b'"'
                && text[stop] != b'\''
            {
                stop += 1;
            }
            if stop > end {
                return Some(Span {
                    keep_until: end,
                    end: stop,
                });
            }
        }
    }
    None
}

fn inline_key(text: &[u8], start: usize) -> Option<Span> {
    if start > 0 && is_token_char(text[start - 1]) {
        return None;
    }
    for prefix in INLINE_PREFIXES {
        let after = start + prefix.len();
        if after < text.len() && text[start..].starts_with(prefix.as_bytes()) {
            let mut end = after;
            while end < text.len() && is_token_char(text[end]) {
                end += 1;
            }
            if end - start >= 16 {
                return Some(Span {
                    keep_until: start,
                    end,
                });
            }
        }
    }
    None
}

fn github_token(text: &[u8], start: usize) -> Option<Span> {
    if start + 40 > text.len()
        || !GITHUB_PREFIXES
            .iter()
            .any(|p| text[start..].starts_with(p.as_bytes()))
    {
        return None;
    }
    let mut end = start + 4;
    while end < text.len() && is_token_char(text[end]) {
        end += 1;
    }
    (end - (start + 4) >= 36).then_some(Span {
        keep_until: start,
        end,
    })
}
