//! The deterministic contract behind state-trigger matching.
//!
//! This module is the first explicit boundary in FeltDB between a *deterministic
//! semantic contract* and the *runtime* that executes it. It exists so the
//! question "did this committed change match this trigger?" can be answered
//! identically by more than one implementation.
//!
//! ```text
//!               canonical input bytes
//!                        │
//!         ┌──────────────┴──────────────┐
//!         │                             │
//!   native evaluation             WASM evaluation
//!         │                             │
//!         └──────────────┬──────────────┘
//!                        │
//!               canonical output bytes
//! ```
//!
//! The contract is *not* a plugin system. It defines one decision with one
//! shape, and FeltDB remains authoritative for everything around it: state,
//! committed changes, workload creation, idempotency, authorization,
//! durability, provenance and execution lifecycle. A contract implementation
//! returns a decision; it never mutates FeltDB.
//!
//! # Determinism budget
//!
//! Evaluation reads nothing but its input. It does not consult a clock, the
//! filesystem, the network, a random source, or any process-global mutable
//! state, and its output contains no values derived from any of those.
//!
//! # Relationship to production
//!
//! [`crate::state_trigger::StateTrigger::matches`] delegates to
//! [`evaluate_trigger`] on the typed native path. Canonical bytes are the
//! *wire* form used to carry the same input into another runtime; production
//! evaluation does not pay for encoding on every candidate pair.

use crate::operation::{Operation, OperationType};
use crate::state_trigger::{StateTrigger, StateTriggerChangeKind, StateTriggerPredicate};
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Stable identity of this contract. It is part of the input and the output so
/// that a decision can never be silently attributed to a different contract.
pub const TRIGGER_MATCH_CONTRACT_ID: &str = "feltdb.trigger.match";

/// Version of this contract's input shape, output shape and semantics.
///
/// Any change to the fields, the ordering of the match checks, or the reasons
/// reported is a new version. Implementations that disagree about the version
/// must not be compared for conformance.
pub const TRIGGER_MATCH_CONTRACT_VERSION: u32 = 1;

/// The kind of committed change, as the contract names it.
///
/// The contract keeps its own enum rather than reusing [`OperationType`] so the
/// wire form does not move when an unrelated Rust type is renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractChangeKind {
    Insert,
    Update,
    Delete,
}

impl ContractChangeKind {
    /// The contract's projection of a durable operation type.
    pub fn from_operation_type(value: &OperationType) -> Self {
        match value {
            OperationType::Insert => Self::Insert,
            OperationType::Update => Self::Update,
            OperationType::Delete => Self::Delete,
        }
    }

    /// The contract's projection of a trigger's declared change kind.
    pub fn from_trigger_change_kind(value: StateTriggerChangeKind) -> Self {
        match value {
            StateTriggerChangeKind::Insert => Self::Insert,
            StateTriggerChangeKind::Update => Self::Update,
            StateTriggerChangeKind::Delete => Self::Delete,
        }
    }
}

/// The part of a committed change the decision actually depends on.
///
/// The audit that produced this contract established that trigger matching
/// reads exactly three things from an [`Operation`]: its capability
/// (collection), its operation type, and its value. `op_id`, `instance_id`,
/// `sequence`, `timestamp_ms`, `rust_type`, `content_hash` and `vector_clock`
/// are deliberately absent — including them would put a clock reading and a
/// node identity inside a contract that must be reproducible anywhere.
///
/// Fields are declared in alphabetical order because the canonical encoding is
/// declaration order for structs and sorted order for JSON objects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractChange {
    pub change_kind: ContractChangeKind,
    pub collection: String,
    /// The committed record value, when the change carried one.
    ///
    /// Absence and `null` are different: a delete carries no value at all,
    /// while an insert of a JSON `null` carries one. The canonical encoding
    /// distinguishes them by whether the `value` key is present, so a
    /// whole-record predicate (`field: ""`) against `null` is decidable.
    #[serde(
        default,
        deserialize_with = "deserialize_present",
        skip_serializing_if = "Option::is_none"
    )]
    pub value: Option<Value>,
}

