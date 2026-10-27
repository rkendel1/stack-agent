use inv_model::{Item, Sku};
use inv_store::{Store, StoreError};

fn sku(s: &str) -> Sku {
    Sku::new(s).unwrap()
}

#[test]
fn a_reservation_invalidates_the_version_it_was_made_against() {
    let mut s = Store::new();
    s.add(Item { sku: sku("MUG-01"), name: "Mug".into(), on_hand: 10 });
    let (_, v) = s.reserve(&sku("MUG-01"), 4, 1_000, 1).unwrap();
    assert_eq!(v, 2);
    assert_eq!(s.get(&sku("MUG-01")).unwrap().1, 2);
    assert!(matches!(s.reserve(&sku("MUG-01"), 1, 1_000, 1), Err(StoreError::Conflict { expected: 1, actual: 2, .. })));
    assert!(matches!(s.set_on_hand(&sku("MUG-01"), 1, 1), Err(StoreError::Conflict { .. })));
    assert_eq!(s.reserve(&sku("MUG-01"), 1, 1_000, 2).unwrap().1, 3);
}

#[test]
fn a_refused_reservation_changes_nothing() {
    let mut s = Store::new();
    s.add(Item { sku: sku("MUG-01"), name: "Mug".into(), on_hand: 2 });
    assert!(s.reserve(&sku("MUG-01"), 5, 1_000, 1).is_err());
    assert_eq!(s.get(&sku("MUG-01")).unwrap().1, 1);
}
