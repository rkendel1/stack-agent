//! # fx-single-attempt
//!
//! Adapts a single-attempt provider port (Easy-LLM's `generateOnce` / `streamOnce`,
//! reached through the R4 host bridge) to the [`fx_core::Capability`] trait.
//!
//! This crate is a translation layer and defines no second contract. Delivery
//! certainty, failure origin, events and outcomes are `fx-core`'s; the executor
//! (`fx_core::execute`) owns validation, ordering, bounds and the terminal outcome.
//! Like `fx-core` it has no network, filesystem, process, environment, clock, thread
//! or async-runtime access, and no dependency beyond `fx-core`.
//!
//! See `docs/rebuild/SINGLE_ATTEMPT.md` for the provider-side audit and the mapping.

mod adapter;
mod mapping;
mod port;
pub mod testing;

pub use adapter::{Attempt, SingleAttemptCapability};
pub use mapping::{delivery_of, first_unsupported, map_failure, map_finish_reason};
pub use port::{
    AbortReport, Observed, PortFailure, PortFailureKind, PortItem, PortPoll, PortSupport,
    PortUsage, ProviderPort, Terminal,
};
