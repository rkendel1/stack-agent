use ratelimit::{parse_policy, Policy, PolicyError};

#[test]
fn common_policies() {
    assert_eq!(parse_policy("100/1m").unwrap(), Policy { limit: 100, window_ms: 60_000, burst: 100 });
    assert_eq!(parse_policy("5/10s").unwrap(), Policy { limit: 5, window_ms: 10_000, burst: 5 });
    assert_eq!(parse_policy("20/500ms burst=40").unwrap(), Policy { limit: 20, window_ms: 500, burst: 40 });
    assert_eq!(parse_policy("100/m").unwrap().window_ms, 60_000);
    assert_eq!(parse_policy("3/2h").unwrap().window_ms, 7_200_000);
}

#[test]
fn errors() {
    assert!(matches!(parse_policy("100"), Err(PolicyError::Syntax(_))));
    assert!(matches!(parse_policy("1/5d"), Err(PolicyError::Unit(_))));
    assert_eq!(parse_policy("0/1s"), Err(PolicyError::Zero));
}

#[test]
fn refill_rate() {
    assert_eq!(parse_policy("100/1m").unwrap().refill_per_sec(), 2);
    assert_eq!(parse_policy("10/1s").unwrap().refill_per_sec(), 10);
}
