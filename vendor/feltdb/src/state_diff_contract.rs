//! The deterministic contract behind FeltDB's semantic state diff.
//!
//! This is the second specimen of the pattern established by
//! [`crate::trigger_contract`]: a decision FeltDB already makes, drawn behind a
//! runtime-independent boundary so that more than one implementation can be
//! held to it.
//!
//! ```text
//!        canonical state A  +  canonical state B
//!                        │
//!         ┌──────────────┴──────────────┐
//!         │                             │
//!   native evaluation             WASM evaluation
//!         │                             │
//!         └──────────────┬──────────────┘
//!                        │
//!               canonical diff bytes
//! ```
//!
//! # This module adds no diff semantics
//!
//! [`SemanticDiff::compute`] is untouched and remains authoritative. Everything
//! here is envelope: identity, version, canonical encoding, and the decision to
//! report an unusable input rather than fail. If this module and
//! `SemanticDiff::compute` ever disagree, `SemanticDiff::compute` is right.
//!
//! # Determinism budget
//!
//! Evaluation reads nothing but its two states. No clock, filesystem, network,
//! random source, process identity, authentication context, authority metadata
//! or process-global mutable state participates, and the audit that produced
//! this contract found none in the implementation either.
//!
//! # What the audit established about the semantics
//!
//! These are properties of the existing implementation, recorded here because
//! the contract is defined against them rather than against an idea of what a
//! diff should do:
//!
//! - **Arrays are positional.** A reorder is not detected; `[1,2] -> [2,1]`
//!   reports `Changed` at index 0 and at index 1. Elements past the shorter
//!   length become `Added` or `Removed` at their index.
//! - **Absence and null are different, in both directions.** `missing -> value`
//!   is `Added`, `null -> value` is `Changed`, `value -> missing` is `Removed`,
//!   `value -> null` is `Changed`.
//! - **`old_value` and `new_value` are `Option`, encoded as `null` when absent.**
//!   That is ambiguous with a JSON null on its own, and `kind` is what
//!   disambiguates: `missing -> null` is `Added` with `new_value: null`.
//! - **Numbers compare by `serde_json` variant.** `42` and `42.0` differ, so
//!   they report `Changed`; `-0` is a float and so differs from the integer
//!   `0`; `-0.0` and `0.0` are equal under IEEE equality and report nothing.
//! - **A type transition is one `Changed`,** carrying both whole values, at the
//!   path where it happens — including at the empty path, for the states
//!   themselves.
//! - **Ordering is total.** Changes sort by path, then by kind. Paths are
//!   unique within one diff, so the ordering does not depend on sort stability.

use crate::state_model::SemanticDiff;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Stable identity of this contract.
pub const STATE_DIFF_CONTRACT_ID: &str = "feltdb.state.diff";

/// Version of this contract's input shape, output shape and semantics.
///
/// The output shape is [`SemanticDiff`]'s own serialization, so a change to
/// that type is a change to this contract and must bump this.
pub const STATE_DIFF_CONTRACT_VERSION: u32 = 1;

/// The complete deterministic input: two states, and the contract's identity.
///
/// Members are declared alphabetically so the canonical struct encoding matches
/// the sorted-key encoding `serde_json` gives every JSON object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateDiffInput {
    pub contract: String,
    pub state_a: Value,
    pub state_b: Value,
    pub version: u32,
}

impl StateDiffInput {
    /// Builds an input stamped with this contract's identity and version.
    pub fn new(state_a: Value, state_b: Value) -> Self {
        Self {
            contract: STATE_DIFF_CONTRACT_ID.to_string(),
            state_a,
            state_b,
            version: STATE_DIFF_CONTRACT_VERSION,
        }
    }
}

/// Why an input could not be evaluated.
///
/// Three codes, matching the vocabulary [`crate::trigger_contract`] uses. As
/// there, an error carries a code and nothing else: a free-form message would
/// be a divergence surface between implementations that says nothing about the
/// semantics under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ContractErrorCode {
    /// The bytes are not a well-formed instance of this contract's input.
    ContractInputInvalid,
    /// The input declares a different contract.
    ContractIdMismatch,
    /// The input declares a version this implementation does not implement.
    ContractVersionUnsupported,
}

/// The error body of a contract output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractErrorBody {
    pub code: ContractErrorCode,
}

/// The complete deterministic output.
///
/// Exactly one of `diff` and `error` is present. `diff` is [`SemanticDiff`]
/// serialized exactly as it already serializes — this contract does not
/// reshape it, because the existing semantic result *is* the contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateDiffOutput {
    pub contract: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<SemanticDiff>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ContractErrorBody>,
    pub version: u32,
}

impl StateDiffOutput {
    fn computed(diff: SemanticDiff) -> Self {
        Self {
            contract: STATE_DIFF_CONTRACT_ID.to_string(),
            diff: Some(diff),
            error: None,
            version: STATE_DIFF_CONTRACT_VERSION,
        }
    }

    fn failure(code: ContractErrorCode) -> Self {
        Self {
            contract: STATE_DIFF_CONTRACT_ID.to_string(),
            diff: None,
            error: Some(ContractErrorBody { code }),
            version: STATE_DIFF_CONTRACT_VERSION,
        }
    }
}

/// The runtime that executed a contract, recorded in evidence.
///
/// Only [`ContractRuntime::Native`] is wired anywhere. As with the trigger
/// contract, the WASM variant exists so a recorded runtime can be named
/// honestly; it is not a selection switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractRuntime {
    Native,
    Wasm,
}

impl ContractRuntime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Wasm => "wasm",
        }
    }
}

/// The deterministic contract, natively implemented.
///
/// A thin adapter over the existing implementation, deliberately holding no
/// diff logic of its own.
pub fn diff_states(state_a: &Value, state_b: &Value) -> SemanticDiff {
    SemanticDiff::compute(state_a, state_b)
}

/// The canonical byte encoding of a contract input.
pub fn canonical_input_bytes(input: &StateDiffInput) -> Vec<u8> {
    serde_json::to_vec(input).expect("contract input is always serializable")
}

/// The canonical byte encoding of a contract output.
pub fn canonical_output_bytes(output: &StateDiffOutput) -> Vec<u8> {
    serde_json::to_vec(output).expect("contract output is always serializable")
}

/// Content identity of a contract input, for provenance.
pub fn input_digest(input: &StateDiffInput) -> String {
    format!("sha256:{:x}", Sha256::digest(canonical_input_bytes(input)))
}

/// The runtime-independent entry point: canonical bytes in, canonical bytes out.
///
/// This is the exact shape a foreign runtime implements. It never panics and
/// never returns anything but a canonical output encoding, so an unusable input
/// is a contract decision rather than a host-specific failure mode.
pub fn evaluate_canonical(input: &[u8]) -> Vec<u8> {
    canonical_output_bytes(&evaluate_canonical_output(input))
}

/// [`evaluate_canonical`] before encoding, for callers that want the value.
pub fn evaluate_canonical_output(input: &[u8]) -> StateDiffOutput {
    let Ok(parsed) = serde_json::from_slice::<StateDiffInput>(input) else {
        return StateDiffOutput::failure(ContractErrorCode::ContractInputInvalid);
    };
    if parsed.contract != STATE_DIFF_CONTRACT_ID {
        return StateDiffOutput::failure(ContractErrorCode::ContractIdMismatch);
    }
    if parsed.version != STATE_DIFF_CONTRACT_VERSION {
        return StateDiffOutput::failure(ContractErrorCode::ContractVersionUnsupported);
    }
    StateDiffOutput::computed(diff_states(&parsed.state_a, &parsed.state_b))
}
