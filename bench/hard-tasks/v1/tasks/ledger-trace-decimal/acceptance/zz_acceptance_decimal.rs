use ledger_core::Amount;

fn cents(s: &str) -> i64 {
    Amount::parse(s).unwrap().cents()
}

#[test]
fn one_decimal_means_tenths() {
    assert_eq!(cents("12.5"), 1250);
    assert_eq!(cents("0.5"), 50);
    assert_eq!(cents("-0.5"), -50);
    assert_eq!(cents("1,000.1"), 100010);
    assert_eq!(cents(".5"), 50);
}

#[test]
fn two_decimals_unchanged() {
    assert_eq!(cents("12.05"), 1205);
    assert_eq!(cents("12.50"), 1250);
    assert_eq!(cents("-3.07"), -307);
    assert_eq!(cents("7"), 700);
}

#[test]
fn round_trip_through_display() {
    for s in ["4.2", "62.5", "0.1", "1,234.5"] {
        let a = Amount::parse(s).unwrap();
        assert_eq!(Amount::parse(&a.to_string()).unwrap(), a, "{s}");
    }
}
