use std::collections::BTreeMap;
use std::fmt;

use crate::Amount;

/// One side of a transaction: an amount posted to an account (`assets:bank`, `expenses:food`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posting {
    pub account: String,
    pub amount: Amount,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transaction {
    /// `YYYY-MM-DD`.
    pub date: String,
    pub description: String,
    pub postings: Vec<Posting>,
}

impl Transaction {
    pub fn new(date: &str, description: &str) -> Transaction {
        Transaction { date: date.to_string(), description: description.to_string(), postings: Vec::new() }
    }

    pub fn post(mut self, account: &str, amount: Amount) -> Transaction {
        self.postings.push(Posting { account: account.to_string(), amount });
        self
    }

    pub fn total(&self) -> Amount {
        self.postings.iter().map(|p| p.amount).sum()
    }

    /// `YYYY-MM`.
    pub fn month(&self) -> &str {
        self.date.get(..7).unwrap_or(&self.date)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    Unbalanced { description: String, off_by: Amount },
    NoPostings { description: String },
    BadAccount(String),
}

impl fmt::Display for LedgerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LedgerError::Unbalanced { description, off_by } => {
                write!(f, "transaction {description:?} does not balance (off by {off_by})")
            }
            LedgerError::NoPostings { description } => write!(f, "transaction {description:?} has no postings"),
            LedgerError::BadAccount(a) => write!(f, "not an account name: {a:?}"),
        }
    }
}

impl std::error::Error for LedgerError {}

/// Balanced transactions, and the balance of every account they touch.
#[derive(Debug, Default, Clone)]
pub struct Ledger {
    transactions: Vec<Transaction>,
    balances: BTreeMap<String, Amount>,
}

fn valid_account(name: &str) -> bool {
    !name.is_empty() && name.split(':').all(|part| !part.is_empty() && !part.contains(char::is_whitespace))
}

impl Ledger {
    pub fn new() -> Ledger {
        Ledger::default()
    }

    /// Records a transaction if its postings balance; otherwise nothing changes.
    pub fn record(&mut self, tx: Transaction) -> Result<(), LedgerError> {
        if tx.postings.is_empty() {
            return Err(LedgerError::NoPostings { description: tx.description });
        }
        if let Some(bad) = tx.postings.iter().find(|p| !valid_account(&p.account)) {
            return Err(LedgerError::BadAccount(bad.account.clone()));
        }
        let total = tx.total();
        if total != Amount::ZERO {
            return Err(LedgerError::Unbalanced { description: tx.description, off_by: total });
        }
        for p in &tx.postings {
            *self.balances.entry(p.account.clone()).or_default() += p.amount;
        }
        self.transactions.push(tx);
        Ok(())
    }

    pub fn balance(&self, account: &str) -> Amount {
        self.balances.get(account).copied().unwrap_or_default()
    }

    /// Every account with its balance, by account name.
    pub fn balances(&self) -> impl Iterator<Item = (&str, Amount)> {
        self.balances.iter().map(|(k, v)| (k.as_str(), *v))
    }

    pub fn transactions(&self) -> &[Transaction] {
        &self.transactions
    }
}