impl ContractChange {
    /// Projects a committed operation onto the contract's input.
    pub fn from_operation(operation: &Operation) -> Self {
        Self {
            change_kind: ContractChangeKind::from_operation_type(&operation.op_type),
            collection: operation.capability.clone(),
            value: operation.value.clone(),
        }
    }
}

/// The part of a trigger definition the decision actually depends on.
///
/// A [`StateTrigger`] also carries tenancy, authorization snapshots, retry
/// policy and scheduling. None of it participates in matching, and none of it
/// belongs in a contract that a foreign runtime evaluates: an authorization
/// snapshot handed to a replaceable implementation would be a second source of
/// truth for authorization. FeltDB keeps all of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractTrigger {
    pub change_kind: ContractChangeKind,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicate: Option<ContractPredicate>,
    pub source_collection: String,
}

impl ContractTrigger {
    /// Projects a durable trigger definition onto the contract's input.
    pub fn from_state_trigger(trigger: &StateTrigger) -> Self {
        Self {
            change_kind: ContractChangeKind::from_trigger_change_kind(trigger.change_kind),
            enabled: trigger.enabled,
            predicate: trigger.predicate.as_ref().map(ContractPredicate::from),
            source_collection: trigger.source_collection.clone(),
        }
    }
}

/// The deterministic equality predicate the state-trigger evaluator already had.
///
/// `field` is a dot-separated path read through JSON objects only; `equals` is
/// compared with JSON value equality, which distinguishes integers from floats
/// exactly as `serde_json` does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractPredicate {
    pub equals: Value,
    pub field: String,
}

impl From<&StateTriggerPredicate> for ContractPredicate {
    fn from(value: &StateTriggerPredicate) -> Self {
        Self {
            equals: value.equals.clone(),
            field: value.field.clone(),
        }
    }
}

/// The complete deterministic input to one trigger-match decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerMatchInput {
    pub change: ContractChange,
    pub contract: String,
    pub trigger: ContractTrigger,
    pub version: u32,
}

impl TriggerMatchInput {
    /// Builds an input stamped with this contract's identity and version.
    pub fn new(change: ContractChange, trigger: ContractTrigger) -> Self {
        Self {
            change,
            contract: TRIGGER_MATCH_CONTRACT_ID.to_string(),
            trigger,
            version: TRIGGER_MATCH_CONTRACT_VERSION,
        }
    }

    /// The input for deciding whether `trigger` matches `operation`.
    pub fn for_operation(trigger: &StateTrigger, operation: &Operation) -> Self {
        Self::new(
            ContractChange::from_operation(operation),
            ContractTrigger::from_state_trigger(trigger),
        )
    }
}

/// Why the decision came out the way it did.
///
/// The reason is part of the contract, not a diagnostic afterthought: it fixes
/// the order in which the checks are applied, so two implementations cannot
/// agree on `matched` by accident while disagreeing about the semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchReason {
    /// Every check passed.
    Matched,
    /// The trigger is disabled.
    TriggerDisabled,
    /// The change's collection is not the trigger's source collection.
    CollectionMismatch,
    /// The change kind is not the trigger's change kind.
    ChangeKindMismatch,
    /// A predicate is declared but the change carried no value.
    ChangeValueAbsent,
    /// The predicate's field path does not resolve in the change value.
    PredicateFieldAbsent,
    /// The predicate's field resolved to a value that is not `equals`.
    PredicateValueMismatch,
}

/// The decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchOutcome {
    pub matched: bool,
    pub reason: MatchReason,
}

/// Why an input could not be evaluated.
///
/// Error identity is a stable code and nothing else. A free-form message would
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

