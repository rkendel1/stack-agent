use crate::{from_env, parse_file, FileError, Value};

/// Merges `top` over `base`: tables merge key by key, at every depth; anything else in `top`
/// replaces what `base` has.
pub fn merge(base: Value, top: Value) -> Value {
    match (base, top) {
        (Value::Table(mut b), Value::Table(t)) => {
            for (k, tv) in t {
                let merged = match b.remove(&k) {
                    Some(bv) => merge(bv, tv),
                    None => tv,
                };
                b.insert(k, merged);
            }
            Value::Table(b)
        }
        (_, top) => top,
    }
}

/// defaults < file < environment.
pub fn load<'a>(defaults: Value, file: &str, env: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<Value, FileError> {
    let file = parse_file(file)?;
    Ok(merge(merge(defaults, file), from_env(env)))
}
