use semver_lite::{parse_req, parse_version};

#[test]
fn tilde_with_two_parts() {
    let r = parse_req("~1.2").unwrap();
    assert!(r.matches(&parse_version("1.2.9").unwrap()));
    assert!(!r.matches(&parse_version("1.3.0").unwrap()));
}
