use ledger_core::Amount;

#[test]
fn negatives_display() {
    for (c, s) in [(-5, "-0.05"), (-50, "-0.50"), (-310, "-3.10"), (-123456, "-1,234.56"), (-100000000, "-1,000,000.00"), (0, "0.00"), (123456, "1,234.56")] {
        assert_eq!(Amount::from_cents(c).to_string(), s);
    }
}

#[test]
fn display_round_trips() {
    for c in [-1, -99, -100, -101, -99999, 7, 100000] {
        assert_eq!(Amount::parse(&Amount::from_cents(c).to_string()).unwrap().cents(), c);
    }
}

#[test]
fn padding_still_aligns() {
    assert_eq!(format!("{:>8}", Amount::from_cents(-310)), "   -3.10");
}
