//! Opt-in experimentation for Chip: run a candidate behavior and a baseline on the same controlled
//! tasks, judge both by the same host-defined criteria over host-observed evidence, and report the
//! comparison with its provenance.
//!
//! This crate composes existing contracts and owns none of the authority they carry:
//!
//! - a run is an ordinary Chip work ([`chip_core::Agent::run_work_with_context_policy`]): Chip's loop
//!   validates every proposal, authorizes and executes requests, enforces the limits, the deadline
//!   and cancellation (its [`WorkControl`](chip_core::WorkControl)), and refuses completions its
//!   required observations do not support;
//! - a model, when one is used, is behind Chip's own model boundary
//!   ([`chip_core::ModelDecisionBoundary`]);
//! - capabilities come through a [`CapabilitySet`](chip_core::CapabilitySet) built per run.
//!
//! Nothing here is on a production path: `stack-agent work`, `verify` and `serve` never construct a
//! [`Runner`]. There is no global registry, no runtime loading and no environment switch; a host
//! builds a [`Registry`] and calls a [`Runner`] explicitly. See `docs/product/experimentation.md`.

pub mod criteria;
pub mod definition;
pub mod plugin;
pub mod reference;
pub mod registry;
pub mod report;
pub mod runner;

pub use criteria::{Criterion, CriterionResult, HostObservation};
pub use definition::{
    DefinitionError, EvidenceField, ExperimentDefinition, Mode, ModelSpec, RunLimits, Versioned,
};
pub use plugin::{
    CapabilityFixture, Confinement, ExtensionPoint, RecordedExecution, Recording, Strategy,
    TaskFixture,
};
pub use registry::{Registry, ResolvedExperiment, ResolvedStrategy};
pub use report::{Provenance, Redactor, report_json, summary};
pub use runner::{
    CancelHandle, Cleanup, Comparison, Conclusion, ExperimentResult, RunRecord, Runner, Side,
    Termination, Verdict,
};
