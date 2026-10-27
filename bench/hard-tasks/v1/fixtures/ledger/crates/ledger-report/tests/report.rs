use ledger_core::{Amount, Ledger, Transaction};
use ledger_report::{monthly_totals, statement};

const STATEMENT: &str = "date,description,account,amount\n\
    2026-01-02,rent,expenses:rent,900\n\
    2026-01-02,rent,assets:bank,-900\n\
    2026-01-09,groceries,expenses:food:groceries,62.5\n\
    2026-01-09,groceries,assets:bank,-62.5\n\
    2026-02-01,coffee,expenses:food:dining,4.2\n\
    2026-02-01,coffee,assets:bank,-4.2\n";

fn ledger() -> Ledger {
    let mut l = Ledger::new();
    for tx in ledger_import_lite(STATEMENT) {
        l.record(tx).unwrap();
    }
    l
}

// The report crate does not depend on the importer; this reads the same simple format.
fn ledger_import_lite(text: &str) -> Vec<Transaction> {
    let mut out: Vec<Transaction> = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.trim().split(',').collect();
        let amount = Amount::parse(f[3]).unwrap();
        match out.last_mut() {
            Some(tx) if tx.date == f[0] && tx.description == f[1] => {
                tx.postings.push(ledger_core::Posting { account: f[2].to_string(), amount })
            }
            _ => out.push(Transaction::new(f[0], f[1]).post(f[2], amount)),
        }
    }
    out
}

#[test]
fn monthly_expense_totals_match_the_bank_statement() {
    let totals = monthly_totals(&ledger(), "expenses");
    assert_eq!(
        totals,
        vec![("2026-01".to_string(), Amount::from_cents(96250)), ("2026-02".to_string(), Amount::from_cents(420))]
    );
}

#[test]
fn prefixes_match_whole_segments() {
    let mut l = Ledger::new();
    l.record(Transaction::new("2026-03-01", "x").post("expensesx", Amount::from_cents(5)).post("assets:bank", Amount::from_cents(-5)))
        .unwrap();
    assert!(monthly_totals(&l, "expenses").is_empty());
}

#[test]
fn statement_lists_postings_and_total() {
    let s = statement(&ledger(), "assets:bank");
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines.len(), 4);
    assert!(lines[0].starts_with("2026-01-02  rent"), "{s}");
    assert!(lines[3].starts_with("total"), "{s}");
    assert!(lines[3].ends_with("-966.70"), "{s}");
}
