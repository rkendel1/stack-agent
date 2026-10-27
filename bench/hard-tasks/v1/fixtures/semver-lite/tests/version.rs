use semver_lite::{parse_version, Version, VersionError};

#[test]
fn parses() {
    assert_eq!(parse_version("1.2.3").unwrap(), Version::new(1, 2, 3));
    assert_eq!(parse_version("1.0.0-alpha.1").unwrap().to_string(), "1.0.0-alpha.1");
    assert_eq!(parse_version("1.0.0+build.5").unwrap(), Version::new(1, 0, 0));
    assert!(matches!(parse_version("01.2.3"), Err(VersionError::LeadingZero(_))));
    assert!(parse_version("1.2").is_err());
}

#[test]
fn precedence() {
    let v = |s| parse_version(s).unwrap();
    assert!(v("1.0.0-alpha") < v("1.0.0"));
    assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"));
    assert!(v("1.0.0-alpha.1") < v("1.0.0-beta"));
    assert!(v("1.0.0-beta.2") < v("1.0.0-beta.11"));
    assert!(v("1.0.0-rc.1") < v("1.0.0"));
    assert!(v("2.0.0") > v("1.99.99"));
}
