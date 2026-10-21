//! Resolving an open intent from evidence outside Stack-Agent's own history.
//!
//! An intent is open when the process that recorded it never recorded an outcome: the operation
//! may have reached its destination, and its result was lost. [`begin`] refuses such an intent as
//! [`StateError::OutcomeUncertain`] and never repeats it. [`reconcile`] is the other way out: it
//! *asks the destination what happened* about the original operation (its action key and binding),
//! and resolves the intent only if the answer is strong enough. It never dispatches the operation.
//!
//! The destination supplies two independent facts through a [`Reconciler`]:
//!
//! * [`DestinationStatus`]: what its own record says about the operation identified by the
//!   original key (a read; it must not execute anything), and
//! * [`BusinessCheck`]: whether the business effect the request was for is present in the
//!   destination's own state (a second read, of the effect, not of the operation's record).
//!
//! Chip, not the reconciler, combines them ([`decide`]):
//!
//! | destination status | business effect | finding |
//! | --- | --- | --- |
//! | committed, for this key and binding | present | `Completed` |
//! | committed | absent / unreadable | `Indeterminate` (the report is not the outcome) |
//! | not applied, and barred from applying later | absent | `NotExecuted` |
//! | not applied, and barred | present / unreadable | `Indeterminate` |
//! | not applied, not barred | any | `Indeterminate` (a missing record is not proof) |
//! | unknown, or about another operation | any | `Indeterminate` |
//!
//! `Completed` writes the observation the lost result would have been (marked as reconciled, with
//! the destination's reported text, never a model's), so the next start restores it like any other
//! and the goal is judged on it. `NotExecuted` resolves the intent as not executed and permits no
//! more than the normal path does. `Indeterminate` changes nothing about the intent: it stays open
//! and the next start still refuses it. Every query that was made is recorded as a
//! `chip.Reconciliation` record, whatever it found.
//!
//! What this does not do: it cannot make a capability reconcilable. An intent whose capability
//! carries no action key has no identity to ask about, and a capability with no [`Reconciler`] has
//! no one to ask; both stay uncertain and the reason is reported.

use std::collections::BTreeMap;

use super::*;

pub const RECONCILIATION_COLLECTION: &str = "chip.Reconciliation";
pub const RECONCILIATION_SCHEMA: &str = "chip.reconciliation.v1";
/// The execution id of the observation a `NotExecuted` finding records. Not an operator, not an
/// execution: [`ObservationRecord::is_settlement`] is false for it and `restore` skips it.
pub const RECONCILIATION_EXECUTION_ID: &str = "reconciliation";
const RECONCILED_OUTPUT: &str =
    "reconciled from the destination's own record, not from the original response:";
/// How long a reconciliation holds the task while it queries and records.
const RECONCILE_LEASE_MS: u64 = 60_000;

/// The original logical operation, exactly as its intent recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRef {
    pub key: String,
    pub binding: String,
}

/// What the destination is asked about. It names the original operation; it carries the typed
/// inputs so the destination can check the *expected* effect, and they are not copied into any
/// record this module writes.
#[derive(Debug, Clone, PartialEq)]
pub struct ReconcileQuery {
    pub task_id: String,
    pub intent_key: String,
    pub capability: String,
    pub operation: OperationRef,
    pub inputs: BTreeMap<String, InputValue>,
}

/// What the destination's own record says about one operation. A lookup: it must not execute it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestinationStatus {
    /// It applied the operation identified by `operation`, and reports `reported` for it.
    Committed {
        operation: OperationRef,
        receipt: Option<String>,
        reported: String,
    },
    /// It has no record of applying it. `barred` is the destination's guarantee that the
    /// operation cannot take effect later (it was refused, fenced or expired); without it a
    /// missing record proves nothing, because the request may still be in flight.
    NotApplied { barred: bool, basis: String },
    /// It could not say.
    Unknown { reason: String },
}

/// Whether the business effect the request was for is in the destination's state, read
/// independently of the operation's own record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusinessCheck {
    Present { evidence: String },
    Absent { evidence: String },
    Unavailable { reason: String },
}

