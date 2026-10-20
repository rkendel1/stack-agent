//! The deterministic contract behind durable agent-state transitions.
//!
//! ```text
//!     canonical input  ->  deterministic decision  ->  canonical output
//! ```
//!
//! The decision is: *given these records, at this authority time, may this
//! operation proceed, and if so which writes and preconditions make it so?*
//!
//! The contract is a **decision, not a transaction executor**. It reads nothing
//! but its input, never consults a clock (the authority's time arrives as
//! `now_ms`), never reads storage and never commits anything. A caller that
//! wants the decision to become real must
//!
//! 1. read the records the operation names, with their versions;
//! 2. obtain the authority's time and pass it as `now_ms`;
//! 3. call [`evaluate`];
//! 4. commit the returned writes and guards through one guarded transaction,
//!    each write carrying the precondition the contract attached to it; and
//! 5. report success only if that commit succeeded.
//!
//! A precondition that fails is a lost race, not a reason to resend the same
//! writes: the caller re-reads and decides again. [`crate::agent_state`] is the
//! production implementation of those five steps.
//!
//! # What the contract enforces, and what it cannot
//!
//! It enforces lifecycle legality, ownership, fencing, lease expiry, evidence
//! preconditions and the uncertain-outcome rules. It does **not** verify the
//! external world. Evidence arrives as a list of *observations* that FeltDB
//! itself made when it read the cited records; the contract requires every
//! cited record to have been observed and pins the observed version with a
//! guard, so the citation cannot be committed against a record that has
//! changed. A caller cannot assert that something was verified: there is no
//! such field in the input.
//!
//! Record identities, version-numbering and lease predicates are defined by the
//! existing guarded-transaction primitive; this module only names them.

use serde::de::{DeserializeOwned, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

pub const AGENT_TRANSITION_CONTRACT_ID: &str = "feltdb.agent.transition";
pub const AGENT_TRANSITION_CONTRACT_VERSION: u32 = 1;

pub const TASK_COLLECTION: &str = "_feltdb.AgentTask";
pub const CHECKPOINT_COLLECTION: &str = "_feltdb.AgentCheckpoint";
pub const HANDOFF_COLLECTION: &str = "_feltdb.AgentHandoff";
pub const INTENT_COLLECTION: &str = "_feltdb.AgentIntent";

/// Identifiers (task, actor, lease, handoff, idempotency key): 1..=128 bytes of
/// `[A-Za-z0-9_.:@-]`. The composite record ids use `~` as a separator, which
/// is outside this set, so two different (task, key) pairs cannot collide.
pub const MAX_IDENTIFIER_BYTES: usize = 128;
pub const MAX_TEXT_BYTES: usize = 1024;
pub const MAX_EVIDENCE: usize = 16;
pub const MAX_LEASE_MS: u64 = 86_400_000;
/// Evidence collections beginning with this prefix are the agent's own
/// records. A claim cannot be evidence for itself.
pub const AGENT_COLLECTION_PREFIX: &str = "_feltdb.Agent";

/// Why a decision is a refusal. Codes carry no message on purpose: a free-form
/// string would be a divergence surface that says nothing about semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    ContractInputInvalid,
    ContractIdMismatch,
    ContractVersionUnsupported,
    NotFound,
    AlreadyExists,
    InvalidTransition,
    Terminal,
    FenceStale,
    OwnershipConflict,
    LeaseMismatch,
    LeaseExpired,
    LeaseHeld,
    VersionConflict,
    EvidenceInvalid,
    EvidenceRequired,
    OutcomeUncertain,
    IntentUnknown,
    IntentResolved,
    HandoffClaimed,
    HandoffStale,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContractInputInvalid => "CONTRACT_INPUT_INVALID",
            Self::ContractIdMismatch => "CONTRACT_ID_MISMATCH",
            Self::ContractVersionUnsupported => "CONTRACT_VERSION_UNSUPPORTED",
            Self::NotFound => "NOT_FOUND",
            Self::AlreadyExists => "ALREADY_EXISTS",
            Self::InvalidTransition => "INVALID_TRANSITION",
            Self::Terminal => "TERMINAL",
            Self::FenceStale => "FENCE_STALE",
            Self::OwnershipConflict => "OWNERSHIP_CONFLICT",
            Self::LeaseMismatch => "LEASE_MISMATCH",
            Self::LeaseExpired => "LEASE_EXPIRED",
            Self::LeaseHeld => "LEASE_HELD",
            Self::VersionConflict => "VERSION_CONFLICT",
            Self::EvidenceInvalid => "EVIDENCE_INVALID",
            Self::EvidenceRequired => "EVIDENCE_REQUIRED",
            Self::OutcomeUncertain => "OUTCOME_UNCERTAIN",
            Self::IntentUnknown => "INTENT_UNKNOWN",
            Self::IntentResolved => "INTENT_RESOLVED",
            Self::HandoffClaimed => "HANDOFF_CLAIMED",
            Self::HandoffStale => "HANDOFF_STALE",
        }
    }
}

use ErrorCode as E;

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Deserializes a member that, when present, must not be `null`.
fn strict<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    T::deserialize(deserializer).map(Some)
}

