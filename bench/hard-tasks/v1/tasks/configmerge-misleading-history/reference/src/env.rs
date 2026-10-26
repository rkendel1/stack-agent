use crate::{parse_value, Value};

/// The `APP__` variables among `vars`, as a table: `APP__DB__PORT=5432` is `db.port = 5432`.
/// Segments are lowercased; empty segments make the variable ignored.
pub fn from_env<'a>(vars: impl IntoIterator<Item = (&'a str, &'a str)>) -> Value {
    let mut out = Value::table();
    for (name, value) in vars {
        let Some(rest) = name.strip_prefix("APP__") else { continue };
        let segments: Vec<String> = rest.split("__").map(|s| s.to_ascii_lowercase()).collect();
        if segments.iter().any(String::is_empty) {
            continue;
        }
        out.set(&segments.join("."), parse_value(value));
    }
    out
}