/// A destination's answers about operations it was asked to perform. Both methods are reads.
#[async_trait::async_trait]
pub trait Reconciler: Send + Sync {
    /// Whether this reconciler can be asked about `capability`.
    fn handles(&self, capability: &str) -> bool;
    async fn status(&self, query: &ReconcileQuery) -> DestinationStatus;
    async fn verify(&self, query: &ReconcileQuery) -> BusinessCheck;
}

/// What reconciliation found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// Independent evidence establishes that the operation completed and its business effect is
    /// present.
    Completed {
        receipt: Option<String>,
        reported: String,
    },
    /// Authoritative evidence establishes that it did not execute and cannot later.
    NotExecuted { basis: String },
    /// Evidence cannot establish either safely. The intent stays open.
    Indeterminate { reason: String },
}

impl Finding {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Completed { .. } => "completed",
            Self::NotExecuted { .. } => "not_executed",
            Self::Indeterminate { .. } => "indeterminate",
        }
    }
}

/// Chip's reading of the two answers. Pure: nothing here reaches reality.
pub fn decide(
    query: &ReconcileQuery,
    status: &DestinationStatus,
    check: Option<&BusinessCheck>,
) -> Finding {
    let indeterminate = |reason: String| Finding::Indeterminate { reason };
    match (status, check) {
        (DestinationStatus::Unknown { reason }, _) => {
            indeterminate(format!("the destination could not say: {reason}"))
        }
        (DestinationStatus::Committed { operation, .. }, _) if *operation != query.operation => {
            indeterminate(
                "the destination answered about another operation than the original (key or binding differ)"
                    .into(),
            )
        }
        (
            DestinationStatus::Committed {
                receipt, reported, ..
            },
            Some(BusinessCheck::Present { .. }),
        ) => Finding::Completed {
            receipt: receipt.clone(),
            reported: reported.clone(),
        },
        (DestinationStatus::Committed { .. }, Some(BusinessCheck::Absent { evidence })) => {
            indeterminate(format!(
                "the destination reports the operation committed, but its business effect is absent: {evidence}"
            ))
        }
        (DestinationStatus::Committed { .. }, other) => indeterminate(format!(
            "the destination reports the operation committed, but its business effect could not be verified{}",
            unavailable(other)
        )),
        (DestinationStatus::NotApplied { barred: false, .. }, _) => indeterminate(
            "the destination has no record of the operation, but does not guarantee it cannot still be applied; a missing record is not proof"
                .into(),
        ),
        (
            DestinationStatus::NotApplied {
                barred: true,
                basis,
            },
            Some(BusinessCheck::Absent { .. }),
        ) => Finding::NotExecuted {
            basis: basis.clone(),
        },
        (DestinationStatus::NotApplied { barred: true, .. }, Some(BusinessCheck::Present { evidence })) => {
            indeterminate(format!(
                "the destination says the operation was not applied, but its business effect is present: {evidence}"
            ))
        }
        (DestinationStatus::NotApplied { barred: true, .. }, other) => indeterminate(format!(
            "the destination says the operation was not applied, but the absence of its effect could not be verified{}",
            unavailable(other)
        )),
    }
}

fn unavailable(check: Option<&BusinessCheck>) -> String {
    match check {
        Some(BusinessCheck::Unavailable { reason }) => format!(": {reason}"),
        _ => String::new(),
    }
}

/// One reconciliation query and what Chip made of it, as FeltDB keeps it. The request's inputs are
/// not here: the key and binding identify the operation. `finding` is what was found; whether the
/// intent was resolved is the intent's own state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationRecord {
    pub schema: String,
    pub task_id: String,
    pub intent_key: String,
    /// 1 for the first query about this intent, then counting up.
    pub query_no: u64,
    pub capability: String,
    pub action_key: String,
    pub binding: String,
    pub finding: String,
    /// `committed`, `not_applied_barred`, `not_applied`, `unknown` or `mismatched`.
    pub destination: String,
    pub destination_detail: String,
    /// `present`, `absent`, `unavailable` or `not_asked`.
    pub business_effect: String,
    pub business_detail: String,
    pub receipt_id: Option<String>,
    /// Why the evidence was insufficient (for `indeterminate`).
    pub reason: Option<String>,
    pub decided_by: String,
}

