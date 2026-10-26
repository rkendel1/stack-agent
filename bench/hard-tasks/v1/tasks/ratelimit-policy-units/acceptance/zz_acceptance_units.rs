use ratelimit::{parse_policy, PolicyError};

#[test]
fn every_unit() {
    assert_eq!(parse_policy("20/500ms").unwrap().window_ms, 500);
    assert_eq!(parse_policy("1/1ms").unwrap().window_ms, 1);
    assert_eq!(parse_policy("100/1m").unwrap().window_ms, 60_000);
    assert_eq!(parse_policy("100/m").unwrap().window_ms, 60_000);
    assert_eq!(parse_policy("5/10s").unwrap().window_ms, 10_000);
    assert_eq!(parse_policy("3/2h").unwrap().window_ms, 7_200_000);
}

#[test]
fn unknown_units_are_still_refused() {
    assert!(matches!(parse_policy("1/5min"), Err(PolicyError::Unit(_))));
    assert!(matches!(parse_policy("1/5mss"), Err(PolicyError::Unit(_))));
    assert!(matches!(parse_policy("1/5d"), Err(PolicyError::Unit(_))));
}

#[test]
fn burst_with_milliseconds() {
    let p = parse_policy("20/500ms burst=40").unwrap();
    assert_eq!((p.limit, p.window_ms, p.burst), (20, 500, 40));
    assert_eq!(p.refill_per_sec(), 40);
}
