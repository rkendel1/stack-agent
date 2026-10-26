use semver_lite::parse_version;

#[test]
fn semver_spec_example_order() {
    let order = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0"];
    for w in order.windows(2) {
        assert!(parse_version(w[0]).unwrap() < parse_version(w[1]).unwrap(), "{} < {}", w[0], w[1]);
    }
}

#[test]
fn numeric_identifiers_compare_numerically() {
    let v = |s| parse_version(s).unwrap();
    assert!(v("1.0.0-9") < v("1.0.0-10"));
    assert!(v("1.0.0-alpha.99") < v("1.0.0-alpha.100"));
    assert!(v("1.0.0-2") < v("1.0.0-a"));
    assert_eq!(v("1.0.0-alpha.10"), v("1.0.0-alpha.10"));
}