impl ReconciliationRecord {
    /// Whether an operator still has to settle the intent: nothing established what happened.
    pub fn needs_settlement(&self) -> bool {
        self.finding == "indeterminate"
    }
}

/// What [`reconcile`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct Reconciled {
    pub finding: Finding,
    /// The `chip.Reconciliation` record of the query; `None` when nothing could be asked.
    pub record_id: Option<String>,
    /// Whether the intent was resolved by it.
    pub resolved: bool,
}

/// Why an open intent cannot be asked about at all, or the request it would ask about.
fn askable(
    intent: &IntentValue,
    reconciler: Option<&dyn Reconciler>,
    task_id: &str,
) -> Result<ReconcileQuery, String> {
    let Some(tag) = ActionTag::parse(&intent.action) else {
        return Err("its capability does not carry an action key, so the original operation has no identity a destination could be asked about".into());
    };
    let Some(reconciler) = reconciler.filter(|r| r.handles(&tag.capability)) else {
        return Err(format!(
            "no reconciler is available for {}: nothing can say what the destination did",
            tag.capability
        ));
    };
    let _ = reconciler;
    let recorded = RecordedRequest {
        capability: tag.capability.clone(),
        inputs: action_recorded_inputs(&intent.action)
            .ok_or("the request's inputs were not recorded (too long for the intent), so what the effect should have been cannot be checked")?,
    };
    let inputs = recorded.typed_inputs()?;
    if binding_of(&recorded) != tag.binding {
        return Err("the recorded inputs do not hash to the binding the intent recorded; they are not the original request".into());
    }
    Ok(ReconcileQuery {
        task_id: task_id.into(),
        intent_key: intent.key.clone(),
        capability: tag.capability,
        operation: OperationRef {
            key: tag.key,
            binding: tag.binding,
        },
        inputs,
    })
}

/// The execution identity of an intent key `a<attempt>.<execution id>`, if it is recoverable from
/// the key (a long id is hashed in it and cannot be).
fn execution_of(key: &str) -> Option<(u64, String)> {
    let rest = key.strip_prefix('a')?;
    let (attempt, execution) = rest.split_once('.')?;
    let attempt: u64 = attempt.parse().ok()?;
    (!execution.starts_with('h') || execution.len() != 33)
        .then(|| (attempt, execution.to_string()))
        .filter(|(a, e)| intent_key(*a, e) == key)
}

