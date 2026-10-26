use crate::{parse_value, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    Line { line: usize, text: String },
}

/// Parses `key = value` lines under optional `[section]` or `[section.sub]` headers. `#` starts a
/// comment line.
pub fn parse_file(text: &str) -> Result<Value, FileError> {
    let mut out = Value::table();
    let mut section = String::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_string();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(FileError::Line { line: i + 1, text: raw.to_string() });
        };
        let key = k.trim();
        if key.is_empty() {
            return Err(FileError::Line { line: i + 1, text: raw.to_string() });
        }
        let path = if section.is_empty() { key.to_string() } else { format!("{section}.{key}") };
        out.set(&path, parse_value(v));
    }
    Ok(out)
}
