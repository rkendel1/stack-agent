//! The deterministic contract behind FeltDB's conflict classification.
//!
//! The third specimen of the pattern established by [`crate::trigger_contract`]
//! and [`crate::state_diff_contract`], and the first that reasons over
//! *competing* changes rather than deciding or computing over one input.
//!
//! ```text
//!        base + left + right, as canonical content
//!                        │
//!         ┌──────────────┴──────────────┐
//!         │                             │
//!   native evaluation             WASM evaluation
//!         │                             │
//!         └──────────────┬──────────────┘
//!                        │
//!            canonical classification bytes
//! ```
//!
//! # This module adds no classification semantics
//!
//! [`ConflictClassification::classify`] is untouched and remains authoritative.
//! Everything here is envelope: identity, version, canonical encoding, and the
//! decision to report an unusable input rather than fail. If this module and
//! `classify` ever disagree, `classify` is right.
//!
//! # Why the input is three strings and not three revisions
//!
//! `classify` takes three [`StateRevision`]s and reads exactly one field of
//! each: `content`. It never looks at `id`, `parent_id`, `authority`,
//! `timestamp_ms` or `metadata`, and a unit test in this module proves it.
//!
//! Taking revisions into the contract would therefore drag a clock reading into
//! it for nothing — `StateRevision::initial` calls `SystemTime::now()` — along
//! with an authority identity that has no bearing on the answer. The contract
//! takes the three content strings instead, which is the real semantic
//! boundary.
//!
//! It is a *string* rather than parsed JSON on purpose: `classify` parses with
//! `unwrap_or(Value::Null)`, so content that is not JSON classifies as JSON
//! null instead of failing. That tolerance is part of the semantics, and a
//! contract taking parsed values could not express it.
//!
//! # Determinism budget
//!
//! Evaluation reads nothing but its three strings. No clock, filesystem,
//! network, random source, process identity, authentication context, authority
//! metadata, revision identity, timestamp, operation provenance or
//! process-global mutable state participates, and the audit found none in the
//! implementation either.
//!
//! # What the audit established about the semantics
//!
//! Recorded here because the contract is defined against the implementation's
//! actual behaviour rather than against an idea of what a three-way merge
//! should do. Several of these will surprise a reader who expects a merge tool.
//!
//! - **There are three classes and no fourth.** `Independent`, `Convergent`,
//!   `Conflict`. There is no "no change" class: `classify(a, a, a)` returns an
//!   empty conflict list whose `overall` is `Independent`.
//! - **Classification is per overlapping path.** Two changes interact when
//!   their paths are equal *or* when one contains the other. A branch replacing
//!   `o` and a branch editing `o.a` cannot both be applied, so both paths are
//!   `Conflict`. Overlap is structural, over whole path components, so `a.b`
//!   and `a.bc` are disjoint. Version 1 of this contract collided on equal
//!   paths only and reported that case as `Independent`; see
//!   [state model](../../../docs/architecture/state-conflict-semantics.md).
//! - **Convergence ignores `old_value`.** Two sides agree when their change
//!   kind and their new value are equal, so both removing a member is
//!   `Convergent`, and both adding the same value is `Convergent`.
//! - **Differing kinds always conflict**, even when the outcome would agree —
//!   removing on one side and changing on the other is a `Conflict`. The kind
//!   half of the convergence test cannot actually decide anything, though:
//!   both diffs share a base, so whether a path resolves in it is a property of
//!   the base alone, and the one kind pair that can differ — `Removed` against
//!   `Changed` — always disagrees on the new value too, because a removal has
//!   none. The conformance suite asserts that redundancy rather than assuming
//!   it, and it is why relabelling a change kind is invisible to this
//!   contract while the diff contract catches it.
//! - **`base_value` is the common ancestor's value at the path**, read from the
//!   base state. Version 1 read it from the left branch's change, which
//!   reported `null` for a path only the right branch touched even when the
//!   base held a value there. Correcting it makes the whole result symmetric
//!   under a branch swap apart from the two branch values exchanging.
//! - **Absent and null collapse in the recorded values.** All three value
//!   fields are `Option<Value>` flattened from an optional change, so "no
//!   change here" and "the value is JSON null" both encode as `null`. The
//!   `classification` field is what carries the meaning.
//! - **`overall` is the maximum severity present**: `Conflict` if any path
//!   conflicts, else `Convergent` if any path converges, else `Independent`.
//! - **Ordering is total.** Paths are unique — they are the union of two
//!   path-keyed maps — and the result is sorted by path.
//! - **Diff semantics are inherited**, including positional arrays, so the same
//!   index changed on both sides conflicts while different indexes remain
//!   independent — under positional semantics both index changes survive a
//!   merge. Removing an array element while the other branch edits inside it
//!   *is* an overlap, and conflicts.

