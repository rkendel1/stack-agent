use ledger_core::{Amount, Ledger};
use ledger_import::{import, parse_amount_field};

#[test]
fn parentheses_are_negative() {
    assert_eq!(parse_amount_field("(4.00)").unwrap(), Amount::from_cents(-400));
    assert_eq!(parse_amount_field(" (1,234.50) ").unwrap(), Amount::from_cents(-123450));
    assert_eq!(parse_amount_field("4.00").unwrap(), Amount::from_cents(400));
    assert_eq!(parse_amount_field("-4.00").unwrap(), Amount::from_cents(-400));
}

#[test]
fn a_refund_balances_and_reduces_the_expense() {
    let mut l = Ledger::new();
    let text = "date,description,account,amount\n\
                2026-01-02,groceries,expenses:food,40.00\n\
                2026-01-02,groceries,assets:bank,(40.00)\n\
                2026-01-07,refund,assets:bank,4.00\n\
                2026-01-07,refund,expenses:food,(4.00)\n";
    assert_eq!(import(&mut l, text).unwrap(), 2);
    assert_eq!(l.balance("expenses:food"), Amount::from_cents(3600));
}
