use std::cmp::Ordering;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Prerelease {
    Numeric(u64),
    Alpha(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Vec<Prerelease>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionError {
    Syntax(String),
    LeadingZero(String),
}

impl fmt::Display for VersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VersionError::Syntax(s) => write!(f, "not a version: {s:?}"),
            VersionError::LeadingZero(s) => write!(f, "numeric part with a leading zero: {s:?}"),
        }
    }
}

fn number(part: &str, whole: &str) -> Result<u64, VersionError> {
    if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
        return Err(VersionError::Syntax(whole.to_string()));
    }
    if part.len() > 1 && part.starts_with('0') {
        return Err(VersionError::LeadingZero(whole.to_string()));
    }
    part.parse().map_err(|_| VersionError::Syntax(whole.to_string()))
}

/// Parses `MAJOR.MINOR.PATCH[-PRERELEASE]`. Build metadata (`+...`) is ignored.
pub fn parse_version(text: &str) -> Result<Version, VersionError> {
    let s = text.trim();
    let s = s.split_once('+').map_or(s, |(v, _)| v);
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (s, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return Err(VersionError::Syntax(text.to_string()));
    }
    let pre = match pre {
        None => Vec::new(),
        Some(p) => p
            .split('.')
            .map(|id| {
                if id.is_empty() {
                    Err(VersionError::Syntax(text.to_string()))
                } else if id.chars().all(|c| c.is_ascii_digit()) {
                    number(id, text).map(Prerelease::Numeric)
                } else if id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    Ok(Prerelease::Alpha(id.to_string()))
                } else {
                    Err(VersionError::Syntax(text.to_string()))
                }
            })
            .collect::<Result<_, _>>()?,
    };
    Ok(Version { major: number(parts[0], text)?, minor: number(parts[1], text)?, patch: number(parts[2], text)?, pre })
}

impl Version {
    pub fn new(major: u64, minor: u64, patch: u64) -> Version {
        Version { major, minor, patch, pre: Vec::new() }
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

fn cmp_pre_id(a: &Prerelease, b: &Prerelease) -> Ordering {
    match (a, b) {
        (Prerelease::Numeric(x), Prerelease::Numeric(y)) => x.cmp(y),
        (Prerelease::Numeric(_), Prerelease::Alpha(_)) => Ordering::Less,
        (Prerelease::Alpha(_), Prerelease::Numeric(_)) => Ordering::Greater,
        (Prerelease::Alpha(x), Prerelease::Alpha(y)) => x.cmp(y),
    }
}

impl Ord for Version {
    /// Semantic version precedence: major, minor, patch; a pre-release sorts before its release;
    /// pre-release identifiers compare left to right, numbers numerically and below text.
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => {
                    for (a, b) in self.pre.iter().zip(&other.pre) {
                        let o = cmp_pre_id(a, b);
                        if o != Ordering::Equal {
                            return o;
                        }
                    }
                    self.pre.len().cmp(&other.pre.len())
                }
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        for (i, p) in self.pre.iter().enumerate() {
            f.write_str(if i == 0 { "-" } else { "." })?;
            match p {
                Prerelease::Numeric(n) => write!(f, "{n}")?,
                Prerelease::Alpha(s) => f.write_str(s)?,
            }
        }
        Ok(())
    }
}
