use chip_graph::StoreError;

#[test]
fn the_new_variant_displays_its_message() {
    assert_eq!(
        StoreError::Unreadable("disk".into()).to_string(),
        "unreadable snapshot: disk"
    );
}

#[test]
fn it_is_an_error_and_the_other_variants_are_unchanged() {
    fn takes_error(_: &dyn std::error::Error) {}
    takes_error(&StoreError::Unreadable("x".into()));
    assert_eq!(StoreError::NotFound.to_string(), "no architecture snapshot found");
    assert_eq!(StoreError::Io("a".into()).to_string(), "snapshot io error: a");
    assert_eq!(StoreError::Invalid("b".into()).to_string(), "invalid snapshot: b");
}