/// The complete deterministic output of one trigger-match decision.
///
/// Exactly one of `error` and `outcome` is present. Both carry the evaluating
/// implementation's own contract identity and version, never the input's, so a
/// mismatch cannot be laundered back into agreement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriggerMatchOutput {
    pub contract: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ContractErrorBody>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<MatchOutcome>,
    pub version: u32,
}

impl TriggerMatchOutput {
    fn decision(matched: bool, reason: MatchReason) -> Self {
        Self {
            contract: TRIGGER_MATCH_CONTRACT_ID.to_string(),
            error: None,
            outcome: Some(MatchOutcome { matched, reason }),
            version: TRIGGER_MATCH_CONTRACT_VERSION,
        }
    }

    fn failure(code: ContractErrorCode) -> Self {
        Self {
            contract: TRIGGER_MATCH_CONTRACT_ID.to_string(),
            error: Some(ContractErrorBody { code }),
            outcome: None,
            version: TRIGGER_MATCH_CONTRACT_VERSION,
        }
    }

    /// Whether the trigger matched. An unevaluatable input never matches.
    pub fn matched(&self) -> bool {
        self.outcome.is_some_and(|outcome| outcome.matched)
    }
}

/// The deterministic contract, natively implemented.
///
/// This is the whole semantic authority for state-trigger matching. The checks
/// run in a fixed order and the first failing one names the reason, which is
/// the same short-circuit order the state-trigger evaluator has always used.
pub fn evaluate_trigger(input: &TriggerMatchInput) -> TriggerMatchOutput {
    let change = &input.change;
    let trigger = &input.trigger;

    if !trigger.enabled {
        return TriggerMatchOutput::decision(false, MatchReason::TriggerDisabled);
    }
    if change.collection != trigger.source_collection {
        return TriggerMatchOutput::decision(false, MatchReason::CollectionMismatch);
    }
    if change.change_kind != trigger.change_kind {
        return TriggerMatchOutput::decision(false, MatchReason::ChangeKindMismatch);
    }
    let Some(predicate) = trigger.predicate.as_ref() else {
        return TriggerMatchOutput::decision(true, MatchReason::Matched);
    };
    let Some(value) = change.value.as_ref() else {
        return TriggerMatchOutput::decision(false, MatchReason::ChangeValueAbsent);
    };
    let Some(resolved) = resolve_field(value, &predicate.field) else {
        return TriggerMatchOutput::decision(false, MatchReason::PredicateFieldAbsent);
    };
    if resolved != &predicate.equals {
        return TriggerMatchOutput::decision(false, MatchReason::PredicateValueMismatch);
    }
    TriggerMatchOutput::decision(true, MatchReason::Matched)
}

/// Reads a dot-separated field path, descending through JSON objects only.
///
/// An empty path is the whole value. Array elements are not addressable, which
/// is the behaviour the state-trigger evaluator shipped with: `Value::get` with
/// a string key returns nothing for an array.
fn resolve_field<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(value);
    }
    let mut current = value;
    for segment in path.split('.') {
        current = current.as_object()?.get(segment)?;
    }
    Some(current)
}

/// The canonical byte encoding of a contract input.
///
/// FeltDB already canonicalizes by serializing sorted structures with
/// `serde_json` (see [`crate::application::canonical_bytes`] and
/// [`crate::state_contract::canonical_schema_bytes`]). This contract uses the
/// same convention: `serde_json::Map` is a `BTreeMap`, so every JSON object is
/// emitted in sorted key order, and contract structs declare their fields in
/// alphabetical order so struct encoding matches object encoding.
pub fn canonical_input_bytes(input: &TriggerMatchInput) -> Vec<u8> {
    serde_json::to_vec(input).expect("contract input is always serializable")
}

/// The canonical byte encoding of a contract output.
pub fn canonical_output_bytes(output: &TriggerMatchOutput) -> Vec<u8> {
    serde_json::to_vec(output).expect("contract output is always serializable")
}