use crate::state_model::{ConflictClassification, StateId, StateRevision};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Stable identity of this contract.
pub const STATE_CONFLICT_CONTRACT_ID: &str = "feltdb.state.conflict";

/// Version of this contract's input shape, output shape and semantics.
///
/// The output shape is [`ConflictClassification`]'s own serialization, so a
/// change to that type is a change to this contract and must bump this.
pub const STATE_CONFLICT_CONTRACT_VERSION: u32 = 2;

/// The complete deterministic input: three state contents, and the contract's
/// identity.
///
/// Members are declared alphabetically so the canonical struct encoding matches
/// the sorted-key encoding `serde_json` gives every JSON object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateConflictInput {
    /// The common ancestor's content.
    pub base: String,
    pub contract: String,
    /// The first branch's content.
    pub left: String,
    /// The second branch's content.
    pub right: String,
    pub version: u32,
}

impl StateConflictInput {
    /// Builds an input stamped with this contract's identity and version.
    pub fn new(base: String, left: String, right: String) -> Self {
        Self {
            base,
            contract: STATE_CONFLICT_CONTRACT_ID.to_string(),
            left,
            right,
            version: STATE_CONFLICT_CONTRACT_VERSION,
        }
    }
}

/// Why an input could not be evaluated.
///
/// The same three codes the other two contracts use. Note that unparseable
/// *state content* is not one of them: that is a classification, not an error.
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
/// Exactly one of `classification` and `error` is present. `classification` is
/// [`ConflictClassification`] serialized exactly as it already serializes —
/// this contract does not reshape it, because the existing result is the
/// contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateConflictOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<ConflictClassification>,
    pub contract: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ContractErrorBody>,
    pub version: u32,
}

impl StateConflictOutput {
    fn classified(classification: ConflictClassification) -> Self {
        Self {
            classification: Some(classification),
            contract: STATE_CONFLICT_CONTRACT_ID.to_string(),
            error: None,
            version: STATE_CONFLICT_CONTRACT_VERSION,
        }
    }

    fn failure(code: ContractErrorCode) -> Self {
        Self {
            classification: None,
            contract: STATE_CONFLICT_CONTRACT_ID.to_string(),
            error: Some(ContractErrorBody { code }),
            version: STATE_CONFLICT_CONTRACT_VERSION,
        }
    }
}

/// The runtime that executed a contract, recorded in evidence.
///
/// Only [`ContractRuntime::Native`] is wired anywhere.
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

/// Wraps state content in the revision shape `classify` expects.
///
/// Every field but `content` is fixed, because `classify` reads no other field
/// and the contract must not acquire an input the decision does not use. In
/// particular nothing here reads a clock, unlike `StateRevision::initial`.
fn revision(content: &str) -> StateRevision {
    StateRevision {
        id: StateId::compute(content),
        resource: String::new(),
        content: content.to_string(),
        content_id: StateId::compute(content),
        parent_id: None,
        sequence: 0,
        authority: String::new(),
        timestamp_ms: 0,
        metadata: BTreeMap::new(),
    }
}

/// The deterministic contract, natively implemented.
///
/// A thin adapter over the existing implementation, deliberately holding no
/// classification logic of its own.
pub fn classify_states(base: &str, left: &str, right: &str) -> ConflictClassification {
    ConflictClassification::classify(&revision(base), &revision(left), &revision(right))
}

/// The canonical byte encoding of a contract input.
pub fn canonical_input_bytes(input: &StateConflictInput) -> Vec<u8> {
    serde_json::to_vec(input).expect("contract input is always serializable")
}

/// The canonical byte encoding of a contract output.
pub fn canonical_output_bytes(output: &StateConflictOutput) -> Vec<u8> {
    serde_json::to_vec(output).expect("contract output is always serializable")
}

/// Content identity of a contract input, for provenance.
pub fn input_digest(input: &StateConflictInput) -> String {
    format!("sha256:{:x}", Sha256::digest(canonical_input_bytes(input)))
}

/// The runtime-independent entry point: canonical bytes in, canonical bytes out.
///
/// Never panics and never returns anything but a canonical output encoding, so
/// an unusable input is a contract decision rather than a host-specific failure
/// mode. Unparseable *state content* is not an unusable input: it classifies as
/// JSON null, which is what the existing implementation does.
pub fn evaluate_canonical(input: &[u8]) -> Vec<u8> {
    canonical_output_bytes(&evaluate_canonical_output(input))
}

