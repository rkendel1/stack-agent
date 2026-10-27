use configmerge::{load, merge, Value};

fn t(pairs: &[(&str, i64)]) -> Value {
    let mut v = Value::table();
    for (k, n) in pairs {
        v.set(k, Value::Int(*n));
    }
    v
}

#[test]
fn later_layers_win_at_every_depth() {
    let base = t(&[("a", 1), ("s.b", 1), ("s.t.c", 1), ("s.t.u.d", 1), ("s.t.u.keep", 7)]);
    let top = t(&[("a", 2), ("s.b", 2), ("s.t.c", 2), ("s.t.u.d", 2), ("s.t.new", 3)]);
    let m = merge(base, top);
    for k in ["a", "s.b", "s.t.c", "s.t.u.d"] {
        assert_eq!(m.get(k), Some(&Value::Int(2)), "{k}");
    }
    assert_eq!(m.get("s.t.u.keep"), Some(&Value::Int(7)));
    assert_eq!(m.get("s.t.new"), Some(&Value::Int(3)));
}

#[test]
fn a_scalar_replaces_a_table_and_back() {
    assert_eq!(merge(t(&[("x.y", 1)]), t(&[("x", 5)])).get("x"), Some(&Value::Int(5)));
    assert_eq!(merge(t(&[("x", 5)]), t(&[("x.y", 1)])).get("x.y"), Some(&Value::Int(1)));
}

#[test]
fn full_precedence() {
    let mut d = Value::table();
    d.set("db.pool.size", Value::Int(4));
    d.set("db.host", Value::Str("localhost".into()));
    let c = load(d, "[db]\nhost = file\n[db.pool]\nsize = 16\n", [("APP__DB__POOL__SIZE", "64")]).unwrap();
    assert_eq!(c.get("db.pool.size"), Some(&Value::Int(64)));
    assert_eq!(c.get("db.host"), Some(&Value::Str("file".into())));
}
