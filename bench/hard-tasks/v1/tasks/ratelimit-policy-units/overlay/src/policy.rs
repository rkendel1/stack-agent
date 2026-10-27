use std::fmt;

/// A parsed rate policy: `limit` requests per `window_ms`, with an optional burst (the bucket
/// capacity; the limit when absent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub limit: u64,
    pub window_ms: u64,
    pub burst: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    Syntax(String),
    Unit(String),
    Zero,
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PolicyError::Syntax(s) => write!(f, "not a policy: {s:?} (expected e.g. 100/1m)"),
            PolicyError::Unit(u) => write!(f, "unknown unit {u:?} (ms, s, m, h)"),
            PolicyError::Zero => write!(f, "a policy admits at least one request per non-empty window"),
        }
    }
}

fn unit_ms(unit: &str) -> Result<u64, PolicyError> {
    match unit {
        u if u.starts_with('m') => Ok(60_000),
        "ms" => Ok(1),
        "s" => Ok(1_000),
        "h" => Ok(3_600_000),
        other => Err(PolicyError::Unit(other.to_string())),
    }
}

fn parse_window(text: &str) -> Result<u64, PolicyError> {
    let split = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (count, unit) = text.split_at(split);
    if unit.is_empty() {
        return Err(PolicyError::Syntax(text.to_string()));
    }
    let count: u64 = if count.is_empty() { 1 } else { count.parse().map_err(|_| PolicyError::Syntax(text.to_string()))? };
    Ok(count * unit_ms(unit)?)
}

/// Parses `LIMIT/WINDOW [burst=N]`, e.g. `100/1m`, `5/10s`, `20/500ms burst=40`, `100/m`.
pub fn parse_policy(text: &str) -> Result<Policy, PolicyError> {
    let mut parts = text.split_whitespace();
    let rate = parts.next().ok_or_else(|| PolicyError::Syntax(text.to_string()))?;
    let (limit, window) = rate.split_once('/').ok_or_else(|| PolicyError::Syntax(text.to_string()))?;
    let limit: u64 = limit.parse().map_err(|_| PolicyError::Syntax(text.to_string()))?;
    let window_ms = parse_window(window)?;
    let mut burst = limit;
    for extra in parts {
        match extra.strip_prefix("burst=") {
            Some(n) => burst = n.parse().map_err(|_| PolicyError::Syntax(text.to_string()))?,
            None => return Err(PolicyError::Syntax(text.to_string())),
        }
    }
    if limit == 0 || window_ms == 0 || burst == 0 {
        return Err(PolicyError::Zero);
    }
    Ok(Policy { limit, window_ms, burst })
}

impl Policy {
    /// The refill rate that admits `limit` per window, in tokens per second (rounded up, at least 1).
    pub fn refill_per_sec(&self) -> u64 {
        (self.limit * 1000).div_ceil(self.window_ms).max(1)
    }
}
