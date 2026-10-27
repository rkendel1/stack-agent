use inv_api::{handle, Request};
use inv_model::{Item, Sku};
use inv_store::Store;

#[test]
fn the_next_request_at_the_expiry_time_sees_the_stock() {
    let mut s = Store::new();
    s.add(Item { sku: Sku::new("MUG-01").unwrap(), name: "Mug".into(), on_hand: 3 });
    assert_eq!(handle(&mut s, Request::Reserve { sku: "MUG-01".into(), quantity: 3, ttl_ms: 100, version: 1 }, 50).status, 200);
    assert_eq!(handle(&mut s, Request::Available { sku: "MUG-01".into() }, 149).body, "0");
    assert_eq!(handle(&mut s, Request::Available { sku: "MUG-01".into() }, 150).body, "3");
}
