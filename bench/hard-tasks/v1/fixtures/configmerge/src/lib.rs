//! Layered configuration.

mod env;
mod file;
mod merge;
mod value;

pub use env::from_env;
pub use file::{parse_file, FileError};
pub use merge::{load, merge};
pub use value::{parse_value, Value};
