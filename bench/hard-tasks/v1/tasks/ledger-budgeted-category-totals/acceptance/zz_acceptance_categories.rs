use ledger_core::{Amount, Ledger, Transaction};
use ledger_report::category_totals;

fn ledger() -> Ledger {
    let mut l = Ledger::new();
    for (acct, c) in [("expenses:food:groceries", 6250), ("expenses:food:dining", 420), ("expenses:rent", 90000), ("income:salary", -100000), ("income:refunds", 0)] {
        let a = Amount::from_cents(c);
        l.record(Transaction::new("2026-01-01", acct).post(acct, a).post("assets:bank", -a)).unwrap();
    }
    l
}

#[test]
fn rolls_up_to_depth() {
    let got = category_totals(&ledger(), 2);
    assert_eq!(got, vec![
        ("assets:bank".to_string(), Amount::from_cents(100000 - 6250 - 420 - 90000)),
        ("expenses:food".to_string(), Amount::from_cents(6670)),
        ("expenses:rent".to_string(), Amount::from_cents(90000)),
        ("income:salary".to_string(), Amount::from_cents(-100000)),
    ]);
}

#[test]
fn depth_one_and_deep() {
    let one = category_totals(&ledger(), 1);
    assert_eq!(one.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), vec!["assets", "expenses", "income"]);
    assert_eq!(one[1].1, Amount::from_cents(96670));
    let deep = category_totals(&ledger(), 5);
    assert!(deep.iter().any(|(k, a)| k == "expenses:food:dining" && *a == Amount::from_cents(420)));
    assert!(!deep.iter().any(|(k, _)| k == "income:refunds"), "zero roll-ups are left out");
}

#[test]
#[should_panic]
fn depth_zero_is_refused() {
    category_totals(&ledger(), 0);
}
