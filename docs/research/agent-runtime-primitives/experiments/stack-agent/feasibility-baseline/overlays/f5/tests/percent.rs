use tally::percent::clamp_percent;

#[test]
fn a_percentage_over_100_is_limited_to_100() {
    assert_eq!(clamp_percent(150), 100);
}

#[test]
fn a_valid_percentage_is_unchanged() {
    assert_eq!(clamp_percent(40), 40);
}
