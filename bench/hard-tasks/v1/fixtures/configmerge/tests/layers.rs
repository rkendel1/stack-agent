use configmerge::{load, parse_value, Value};

fn defaults() -> Value {
    let mut d = Value::table();
    d.set("db.host", Value::Str("localhost".into()));
    d.set("db.port", Value::Int(5432));
    d.set("db.pool.size", Value::Int(4));
    d.set("db.pool.timeout_ms", Value::Int(1_000));
    d.set("log.level", Value::Str("info".into()));
    d
}

const FILE: &str = "# app config\n[db]\nhost = db.internal\n[db.pool]\nsize = 16\n[log]\nlevel = debug\n";

#[test]
fn file_overrides_defaults_deeply() {
    let c = load(defaults(), FILE, []).unwrap();
    assert_eq!(c.get("db.host"), Some(&Value::Str("db.internal".into())));
    assert_eq!(c.get("db.port"), Some(&Value::Int(5432)));
    assert_eq!(c.get("db.pool.size"), Some(&Value::Int(16)));
    assert_eq!(c.get("db.pool.timeout_ms"), Some(&Value::Int(1_000)));
}

#[test]
fn environment_wins_over_file_at_every_depth() {
    let env = [("APP__DB__HOST", "db.prod"), ("APP__DB__POOL__SIZE", "64"), ("APP__LOG__LEVEL", "warn"), ("HOME", "/root")];
    let c = load(defaults(), FILE, env).unwrap();
    assert_eq!(c.get("db.host"), Some(&Value::Str("db.prod".into())));
    assert_eq!(c.get("db.pool.size"), Some(&Value::Int(64)));
    assert_eq!(c.get("db.pool.timeout_ms"), Some(&Value::Int(1_000)));
    assert_eq!(c.get("log.level"), Some(&Value::Str("warn".into())));
}

#[test]
fn typed_values() {
    assert_eq!(parse_value("42"), Value::Int(42));
    assert_eq!(parse_value("TRUE"), Value::Bool(true));
    assert_eq!(parse_value("a, b,,c,"), Value::List(vec!["a".into(), "b".into(), "c".into()]));
    assert_eq!(parse_value("\"quoted\""), Value::Str("quoted".into()));
}