/// Deserializes a member that must be present and may be `null`.
fn nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTransitionInput {
    pub contract: String,
    pub now_ms: u64,
    pub operation: Operation,
    pub records: Records,
    pub version: u32,
}

/// Every member any operation may carry. Which are required and which are
/// forbidden is a property of `op`, checked in [`Operation::validate`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub op: String,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub action: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub actor: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub claimant: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub conclusion: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub evidence: Option<Vec<EvidenceRef>>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub expected_version: Option<u64>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub fence: Option<u64>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub handoff_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub idempotency_key: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub lease_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub lease_ms: Option<u64>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub objective: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub result: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub status: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub summary: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub task_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub to: Option<String>,
}

/// A caller's citation. It says what is cited, never that it was verified.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    pub kind: String,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub collection: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub id: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub uri: Option<String>,
}

/// The records an operation was decided against. A key is present exactly when
/// the record exists; the operation determines which keys are legal.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Records {
    /// What FeltDB itself observed when it read each cited record. Never
    /// supplied by the caller of an operation.
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub evidence: Option<Vec<Observation>>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub handoff: Option<Stored<HandoffValue>>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub intent: Option<Stored<IntentValue>>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub task: Option<Stored<TaskValue>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub collection: String,
    pub id: String,
    pub version: u64,
}

/// A stored record: its value and the authority's version of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stored<T> {
    pub value: T,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    /// `expiresAt` and `leaseId` keep the spelling the guarded-transaction
    /// primitive reads, so its lease predicate applies to these records.
    #[serde(rename = "expiresAt")]
    pub expires_at: u64,
    #[serde(rename = "leaseId")]
    pub lease_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskValue {
    pub checkpoint_version: u64,
    pub created_at_ms: u64,
    pub fence: u64,
    #[serde(deserialize_with = "nullable")]
    pub lease: Option<Lease>,
    pub objective: String,
    pub owner: String,
    pub status: String,
    pub task_id: String,
    pub unresolved_intents: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffValue {
    pub checkpoint_version: u64,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub claimant: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub claimed_fence: Option<u64>,
    pub from: String,
    pub from_fence: u64,
    pub handoff_id: String,
    pub status: String,
    pub task_id: String,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub to: Option<String>,
}

/// A citation as stored: what was cited and whether FeltDB observed it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Citation {
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub collection: Option<String>,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub id: Option<String>,
    pub kind: String,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub uri: Option<String>,
    pub verified: bool,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub version: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentValue {
    pub action: String,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub conclusion: Option<String>,
    pub evidence: Vec<Citation>,
    pub intended_at_ms: u64,
    pub intended_fence: u64,
    pub key: String,
    #[serde(
        default,
        deserialize_with = "strict",
        skip_serializing_if = "Option::is_none"
    )]
    pub result: Option<String>,
    pub state: String,
    pub task_id: String,
    pub verified: bool,
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// What a committed write must find in the record it replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// The record must not exist.
    Absent,
    /// The record must be at `version`, and when `lease_id` is present it must
    /// also hold that lease, unexpired, at commit time on the authority.
    Version {
        version: u64,
        lease_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Write {
    pub collection: &'static str,
    pub id: String,
    pub expect: Expect,
    pub value: Value,
}

/// A record the decision depended on but does not change. It must still be at
/// `version` (and hold `lease_id`, when present) at commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guard {
    pub collection: String,
    pub id: String,
    pub version: u64,
    pub lease_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Apply these writes and guards atomically, or none of them.
    Commit {
        writes: Vec<Write>,
        guards: Vec<Guard>,
    },
    /// The operation is a recognized repeat and changes nothing.
    Noop,
}

type Outcome = Result<Decision, ErrorCode>;

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Canonical bytes in, canonical bytes out. Every input, including a malformed
/// one, yields a valid output: an unusable input is a decision, never a trap.
pub fn evaluate_canonical(input: &[u8]) -> Vec<u8> {
    encode(&evaluate_bytes(input))
}

/// Parses `input` and decides.
pub fn evaluate_bytes(input: &[u8]) -> Outcome {
    let parsed: AgentTransitionInput =
        serde_json::from_slice(input).map_err(|_| E::ContractInputInvalid)?;
    evaluate(&parsed)
}

/// The decision itself. Shape before identity, so an input that is both
/// malformed and misaddressed reports the same thing on every implementation.
pub fn evaluate(input: &AgentTransitionInput) -> Outcome {
    input.operation.validate()?;
    input.records.validate_for(&input.operation.op)?;
    if input.contract != AGENT_TRANSITION_CONTRACT_ID {
        return Err(E::ContractIdMismatch);
    }
    if input.version != AGENT_TRANSITION_CONTRACT_VERSION {
        return Err(E::ContractVersionUnsupported);
    }
    let now = input.now_ms;
    let op = &input.operation;
    let records = &input.records;
    match op.op.as_str() {
        "create_task" => create_task(op, records, now),
        "acquire" => acquire(op, records, now),
        "renew" => renew(op, records, now),
        "checkpoint" => checkpoint(op, records, now),
        "record_intent" => record_intent(op, records, now),
        "record_outcome" => record_outcome(op, records, now),
        "reconcile" => reconcile(op, records, now),
        "create_handoff" => create_handoff(op, records, now),
        "claim_handoff" => claim_handoff(op, records, now),
        "accept_handoff" => accept_handoff(op, records, now),
        _ => Err(E::ContractInputInvalid),
    }
}

