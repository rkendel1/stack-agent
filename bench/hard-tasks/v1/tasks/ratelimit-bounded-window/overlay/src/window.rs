/// At most `limit` requests in any window of `window_ms` milliseconds.
///
/// It keeps the times of the requests it admitted that are still inside the window, never more
/// than `limit` of them.
#[derive(Debug, Clone)]
pub struct SlidingWindow {
    limit: usize,
    window_ms: u64,
    admitted: Vec<u64>,
}

impl SlidingWindow {
    pub fn new(limit: usize, window_ms: u64) -> SlidingWindow {
        assert!(limit > 0 && window_ms > 0, "a window admits something over some time");
        SlidingWindow { limit, window_ms, admitted: Vec::new() }
    }

    fn inside(&self, now_ms: u64) -> impl Iterator<Item = u64> + '_ {
        self.admitted.iter().copied().filter(move |t| now_ms.saturating_sub(*t) < self.window_ms)
    }

    /// Admits a request at `now_ms` if fewer than `limit` were admitted in the window ending now.
    /// Times must not go backwards.
    pub fn try_acquire(&mut self, now_ms: u64) -> bool {
        if self.inside(now_ms).count() < self.limit {
            self.admitted.push(now_ms);
            true
        } else {
            false
        }
    }

    /// Requests admitted in the window ending at `now_ms`.
    pub fn in_window(&mut self, now_ms: u64) -> usize {
        self.inside(now_ms).count()
    }

    /// When the next request would be admitted: `now_ms` if now.
    pub fn next_admission_ms(&mut self, now_ms: u64) -> u64 {
        if self.inside(now_ms).count() < self.limit {
            now_ms
        } else {
            self.inside(now_ms).min().unwrap_or(now_ms) + self.window_ms
        }
    }

    /// How many request times are stored (for memory accounting).
    pub fn stored(&self) -> usize {
        self.admitted.len()
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn window_ms(&self) -> u64 {
        self.window_ms
    }
}
