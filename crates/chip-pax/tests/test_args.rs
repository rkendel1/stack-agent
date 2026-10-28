//! Operator-supplied arguments after `pax test`: carried exactly, validated, and absent by default.

use chip_pax::PaxExecutor;
use std::ffi::OsString;

fn tail(pax: &PaxExecutor) -> Vec<OsString> {
    pax.invocation().split_off(4)
}

#[test]
fn by_default_the_invocation_is_the_whole_suite_and_nothing_else() {
    let pax = PaxExecutor::new("/tmp/project");
    assert_eq!(
        pax.invocation(),
        ["--dir", "/tmp/project", "--json", "test"].map(OsString::from)
    );
}

#[test]
fn operator_arguments_follow_test_in_order() {
    let pax = PaxExecutor::new("/tmp/project")
        .with_test_args(vec!["my_filter".into(), "--nocapture".into()])
        .unwrap();
    assert_eq!(tail(&pax), ["my_filter", "--nocapture"].map(OsString::from));
}

#[test]
fn an_argument_that_is_empty_long_or_has_control_characters_is_refused_not_trimmed() {
    for bad in [
        String::new(),
        "x".repeat(201),
        "a\nb".to_string(),
        "a\0b".to_string(),
    ] {
        assert!(PaxExecutor::new("/tmp").with_test_args(vec![bad]).is_err());
    }
}
