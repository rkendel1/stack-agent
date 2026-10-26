//! The stock store: items with versions, and reservations that expire.

mod reservations;

use std::collections::BTreeMap;

use inv_model::{Item, Sku};

pub use reservations::Reservation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    UnknownSku(Sku),
    Conflict { sku: Sku, expected: u64, actual: u64 },
    Insufficient { sku: Sku, requested: u32, available: u32 },
    UnknownReservation(u64),
}

#[derive(Debug, Clone)]
struct Entry {
    item: Item,
    version: u64,
}

/// Items by SKU, each with a version that every change increments, and the active reservations.
#[derive(Debug, Default)]
pub struct Store {
    items: BTreeMap<Sku, Entry>,
    reservations: BTreeMap<u64, Reservation>,
    next_reservation: u64,
}

impl Store {
    pub fn new() -> Store {
        Store { next_reservation: 1, ..Store::default() }
    }

    /// Adds an item at version 1, or replaces nothing if the SKU exists (returns false).
    pub fn add(&mut self, item: Item) -> bool {
        if self.items.contains_key(&item.sku) {
            return false;
        }
        self.items.insert(item.sku.clone(), Entry { item, version: 1 });
        true
    }

    pub fn get(&self, sku: &Sku) -> Option<(&Item, u64)> {
        self.items.get(sku).map(|e| (&e.item, e.version))
    }

    fn entry(&mut self, sku: &Sku, expected: u64) -> Result<&mut Entry, StoreError> {
        let e = self.items.get_mut(sku).ok_or_else(|| StoreError::UnknownSku(sku.clone()))?;
        if e.version != expected {
            return Err(StoreError::Conflict { sku: sku.clone(), expected, actual: e.version });
        }
        Ok(e)
    }

    /// Sets the quantity on hand, if the item is still at `expected` version. Returns the new version.
    pub fn set_on_hand(&mut self, sku: &Sku, on_hand: u32, expected: u64) -> Result<u64, StoreError> {
        let e = self.entry(sku, expected)?;
        e.item.on_hand = on_hand;
        e.version += 1;
        Ok(e.version)
    }

    /// Stock on hand minus what active reservations hold.
    pub fn available(&self, sku: &Sku) -> Result<u32, StoreError> {
        let e = self.items.get(sku).ok_or_else(|| StoreError::UnknownSku(sku.clone()))?;
        let held: u32 = self.reservations.values().filter(|r| &r.sku == sku).map(|r| r.quantity).sum();
        Ok(e.item.on_hand.saturating_sub(held))
    }

    /// Reserves `quantity` until `expires_at_ms`, if the item is at `expected` version and enough is
    /// available. A reservation is a change: the item's version increments. Returns the
    /// reservation id and the new version.
    pub fn reserve(&mut self, sku: &Sku, quantity: u32, expires_at_ms: u64, expected: u64) -> Result<(u64, u64), StoreError> {
        let available = self.available(sku)?;
        if quantity > available {
            return Err(StoreError::Insufficient { sku: sku.clone(), requested: quantity, available });
        }
        let e = self.entry(sku, expected)?;
        e.version += 1;
        let version = e.version;
        let id = self.next_reservation;
        self.next_reservation += 1;
        self.reservations.insert(id, Reservation { id, sku: sku.clone(), quantity, expires_at_ms });
        Ok((id, version))
    }

    /// Releases a reservation before it expires.
    pub fn release(&mut self, id: u64) -> Result<(), StoreError> {
        self.reservations.remove(&id).map(|_| ()).ok_or(StoreError::UnknownReservation(id))
    }

    /// Releases every reservation that has expired at `now_ms`. Returns how many were released.
    pub fn sweep(&mut self, now_ms: u64) -> usize {
        reservations::sweep(&mut self.reservations, now_ms)
    }

    pub fn reservations(&self) -> impl Iterator<Item = &Reservation> {
        self.reservations.values()
    }
}
