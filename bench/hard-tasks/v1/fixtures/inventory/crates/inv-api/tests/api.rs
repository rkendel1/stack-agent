use inv_api::{handle, Request};
use inv_model::{Item, Sku};
use inv_store::Store;

fn store() -> Store {
    let mut s = Store::new();
    s.add(Item { sku: Sku::new("MUG-01").unwrap(), name: "Mug".into(), on_hand: 10 });
    s
}

#[test]
fn lenient_sku_input() {
    let mut s = store();
    let r = handle(&mut s, Request::Available { sku: " mug-01 ".into() }, 0);
    assert_eq!((r.status, r.body.as_str()), (200, "10"));
}

#[test]
fn statuses() {
    let mut s = store();
    assert_eq!(handle(&mut s, Request::Available { sku: "NOPE-1".into() }, 0).status, 404);
    assert_eq!(handle(&mut s, Request::Available { sku: "x".into() }, 0).status, 400);
    assert_eq!(handle(&mut s, Request::SetOnHand { sku: "MUG-01".into(), on_hand: 3, version: 9 }, 0).status, 409);
    assert_eq!(handle(&mut s, Request::Reserve { sku: "MUG-01".into(), quantity: 0, ttl_ms: 5, version: 1 }, 0).status, 400);
}

#[test]
fn reservations_expire_between_requests() {
    let mut s = store();
    let r = handle(&mut s, Request::Reserve { sku: "MUG-01".into(), quantity: 10, ttl_ms: 500, version: 1 }, 1_000);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(handle(&mut s, Request::Available { sku: "MUG-01".into() }, 1_499).body, "0");
    assert_eq!(handle(&mut s, Request::Available { sku: "MUG-01".into() }, 1_500).body, "10");
}
