//! Bank statements exported as CSV, read into balanced transactions.
//!
//! The format: a header line `date,description,account,amount`, then one posting per line. Lines
//! with the same date and description, one after another, form one transaction. Fields may be
//! quoted (`"Smith, J."`); a quote inside a quoted field is doubled. Amounts may use accounting
//! negatives in parentheses: `(4.00)` is `-4.00`.

mod csv;

use ledger_core::{Amount, Ledger, LedgerError, ParseAmountError, Transaction};

pub use csv::split_record;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    MissingHeader,
    Fields { line: usize, found: usize },
    Amount { line: usize, error: ParseAmountError },
    Ledger { line: usize, error: LedgerError },
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::MissingHeader => write!(f, "the statement has no header line"),
            ImportError::Fields { line, found } => write!(f, "line {line}: expected 4 fields, found {found}"),
            ImportError::Amount { line, error } => write!(f, "line {line}: {error}"),
            ImportError::Ledger { line, error } => write!(f, "line {line}: {error}"),
        }
    }
}

impl std::error::Error for ImportError {}

pub const HEADER: &str = "date,description,account,amount";

/// Parses one amount field, accepting accounting negatives: `(4.00)` is `-4.00`.
pub fn parse_amount_field(field: &str) -> Result<Amount, ParseAmountError> {
    let f = field.trim();
    match f.strip_prefix('(').and_then(|rest| rest.strip_suffix(')')) {
        Some(inner) => Amount::parse(inner).map(|a| -a),
        None => Amount::parse(f),
    }
}

/// Reads a statement into transactions, in order, without recording them.
pub fn read_statement(text: &str) -> Result<Vec<Transaction>, ImportError> {
    let mut lines = text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty());
    match lines.next() {
        Some((_, h)) if h.trim().eq_ignore_ascii_case(HEADER) => {}
        _ => return Err(ImportError::MissingHeader),
    }
    let mut out: Vec<Transaction> = Vec::new();
    for (i, line) in lines {
        let n = i + 1;
        let fields = split_record(line);
        if fields.len() != 4 {
            return Err(ImportError::Fields { line: n, found: fields.len() });
        }
        let amount = parse_amount_field(&fields[3]).map_err(|error| ImportError::Amount { line: n, error })?;
        let (date, description, account) = (fields[0].trim(), fields[1].trim(), fields[2].trim());
        match out.last_mut() {
            Some(tx) if tx.date == date && tx.description == description => {
                tx.postings.push(ledger_core::Posting { account: account.to_string(), amount });
            }
            _ => out.push(Transaction::new(date, description).post(account, amount)),
        }
    }
    Ok(out)
}

/// Reads a statement and records every transaction in `ledger`. Stops at the first error; the
/// transactions before it stay recorded.
pub fn import(ledger: &mut Ledger, text: &str) -> Result<usize, ImportError> {
    let txs = read_statement(text)?;
    let count = txs.len();
    for (i, tx) in txs.into_iter().enumerate() {
        ledger.record(tx).map_err(|error| ImportError::Ledger { line: i + 2, error })?;
    }
    Ok(count)
}
