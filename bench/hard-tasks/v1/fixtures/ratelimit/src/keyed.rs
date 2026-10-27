use std::collections::HashMap;

use crate::{Policy, TokenBucket};

/// One token bucket per key, created full on first use.
pub struct KeyedLimiter {
    policy: Policy,
    buckets: HashMap<String, TokenBucket>,
}

impl KeyedLimiter {
    pub fn new(policy: Policy) -> KeyedLimiter {
        KeyedLimiter { policy, buckets: HashMap::new() }
    }

    pub fn try_acquire(&mut self, key: &str, now_ms: u64) -> bool {
        let p = self.policy;
        self.buckets
            .entry(key.to_string())
            .or_insert_with(|| TokenBucket::new(p.burst, p.refill_per_sec(), now_ms))
            .try_acquire(1, now_ms)
    }

    pub fn keys(&self) -> usize {
        self.buckets.len()
    }
}
