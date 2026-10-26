use std::fmt;
use std::ops::{Add, AddAssign, Neg, Sub};

/// An exact amount of money, in cents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Amount(i64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseAmountError {
    Empty,
    Invalid(String),
    TooManyDecimals(String),
}

impl fmt::Display for ParseAmountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseAmountError::Empty => write!(f, "empty amount"),
            ParseAmountError::Invalid(s) => write!(f, "not an amount: {s:?}"),
            ParseAmountError::TooManyDecimals(s) => write!(f, "more than two decimals: {s:?}"),
        }
    }
}

impl std::error::Error for ParseAmountError {}

impl Amount {
    pub const ZERO: Amount = Amount(0);

    pub fn from_cents(cents: i64) -> Amount {
        Amount(cents)
    }

    pub fn cents(self) -> i64 {
        self.0
    }

    pub fn is_negative(self) -> bool {
        self.0 < 0
    }

    pub fn abs(self) -> Amount {
        Amount(self.0.abs())
    }

    /// Parses decimal text: an optional sign, digits with optional `,` thousands separators, and
    /// at most two decimals. `"12"`, `"12.5"`, `"12.05"`, `"-3.10"` and `"1,234.56"` are accepted.
    pub fn parse(text: &str) -> Result<Amount, ParseAmountError> {
        let s = text.trim();
        if s.is_empty() {
            return Err(ParseAmountError::Empty);
        }
        let (negative, digits) = match s.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        let (whole, frac) = match digits.split_once('.') {
            Some((w, f)) => (w, f),
            None => (digits, ""),
        };
        if frac.len() > 2 {
            return Err(ParseAmountError::TooManyDecimals(text.to_string()));
        }
        let whole_digits: String = whole.chars().filter(|c| *c != ',').collect();
        if whole_digits.is_empty() && frac.is_empty() {
            return Err(ParseAmountError::Invalid(text.to_string()));
        }
        if !whole_digits.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
            return Err(ParseAmountError::Invalid(text.to_string()));
        }
        let whole_value: i64 = if whole_digits.is_empty() {
            0
        } else {
            whole_digits.parse().map_err(|_| ParseAmountError::Invalid(text.to_string()))?
        };
        let frac_value: i64 = match frac.len() {
            0 => 0,
            1 => frac.parse::<i64>().unwrap() * 10,
            _ => frac.parse::<i64>().unwrap(),
        };
        let cents = whole_value * 100 + frac_value;
        Ok(Amount(if negative { -cents } else { cents }))
    }

    /// Splits the amount into `parts` amounts that differ by at most one cent and sum to it
    /// exactly. The first parts receive the extra cents.
    pub fn split(self, parts: usize) -> Vec<Amount> {
        assert!(parts > 0, "cannot split into zero parts");
        let n = parts as i64;
        let base = self.0.div_euclid(n);
        let extra = self.0.rem_euclid(n);
        (0..n).map(|i| Amount(base + if i >= n - extra { 1 } else { 0 })).collect()
    }
}

impl fmt::Display for Amount {
    /// `1234.5` cents-wise as `1,234.50`; negatives as `-3.10`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        let whole = abs / 100;
        let frac = abs % 100;
        let digits = whole.to_string();
        let mut grouped = String::new();
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i) % 3 == 0 {
                grouped.push(',');
            }
            grouped.push(c);
        }
        let text = format!("{sign}{grouped}.{frac:02}");
        f.pad(&text)
    }
}

impl Add for Amount {
    type Output = Amount;
    fn add(self, rhs: Amount) -> Amount {
        Amount(self.0 + rhs.0)
    }
}

impl AddAssign for Amount {
    fn add_assign(&mut self, rhs: Amount) {
        self.0 += rhs.0;
    }
}

impl Sub for Amount {
    type Output = Amount;
    fn sub(self, rhs: Amount) -> Amount {
        Amount(self.0 - rhs.0)
    }
}

impl Neg for Amount {
    type Output = Amount;
    fn neg(self) -> Amount {
        Amount(-self.0)
    }
}

impl std::iter::Sum for Amount {
    fn sum<I: Iterator<Item = Amount>>(iter: I) -> Amount {
        iter.fold(Amount::ZERO, |a, b| a + b)
    }
}