/// Asks the destination what happened to the open intent `intent_key`, and resolves it only on
/// sufficient evidence. See the module documentation.
///
/// Nothing is dispatched. The intent is resolved by the same transition and the same kind of
/// record that [`begin`] and [`settle`] use, and only if the task's lease can be taken (no worker
/// holds it). Refused, with nothing written, when:
///
/// * the task or intent does not exist, the task is finished or the intent is resolved already
///   (so a repeated reconciliation, or one after an operator's settlement, changes nothing);
/// * the intent has an observation record of its own;
/// * a worker holds the task's lease.
///
/// Whether the intent cannot be asked about at all is not a refusal: it is an `Indeterminate`
/// finding with the reason, and nothing is written.
pub async fn reconcile(
    store: &AgentStore,
    task_id: &str,
    intent_key: &str,
    decided_by: &str,
    reconciler: Option<&dyn Reconciler>,
) -> Result<Reconciled, StateError> {
    check_contract(
        AGENT_TRANSITION_CONTRACT_ID,
        AGENT_TRANSITION_CONTRACT_VERSION,
    )?;
    let refused = |reason: String| StateError::ReconciliationRefused {
        task_id: task_id.into(),
        intent: intent_key.into(),
        reason,
    };
    for (what, value) in [
        ("task id", task_id),
        ("intent", intent_key),
        ("decided-by", decided_by),
    ] {
        if !is_identifier(value) {
            return Err(refused(format!(
                "the {what} {value:?} must be 1 to {MAX_IDENTIFIER_BYTES} bytes of A-Z a-z 0-9 _ . : @ -"
            )));
        }
    }
    let id = format!("{task_id}~{intent_key}");
    let check = |store: &AgentStore| -> Result<IntentValue, StateError> {
        let Some((_, task)) = store.task(task_id)? else {
            return Err(refused("there is no such task".into()));
        };
        if matches!(task.status.as_str(), "completed" | "cancelled") {
            return Err(refused(format!("the task is {}", task.status)));
        }
        let Some((_, value)) = store.read(INTENT_COLLECTION, &id)? else {
            return Err(refused("the task has no such intent".into()));
        };
        let intent: IntentValue =
            serde_json::from_value(value).map_err(|e| StateError::InvalidHistory {
                task_id: task_id.into(),
                detail: format!("intent {id} does not parse: {e}"),
            })?;
        if intent.state == "resolved" {
            return Err(refused(format!(
                "it is resolved already (conclusion {})",
                intent.conclusion.as_deref().unwrap_or("from its outcome")
            )));
        }
        if store.observation(&id)?.is_some() {
            return Err(refused(
                "it has an observation record; the next start reconciles it from that".into(),
            ));
        }
        Ok(intent)
    };
    let intent = check(store)?;
    let query = match askable(&intent, reconciler, task_id) {
        Ok(query) => query,
        Err(reason) => {
            return Ok(Reconciled {
                finding: Finding::Indeterminate { reason },
                record_id: None,
                resolved: false,
            });
        }
    };
    let reconciler = reconciler.expect("askable proved there is one");

    let lease_id = unique("reconcile");
    store
        .apply(
            "acquire",
            Operation {
                op: "acquire".into(),
                task_id: Some(task_id.into()),
                actor: Some(decided_by.into()),
                lease_id: Some(lease_id.clone()),
                lease_ms: Some(RECONCILE_LEASE_MS),
                ..Operation::default()
            },
        )
        .map_err(|e| match e {
            StateError::Refused {
                code: ErrorCode::LeaseHeld,
                ..
            }
            | StateError::Conflict { .. } => refused(
                "a worker or another recovery holds the task's lease; try again once it is released or has expired".into(),
            ),
            other => other,
        })?;
    // Everything from here gives the lease back, whatever happens.
    let outcome = held(
        store, task_id, intent_key, decided_by, &lease_id, &check, &query, reconciler,
    )
    .await;
    let fence = fence_of(store, task_id).unwrap_or_default();
    let _ = release(store, task_id, decided_by, fence, &lease_id);
    outcome
}