/// Content identity of a contract input, for provenance.
pub fn input_digest(input: &TriggerMatchInput) -> String {
    format!("sha256:{:x}", Sha256::digest(canonical_input_bytes(input)))
}

/// The runtime that executed a contract, recorded in evidence.
///
/// Only [`ContractRuntime::Native`] is wired into production. The variant for
/// WASM exists so that recorded provenance is honest about which
/// implementation produced a decision if that ever changes; it is not a
/// selection switch, and nothing in this crate constructs it.
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

/// The runtime-independent entry point: canonical bytes in, canonical bytes out.
///
/// This is the exact shape a foreign runtime implements. It never panics and
/// never returns anything but a canonical output encoding, so a malformed input
/// is a contract *decision* rather than a host-specific failure mode.
pub fn evaluate_canonical(input: &[u8]) -> Vec<u8> {
    canonical_output_bytes(&evaluate_canonical_output(input))
}

/// [`evaluate_canonical`] before encoding, for callers that want the value.
pub fn evaluate_canonical_output(input: &[u8]) -> TriggerMatchOutput {
    let Ok(parsed) = serde_json::from_slice::<TriggerMatchInput>(input) else {
        return TriggerMatchOutput::failure(ContractErrorCode::ContractInputInvalid);
    };
    if parsed.contract != TRIGGER_MATCH_CONTRACT_ID {
        return TriggerMatchOutput::failure(ContractErrorCode::ContractIdMismatch);
    }
    if parsed.version != TRIGGER_MATCH_CONTRACT_VERSION {
        return TriggerMatchOutput::failure(ContractErrorCode::ContractVersionUnsupported);
    }
    evaluate_trigger(&parsed)
}

