//! Reports over a ledger: a plain-text statement, monthly totals and category roll-ups.

use std::collections::BTreeMap;

use ledger_core::{Amount, Ledger};

/// The total posted to accounts under `prefix` (`"expenses"` covers `expenses:food` and
/// `expenses:rent`, not `expensesx`), per month (`YYYY-MM`), in month order.
pub fn monthly_totals(ledger: &Ledger, prefix: &str) -> Vec<(String, Amount)> {
    let mut months: BTreeMap<String, Amount> = BTreeMap::new();
    for tx in ledger.transactions() {
        for p in &tx.postings {
            if under(&p.account, prefix) {
                *months.entry(tx.month().to_string()).or_default() += p.amount;
            }
        }
    }
    months.into_iter().collect()
}

fn under(account: &str, prefix: &str) -> bool {
    account == prefix || account.strip_prefix(prefix).is_some_and(|rest| rest.starts_with(':'))
}

/// One line per posting, `date  description  account  amount`, with the amount column right
/// aligned to the widest amount, then a line with the total of all postings to `account`.
pub fn statement(ledger: &Ledger, account: &str) -> String {
    let rows: Vec<(&str, &str, Amount)> = ledger
        .transactions()
        .iter()
        .flat_map(|tx| {
            tx.postings
                .iter()
                .filter(|p| p.account == account)
                .map(move |p| (tx.date.as_str(), tx.description.as_str(), p.amount))
        })
        .collect();
    let width = rows.iter().map(|(_, _, a)| a.to_string().len()).max().unwrap_or(4).max(4);
    let desc = rows.iter().map(|(_, d, _)| d.len()).max().unwrap_or(0);
    let mut out = String::new();
    for (date, d, amount) in &rows {
        out.push_str(&format!("{date}  {d:<desc$}  {amount:>width$}\n"));
    }
    let total: Amount = rows.iter().map(|(_, _, a)| *a).sum();
    out.push_str(&format!("{:<w$}  {total:>width$}\n", "total", w = 10 + 2 + desc));
    out
}