#[allow(clippy::too_many_arguments)]
async fn held(
    store: &AgentStore,
    task_id: &str,
    intent_key: &str,
    decided_by: &str,
    lease_id: &str,
    check: &(dyn Fn(&AgentStore) -> Result<IntentValue, StateError> + Sync),
    query: &ReconcileQuery,
    reconciler: &dyn Reconciler,
) -> Result<Reconciled, StateError> {
    // The state may have moved between the first read and the lease.
    let intent = check(store)?;
    let (_, task) = store.task(task_id)?.ok_or(StateError::InvalidHistory {
        task_id: task_id.into(),
        detail: "the task is missing during reconciliation".into(),
    })?;
    let fence = task.fence;

    let status = reconciler.status(query).await;
    let check_needed = !matches!(status, DestinationStatus::Unknown { .. });
    let business = if check_needed {
        Some(reconciler.verify(query).await)
    } else {
        None
    };
    let mut finding = decide(query, &status, business.as_ref());
    // A `Completed` observation must name the execution it stands for.
    let execution = execution_of(intent_key);
    if matches!(finding, Finding::Completed { .. }) && execution.is_none() {
        finding = Finding::Indeterminate {
            reason: "the original execution's identity cannot be recovered from its intent key, so the observation it would have recorded cannot be written".into(),
        };
    }

    let (destination, destination_detail, receipt_id) = match &status {
        DestinationStatus::Committed {
            operation,
            receipt,
            reported,
        } => (
            if *operation == query.operation {
                "committed"
            } else {
                "mismatched"
            },
            clip(reported, 512),
            receipt.clone(),
        ),
        DestinationStatus::NotApplied { barred, basis } => (
            if *barred {
                "not_applied_barred"
            } else {
                "not_applied"
            },
            clip(basis, 512),
            None,
        ),
        DestinationStatus::Unknown { reason } => ("unknown", clip(reason, 512), None),
    };
    let (business_effect, business_detail) = match &business {
        Some(BusinessCheck::Present { evidence }) => ("present", clip(evidence, 512)),
        Some(BusinessCheck::Absent { evidence }) => ("absent", clip(evidence, 512)),
        Some(BusinessCheck::Unavailable { reason }) => ("unavailable", clip(reason, 512)),
        None => ("not_asked", String::new()),
    };
    let prior = store
        .ids(RECONCILIATION_COLLECTION, task_id)?
        .into_iter()
        .filter(|rid| rid.starts_with(&format!("{task_id}~{intent_key}~r")))
        .count() as u64;
    let query_no = prior + 1;
    let record_id = format!("{task_id}~{intent_key}~r{query_no:04}");
    let record = ReconciliationRecord {
        schema: RECONCILIATION_SCHEMA.into(),
        task_id: task_id.into(),
        intent_key: intent_key.into(),
        query_no,
        capability: query.capability.clone(),
        action_key: query.operation.key.clone(),
        binding: query.operation.binding.clone(),
        finding: finding.name().into(),
        destination: destination.into(),
        destination_detail,
        business_effect: business_effect.into(),
        business_detail,
        receipt_id: receipt_id.clone(),
        reason: match &finding {
            Finding::Indeterminate { reason } => Some(clip(reason, 512)),
            _ => None,
        },
        decided_by: decided_by.into(),
    };
    store.create(
        RECONCILIATION_COLLECTION,
        &record_id,
        serde_json::to_value(&record).expect("a reconciliation serializes"),
    )?;

    let id = format!("{task_id}~{intent_key}");
    let observation = match &finding {
        Finding::Indeterminate { .. } => {
            return Ok(Reconciled {
                finding,
                record_id: Some(record_id),
                resolved: false,
            });
        }
        Finding::Completed { receipt, reported } => {
            let (attempt, execution_id) = execution.expect("checked above");
            ObservationRecord {
                schema: OBSERVATION_SCHEMA.into(),
                task_id: task_id.into(),
                intent_key: intent_key.into(),
                execution_id,
                attempt,
                status: "success".into(),
                executed: true,
                output: clip(&format!("{RECONCILED_OUTPUT} {reported}"), MAX_TEXT_BYTES),
                receipt_id: receipt.clone(),
                request: Some(RecordedRequest {
                    capability: query.capability.clone(),
                    inputs: action_recorded_inputs(&intent.action)
                        .expect("askable proved the inputs were recorded"),
                }),
            }
        }
        Finding::NotExecuted { basis } => ObservationRecord {
            schema: OBSERVATION_SCHEMA.into(),
            task_id: task_id.into(),
            intent_key: intent_key.into(),
            execution_id: RECONCILIATION_EXECUTION_ID.into(),
            attempt: 0,
            status: "error".into(),
            executed: false,
            output: clip(
                &format!("{RECONCILED_OUTPUT} the operation did not execute and cannot: {basis}"),
                MAX_TEXT_BYTES,
            ),
            receipt_id: None,
            request: None,
        },
    };
    // Created only if absent: an operator's settlement, or the original worker's own late
    // observation, that was recorded first stays what it is.
    store
        .create(
            OBSERVATION_COLLECTION,
            &id,
            serde_json::to_value(&observation).expect("an observation serializes"),
        )
        .map_err(|e| match e {
            StateError::Conflict { .. } => StateError::ReconciliationRefused {
                task_id: task_id.into(),
                intent: intent_key.into(),
                reason: "another observation or settlement was recorded first; it stays authoritative and the next start reconciles from it".into(),
            },
            other => other,
        })?;
    let owner = |op: &str| Operation {
        op: op.into(),
        task_id: Some(task_id.into()),
        actor: Some(decided_by.into()),
        fence: Some(fence),
        lease_id: Some(lease_id.into()),
        ..Operation::default()
    };
    let completed = matches!(finding, Finding::Completed { .. });
    store.apply(
        "reconcile",
        Operation {
            idempotency_key: Some(intent_key.into()),
            conclusion: Some(
                if completed {
                    "executed"
                } else {
                    "not_executed"
                }
                .into(),
            ),
            evidence: Some(vec![record_ref(OBSERVATION_COLLECTION, &id)]),
            ..owner("reconcile")
        },
    )?;
    let (_, task) = store.task(task_id)?.ok_or(StateError::InvalidHistory {
        task_id: task_id.into(),
        detail: "the task is missing after reconciliation".into(),
    })?;
    store.apply(
        "checkpoint",
        Operation {
            expected_version: Some(task.checkpoint_version),
            summary: Some(clip(
                &format!(
                    "Reconciled intent {intent_key} ({}) from the destination's own record: {}. Query {record_id}.",
                    query.capability,
                    if completed {
                        "the operation completed and its business effect is present"
                    } else {
                        "the operation did not execute and cannot"
                    }
                ),
                MAX_TEXT_BYTES,
            )),
            evidence: Some(vec![record_ref(OBSERVATION_COLLECTION, &id)]),
            status: Some(task.status.clone()),
            ..owner("checkpoint")
        },
    )?;
    Ok(Reconciled {
        finding,
        record_id: Some(record_id),
        resolved: true,
    })
}

