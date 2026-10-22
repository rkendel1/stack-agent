//! **Experimental.** Chip's deterministic single-symbol repair loop.
//!
//! Chip decides what happens next. FX proposes one bounded change. Code enforces the contract.
//! Tests establish success.
//!
//! * [`machine`]: the Chip-owned state machine (acceptance, targeting, one decision, validation,
//!   apply, re-run, revert), checkpointed in FeltDB ([`store`]) at every transition.
//! * [`contract`]: the versioned, bounded [`contract::TaskPacket`] Chip sends, and the four
//!   operations a reply may propose, validated strictly. A reply cannot name a file, offset,
//!   command or tool, and cannot declare the task done.
//! * [`symbols`] and [`edit`]: Chip's own index of the checkout (the `syn` parser `chip-graph`
//!   uses) and the edit boundary: a body is parsed, spliced into the span Chip resolved, reparsed,
//!   and refused if the signature or anything outside the body would change.
//! * [`acceptance`] and [`target`]: the task's acceptance command, observed, and the one symbol its
//!   diagnostics point to.
//! * [`fx`]: FX as a pure inference function over a configured `chip-inference` provider.
//!
//! Nothing here is on the product path: `stack-agent work`, `verify` and `serve` do not use it.

pub mod acceptance;
pub mod contract;
pub mod edit;
pub mod fx;
pub mod machine;
pub mod store;
pub mod symbols;
pub mod target;

pub use machine::{
    Budgets, LoopConfig, LoopError, LoopResult, Phase, RunState, TaskDefinition, Termination,
    TerminationReason, run,
};
