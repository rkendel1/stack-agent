use tally::greeting::greet;
use tally::math::{add, triple};
use tally::util::{checksum, label};

#[test]
fn add_adds() {
    assert_eq!(add(2, 3), 5);
}

#[test]
fn triple_triples() {
    assert_eq!(triple(4), 12);
}

#[test]
fn greet_addresses_the_name() {
    assert!(greet("sam").ends_with(", sam"));
}

#[test]
fn util_still_works() {
    assert_eq!(checksum(), 17);
    assert_eq!(label(3), "item-3");
}
