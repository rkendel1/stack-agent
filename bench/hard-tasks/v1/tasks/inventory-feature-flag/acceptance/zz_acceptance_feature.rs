use inv_api::{handle, Request};
use inv_model::{Item, Sku};
use inv_store::Store;

#[test]
fn lenient_parsing_is_back() {
    let mut s = Store::new();
    s.add(Item { sku: Sku::new("MUG-01").unwrap(), name: "Mug".into(), on_hand: 3 });
    assert_eq!(handle(&mut s, Request::Available { sku: "  mug-01".into() }, 0).body, "3");
    assert_eq!(handle(&mut s, Request::Available { sku: "m".into() }, 0).status, 400);
}

#[test]
fn the_model_keeps_parsing_optional() {
    let model = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../inv-model/Cargo.toml")).unwrap();
    assert!(model.contains("default = []"), "inv-model's default features changed");
}