// ---------------------------------------------------------------------------
// Shape validation
// ---------------------------------------------------------------------------

fn identifier_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'@' | b'-'))
}

fn text_ok(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_TEXT_BYTES
}

const STATUSES: [&str; 5] = ["ready", "running", "blocked", "completed", "cancelled"];

fn terminal(status: &str) -> bool {
    matches!(status, "completed" | "cancelled")
}

impl Operation {
    /// Required and optional members per operation. Everything else is
    /// forbidden: an operation carries exactly the members it is defined by.
    fn members(&self) -> Option<(&'static [&'static str], &'static [&'static str])> {
        Some(match self.op.as_str() {
            "create_task" => (
                &["task_id", "actor", "objective", "lease_id", "lease_ms"],
                &[],
            ),
            "acquire" => (&["task_id", "actor", "lease_id", "lease_ms"], &[]),
            "renew" => (&["task_id", "actor", "fence", "lease_id", "lease_ms"], &[]),
            "checkpoint" => (
                &[
                    "task_id",
                    "actor",
                    "fence",
                    "lease_id",
                    "expected_version",
                    "summary",
                    "evidence",
                ],
                &["status"],
            ),
            "record_intent" => (
                &[
                    "task_id",
                    "actor",
                    "fence",
                    "lease_id",
                    "idempotency_key",
                    "action",
                ],
                &[],
            ),
            "record_outcome" => (
                &[
                    "task_id",
                    "actor",
                    "fence",
                    "lease_id",
                    "idempotency_key",
                    "result",
                    "evidence",
                ],
                &[],
            ),
            "reconcile" => (
                &[
                    "task_id",
                    "actor",
                    "fence",
                    "lease_id",
                    "idempotency_key",
                    "conclusion",
                    "evidence",
                ],
                &[],
            ),
            "create_handoff" => (
                &["task_id", "handoff_id", "actor", "fence", "lease_id"],
                &["to"],
            ),
            "claim_handoff" => (&["handoff_id", "claimant", "lease_id", "lease_ms"], &[]),
            "accept_handoff" => (&["handoff_id", "claimant", "fence", "lease_id"], &[]),
            _ => return None,
        })
    }

    fn present(&self, name: &str) -> bool {
        match name {
            "action" => self.action.is_some(),
            "actor" => self.actor.is_some(),
            "claimant" => self.claimant.is_some(),
            "conclusion" => self.conclusion.is_some(),
            "evidence" => self.evidence.is_some(),
            "expected_version" => self.expected_version.is_some(),
            "fence" => self.fence.is_some(),
            "handoff_id" => self.handoff_id.is_some(),
            "idempotency_key" => self.idempotency_key.is_some(),
            "lease_id" => self.lease_id.is_some(),
            "lease_ms" => self.lease_ms.is_some(),
            "objective" => self.objective.is_some(),
            "result" => self.result.is_some(),
            "status" => self.status.is_some(),
            "summary" => self.summary.is_some(),
            "task_id" => self.task_id.is_some(),
            "to" => self.to.is_some(),
            _ => false,
        }
    }

    fn validate(&self) -> Result<(), ErrorCode> {
        let (required, optional) = self.members().ok_or(E::ContractInputInvalid)?;
        const ALL: [&str; 17] = [
            "action",
            "actor",
            "claimant",
            "conclusion",
            "evidence",
            "expected_version",
            "fence",
            "handoff_id",
            "idempotency_key",
            "lease_id",
            "lease_ms",
            "objective",
            "result",
            "status",
            "summary",
            "task_id",
            "to",
        ];
        for name in ALL {
            let allowed = required.contains(&name) || optional.contains(&name);
            if self.present(name) && !allowed {
                return Err(E::ContractInputInvalid);
            }
            if required.contains(&name) && !self.present(name) {
                return Err(E::ContractInputInvalid);
            }
        }
        for id in [
            &self.actor,
            &self.claimant,
            &self.handoff_id,
            &self.idempotency_key,
            &self.lease_id,
            &self.task_id,
            &self.to,
        ]
        .into_iter()
        .flatten()
        {
            if !identifier_ok(id) {
                return Err(E::ContractInputInvalid);
            }
        }
        for t in [&self.action, &self.objective, &self.summary]
            .into_iter()
            .flatten()
        {
            if !text_ok(t) {
                return Err(E::ContractInputInvalid);
            }
        }
        if self.fence == Some(0) || self.expected_version == Some(0) {
            return Err(E::ContractInputInvalid);
        }
        if let Some(ms) = self.lease_ms {
            if ms == 0 || ms > MAX_LEASE_MS {
                return Err(E::ContractInputInvalid);
            }
        }
        if let Some(status) = &self.status {
            if !STATUSES.contains(&status.as_str()) {
                return Err(E::ContractInputInvalid);
            }
        }
        if let Some(result) = &self.result {
            if !matches!(result.as_str(), "succeeded" | "failed") {
                return Err(E::ContractInputInvalid);
            }
        }
        if let Some(conclusion) = &self.conclusion {
            if !matches!(conclusion.as_str(), "executed" | "not_executed") {
                return Err(E::ContractInputInvalid);
            }
        }
        if let Some(evidence) = &self.evidence {
            if evidence.len() > MAX_EVIDENCE {
                return Err(E::ContractInputInvalid);
            }
            for reference in evidence {
                reference.validate()?;
            }
        }
        Ok(())
    }
}

