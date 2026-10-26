use ledger_core::{Amount, Ledger, LedgerError, Transaction};

fn a(c: i64) -> Amount {
    Amount::from_cents(c)
}

#[test]
fn balanced_transactions_update_balances() {
    let mut l = Ledger::new();
    l.record(Transaction::new("2026-01-03", "groceries").post("expenses:food", a(4250)).post("assets:bank", a(-4250)))
        .unwrap();
    assert_eq!(l.balance("expenses:food"), a(4250));
    assert_eq!(l.balance("assets:bank"), a(-4250));
}

#[test]
fn unbalanced_transactions_are_refused_and_change_nothing() {
    let mut l = Ledger::new();
    let err = l.record(Transaction::new("2026-01-03", "oops").post("expenses:food", a(100)).post("assets:bank", a(-99)));
    assert!(matches!(err, Err(LedgerError::Unbalanced { .. })));
    assert_eq!(l.transactions().len(), 0);
    assert_eq!(l.balance("expenses:food"), Amount::ZERO);
}

#[test]
fn account_names_are_checked() {
    let mut l = Ledger::new();
    let err = l.record(Transaction::new("2026-01-03", "x").post("expenses::food", a(1)).post("assets:bank", a(-1)));
    assert_eq!(err, Err(LedgerError::BadAccount("expenses::food".into())));
}
