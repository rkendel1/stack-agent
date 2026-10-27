use std::collections::BTreeMap;

use inv_model::Sku;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    pub id: u64,
    pub sku: Sku,
    pub quantity: u32,
    /// The reservation holds stock strictly before this time.
    pub expires_at_ms: u64,
}

impl Reservation {
    pub fn expired(&self, now_ms: u64) -> bool {
        now_ms > self.expires_at_ms
    }
}

/// Removes the reservations that have expired at `now_ms`.
pub(crate) fn sweep(reservations: &mut BTreeMap<u64, Reservation>, now_ms: u64) -> usize {
    let before = reservations.len();
    reservations.retain(|_, r| !r.expired(now_ms));
    before - reservations.len()
}
