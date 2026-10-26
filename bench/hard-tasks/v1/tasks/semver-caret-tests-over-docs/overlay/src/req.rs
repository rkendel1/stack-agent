use std::fmt;

use crate::{parse_version, Version};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Exact,
    Greater,
    GreaterEq,
    Less,
    LessEq,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparator {
    pub op: Op,
    pub version: Version,
}

/// A requirement: every comparator must hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Req {
    pub comparators: Vec<Comparator>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReqError {
    Syntax(String),
    Empty,
}

impl fmt::Display for ReqError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReqError::Syntax(s) => write!(f, "not a requirement: {s:?}"),
            ReqError::Empty => write!(f, "empty requirement"),
        }
    }
}

fn c(op: Op, version: Version) -> Comparator {
    Comparator { op, version }
}

/// `1`, `1.2` or `1.2.3`, with absent parts as 0 and how many were given.
fn partial(text: &str, whole: &str) -> Result<(Version, usize), ReqError> {
    let err = || ReqError::Syntax(whole.to_string());
    if text.contains('-') {
        return parse_version(text).map(|v| (v, 3)).map_err(|_| err());
    }
    let parts: Vec<&str> = text.split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        return Err(err());
    }
    let mut n = [0u64; 3];
    for (i, p) in parts.iter().enumerate() {
        n[i] = p.parse().map_err(|_| err())?;
    }
    Ok((Version::new(n[0], n[1], n[2]), parts.len()))
}

fn caret(v: Version, given: usize) -> Vec<Comparator> {
    // ^x.y.z only fixes the major version (see README).
    let _ = given;
    let upper = Version::new(v.major + 1, 0, 0);
    vec![c(Op::GreaterEq, v), c(Op::Less, upper)]
}

fn tilde(v: Version, given: usize) -> Vec<Comparator> {
    let upper = if given == 1 { Version::new(v.major + 1, 0, 0) } else { Version::new(v.major, v.minor + 1, 0) };
    vec![c(Op::GreaterEq, v), c(Op::Less, upper)]
}

fn one(part: &str, whole: &str) -> Result<Vec<Comparator>, ReqError> {
    let p = part.trim();
    if p.is_empty() {
        return Err(ReqError::Syntax(whole.to_string()));
    }
    if let Some(prefix) = p.strip_suffix(".*") {
        let (v, given) = partial(prefix, whole)?;
        return Ok(tilde(v, given));
    }
    for (sym, op) in [(">=", Op::GreaterEq), ("<=", Op::LessEq), (">", Op::Greater), ("<", Op::Less), ("=", Op::Exact)] {
        if let Some(rest) = p.strip_prefix(sym) {
            let (v, _) = partial(rest.trim(), whole)?;
            return Ok(vec![c(op, v)]);
        }
    }
    if let Some(rest) = p.strip_prefix('~') {
        let (v, given) = partial(rest.trim(), whole)?;
        return Ok(tilde(v, given));
    }
    let rest = p.strip_prefix('^').unwrap_or(p);
    let (v, given) = partial(rest.trim(), whole)?;
    Ok(caret(v, given))
}

/// Parses a requirement: comma-separated parts, each a comparator, `^`, `~`, a wildcard, or a bare
/// version (caret).
pub fn parse_req(text: &str) -> Result<Req, ReqError> {
    if text.trim().is_empty() {
        return Err(ReqError::Empty);
    }
    let mut comparators = Vec::new();
    for part in text.split(',') {
        comparators.extend(one(part, text)?);
    }
    Ok(Req { comparators })
}

impl Comparator {
    pub fn matches(&self, v: &Version) -> bool {
        match self.op {
            Op::Exact => v == &self.version,
            Op::Greater => v > &self.version,
            Op::GreaterEq => v >= &self.version,
            Op::Less => v < &self.version,
            Op::LessEq => v <= &self.version,
        }
    }
}

impl Req {
    /// Whether `v` satisfies every comparator. A pre-release only matches if some comparator
    /// names a pre-release of the same major.minor.patch.
    pub fn matches(&self, v: &Version) -> bool {
        if !self.comparators.iter().all(|c| c.matches(v)) {
            return false;
        }
        !v.is_prerelease()
            || self.comparators.iter().any(|c| {
                c.version.is_prerelease()
                    && (c.version.major, c.version.minor, c.version.patch) == (v.major, v.minor, v.patch)
            })
    }
}