/// [`evaluate_canonical`] before encoding, for callers that want the value.
pub fn evaluate_canonical_output(input: &[u8]) -> StateConflictOutput {
    let Ok(parsed) = serde_json::from_slice::<StateConflictInput>(input) else {
        return StateConflictOutput::failure(ContractErrorCode::ContractInputInvalid);
    };
    if parsed.contract != STATE_CONFLICT_CONTRACT_ID {
        return StateConflictOutput::failure(ContractErrorCode::ContractIdMismatch);
    }
    if parsed.version != STATE_CONFLICT_CONTRACT_VERSION {
        return StateConflictOutput::failure(ContractErrorCode::ContractVersionUnsupported);
    }
    StateConflictOutput::classified(classify_states(&parsed.base, &parsed.left, &parsed.right))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_model::ConflictClass;

    fn classify(base: &str, left: &str, right: &str) -> String {
        String::from_utf8(canonical_output_bytes(&StateConflictOutput::classified(
            classify_states(base, left, right),
        )))
        .unwrap()
    }

    fn overall(base: &str, left: &str, right: &str) -> ConflictClass {
        classify_states(base, left, right).overall
    }

    #[test]
    fn the_contract_reads_only_revision_content() {
        // Two revisions differing in every field the decision does not use must
        // classify identically. If revision identity, authority, a timestamp or
        // metadata ever reached the classifier, the contract would stop being
        // reproducible anywhere and this would fail.
        let plain = revision(r#"{"x":1}"#);
        let decorated = StateRevision {
            id: StateId::compute("something else entirely"),
            resource: "some:other:resource".into(),
            content: r#"{"x":1}"#.to_string(),
            content_id: StateId::compute("not even the content"),
            parent_id: Some(StateId::compute("a parent")),
            sequence: 41,
            authority: "someone-else".into(),
            timestamp_ms: 1_700_000_000_000,
            metadata: [("k".to_string(), serde_json::json!("v"))]
                .into_iter()
                .collect(),
        };
        let base = revision(r#"{"x":0}"#);
        assert_eq!(
            serde_json::to_string(&ConflictClassification::classify(&base, &plain, &plain))
                .unwrap(),
            serde_json::to_string(&ConflictClassification::classify(
                &base, &decorated, &decorated
            ))
            .unwrap()
        );
    }

    #[test]
    fn identical_states_classify_as_independent_with_no_paths() {
        // There is no "no change" class; this is what the absence of change
        // looks like.
        assert_eq!(
            classify(r#"{"x":1}"#, r#"{"x":1}"#, r#"{"x":1}"#),
            r#"{"classification":{"path_conflicts":[],"overall":"Independent"},"contract":"feltdb.state.conflict","version":2}"#
        );
    }

    #[test]
    fn one_sided_changes_are_independent() {
        assert_eq!(
            overall(r#"{"x":1}"#, r#"{"x":2}"#, r#"{"x":1}"#),
            ConflictClass::Independent
        );
        assert_eq!(
            overall(r#"{"x":1}"#, r#"{"x":1}"#, r#"{"x":2}"#),
            ConflictClass::Independent
        );
    }

    #[test]
    fn agreeing_changes_converge_and_disagreeing_ones_conflict() {
        assert_eq!(
            overall(r#"{"x":1}"#, r#"{"x":2}"#, r#"{"x":2}"#),
            ConflictClass::Convergent
        );
        assert_eq!(
            overall(r#"{"x":1}"#, r#"{"x":2}"#, r#"{"x":3}"#),
            ConflictClass::Conflict
        );
        // Both removing the member agree, because convergence compares the
        // change kind and the new value, and both are absent.
        assert_eq!(overall(r#"{"x":1}"#, "{}", "{}"), ConflictClass::Convergent);
        // Removing on one side and changing on the other does not, because the
        // kinds differ even though a merge tool might reconcile it.
        assert_eq!(
            overall(r#"{"x":1}"#, "{}", r#"{"x":2}"#),
            ConflictClass::Conflict
        );
    }

    #[test]
    fn an_ancestor_descendant_overlap_conflicts() {
        // Left replaces the whole of `o` while right edits `o.a`. The paths are
        // not equal, but one contains the other, and the two changes cannot
        // both be applied: left decides the value of `o` that right was editing
        // inside. Version 1 of this contract reported both paths as
        // `Independent`, which was unsafe as a basis for reconciliation.
        let output = classify_states(
            r#"{"o":{"a":1}}"#,
            r#"{"o":"replaced"}"#,
            r#"{"o":{"a":2}}"#,
        );
        assert_eq!(output.overall, ConflictClass::Conflict);
        assert_eq!(output.path_conflicts.len(), 2);
        assert!(output
            .path_conflicts
            .iter()
            .all(|entry| entry.classification == ConflictClass::Conflict));

        // Removing the parent is the same overlap.
        assert_eq!(
            overall(r#"{"o":{"a":1}}"#, "{}", r#"{"o":{"a":2}}"#),
            ConflictClass::Conflict
        );

        // Sibling paths under an untouched parent still do not overlap.
        assert_eq!(
            overall(
                r#"{"o":{"a":1,"b":1}}"#,
                r#"{"o":{"a":2,"b":1}}"#,
                r#"{"o":{"a":1,"b":2}}"#
            ),
            ConflictClass::Independent
        );
        // Nor do keys that merely share a string prefix.
        assert_eq!(
            overall(
                r#"{"a":{"b":1,"bc":1}}"#,
                r#"{"a":{"b":2,"bc":1}}"#,
                r#"{"a":{"b":1,"bc":2}}"#
            ),
            ConflictClass::Independent
        );
    }

    #[test]
    fn base_value_is_the_base_states_value_at_the_path() {
        // The base holds 1 at `x` and only the right branch changed it.
        // Version 1 read `base_value` from the left branch's change and so
        // reported `null` here, losing the ancestor's value exactly where a
        // reconciler would need it.
        let output = classify_states(r#"{"x":1}"#, r#"{"x":1}"#, r#"{"x":2}"#);
        let entry = &output.path_conflicts[0];
        assert_eq!(entry.base_value, Some(serde_json::json!(1)));
        assert_eq!(entry.left_value, None, "the left branch changed nothing");
        assert_eq!(entry.right_value, Some(serde_json::json!(2)));

        // A path the base does not hold has no base value.
        let added = classify_states("{}", r#"{"x":1}"#, "{}");
        assert_eq!(added.path_conflicts[0].base_value, None);

        // And it does not depend on which branch is called left.
        let forward = classify_states(r#"{"x":1}"#, r#"{"x":1}"#, r#"{"x":2}"#);
        let reversed = classify_states(r#"{"x":1}"#, r#"{"x":2}"#, r#"{"x":1}"#);
        assert_eq!(
            forward.path_conflicts[0].base_value,
            reversed.path_conflicts[0].base_value
        );
    }

    #[test]
    fn unparseable_content_classifies_as_json_null_rather_than_failing() {
        // `classify` parses with `unwrap_or(Value::Null)`. Both sides moving
        // from unparseable base to the same state therefore converge.
        let output = classify_states("not json at all", r#"{"x":1}"#, r#"{"x":1}"#);
        assert_eq!(output.overall, ConflictClass::Convergent);
        assert_eq!(output.path_conflicts[0].path, vec![]);

        // And it is a classification, not a contract error.
        let bytes =
            canonical_input_bytes(&StateConflictInput::new("{".into(), "}".into(), "]".into()));
        let parsed = evaluate_canonical_output(&bytes);
        assert!(parsed.error.is_none());
        assert!(parsed.classification.is_some());
    }

    #[test]
    fn unevaluatable_inputs_are_decisions_rather_than_failures() {
        for (bytes, code) in [
            (&b"not json"[..], ContractErrorCode::ContractInputInvalid),
            (
                br#"{"base":"{}","contract":"other","left":"{}","right":"{}","version":2}"#,
                ContractErrorCode::ContractIdMismatch,
            ),
            (
                br#"{"base":"{}","contract":"feltdb.state.conflict","left":"{}","right":"{}","version":9}"#,
                ContractErrorCode::ContractVersionUnsupported,
            ),
        ] {
            let output = evaluate_canonical_output(bytes);
            assert_eq!(output.error.map(|error| error.code), Some(code));
            assert!(output.classification.is_none());
        }
    }

    #[test]
    fn canonical_round_trip_is_stable() {
        let input = StateConflictInput::new(
            r#"{"x":1}"#.into(),
            r#"{"x":2}"#.into(),
            r#"{"x":3}"#.into(),
        );
        let once = canonical_input_bytes(&input);
        let parsed: StateConflictInput = serde_json::from_slice(&once).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(canonical_input_bytes(&parsed), once);
        assert_eq!(input_digest(&parsed), input_digest(&input));
    }

    #[test]
    fn the_adapter_is_the_existing_classifier() {
        // The adapter must be the classification FeltDB already computes, not a
        // second one that happens to agree.
        for (base, left, right) in [
            (r#"{"x":1}"#, r#"{"x":2}"#, r#"{"x":3}"#),
            (r#"{"a":1,"b":1}"#, r#"{"a":2,"b":1}"#, r#"{"a":1,"b":2}"#),
            ("[1,2]", "[9,2]", "[8,2]"),
        ] {
            assert_eq!(
                serde_json::to_string(&classify_states(base, left, right)).unwrap(),
                serde_json::to_string(&ConflictClassification::classify(
                    &revision(base),
                    &revision(left),
                    &revision(right)
                ))
                .unwrap()
            );
        }
    }
}
