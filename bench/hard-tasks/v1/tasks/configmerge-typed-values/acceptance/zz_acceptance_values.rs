use configmerge::{from_env, parse_value, Value};

#[test]
fn booleans_in_any_case() {
    for s in ["true", "TRUE", "True", " tRuE "] {
        assert_eq!(parse_value(s), Value::Bool(true), "{s:?}");
    }
    assert_eq!(parse_value("FALSE"), Value::Bool(false));
    assert_eq!(parse_value("truth"), Value::Str("truth".into()));
}

#[test]
fn lists_drop_empty_items() {
    assert_eq!(parse_value("a, b,,c,"), Value::List(vec!["a".into(), "b".into(), "c".into()]));
    assert_eq!(parse_value(",x,"), Value::List(vec!["x".into()]));
    assert_eq!(parse_value(",,"), Value::List(vec![]));
}

#[test]
fn other_types_unchanged() {
    assert_eq!(parse_value("-12"), Value::Int(-12));
    assert_eq!(parse_value("\"q\""), Value::Str("q".into()));
    assert_eq!(from_env([("APP__FEATURES", "A,,B")]).get("features"), Some(&Value::List(vec!["A".into(), "B".into()])));
}
