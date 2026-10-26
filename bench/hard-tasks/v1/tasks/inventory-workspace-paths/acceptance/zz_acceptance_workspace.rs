use inv_api::{handle, Request};
use inv_model::{Item, Sku};
use inv_store::Store;

#[test]
fn the_workspace_builds_and_the_api_uses_the_store() {
    let mut s = Store::new();
    s.add(Item { sku: Sku::new("MUG-01").unwrap(), name: "Mug".into(), on_hand: 3 });
    assert_eq!(handle(&mut s, Request::Available { sku: " mug-01 ".into() }, 0).body, "3");
}

#[test]
fn the_workspace_keeps_exactly_its_three_crates() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml");
    let text = std::fs::read_to_string(root).unwrap();
    for c in ["crates/inv-model", "crates/inv-store", "crates/inv-api"] {
        assert!(text.contains(c), "{c} is no longer a member");
    }
    assert!(!text.contains("inv-cli"), "a member that does not exist is still listed");
    let api = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    assert!(api.contains("\"../inv-store\""), "inv-api depends on inv-store by its real path");
}