/// Every reconciliation query recorded for the task, in (intent, query) order.
pub(crate) fn reconciliations(
    store: &AgentStore,
    task_id: &str,
) -> Result<Vec<ReconciliationRecord>, StateError> {
    let mut records = Vec::new();
    for id in store.ids(RECONCILIATION_COLLECTION, task_id)? {
        let Some((_, value)) = store.read(RECONCILIATION_COLLECTION, &id)? else {
            continue;
        };
        schema_of(
            RECONCILIATION_COLLECTION,
            &id,
            &value,
            RECONCILIATION_SCHEMA,
        )?;
        let record: ReconciliationRecord =
            serde_json::from_value(value).map_err(|e| StateError::InvalidHistory {
                task_id: task_id.into(),
                detail: format!("reconciliation {id} does not parse: {e}"),
            })?;
        if record.task_id != task_id
            || id != format!("{task_id}~{}~r{:04}", record.intent_key, record.query_no)
        {
            return Err(StateError::InvalidHistory {
                task_id: task_id.into(),
                detail: format!("reconciliation {id} names another task or place"),
            });
        }
        records.push(record);
    }
    records.sort_by(|a, b| (&a.intent_key, a.query_no).cmp(&(&b.intent_key, b.query_no)));
    Ok(records)
}

/// Why an open intent can or cannot be asked about, for inspection: what an operator would need to
/// know before choosing between another query and a settlement. Needs no reconciler.
pub fn reconcilability(intent: &IntentValue) -> Result<(), String> {
    if intent.state == "resolved" {
        return Err("it is resolved".into());
    }
    let Some(tag) = ActionTag::parse(&intent.action) else {
        return Err("its capability does not carry an action key: the original operation has no identity a destination could be asked about".into());
    };
    let recorded = action_recorded_inputs(&intent.action).map(|inputs| RecordedRequest {
        capability: tag.capability.clone(),
        inputs,
    });
    match recorded {
        None => Err("the request's inputs were not recorded".into()),
        Some(r) if binding_of(&r) != tag.binding => {
            Err("the recorded inputs do not hash to the recorded binding".into())
        }
        Some(_) => Ok(()),
    }
}
