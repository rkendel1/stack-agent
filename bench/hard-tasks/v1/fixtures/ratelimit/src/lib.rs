//! Rate limiting: token buckets, sliding windows, and the policies that configure them.

mod bucket;
mod clock;
mod keyed;
mod policy;
mod window;

pub use bucket::TokenBucket;
pub use clock::{Clock, ManualClock, SystemClock};
pub use keyed::KeyedLimiter;
pub use policy::{parse_policy, Policy, PolicyError};
pub use window::SlidingWindow;
