use std::fmt;

#[derive(Debug)]
pub enum ParseError {
    Bad,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bad")
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug)]
pub enum StoreError {
    Full,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "full")
    }
}

impl std::error::Error for StoreError {}

#[derive(Debug)]
pub enum AuthError {
    Denied,
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "denied")
    }
}

impl std::error::Error for AuthError {}

// Divergent: Display only, no `Error` impl.
#[derive(Debug)]
pub struct LegacyError;

impl fmt::Display for LegacyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "legacy")
    }
}
