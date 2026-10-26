use std::collections::VecDeque;

/// At most `limit` requests in any window of `window_ms` milliseconds.
///
/// It keeps the times of the requests it admitted that are still inside the window, never more
/// than `limit` of them.
#[derive(Debug, Clone)]
pub struct SlidingWindow {
    limit: usize,
    window_ms: u64,
    admitted: VecDeque<u64>,
}

impl SlidingWindow {
    pub fn new(limit: usize, window_ms: u64) -> SlidingWindow {
        assert!(limit > 0 && window_ms > 0, "a window admits something over some time");
        SlidingWindow { limit, window_ms, admitted: VecDeque::with_capacity(limit) }
    }

    fn expire(&mut self, now_ms: u64) {
        while let Some(&t) = self.admitted.front() {
            if now_ms.saturating_sub(t) >= self.window_ms {
                self.admitted.pop_front();
            } else {
                break;
            }
        }
    }

    /// Admits a request at `now_ms` if fewer than `limit` were admitted in the window ending now.
    /// Times must not go backwards.
    pub fn try_acquire(&mut self, now_ms: u64) -> bool {
        self.expire(now_ms);
        if self.admitted.len() < self.limit {
            self.admitted.push_back(now_ms);
            true
        } else {
            false
        }
    }

    /// Requests admitted in the window ending at `now_ms`.
    pub fn in_window(&mut self, now_ms: u64) -> usize {
        self.expire(now_ms);
        self.admitted.len()
    }

    /// When the next request would be admitted: `now_ms` if now.
    pub fn next_admission_ms(&mut self, now_ms: u64) -> u64 {
        self.expire(now_ms);
        if self.admitted.len() < self.limit {
            now_ms
        } else {
            self.admitted.front().copied().unwrap_or(now_ms) + self.window_ms
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
