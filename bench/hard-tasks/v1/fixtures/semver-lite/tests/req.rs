use semver_lite::{parse_req, parse_version};

fn m(req: &str, v: &str) -> bool {
    parse_req(req).unwrap().matches(&parse_version(v).unwrap())
}

#[test]
fn caret_for_stable_versions() {
    assert!(m("^1.2.3", "1.9.0"));
    assert!(!m("^1.2.3", "2.0.0"));
    assert!(!m("^1.2.3", "1.2.2"));
}

#[test]
fn caret_zero_major() {
    // Cargo's rule: for 0.x the leftmost non-zero part is the one that may not change.
    assert!(m("^0.2.3", "0.2.9"));
    assert!(!m("^0.2.3", "0.3.0"));
    assert!(m("^0.0.3", "0.0.3"));
    assert!(!m("^0.0.3", "0.0.4"));
}

#[test]
fn tilde_and_wildcards() {
    assert!(m("~1.2.3", "1.2.9"));
    assert!(!m("~1.2.3", "1.3.0"));
    assert!(m("1.2.*", "1.2.7"));
    assert!(!m("1.2.*", "1.3.0"));
}

#[test]
fn comparators_and_prereleases() {
    assert!(m(">=1.0, <2.0", "1.5.0"));
    assert!(!m(">=1.0, <2.0", "2.0.0"));
    assert!(!m(">=1.0.0", "1.1.0-beta"));
    assert!(m(">=1.1.0-alpha", "1.1.0-beta"));
}

#[test]
fn major_wildcard() {
    assert!(m("1.*", "1.9.9"));
    assert!(!m("1.*", "2.0.0"));
}
