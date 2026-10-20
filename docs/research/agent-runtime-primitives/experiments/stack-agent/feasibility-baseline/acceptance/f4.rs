use tally::math::{add, double, triple};

#[test]
fn double_doubles() {
    assert_eq!(double(0), 0);
    assert_eq!(double(21), 42);
    assert_eq!(double(2_000_000_000), 4_000_000_000);
}

#[test]
fn the_other_functions_are_unchanged() {
    assert_eq!(add(2, 3), 5);
    assert_eq!(triple(4), 12);
}
