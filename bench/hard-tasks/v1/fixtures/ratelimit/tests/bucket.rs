use ratelimit::{Clock, ManualClock, TokenBucket};

#[test]
fn a_full_bucket_admits_its_capacity_then_refuses() {
    let mut b = TokenBucket::new(3, 1, 0);
    assert!(b.try_acquire(1, 0));
    assert!(b.try_acquire(2, 0));
    assert!(!b.try_acquire(1, 0));
}

#[test]
fn refill_is_continuous() {
    let mut b = TokenBucket::new(2, 2, 0);
    assert!(b.try_acquire(2, 0));
    assert!(!b.try_acquire(1, 400));
    assert!(b.try_acquire(1, 500));
}

#[test]
fn wait_ms_says_when() {
    let mut b = TokenBucket::new(5, 10, 0);
    assert!(b.try_acquire(5, 0));
    assert_eq!(b.wait_ms(1, 0), Some(100));
    assert_eq!(b.wait_ms(6, 0), None);
}

#[test]
fn clock_driven() {
    let c = ManualClock::new(1_000);
    let mut b = TokenBucket::new(1, 1, c.now_ms());
    assert!(b.try_acquire_with(1, &c));
    assert!(!b.try_acquire_with(1, &c));
    c.advance(1_000);
    assert!(b.try_acquire_with(1, &c));
}

#[test]
fn long_idle_refills_to_capacity() {
    let mut b = TokenBucket::new(10, 1, 0);
    assert!(b.try_acquire(10, 0));
    // Idle for a little over 49.7 days.
    assert_eq!(b.available((1u64 << 32) + 500), 10);
}
