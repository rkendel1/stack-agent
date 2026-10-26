use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Int(i64),
    Bool(bool),
    Str(String),
    List(Vec<String>),
    Table(BTreeMap<String, Value>),
}

impl Value {
    pub fn table() -> Value {
        Value::Table(BTreeMap::new())
    }

    /// The value at a dotted path (`db.port`).
    pub fn get(&self, path: &str) -> Option<&Value> {
        let mut v = self;
        for key in path.split('.') {
            match v {
                Value::Table(t) => v = t.get(key)?,
                _ => return None,
            }
        }
        Some(v)
    }

    /// Sets the value at a dotted path, creating tables on the way (replacing non-tables).
    pub fn set(&mut self, path: &str, value: Value) {
        let mut keys: Vec<&str> = path.split('.').collect();
        let last = keys.pop().expect("a path has at least one key");
        let mut v = self;
        for key in keys {
            if !matches!(v, Value::Table(_)) {
                *v = Value::table();
            }
            let Value::Table(t) = v else { unreachable!() };
            v = t.entry(key.to_string()).or_insert_with(Value::table);
        }
        if !matches!(v, Value::Table(_)) {
            *v = Value::table();
        }
        let Value::Table(t) = v else { unreachable!() };
        t.insert(last.to_string(), value);
    }
}

/// Types a raw value: an integer, a boolean (any case), a list (if it contains a comma; empty items
/// are dropped and items trimmed), or a string.
pub fn parse_value(raw: &str) -> Value {
    let s = raw.trim();
    if let Ok(n) = s.parse::<i64>() {
        return Value::Int(n);
    }
    match s.to_ascii_lowercase().as_str() {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    if s.contains(',') {
        return Value::List(s.split(',').map(str::trim).filter(|i| !i.is_empty()).map(String::from).collect());
    }
    Value::Str(s.trim_matches('"').to_string())
}
