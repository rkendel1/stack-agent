use inv_model::{Item, Sku};
use inv_store::{Store, StoreError};

fn sku(s: &str) -> Sku {
    Sku::new(s).unwrap()
}

fn store() -> Store {
    let mut s = Store::new();
    s.add(Item { sku: sku("MUG-01"), name: "Mug".into(), on_hand: 10 });
    s
}

#[test]
fn versions_increment_on_change() {
    let mut s = store();
    assert_eq!(s.set_on_hand(&sku("MUG-01"), 12, 1), Ok(2));
    assert!(matches!(s.set_on_hand(&sku("MUG-01"), 5, 1), Err(StoreError::Conflict { actual: 2, .. })));
}

#[test]
fn reservations_hold_stock() {
    let mut s = store();
    let (_, v) = s.reserve(&sku("MUG-01"), 4, 1_000, 1).unwrap();
    assert_eq!(v, 2);
    assert_eq!(s.available(&sku("MUG-01")), Ok(6));
    assert!(matches!(s.reserve(&sku("MUG-01"), 7, 1_000, 2), Err(StoreError::Insufficient { available: 6, .. })));
}

#[test]
fn expired_reservations_are_swept() {
    let mut s = store();
    s.reserve(&sku("MUG-01"), 4, 1_000, 1).unwrap();
    assert_eq!(s.sweep(999), 0);
    assert_eq!(s.sweep(1_000), 1);
    assert_eq!(s.available(&sku("MUG-01")), Ok(10));
}

#[test]
fn release() {
    let mut s = store();
    let (id, _) = s.reserve(&sku("MUG-01"), 4, 1_000, 1).unwrap();
    s.release(id).unwrap();
    assert_eq!(s.release(id), Err(StoreError::UnknownReservation(id)));
}
