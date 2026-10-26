use ratelimit::SlidingWindow;

/// The documented behavior, as a model: every admitted time is kept, admission counts those
/// inside the window.
struct Model {
    limit: usize,
    window_ms: u64,
    times: Vec<u64>,
}

impl Model {
    fn inside(&self, now: u64) -> Vec<u64> {
        self.times.iter().copied().filter(|t| now.saturating_sub(*t) < self.window_ms).collect()
    }
    fn try_acquire(&mut self, now: u64) -> bool {
        if self.inside(now).len() < self.limit {
            self.times.push(now);
            true
        } else {
            false
        }
    }
    fn next(&self, now: u64) -> u64 {
        let i = self.inside(now);
        if i.len() < self.limit { now } else { i.iter().min().unwrap() + self.window_ms }
    }
}

fn sequence(seed: u64, len: usize) -> Vec<u64> {
    let mut x = seed;
    let mut now = 0u64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            now += x % 97;
            now
        })
        .collect()
}

#[test]
fn stored_never_exceeds_the_limit() {
    let mut w = SlidingWindow::new(5, 1_000);
    for t in sequence(7, 5_000) {
        w.try_acquire(t);
        assert!(w.stored() <= 5, "stored {} > limit 5 at {t}", w.stored());
    }
}

#[test]
fn behavior_matches_the_documented_model() {
    for (limit, window) in [(1usize, 50u64), (3, 200), (5, 1_000), (8, 333)] {
        let mut w = SlidingWindow::new(limit, window);
        let mut m = Model { limit, window_ms: window, times: Vec::new() };
        for (i, t) in sequence(limit as u64 * 31 + window, 3_000).into_iter().enumerate() {
            match i % 3 {
                0 => assert_eq!(w.try_acquire(t), m.try_acquire(t), "try_acquire at {t}"),
                1 => assert_eq!(w.in_window(t), m.inside(t).len(), "in_window at {t}"),
                _ => assert_eq!(w.next_admission_ms(t), m.next(t), "next_admission_ms at {t}"),
            }
        }
    }
}

#[test]
fn public_api_is_unchanged() {
    let mut w: SlidingWindow = SlidingWindow::new(2, 10);
    let _: bool = w.try_acquire(0);
    let _: usize = w.in_window(1);
    let _: u64 = w.next_admission_ms(2);
    let _: usize = w.stored();
    let _: usize = w.limit();
    let _: u64 = w.window_ms();
    let c = w.clone();
    let _ = format!("{c:?}");
}

#[test]
fn equal_times_and_window_edges() {
    let mut w = SlidingWindow::new(2, 100);
    assert!(w.try_acquire(5));
    assert!(w.try_acquire(5));
    assert!(!w.try_acquire(5));
    assert!(!w.try_acquire(104));
    assert!(w.try_acquire(105));
    assert_eq!(w.in_window(105), 1);
}
