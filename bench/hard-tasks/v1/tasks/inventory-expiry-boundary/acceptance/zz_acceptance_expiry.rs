use inv_model::{Item, Sku};
use inv_store::{Reservation, Store};

fn sku(s: &str) -> Sku {
    Sku::new(s).unwrap()
}

#[test]
fn a_reservation_expires_at_its_expiry_time() {
    let r = Reservation { id: 1, sku: sku("MUG-01"), quantity: 1, expires_at_ms: 1_000 };
    assert!(!r.expired(999));
    assert!(r.expired(1_000));
    assert!(r.expired(1_001));
}

#[test]
fn sweep_at_the_boundary_releases() {
    let mut s = Store::new();
    s.add(Item { sku: sku("MUG-01"), name: "Mug".into(), on_hand: 10 });
    s.reserve(&sku("MUG-01"), 4, 1_000, 1).unwrap();
    s.reserve(&sku("MUG-01"), 2, 2_000, 2).unwrap();
    assert_eq!(s.sweep(1_000), 1);
    assert_eq!(s.available(&sku("MUG-01")), Ok(8));
    assert_eq!(s.sweep(1_999), 0);
    assert_eq!(s.sweep(2_000), 1);
}