impl EvidenceRef {
    /// Structure only. Whether a cited record exists is decided later, against
    /// the observations; whether the citation is admissible, later still.
    fn validate(&self) -> Result<(), ErrorCode> {
        let bounded =
            |s: &Option<String>| s.as_ref().is_some_and(|s| !s.is_empty() && s.len() <= 256);
        match self.kind.as_str() {
            "record" if bounded(&self.collection) && bounded(&self.id) && self.uri.is_none() => {
                Ok(())
            }
            "external"
                if self.uri.as_ref().is_some_and(|u| text_ok(u))
                    && self.collection.is_none()
                    && self.id.is_none() =>
            {
                Ok(())
            }
            _ => Err(E::ContractInputInvalid),
        }
    }
}

impl Records {
    fn validate_for(&self, op: &str) -> Result<(), ErrorCode> {
        // Which record keys an operation may carry. A key is present exactly
        // when the record exists, so absence of a record is a decision
        // (`NOT_FOUND`, `INTENT_UNKNOWN`), never a malformed input. Evidence
        // observations are a required list, possibly empty, for the operations
        // that cite evidence.
        let (task, handoff, intent, evidence) = match op {
            "create_task" | "acquire" | "renew" => (true, false, false, false),
            "checkpoint" => (true, false, false, true),
            "record_intent" => (true, false, true, false),
            "record_outcome" | "reconcile" => (true, false, true, true),
            "create_handoff" => (true, true, false, false),
            "claim_handoff" | "accept_handoff" => (true, true, false, false),
            _ => return Err(E::ContractInputInvalid),
        };
        if (!task && self.task.is_some())
            || (!handoff && self.handoff.is_some())
            || (!intent && self.intent.is_some())
            || (evidence != self.evidence.is_some())
        {
            return Err(E::ContractInputInvalid);
        }
        if let Some(stored) = &self.task {
            stored.value.validate()?;
        }
        if let Some(stored) = &self.handoff {
            let h = &stored.value;
            if !matches!(h.status.as_str(), "open" | "claimed" | "accepted") {
                return Err(E::ContractInputInvalid);
            }
            // A claimed or accepted handoff names its claimant and the fence
            // that claim created; an open one names neither.
            let claimed = h.claimant.is_some() && h.claimed_fence.is_some();
            let unclaimed = h.claimant.is_none() && h.claimed_fence.is_none();
            if (h.status == "open" && !unclaimed) || (h.status != "open" && !claimed) {
                return Err(E::ContractInputInvalid);
            }
        }
        if let Some(stored) = &self.intent {
            if !matches!(
                stored.value.state.as_str(),
                "intended" | "reported" | "resolved"
            ) {
                return Err(E::ContractInputInvalid);
            }
        }
        if let Some(observed) = &self.evidence {
            for (index, o) in observed.iter().enumerate() {
                if observed[..index]
                    .iter()
                    .any(|p| p.collection == o.collection && p.id == o.id)
                {
                    return Err(E::ContractInputInvalid);
                }
            }
        }
        Ok(())
    }
}

