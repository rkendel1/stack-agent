use semver_lite::{parse_req, parse_version};

fn m(req: &str, v: &str) -> bool {
    parse_req(req).unwrap().matches(&parse_version(v).unwrap())
}

#[test]
fn cargo_caret_rules() {
    let cases = [
        ("^1.2.3", "1.9.9", true), ("^1.2.3", "2.0.0", false),
        ("^0.2.3", "0.2.3", true), ("^0.2.3", "0.2.99", true), ("^0.2.3", "0.3.0", false), ("^0.2.3", "0.2.2", false),
        ("^0.0.3", "0.0.3", true), ("^0.0.3", "0.0.4", false),
        ("^0.2", "0.2.9", true), ("^0.2", "0.3.0", false),
        ("^0.0", "0.0.9", true), ("^0.0", "0.1.0", false),
        ("^0", "0.9.9", true), ("^0", "1.0.0", false),
        ("^1", "1.99.0", true), ("^1", "2.0.0", false),
        ("0.2.3", "0.2.9", true), ("0.2.3", "0.3.0", false),
    ];
    for (req, v, want) in cases {
        assert_eq!(m(req, v), want, "{req} vs {v}");
    }
}

#[test]
fn other_requirements_unchanged() {
    assert!(m("~1.2.3", "1.2.9") && !m("~1.2.3", "1.3.0"));
    assert!(m("1.2.*", "1.2.7") && !m("1.2.*", "1.3.0"));
    assert!(m(">=1.0, <2.0", "1.5.0"));
    assert!(!m(">=1.0.0", "1.1.0-beta"));
}
