use configmerge::{from_env, load, Value};

#[test]
fn environment_names_are_case_insensitive_segments() {
    let e = from_env([("APP__DB__HOST", "h"), ("APP__Log__Level", "warn"), ("APP__DB____X", "ignored"), ("OTHER__A", "no")]);
    assert_eq!(e.get("db.host"), Some(&Value::Str("h".into())));
    assert_eq!(e.get("log.level"), Some(&Value::Str("warn".into())));
    assert_eq!(e.get("other.a"), None);
    assert_eq!(e.get("db.x"), None);
}

#[test]
fn environment_overrides_the_file() {
    let c = load(Value::table(), "[db]\nhost = file\nport = 1\n", [("APP__DB__HOST", "env")]).unwrap();
    assert_eq!(c.get("db.host"), Some(&Value::Str("env".into())));
    assert_eq!(c.get("db.port"), Some(&Value::Int(1)));
}
