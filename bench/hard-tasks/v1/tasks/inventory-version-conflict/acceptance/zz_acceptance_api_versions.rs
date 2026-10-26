use inv_api::{handle, Request};
use inv_model::{Item, Sku};
use inv_store::Store;

#[test]
fn stale_versions_are_conflicts() {
    let mut s = Store::new();
    s.add(Item { sku: Sku::new("MUG-01").unwrap(), name: "Mug".into(), on_hand: 3 });
    assert_eq!(handle(&mut s, Request::Reserve { sku: "MUG-01".into(), quantity: 1, ttl_ms: 100, version: 1 }, 0).status, 200);
    let r = handle(&mut s, Request::Reserve { sku: "MUG-01".into(), quantity: 1, ttl_ms: 100, version: 1 }, 0);
    assert_eq!(r.status, 409, "{}", r.body);
    assert_eq!(handle(&mut s, Request::SetOnHand { sku: "MUG-01".into(), on_hand: 9, version: 7 }, 0).status, 409);
    assert_eq!(handle(&mut s, Request::Reserve { sku: "MUG-01".into(), quantity: 9, ttl_ms: 100, version: 2 }, 0).status, 409);
}
