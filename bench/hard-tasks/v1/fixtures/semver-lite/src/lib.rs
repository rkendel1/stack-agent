//! Semantic versions and requirements.

mod req;
mod version;

pub use req::{parse_req, Comparator, Op, Req, ReqError};
pub use version::{parse_version, Prerelease, Version, VersionError};