impl TaskValue {
    /// A task is terminal exactly when it holds no lease.
    fn validate(&self) -> Result<(), ErrorCode> {
        if !STATUSES.contains(&self.status.as_str())
            || self.fence == 0
            || self.checkpoint_version == 0
            || terminal(&self.status) != self.lease.is_none()
        {
            return Err(E::ContractInputInvalid);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Shared rules
// ---------------------------------------------------------------------------

fn s(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or_default()
}

fn intent_id(task: &str, key: &str) -> String {
    format!("{task}~{key}")
}

fn task_json(value: &TaskValue) -> Value {
    serde_json::to_value(value).expect("task serializes")
}

fn lease_expired(lease: &Lease, now: u64) -> bool {
    now >= lease.expires_at
}

fn new_lease(now: u64, lease_id: &str, lease_ms: u64) -> Result<Lease, ErrorCode> {
    Ok(Lease {
        expires_at: now.checked_add(lease_ms).ok_or(E::ContractInputInvalid)?,
        lease_id: lease_id.to_string(),
    })
}

/// The one ownership rule. Order is part of the contract, so two refusals that
/// both apply always report the same one.
fn owner_checks(
    task: &TaskValue,
    actor: &str,
    fence: u64,
    lease_id: &str,
    now: u64,
) -> Result<Lease, ErrorCode> {
    if terminal(&task.status) {
        return Err(E::Terminal);
    }
    if fence != task.fence {
        return Err(E::FenceStale);
    }
    if actor != task.owner {
        return Err(E::OwnershipConflict);
    }
    let lease = task.lease.clone().ok_or(E::ContractInputInvalid)?;
    if lease_id != lease.lease_id {
        return Err(E::LeaseMismatch);
    }
    if lease_expired(&lease, now) {
        return Err(E::LeaseExpired);
    }
    Ok(lease)
}

fn task_expect(stored: &Stored<TaskValue>, lease: &Lease) -> Expect {
    Expect::Version {
        version: stored.version,
        lease_id: Some(lease.lease_id.clone()),
    }
}

fn task_guard(stored: &Stored<TaskValue>, lease: &Lease) -> Guard {
    Guard {
        collection: TASK_COLLECTION.into(),
        id: stored.value.task_id.clone(),
        version: stored.version,
        lease_id: Some(lease.lease_id.clone()),
    }
}

struct Resolved {
    citations: Vec<Citation>,
    guards: Vec<Guard>,
    verified: usize,
}

/// Turns a caller's citations into stored citations, using only what FeltDB
/// observed. A record citation with no matching observation is refused, as is
/// one aimed at the agent's own records. Every observation must correspond to
/// a cited record, so the native layer cannot smuggle in extras.
fn resolve_evidence(refs: &[EvidenceRef], observed: &[Observation]) -> Result<Resolved, ErrorCode> {
    let mut citations = Vec::new();
    let mut guards = Vec::new();
    let mut cited_records = 0;
    for (index, reference) in refs.iter().enumerate() {
        if refs[..index].iter().any(|p| p == reference) {
            return Err(E::EvidenceInvalid);
        }
        if reference.kind == "external" {
            citations.push(Citation {
                collection: None,
                id: None,
                kind: "external".into(),
                uri: reference.uri.clone(),
                verified: false,
                version: None,
            });
            continue;
        }
        let (collection, id) = (s(&reference.collection), s(&reference.id));
        if collection.starts_with(AGENT_COLLECTION_PREFIX) {
            return Err(E::EvidenceInvalid);
        }
        let seen = observed
            .iter()
            .find(|o| o.collection == collection && o.id == id)
            .ok_or(E::EvidenceInvalid)?;
        cited_records += 1;
        citations.push(Citation {
            collection: Some(collection.into()),
            id: Some(id.into()),
            kind: "record".into(),
            uri: None,
            verified: true,
            version: Some(seen.version),
        });
        guards.push(Guard {
            collection: collection.into(),
            id: id.into(),
            version: seen.version,
            lease_id: None,
        });
    }
    if observed.len() != cited_records {
        return Err(E::ContractInputInvalid);
    }
    Ok(Resolved {
        verified: cited_records,
        citations,
        guards,
    })
}

fn allowed_transition(from: &str, to: &str) -> bool {
    from == to && !terminal(from)
        || matches!(
            (from, to),
            ("ready", "running")
                | ("ready", "cancelled")
                | ("running", "blocked")
                | ("running", "completed")
                | ("running", "cancelled")
                | ("blocked", "running")
                | ("blocked", "cancelled")
        )
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

fn create_task(op: &Operation, records: &Records, now: u64) -> Outcome {
    let (id, actor, objective) = (s(&op.task_id), s(&op.actor), s(&op.objective));
    if let Some(existing) = &records.task {
        let t = &existing.value;
        return if t.objective == objective && t.owner == actor {
            Ok(Decision::Noop)
        } else {
            Err(E::AlreadyExists)
        };
    }
    let lease = new_lease(now, s(&op.lease_id), op.lease_ms.unwrap_or_default())?;
    let task = TaskValue {
        checkpoint_version: 1,
        created_at_ms: now,
        fence: 1,
        lease: Some(lease),
        objective: objective.into(),
        owner: actor.into(),
        status: "ready".into(),
        task_id: id.into(),
        unresolved_intents: 0,
        updated_at_ms: now,
    };
    Ok(Decision::Commit {
        writes: vec![
            Write {
                collection: TASK_COLLECTION,
                id: id.into(),
                expect: Expect::Absent,
                value: task_json(&task),
            },
            Write {
                collection: CHECKPOINT_COLLECTION,
                id: format!("{id}~v1"),
                expect: Expect::Absent,
                value: json!({
                    "actor": actor, "created_at_ms": now, "evidence": [], "fence": 1,
                    "status": "ready", "summary": "Task created.", "task_id": id, "version": 1,
                }),
            },
        ],
        guards: vec![],
    })
}

fn acquire(op: &Operation, records: &Records, now: u64) -> Outcome {
    let stored = records.task.as_ref().ok_or(E::NotFound)?;
    let task = &stored.value;
    if terminal(&task.status) {
        return Err(E::Terminal);
    }
    let held = task.lease.as_ref().ok_or(E::ContractInputInvalid)?;
    if !lease_expired(held, now) {
        return Err(E::LeaseHeld);
    }
    if s(&op.lease_id) == held.lease_id {
        return Err(E::LeaseMismatch);
    }
    let mut next = task.clone();
    next.owner = s(&op.actor).into();
    next.fence = task.fence.checked_add(1).ok_or(E::ContractInputInvalid)?;
    next.lease = Some(new_lease(
        now,
        s(&op.lease_id),
        op.lease_ms.unwrap_or_default(),
    )?);
    next.updated_at_ms = now;
    // The version guard, not a time predicate, protects "this lease is
    // expired": time only moves forward, and any renewal, claim or
    // acquisition changes the record's version.
    Ok(Decision::Commit {
        writes: vec![Write {
            collection: TASK_COLLECTION,
            id: task.task_id.clone(),
            expect: Expect::Version {
                version: stored.version,
                lease_id: None,
            },
            value: task_json(&next),
        }],
        guards: vec![],
    })
}

fn renew(op: &Operation, records: &Records, now: u64) -> Outcome {
    let stored = records.task.as_ref().ok_or(E::NotFound)?;
    let task = &stored.value;
    let lease = owner_checks(
        task,
        s(&op.actor),
        op.fence.unwrap_or_default(),
        s(&op.lease_id),
        now,
    )?;
    let mut next = task.clone();
    next.lease = Some(new_lease(
        now,
        &lease.lease_id,
        op.lease_ms.unwrap_or_default(),
    )?);
    next.updated_at_ms = now;
    Ok(Decision::Commit {
        writes: vec![Write {
            collection: TASK_COLLECTION,
            id: task.task_id.clone(),
            expect: task_expect(stored, &lease),
            value: task_json(&next),
        }],
        guards: vec![],
    })
}

fn checkpoint(op: &Operation, records: &Records, now: u64) -> Outcome {
    let stored = records.task.as_ref().ok_or(E::NotFound)?;
    let task = &stored.value;
    let lease = owner_checks(
        task,
        s(&op.actor),
        op.fence.unwrap_or_default(),
        s(&op.lease_id),
        now,
    )?;
    if op.expected_version != Some(task.checkpoint_version) {
        return Err(E::VersionConflict);
    }
    let status = op.status.clone().unwrap_or_else(|| task.status.clone());
    if !allowed_transition(&task.status, &status) {
        return Err(E::InvalidTransition);
    }
    let resolved = resolve_evidence(
        op.evidence.as_deref().unwrap_or_default(),
        records.evidence.as_deref().unwrap_or_default(),
    )?;
    if status == "completed" {
        // A completion is a claim until FeltDB has seen something that backs
        // it, and it cannot stand while an action may or may not have run.
        if resolved.verified == 0 {
            return Err(E::EvidenceRequired);
        }
        if task.unresolved_intents > 0 {
            return Err(E::OutcomeUncertain);
        }
    }
    let version = task
        .checkpoint_version
        .checked_add(1)
        .ok_or(E::ContractInputInvalid)?;
    let mut next = task.clone();
    next.status = status.clone();
    next.checkpoint_version = version;
    next.updated_at_ms = now;
    if terminal(&status) {
        next.lease = None;
    }
    let mut guards = resolved.guards;
    guards.sort_by(|a, b| (&a.collection, &a.id).cmp(&(&b.collection, &b.id)));
    Ok(Decision::Commit {
        writes: vec![
            Write {
                collection: TASK_COLLECTION,
                id: task.task_id.clone(),
                expect: task_expect(stored, &lease),
                value: task_json(&next),
            },
            Write {
                collection: CHECKPOINT_COLLECTION,
                id: format!("{}~v{}", task.task_id, version),
                expect: Expect::Absent,
                value: json!({
                    "actor": s(&op.actor), "created_at_ms": now, "evidence": resolved.citations,
                    "fence": task.fence, "status": status, "summary": s(&op.summary),
                    "task_id": task.task_id, "version": version,
                }),
            },
        ],
        guards,
    })
}

fn record_intent(op: &Operation, records: &Records, now: u64) -> Outcome {
    let stored = records.task.as_ref().ok_or(E::NotFound)?;
    let task = &stored.value;
    let lease = owner_checks(
        task,
        s(&op.actor),
        op.fence.unwrap_or_default(),
        s(&op.lease_id),
        now,
    )?;
    if task.status != "running" {
        return Err(E::InvalidTransition);
    }
    if let Some(existing) = &records.intent {
        // A key already recorded is never a fresh start. While its outcome is
        // unknown, the action may have run, and saying so is the point.
        return Err(if existing.value.state == "resolved" {
            E::IntentResolved
        } else {
            E::OutcomeUncertain
        });
    }
    let mut next = task.clone();
    next.unresolved_intents = task
        .unresolved_intents
        .checked_add(1)
        .ok_or(E::ContractInputInvalid)?;
    next.updated_at_ms = now;
    let key = s(&op.idempotency_key);
    Ok(Decision::Commit {
        writes: vec![
            Write {
                collection: TASK_COLLECTION,
                id: task.task_id.clone(),
                expect: task_expect(stored, &lease),
                value: task_json(&next),
            },
            Write {
                collection: INTENT_COLLECTION,
                id: intent_id(&task.task_id, key),
                expect: Expect::Absent,
                value: json!({
                    "action": s(&op.action), "evidence": [], "intended_at_ms": now,
                    "intended_fence": task.fence, "key": key, "state": "intended",
                    "task_id": task.task_id, "verified": false,
                }),
            },
        ],
        guards: vec![],
    })
}

/// Shared by `record_outcome` and `reconcile`: the owner-checked update of one
/// intent and the unresolved-intent count on the task.
fn settle(
    op: &Operation,
    records: &Records,
    now: u64,
    allowed_from: &[&str],
    build: impl FnOnce(&IntentValue, Resolved) -> Result<(IntentValue, bool), ErrorCode>,
) -> Outcome {
    let stored = records.task.as_ref().ok_or(E::NotFound)?;
    let task = &stored.value;
    let lease = owner_checks(
        task,
        s(&op.actor),
        op.fence.unwrap_or_default(),
        s(&op.lease_id),
        now,
    )?;
    let intent = records.intent.as_ref().ok_or(E::IntentUnknown)?;
    if !allowed_from.contains(&intent.value.state.as_str()) {
        return Err(E::IntentResolved);
    }
    let resolved = resolve_evidence(
        op.evidence.as_deref().unwrap_or_default(),
        records.evidence.as_deref().unwrap_or_default(),
    )?;
    let guards = resolved.guards.clone();
    let (next_intent, resolves) = build(&intent.value, resolved)?;
    let mut next = task.clone();
    if resolves {
        next.unresolved_intents = task
            .unresolved_intents
            .checked_sub(1)
            .ok_or(E::ContractInputInvalid)?;
    }
    next.updated_at_ms = now;
    let mut guards = guards;
    guards.sort_by(|a, b| (&a.collection, &a.id).cmp(&(&b.collection, &b.id)));
    Ok(Decision::Commit {
        writes: vec![
            Write {
                collection: TASK_COLLECTION,
                id: task.task_id.clone(),
                expect: task_expect(stored, &lease),
                value: task_json(&next),
            },
            Write {
                collection: INTENT_COLLECTION,
                id: intent_id(&task.task_id, s(&op.idempotency_key)),
                expect: Expect::Version {
                    version: intent.version,
                    lease_id: None,
                },
                value: serde_json::to_value(&next_intent).expect("intent serializes"),
            },
        ],
        guards,
    })
}

fn record_outcome(op: &Operation, records: &Records, now: u64) -> Outcome {
    settle(op, records, now, &["intended"], |intent, resolved| {
        // A reported outcome is a claim. Only an outcome FeltDB saw evidence
        // for resolves the intent.
        let verified = resolved.verified > 0;
        let mut next = intent.clone();
        next.result = op.result.clone();
        next.evidence = resolved.citations;
        next.verified = verified;
        next.state = if verified { "resolved" } else { "reported" }.into();
        Ok((next, verified))
    })
}

fn reconcile(op: &Operation, records: &Records, now: u64) -> Outcome {
    settle(
        op,
        records,
        now,
        &["intended", "reported"],
        |intent, resolved| {
            // Settling an uncertain action, in either direction, needs evidence.
            // The absence of an outcome record is not evidence of non-execution.
            if resolved.verified == 0 {
                return Err(E::EvidenceRequired);
            }
            let mut next = intent.clone();
            next.conclusion = op.conclusion.clone();
            next.evidence = [intent.evidence.clone(), resolved.citations].concat();
            next.verified = true;
            next.state = "resolved".into();
            Ok((next, true))
        },
    )
}

fn create_handoff(op: &Operation, records: &Records, now: u64) -> Outcome {
    let stored = records.task.as_ref().ok_or(E::NotFound)?;
    let task = &stored.value;
    let lease = owner_checks(
        task,
        s(&op.actor),
        op.fence.unwrap_or_default(),
        s(&op.lease_id),
        now,
    )?;
    if records.handoff.is_some() {
        return Err(E::AlreadyExists);
    }
    let handoff = HandoffValue {
        checkpoint_version: task.checkpoint_version,
        claimant: None,
        claimed_fence: None,
        from: s(&op.actor).into(),
        from_fence: task.fence,
        handoff_id: s(&op.handoff_id).into(),
        status: "open".into(),
        task_id: task.task_id.clone(),
        to: op.to.clone(),
    };
    Ok(Decision::Commit {
        writes: vec![Write {
            collection: HANDOFF_COLLECTION,
            id: handoff.handoff_id.clone(),
            expect: Expect::Absent,
            value: serde_json::to_value(&handoff).expect("handoff serializes"),
        }],
        guards: vec![task_guard(stored, &lease)],
    })
}

fn claim_handoff(op: &Operation, records: &Records, now: u64) -> Outcome {
    let (handoff_stored, task_stored) = (
        records.handoff.as_ref().ok_or(E::NotFound)?,
        records.task.as_ref().ok_or(E::NotFound)?,
    );
    let (handoff, task) = (&handoff_stored.value, &task_stored.value);
    let claimant = s(&op.claimant);
    if handoff.task_id != task.task_id {
        return Err(E::ContractInputInvalid);
    }
    if handoff.status == "accepted" {
        return Err(E::HandoffClaimed);
    }
    if terminal(&task.status) {
        return Err(E::Terminal);
    }
    if handoff.to.as_deref().is_some_and(|to| to != claimant) {
        return Err(E::OwnershipConflict);
    }
    let held = task.lease.as_ref().ok_or(E::ContractInputInvalid)?;
    if handoff.status == "claimed" {
        if !lease_expired(held, now) {
            let holder_retry = handoff.claimant.as_deref() == Some(claimant)
                && task.owner == claimant
                && held.lease_id == s(&op.lease_id);
            return if holder_retry {
                Ok(Decision::Noop)
            } else {
                Err(E::HandoffClaimed)
            };
        }
        // An abandoned claim is recoverable only while the task is still with
        // the claimant that abandoned it.
        if handoff.claimant.as_deref() != Some(task.owner.as_str()) {
            return Err(E::HandoffStale);
        }
    } else if task.owner != handoff.from
        || task.fence != handoff.from_fence
        || task.checkpoint_version != handoff.checkpoint_version
    {
        return Err(E::HandoffStale);
    }
    if s(&op.lease_id) == held.lease_id {
        return Err(E::LeaseMismatch);
    }
    let fence = task.fence.checked_add(1).ok_or(E::ContractInputInvalid)?;
    let mut next_task = task.clone();
    next_task.owner = claimant.into();
    next_task.fence = fence;
    next_task.lease = Some(new_lease(
        now,
        s(&op.lease_id),
        op.lease_ms.unwrap_or_default(),
    )?);
    next_task.updated_at_ms = now;
    let mut next_handoff = handoff.clone();
    next_handoff.status = "claimed".into();
    next_handoff.claimant = Some(claimant.into());
    next_handoff.claimed_fence = Some(fence);
    Ok(Decision::Commit {
        writes: vec![
            Write {
                collection: HANDOFF_COLLECTION,
                id: handoff.handoff_id.clone(),
                expect: Expect::Version {
                    version: handoff_stored.version,
                    lease_id: None,
                },
                value: serde_json::to_value(&next_handoff).expect("handoff serializes"),
            },
            Write {
                collection: TASK_COLLECTION,
                id: task.task_id.clone(),
                expect: Expect::Version {
                    version: task_stored.version,
                    lease_id: None,
                },
                value: task_json(&next_task),
            },
        ],
        guards: vec![],
    })
}

fn accept_handoff(op: &Operation, records: &Records, now: u64) -> Outcome {
    let (handoff_stored, task_stored) = (
        records.handoff.as_ref().ok_or(E::NotFound)?,
        records.task.as_ref().ok_or(E::NotFound)?,
    );
    let (handoff, task) = (&handoff_stored.value, &task_stored.value);
    let claimant = s(&op.claimant);
    if handoff.task_id != task.task_id {
        return Err(E::ContractInputInvalid);
    }
    match handoff.status.as_str() {
        "accepted" => {
            return if handoff.claimant.as_deref() == Some(claimant) {
                Ok(Decision::Noop)
            } else {
                Err(E::HandoffClaimed)
            };
        }
        "open" => return Err(E::InvalidTransition),
        _ => {}
    }
    if handoff.claimant.as_deref() != Some(claimant) {
        return Err(E::OwnershipConflict);
    }
    let lease = owner_checks(
        task,
        claimant,
        op.fence.unwrap_or_default(),
        s(&op.lease_id),
        now,
    )?;
    if handoff.claimed_fence != Some(task.fence) {
        return Err(E::HandoffStale);
    }
    let mut next = handoff.clone();
    next.status = "accepted".into();
    Ok(Decision::Commit {
        writes: vec![Write {
            collection: HANDOFF_COLLECTION,
            id: handoff.handoff_id.clone(),
            expect: Expect::Version {
                version: handoff_stored.version,
                lease_id: None,
            },
            value: serde_json::to_value(&next).expect("handoff serializes"),
        }],
        guards: vec![task_guard(task_stored, &lease)],
    })
}

// ---------------------------------------------------------------------------
// Canonical output
// ---------------------------------------------------------------------------

fn expect_json(expect: &Expect) -> Value {
    match expect {
        Expect::Absent => json!({"absent": true}),
        Expect::Version {
            version,
            lease_id: None,
        } => json!({"version": version}),
        Expect::Version {
            version,
            lease_id: Some(lease),
        } => json!({"lease_id": lease, "version": version}),
    }
}

/// `serde_json::Map` is a `BTreeMap`, so every object is emitted in sorted key
/// order. Arrays are ordered by the contract, not by this encoder.
pub fn encode(outcome: &Outcome) -> Vec<u8> {
    let mut document = Map::new();
    document.insert("contract".into(), json!(AGENT_TRANSITION_CONTRACT_ID));
    document.insert("version".into(), json!(AGENT_TRANSITION_CONTRACT_VERSION));
    match outcome {
        Err(code) => {
            document.insert("error".into(), json!({"code": code.as_str()}));
        }
        Ok(Decision::Noop) => {
            document.insert("outcome".into(), json!({"kind": "noop"}));
        }
        Ok(Decision::Commit { writes, guards }) => {
            let writes: Vec<Value> = writes
                .iter()
                .map(|w| {
                    json!({
                        "collection": w.collection, "expect": expect_json(&w.expect),
                        "id": w.id, "value": w.value,
                    })
                })
                .collect();
            let guards: Vec<Value> = guards
                .iter()
                .map(|g| match &g.lease_id {
                    None => json!({"collection": g.collection, "id": g.id, "version": g.version}),
                    Some(lease) => json!({
                        "collection": g.collection, "id": g.id, "lease_id": lease, "version": g.version,
                    }),
                })
                .collect();
            document.insert(
                "outcome".into(),
                json!({"guards": guards, "kind": "commit", "writes": writes}),
            );
        }
    }
    serde_json::to_vec(&Value::Object(document)).expect("output serializes")
}
