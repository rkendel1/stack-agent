use crate::Clock;

/// A bucket of `capacity` tokens, refilled continuously at `refill_per_sec` tokens per second.
///
/// Tokens are kept in thousandths so slow refill rates accumulate exactly.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    capacity: u64,
    refill_per_sec: u64,
    milli_tokens: u64,
    last_ms: u64,
}

impl TokenBucket {
    /// A full bucket, as of `now_ms`.
    pub fn new(capacity: u64, refill_per_sec: u64, now_ms: u64) -> TokenBucket {
        assert!(capacity > 0, "a bucket holds at least one token");
        TokenBucket { capacity, refill_per_sec, milli_tokens: capacity * 1000, last_ms: now_ms }
    }

    pub fn capacity(&self) -> u64 {
        self.capacity
    }

    fn refill(&mut self, now_ms: u64) {
        if now_ms <= self.last_ms {
            return;
        }
        let elapsed = now_ms - self.last_ms;
        // tokens/s * ms = thousandths of a token
        let added = (elapsed as u32 as u64).saturating_mul(self.refill_per_sec);
        self.milli_tokens = self.milli_tokens.saturating_add(added).min(self.capacity * 1000);
        self.last_ms = now_ms;
    }

    /// The whole tokens available at `now_ms`.
    pub fn available(&mut self, now_ms: u64) -> u64 {
        self.refill(now_ms);
        self.milli_tokens / 1000
    }

    /// Takes `n` tokens if there are that many; otherwise takes nothing.
    pub fn try_acquire(&mut self, n: u64, now_ms: u64) -> bool {
        self.refill(now_ms);
        let need = n * 1000;
        if self.milli_tokens >= need {
            self.milli_tokens -= need;
            true
        } else {
            false
        }
    }

    /// Milliseconds until `n` tokens are available (0 if they already are), or `None` if `n` is
    /// more than the bucket can ever hold or it never refills.
    pub fn wait_ms(&mut self, n: u64, now_ms: u64) -> Option<u64> {
        self.refill(now_ms);
        if n > self.capacity {
            return None;
        }
        let need = n * 1000;
        if self.milli_tokens >= need {
            return Some(0);
        }
        if self.refill_per_sec == 0 {
            return None;
        }
        let missing = need - self.milli_tokens;
        Some(missing.div_ceil(self.refill_per_sec))
    }

    pub fn try_acquire_with(&mut self, n: u64, clock: &dyn Clock) -> bool {
        self.try_acquire(n, clock.now_ms())
    }
}