/// Deserializes a field so that a present `null` is `Some(Value::Null)`.
///
/// `Option<Value>` would collapse `"value": null` to `None`, erasing the
/// difference between "the change carried no record" and "the change carried
/// JSON null". Key presence is the distinction, and it is one a hand-written
/// parser in another runtime can reproduce without knowing anything about serde.
fn deserialize_present<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::Operation;
    use serde_json::json;

    fn change(kind: ContractChangeKind, collection: &str, value: Option<Value>) -> ContractChange {
        ContractChange {
            change_kind: kind,
            collection: collection.into(),
            value,
        }
    }

    fn trigger(predicate: Option<ContractPredicate>) -> ContractTrigger {
        ContractTrigger {
            change_kind: ContractChangeKind::Insert,
            enabled: true,
            predicate,
            source_collection: "orders".into(),
        }
    }

    fn decide(change: ContractChange, trigger: ContractTrigger) -> (bool, MatchReason) {
        let output = evaluate_trigger(&TriggerMatchInput::new(change, trigger));
        let outcome = output.outcome.expect("a typed input always decides");
        (outcome.matched, outcome.reason)
    }

    #[test]
    fn checks_run_in_a_fixed_order() {
        // Every check fails at once. The reported reason names the first, which
        // is what makes the reason comparable between implementations.
        let mut disabled = trigger(Some(ContractPredicate {
            equals: json!("paid"),
            field: "status".into(),
        }));
        disabled.enabled = false;
        assert_eq!(
            decide(
                change(ContractChangeKind::Delete, "customers", None),
                disabled
            ),
            (false, MatchReason::TriggerDisabled)
        );

        assert_eq!(
            decide(
                change(ContractChangeKind::Delete, "customers", None),
                trigger(None)
            ),
            (false, MatchReason::CollectionMismatch)
        );

        assert_eq!(
            decide(
                change(ContractChangeKind::Delete, "orders", None),
                trigger(None)
            ),
            (false, MatchReason::ChangeKindMismatch)
        );
    }

    #[test]
    fn absent_and_null_record_values_are_different_inputs() {
        let whole_record = || {
            Some(ContractPredicate {
                equals: Value::Null,
                field: String::new(),
            })
        };
        assert_eq!(
            decide(
                change(ContractChangeKind::Insert, "orders", Some(Value::Null)),
                trigger(whole_record())
            ),
            (true, MatchReason::Matched)
        );
        assert_eq!(
            decide(
                change(ContractChangeKind::Insert, "orders", None),
                trigger(whole_record())
            ),
            (false, MatchReason::ChangeValueAbsent)
        );
    }

    #[test]
    fn a_missing_field_is_not_a_null_field() {
        let expects_null = || {
            Some(ContractPredicate {
                equals: Value::Null,
                field: "cancelled_at".into(),
            })
        };
        assert_eq!(
            decide(
                change(
                    ContractChangeKind::Insert,
                    "orders",
                    Some(json!({"cancelled_at": null}))
                ),
                trigger(expects_null())
            ),
            (true, MatchReason::Matched)
        );
        assert_eq!(
            decide(
                change(ContractChangeKind::Insert, "orders", Some(json!({}))),
                trigger(expects_null())
            ),
            (false, MatchReason::PredicateFieldAbsent)
        );
    }

    #[test]
    fn field_paths_descend_through_objects_only() {
        let value = json!({"customer": {"country": "NL"}, "tags": ["a"]});
        assert_eq!(
            resolve_field(&value, "customer.country"),
            Some(&json!("NL"))
        );
        assert_eq!(resolve_field(&value, ""), Some(&value));
        assert_eq!(resolve_field(&value, "tags.0"), None);
        assert_eq!(resolve_field(&value, "customer.missing"), None);
    }

    #[test]
    fn canonical_encoding_sorts_members() {
        let bytes = canonical_input_bytes(&TriggerMatchInput::new(
            change(
                ContractChangeKind::Insert,
                "orders",
                Some(json!({"zeta": 1, "alpha": 2})),
            ),
            trigger(None),
        ));
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            r#"{"change":{"change_kind":"insert","collection":"orders","value":{"alpha":2,"zeta":1}},"contract":"feltdb.trigger.match","trigger":{"change_kind":"insert","enabled":true,"source_collection":"orders"},"version":1}"#
        );
    }

    #[test]
    fn canonical_encoding_omits_an_absent_value_and_keeps_a_null_one() {
        let with_null = canonical_input_bytes(&TriggerMatchInput::new(
            change(ContractChangeKind::Insert, "orders", Some(Value::Null)),
            trigger(None),
        ));
        assert!(String::from_utf8_lossy(&with_null).contains(r#""value":null"#));

        let without = canonical_input_bytes(&TriggerMatchInput::new(
            change(ContractChangeKind::Delete, "orders", None),
            trigger(None),
        ));
        assert!(!String::from_utf8_lossy(&without).contains("value"));
    }

    #[test]
    fn canonical_round_trip_is_stable() {
        let input = TriggerMatchInput::new(
            change(
                ContractChangeKind::Insert,
                "orders",
                Some(json!({"status": "paid", "total": 42})),
            ),
            trigger(Some(ContractPredicate {
                equals: json!("paid"),
                field: "status".into(),
            })),
        );
        let once = canonical_input_bytes(&input);
        let parsed: TriggerMatchInput = serde_json::from_slice(&once).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(canonical_input_bytes(&parsed), once);
        assert_eq!(input_digest(&parsed), input_digest(&input));
    }

    #[test]
    fn unevaluatable_inputs_are_decisions_rather_than_failures() {
        for (bytes, code) in [
            (
                &b"not json"[..],
                ContractErrorCode::ContractInputInvalid,
            ),
            (
                br#"{"change":{"change_kind":"insert","collection":"o"},"contract":"other","trigger":{"change_kind":"insert","enabled":true,"source_collection":"o"},"version":1}"#,
                ContractErrorCode::ContractIdMismatch,
            ),
            (
                br#"{"change":{"change_kind":"insert","collection":"o"},"contract":"feltdb.trigger.match","trigger":{"change_kind":"insert","enabled":true,"source_collection":"o"},"version":9}"#,
                ContractErrorCode::ContractVersionUnsupported,
            ),
        ] {
            let output = evaluate_canonical_output(bytes);
            assert_eq!(output.error.map(|error| error.code), Some(code));
            assert!(!output.matched(), "an unevaluatable input never matches");
        }
    }

    #[test]
    fn the_contract_reads_only_what_the_decision_depends_on() {
        // Two operations that differ in every field the contract does not
        // project must produce identical canonical input. If a clock reading,
        // a node identity or a content hash ever leaked into the contract, the
        // decision would stop being reproducible anywhere and this would fail.
        let one = Operation::insert(
            1,
            "node-a".into(),
            1,
            "orders:1".into(),
            json!({"status": "paid"}),
            "Order".into(),
            "orders".into(),
        );
        let mut two = Operation::insert(
            999,
            "node-b".into(),
            77,
            "orders:2".into(),
            json!({"status": "paid"}),
            "OtherOrder".into(),
            "orders".into(),
        );
        two.timestamp_ms = one.timestamp_ms + 1_000_000;
        two.content_hash = "different".into();

        assert_eq!(
            ContractChange::from_operation(&one),
            ContractChange::from_operation(&two)
        );
    }

    #[test]
    fn the_native_contract_decides_production_matching() {
        // The production entry point and the contract are the same decision,
        // not two implementations that happen to agree today.
        use crate::authorization::{
            Grant, GrantConstraints, GrantSigner, GrantStatus, ResourceScope, ResourceScopeKind,
            Subject,
        };
        use crate::state_trigger::{StateTrigger, StateTriggerChangeKind, StateTriggerPredicate};
        use std::collections::BTreeSet;

        let subject = Subject::Agent("a".into());
        let signed = GrantSigner::new("k", vec![7; 32])
            .unwrap()
            .sign(Grant {
                contract_version: 1,
                id: "g".into(),
                issuer: Subject::Human("o".into()),
                subject: subject.clone(),
                tenant_id: "t".into(),
                application_id: "app".into(),
                capabilities: ["workload:execute".into()].into(),
                resources: vec![ResourceScope {
                    kind: ResourceScopeKind::Workload,
                    uri: "flow://app/workloads/*".into(),
                }],
                constraints: GrantConstraints::default(),
                issued_at: 1,
                expires_at: 9999,
                parent_grant: None,
                revision: 1,
                status: GrantStatus::Active,
            })
            .unwrap();
        let state_trigger = StateTrigger {
            trigger_id: "t1".into(),
            source_collection: "orders".into(),
            change_kind: StateTriggerChangeKind::Insert,
            predicate: Some(StateTriggerPredicate {
                field: "status".into(),
                equals: json!("paid"),
            }),
            target_capability: "identity".into(),
            tenant_id: "t".into(),
            application_id: "app".into(),
            environment: "production".into(),
            definition_revision: "r".into(),
            requested_by: subject,
            authorization_snapshot: signed,
            capability_snapshot: BTreeSet::new(),
            connection_scopes: BTreeSet::new(),
            priority: 0,
            retry_policy: Default::default(),
            timeouts: Default::default(),
            not_before: None,
            enabled: true,
        };

        for (value, expected) in [
            (json!({"status": "paid"}), true),
            (json!({"status": "draft"}), false),
        ] {
            let operation = Operation::insert(
                1,
                "node".into(),
                1,
                "orders:1".into(),
                value,
                "Order".into(),
                "orders".into(),
            );
            assert_eq!(state_trigger.matches(&operation), expected);
            assert_eq!(
                evaluate_trigger(&state_trigger.contract_input(&operation)).matched(),
                expected
            );
            // And through the wire form, which is what a foreign runtime sees.
            let bytes = canonical_input_bytes(&state_trigger.contract_input(&operation));
            assert_eq!(evaluate_canonical_output(&bytes).matched(), expected);
        }
    }
}
