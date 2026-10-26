//! SKUs, quantities and items.

use std::fmt;

/// A stock-keeping unit: 3 to 32 characters, uppercase ASCII letters, digits and `-`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sku(String);

impl Sku {
    pub fn new(s: &str) -> Option<Sku> {
        let ok = (3..=32).contains(&s.len()) && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-');
        ok.then(|| Sku(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Lenient parsing for user input: trims and uppercases first.
    #[cfg(feature = "parse")]
    pub fn parse(input: &str) -> Option<Sku> {
        Sku::new(&input.trim().to_ascii_uppercase())
    }
}

impl fmt::Display for Sku {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub sku: Sku,
    pub name: String,
    pub on_hand: u32,
}
