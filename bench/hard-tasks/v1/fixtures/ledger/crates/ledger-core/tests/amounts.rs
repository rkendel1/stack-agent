use ledger_core::{Amount, ParseAmountError};

fn cents(s: &str) -> i64 {
    Amount::parse(s).unwrap().cents()
}

#[test]
fn whole_and_two_decimal_amounts() {
    assert_eq!(cents("12"), 1200);
    assert_eq!(cents("12.05"), 1205);
    assert_eq!(cents("0.99"), 99);
    assert_eq!(cents("1,234.56"), 123456);
}

#[test]
fn signs() {
    assert_eq!(cents("-3.10"), -310);
    assert_eq!(cents("+7"), 700);
}

#[test]
fn rejects_what_is_not_an_amount() {
    assert_eq!(Amount::parse(""), Err(ParseAmountError::Empty));
    assert!(matches!(Amount::parse("12.345"), Err(ParseAmountError::TooManyDecimals(_))));
    assert!(matches!(Amount::parse("1x"), Err(ParseAmountError::Invalid(_))));
}

#[test]
fn display_groups_thousands() {
    assert_eq!(Amount::from_cents(123456).to_string(), "1,234.56");
    assert_eq!(Amount::from_cents(5).to_string(), "0.05");
}

#[test]
fn split_sums_exactly() {
    let parts = Amount::from_cents(1000).split(3);
    assert_eq!(parts.iter().copied().sum::<Amount>(), Amount::from_cents(1000));
}
