use ledger_core::{Amount, Ledger, Transaction};
use ledger_report::monthly_totals;

#[test]
fn monthly_totals_with_single_decimal_amounts() {
    let mut l = Ledger::new();
    for (d, acct, amt) in [("2026-01-09", "expenses:food:groceries", "62.5"), ("2026-02-01", "expenses:food:dining", "4.2")] {
        let a = Amount::parse(amt).unwrap();
        l.record(Transaction::new(d, "x").post(acct, a).post("assets:bank", -a)).unwrap();
    }
    assert_eq!(
        monthly_totals(&l, "expenses"),
        vec![("2026-01".to_string(), Amount::from_cents(6250)), ("2026-02".to_string(), Amount::from_cents(420))]
    );
}
