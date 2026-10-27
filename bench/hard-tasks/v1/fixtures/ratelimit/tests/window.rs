use ratelimit::SlidingWindow;

#[test]
fn admits_the_limit_per_window() {
    let mut w = SlidingWindow::new(2, 1_000);
    assert!(w.try_acquire(0));
    assert!(w.try_acquire(10));
    assert!(!w.try_acquire(20));
    assert!(w.try_acquire(1_000));
}

#[test]
fn next_admission() {
    let mut w = SlidingWindow::new(1, 500);
    assert!(w.try_acquire(100));
    assert_eq!(w.next_admission_ms(200), 600);
}
