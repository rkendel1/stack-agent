use ratelimit::TokenBucket;

#[test]
fn any_idle_period_refills_to_capacity() {
    for idle in [(1u64 << 32) + 500, (1u64 << 33) + 1, 90 * 24 * 3_600_000, u64::MAX / 2] {
        let mut b = TokenBucket::new(10, 1, 0);
        assert!(b.try_acquire(10, 0));
        assert_eq!(b.available(idle), 10, "idle {idle} ms");
    }
}

#[test]
fn fast_refill_rates_do_not_wrap() {
    let mut b = TokenBucket::new(1_000_000, 5_000_000, 0);
    assert!(b.try_acquire(1_000_000, 0));
    assert_eq!(b.available(1_000), 1_000_000);
}

#[test]
fn short_waits_unchanged() {
    let mut b = TokenBucket::new(2, 2, 0);
    assert!(b.try_acquire(2, 0));
    assert!(!b.try_acquire(1, 400));
    assert!(b.try_acquire(1, 500));
}
