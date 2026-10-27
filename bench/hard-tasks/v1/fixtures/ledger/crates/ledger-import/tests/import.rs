use ledger_core::{Amount, Ledger};
use ledger_import::{import, read_statement, split_record, ImportError};

#[test]
fn quoted_fields() {
    assert_eq!(split_record(r#"2026-01-02,"Smith, J.",assets:bank,5"#)[1], "Smith, J.");
    assert_eq!(split_record(r#"a,"say ""hi""",c"#)[1], r#"say "hi""#);
}

#[test]
fn consecutive_lines_form_one_transaction() {
    let text = "date,description,account,amount\n\
                2026-01-02,rent,expenses:rent,900.00\n\
                2026-01-02,rent,assets:bank,-900.00\n\
                2026-01-05,coffee,expenses:food,3.50\n\
                2026-01-05,coffee,assets:cash,-3.50\n";
    let txs = read_statement(text).unwrap();
    assert_eq!(txs.len(), 2);
    assert_eq!(txs[0].postings.len(), 2);
}

#[test]
fn import_records_balances() {
    let mut l = Ledger::new();
    let text = "date,description,account,amount\n\
                2026-01-02,rent,expenses:rent,900.00\n\
                2026-01-02,rent,assets:bank,-900.00\n";
    assert_eq!(import(&mut l, text).unwrap(), 1);
    assert_eq!(l.balance("assets:bank"), Amount::from_cents(-90000));
}

#[test]
fn errors_name_the_line() {
    let text = "date,description,account,amount\n2026-01-02,rent,expenses:rent\n";
    assert_eq!(read_statement(text), Err(ImportError::Fields { line: 2, found: 3 }));
    assert_eq!(read_statement("2026-01-02,a,b,1\n"), Err(ImportError::MissingHeader));
}

#[test]
fn accounting_negatives() {
    let text = "date,description,account,amount\n\
                2026-01-07,refund,assets:bank,4.00\n\
                2026-01-07,refund,expenses:food,(4.00)\n";
    let txs = read_statement(text).unwrap();
    assert_eq!(txs[0].postings[1].amount, Amount::from_cents(-400));
}
