//! The Chip-owned repair loop. Every transition is this code, deterministic and checkpointed; FX is
//! asked for one decision at a time and its reply is only ever a proposal.
//!
//! ```text
//! Acceptance ──pass──▶ Done(success)
//!     │fail
//!     ▼
//! Target ──none/ambiguous──▶ (one classification) ──▶ Done(no target / ambiguous)
//!     │one symbol
//!     ▼
//! Decide ──▶ DecisionInFlight ──▶ validate ──rejected──▶ Target (new packet) │ Done(repeated rejection)
//!                                     │ need_symbol ──▶ Target (packet with context)
//!                                     │ valid body
//!                                     ▼
//!                                   Apply ──▶ Acceptance(patch under test) ──fail──▶ Revert ──▶ Target
//! ```
//!
//! Intents before effects. Entering `DecisionInFlight` is committed before the inference call; a
//! run resumed there ends `inference_outcome_unknown` rather than ask again. `Apply` and `Revert`
//! carry the file's before and after digests; on resume the file is reconciled against them and
//! written at most once.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::acceptance::{self, AcceptanceRun, Diagnostic, RequiredEvidence};
use crate::contract::{
    self, Acceptance, ClassifyPacket, Constraints, ContextSymbol, Decision, Neighbor, Op,
    PacketLimits, Rejected, ReplyError, SymbolRef, TaskPacket,
};
use crate::edit::{self, EditError, WriteOutcome};
use crate::fx::Fx;
use crate::store::{Commit, EVENT_SCHEMA, Event, LoopStore, StoreError};
use crate::symbols::{SymbolIndex, SymbolRecord, sha256};
use crate::target::{self, Target};

pub const RUN_SCHEMA: &str = "chip.symbol-loop.run.v1";
pub const TASK_SCHEMA: &str = "chip.symbol-loop.task.v1";

/// What one repair task is: fixed by Chip (or its operator) before the loop starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDefinition {
    pub schema: String,
    pub task_id: String,
    /// One sentence.
    pub goal: String,
    /// The exact acceptance command, as argv, run in the candidate worktree.
    pub acceptance: Vec<String>,
    pub evidence: RequiredEvidence,
    #[serde(default = "yes")]
    pub signature_frozen: bool,
}

fn yes() -> bool {
    true
}

/// Explicit limits. Every termination below one of them has its own reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budgets {
    pub max_decisions: u32,
    pub max_elapsed_ms: u64,
    pub max_rejected_per_attempt: u32,
    pub max_context_requests: u32,
    /// Source changes one decision may make: one symbol body (the only edit there is).
    pub max_changes_per_decision: u32,
    pub limits: PacketLimits,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            max_decisions: 8,
            max_elapsed_ms: 10 * 60 * 1000,
            max_rejected_per_attempt: 2,
            max_context_requests: 2,
            max_changes_per_decision: 1,
            limits: PacketLimits::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopConfig {
    pub experiment_id: String,
    pub run_id: String,
    pub budgets: Budgets,
    /// For recovery tests and controlled interruption: return after this many committed
    /// transitions in this session, as a crash after a checkpoint would.
    pub halt_after_transitions: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Acceptance,
    Target,
    ClassifyInFlight,
    Decide,
    DecisionInFlight,
    Apply,
    Revert,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminationReason {
    Success,
    BudgetExceeded,
    RepeatedRejection,
    AmbiguousTarget,
    NoTarget,
    /// The failure needs more than one function-body replacement, or a declaration change.
    RepairNotExpressible,
    InvalidDecision,
    StaleSource,
    PacketTooLarge,
    InferenceOutcomeUnknown,
    InfrastructureFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Termination {
    pub reason: TerminationReason,
    pub detail: String,
    /// The reporting class: pass, no_parse, bad_symbol, signature_moved, stale_source,
    /// test_still_fails, budget_exceeded or infrastructure_failure (or no_target,
    /// ambiguous_target, not_expressible, inference_outcome_unknown, packet_too_large for the
    /// loop's own stops).
    pub class: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceSummary {
    pub exit_status: Option<i32>,
    pub passed: bool,
    pub timed_out: bool,
    pub passed_tests: u64,
    pub failed_tests: u64,
    pub duration_ms: u64,
    pub output_digest: String,
    pub excerpt: String,
    pub diagnostics: Vec<Diagnostic>,
}

impl AcceptanceSummary {
    fn of(run: &AcceptanceRun, limits: &PacketLimits) -> Self {
        Self {
            exit_status: run.exit_status,
            passed: run.passed,
            timed_out: run.timed_out,
            passed_tests: run.passed_tests,
            failed_tests: run.failed_tests,
            duration_ms: run.duration_ms,
            output_digest: run.output_digest.clone(),
            excerpt: acceptance::excerpt(run, limits.max_excerpt_bytes),
            diagnostics: run.diagnostics.iter().take(32).cloned().collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetState {
    pub id: String,
    pub fingerprint: String,
    pub attempt: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    pub body_digest: String,
    pub body: String,
    pub error: String,
    pub class: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchRecord {
    pub patch_id: String,
    pub symbol: String,
    pub path: String,
    pub before_sha: String,
    pub after_sha: String,
    pub body_digest: String,
    pub body: String,
    pub diff: String,
    pub before_text: String,
    pub after_text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub unreported_calls: u32,
}

/// Everything needed to resume, in FeltDB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunState {
    pub schema: String,
    pub experiment_id: String,
    pub run_id: String,
    pub task_id: String,
    pub seq: u64,
    pub phase: Phase,
    pub elapsed_ms: u64,
    pub decisions_used: u32,
    pub classification_used: bool,
    /// The candidate the one classification chose, kept so a resumed run does not ask again.
    pub classified_target: Option<String>,
    pub valid_bodies: u32,
    pub acceptance_runs: u32,
    pub target: Option<TargetState>,
    pub base_acceptance: Option<AcceptanceSummary>,
    pub last_acceptance: Option<AcceptanceSummary>,
    pub rejected: Vec<Rejection>,
    pub rejection_log: Vec<Rejection>,
    pub context: Vec<ContextSymbol>,
    pub context_requests_used: u32,
    pub classify_packet: Option<ClassifyPacket>,
    pub packet: Option<TaskPacket>,
    pub packet_digest: Option<String>,
    pub decisions: Vec<Value>,
    pub pending_patch: Option<PatchRecord>,
    pub under_test: Option<PatchRecord>,
    pub applied: Option<PatchRecord>,
    pub stop_after_revert: Option<Termination>,
    pub usage: Usage,
    pub termination: Option<Termination>,
}

/// The run's outcome, as the checkpoint records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopResult {
    pub run_id: String,
    pub termination: Option<Termination>,
    /// `false` when the run was halted (`halt_after_transitions`) before it ended.
    pub finished: bool,
    pub decisions: u32,
    pub valid_bodies: u32,
    pub acceptance_runs: u32,
    pub patch_applied: Option<PatchRecord>,
    pub rejections: Vec<Rejection>,
    pub elapsed_ms: u64,
    pub usage: Usage,
}

#[derive(Debug)]
pub enum LoopError {
    Store(StoreError),
    Definition(String),
}

impl std::fmt::Display for LoopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoopError::Store(e) => write!(f, "{e}"),
            LoopError::Definition(m) => write!(f, "task definition: {m}"),
        }
    }
}

impl From<StoreError> for LoopError {
    fn from(e: StoreError) -> Self {
        LoopError::Store(e)
    }
}

fn reply_class(e: &ReplyError) -> &'static str {
    match e {
        ReplyError::UnauthorizedSymbol(_)
        | ReplyError::NotACandidate(_)
        | ReplyError::OperationNotAllowed(_) => "bad_symbol",
        _ => "no_parse",
    }
}

fn termination(reason: TerminationReason, class: &str, detail: impl Into<String>) -> Termination {
    Termination {
        reason,
        detail: detail.into(),
        class: class.to_string(),
    }
}

struct Session<'a> {
    task: &'a TaskDefinition,
    root: &'a Path,
    store: &'a LoopStore,
    fx: &'a dyn Fx,
    cfg: &'a LoopConfig,
    state: RunState,
    version: Option<u64>,
    since_commit: Instant,
    committed_here: u64,
    /// The latest target ranking's explanation, recorded with the transition it led to.
    ranking: Option<Value>,
}

impl Session<'_> {
    fn elapsed_ms(&self) -> u64 {
        self.state.elapsed_ms + self.since_commit.elapsed().as_millis() as u64
    }

    fn remaining(&self) -> Duration {
        Duration::from_millis(
            self.cfg
                .budgets
                .max_elapsed_ms
                .saturating_sub(self.elapsed_ms()),
        )
    }

    /// Commits the state after `mutate`, moving to `to`, with `detail` in the event.
    fn transition(
        &mut self,
        to: Phase,
        detail: Value,
        mutate: impl FnOnce(&mut RunState),
    ) -> Result<(), LoopError> {
        let from = self.state.phase;
        let mut next = self.state.clone();
        mutate(&mut next);
        next.phase = to;
        next.seq += 1;
        next.elapsed_ms = self.elapsed_ms();
        let event = Event {
            schema: EVENT_SCHEMA.to_string(),
            run_id: next.run_id.clone(),
            seq: next.seq,
            from: format!("{from:?}"),
            to: format!("{to:?}"),
            detail,
            elapsed_ms: next.elapsed_ms,
        };
        match self.store.commit(self.version, &next, &event)? {
            Commit::Committed | Commit::Replayed => {}
        }
        let (version, stored) = self
            .store
            .load(&next.run_id)?
            .ok_or_else(|| StoreError::Corrupt("the run disappeared after a commit".into()))?;
        self.version = Some(version);
        self.state = stored;
        self.since_commit = Instant::now();
        self.committed_here += 1;
        Ok(())
    }

    fn finish(&mut self, t: Termination) -> Result<(), LoopError> {
        let detail = json!({"termination": t});
        self.transition(Phase::Done, detail, |s| s.termination = Some(t))
    }

    fn halted(&self) -> bool {
        self.cfg
            .halt_after_transitions
            .is_some_and(|n| self.committed_here >= n)
    }

    /// A rejection in the current attempt; the run ends when the attempt has had its last.
    fn reject(
        &mut self,
        body: &str,
        error: String,
        class: &str,
        then: Phase,
    ) -> Result<(), LoopError> {
        let limits = self.cfg.budgets.limits;
        let r = Rejection {
            body_digest: sha256(body.as_bytes()),
            body: contract::head(body, limits.max_body_bytes),
            error: contract::head(&error, limits.max_error_bytes),
            class: class.to_string(),
        };
        let exhausted =
            self.state.rejected.len() as u32 + 1 >= self.cfg.budgets.max_rejected_per_attempt;
        let detail =
            json!({"rejected": {"class": class, "error": r.error, "body_digest": r.body_digest}});
        self.transition(then, detail, |s| {
            s.rejected.push(r.clone());
            s.rejection_log.push(r);
            s.packet = None;
            s.packet_digest = None;
        })?;
        if exhausted && then != Phase::Revert {
            let t = termination(
                TerminationReason::RepeatedRejection,
                class,
                format!(
                    "{} proposals rejected for {}; the last: {error}. Scope is not widened automatically",
                    self.state.rejected.len(),
                    self.state
                        .target
                        .as_ref()
                        .map(|t| t.id.as_str())
                        .unwrap_or("?")
                ),
            );
            self.finish(t)?;
        }
        Ok(())
    }

    fn acceptance(&mut self) -> Result<(), LoopError> {
        if self.remaining().is_zero() {
            return self.finish(termination(
                TerminationReason::BudgetExceeded,
                "budget_exceeded",
                "the elapsed-time budget ran out before acceptance",
            ));
        }
        let run = acceptance::run(
            self.root,
            &self.task.acceptance,
            self.remaining(),
            &self.task.evidence,
        );
        let summary = AcceptanceSummary::of(&run, &self.cfg.budgets.limits);
        let detail = json!({"acceptance": {"exit_status": run.exit_status, "passed": run.passed, "passed_tests": run.passed_tests, "failed_tests": run.failed_tests, "timed_out": run.timed_out, "output_digest": run.output_digest, "patch_under_test": self.state.under_test.as_ref().map(|p| p.patch_id.clone())}});
        if let Some(why) = &run.spawn_error {
            let why = why.clone();
            self.transition(Phase::Acceptance, detail, |s| s.acceptance_runs += 1)?;
            return self.finish(termination(
                TerminationReason::InfrastructureFailure,
                "infrastructure_failure",
                why,
            ));
        }
        if run.timed_out {
            self.transition(Phase::Acceptance, detail, |s| s.acceptance_runs += 1)?;
            return self.finish(termination(
                TerminationReason::BudgetExceeded,
                "budget_exceeded",
                "the acceptance command ran past the elapsed-time budget",
            ));
        }
        if run.passed {
            let applied = self.state.under_test.clone();
            self.transition(Phase::Acceptance, detail, |s| {
                s.acceptance_runs += 1;
                s.last_acceptance = Some(summary);
                s.applied = applied;
                s.under_test = None;
            })?;
            let what = match &self.state.applied {
                Some(p) => format!(
                    "acceptance passed with patch {} to {}",
                    p.patch_id, p.symbol
                ),
                None => "acceptance passed with no change".to_string(),
            };
            return self.finish(termination(TerminationReason::Success, "pass", what));
        }
        match self.state.under_test.clone() {
            // The patch under test did not pass: its body is rejected, and the file is restored
            // before anything else happens.
            Some(patch) => {
                let error = summary.excerpt.clone();
                let exhausted = self.state.rejected.len() as u32 + 1
                    >= self.cfg.budgets.max_rejected_per_attempt;
                self.transition(Phase::Acceptance, detail, |s| {
                    s.acceptance_runs += 1;
                    s.last_acceptance = Some(summary);
                })?;
                self.reject(
                    &patch.body,
                    error.clone(),
                    "test_still_fails",
                    Phase::Revert,
                )?;
                if exhausted {
                    let t = termination(
                        TerminationReason::RepeatedRejection,
                        "test_still_fails",
                        format!(
                            "{} rejected bodies for {}; the last still fails acceptance. Scope is not widened automatically",
                            self.state.rejected.len(),
                            patch.symbol
                        ),
                    );
                    self.transition(Phase::Revert, json!({"stop_after_revert": t}), |s| {
                        s.stop_after_revert = Some(t)
                    })?;
                }
                Ok(())
            }
            None => self.transition(Phase::Target, detail, |s| {
                s.acceptance_runs += 1;
                s.last_acceptance = Some(summary.clone());
                s.base_acceptance = Some(summary);
            }),
        }
    }

    fn index(&mut self) -> Result<Option<SymbolIndex>, LoopError> {
        match SymbolIndex::build(self.root) {
            Ok(i) => Ok(Some(i)),
            Err(e) => {
                self.finish(termination(
                    TerminationReason::InfrastructureFailure,
                    "infrastructure_failure",
                    format!("cannot index the worktree: {e}"),
                ))?;
                Ok(None)
            }
        }
    }

    fn base_run(&self) -> AcceptanceRun {
        let b = self
            .state
            .base_acceptance
            .clone()
            .expect("a target is resolved after an acceptance run");
        AcceptanceRun {
            exit_status: b.exit_status,
            timed_out: b.timed_out,
            spawn_error: None,
            duration_ms: b.duration_ms,
            passed_tests: b.passed_tests,
            failed_tests: b.failed_tests,
            passed: b.passed,
            diagnostics: b.diagnostics,
            output_tail: b.excerpt,
            output_digest: b.output_digest,
        }
    }

    fn target(&mut self) -> Result<(), LoopError> {
        let Some(index) = self.index()? else {
            return Ok(());
        };
        // Ranked and explained before anything is chosen; the explanation goes into the checkpoint.
        let ranking = target::rank(&index, &self.base_run());
        let explained = ranking.summary(target::MAX_CANDIDATES);
        self.ranking = Some(explained.clone());
        let resolved = target::decide(&ranking);
        let id = match resolved {
            Target::Single(id) => id,
            Target::NotExpressible(reasons) => {
                return self.finish(termination(
                    TerminationReason::RepairNotExpressible,
                    "not_expressible",
                    format!(
                        "a single function-body replacement cannot repair this failure: {}",
                        reasons.join("; ")
                    ),
                ));
            }
            Target::None => {
                return self.finish(termination(
                    TerminationReason::NoTarget,
                    "no_target",
                    format!(
                        "no single-symbol repair target follows from the diagnostics{}",
                        if ranking.notes.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", ranking.notes.join("; "))
                        }
                    ),
                ));
            }
            Target::Ambiguous(candidates)
                if self
                    .state
                    .classified_target
                    .as_ref()
                    .is_some_and(|c| candidates.contains(c)) =>
            {
                self.state.classified_target.clone().expect("checked")
            }
            Target::Ambiguous(candidates) => {
                if self.state.classification_used {
                    return self.finish(termination(
                        TerminationReason::AmbiguousTarget,
                        "ambiguous_target",
                        format!("still ambiguous after the one classification: {candidates:?}"),
                    ));
                }
                if self.state.decisions_used >= self.cfg.budgets.max_decisions {
                    return self.finish(termination(
                        TerminationReason::BudgetExceeded,
                        "budget_exceeded",
                        "no decision left for the classification",
                    ));
                }
                let packet = ClassifyPacket {
                    version: contract::CLASSIFY_VERSION.to_string(),
                    goal: self.task.goal.clone(),
                    acceptance: self.acceptance_view(),
                    candidates: candidates
                        .iter()
                        .filter_map(|c| index.get(c))
                        .map(|s| Neighbor {
                            id: s.id.clone(),
                            signature: contract::head(
                                &s.declaration,
                                self.cfg.budgets.limits.max_neighbor_bytes,
                            ),
                        })
                        .collect(),
                };
                return self.transition(
                    Phase::ClassifyInFlight,
                    json!({"classify": candidates, "ranking": explained}),
                    |s| {
                        s.classification_used = true;
                        s.decisions_used += 1;
                        s.classify_packet = Some(packet);
                    },
                );
            }
        };
        self.prepare_packet(&index, &id)
    }

    fn acceptance_view(&self) -> Acceptance {
        let b = self.state.base_acceptance.as_ref();
        Acceptance {
            command: self.task.acceptance.clone(),
            exit_status: b.and_then(|b| b.exit_status),
            failure_excerpt: b.map(|b| b.excerpt.clone()).unwrap_or_default(),
        }
    }

    fn prepare_packet(&mut self, index: &SymbolIndex, id: &str) -> Result<(), LoopError> {
        let Some(record) = index.get(id).cloned() else {
            return self.finish(termination(
                TerminationReason::NoTarget,
                "no_target",
                format!("{id} is not in the index"),
            ));
        };
        let same = self
            .state
            .target
            .as_ref()
            .is_some_and(|t| t.id == record.id);
        let attempt = self.state.target.as_ref().map(|t| t.attempt).unwrap_or(0) + u32::from(!same);
        let limits = self.cfg.budgets.limits;
        let text = std::fs::read_to_string(self.root.join(&record.path)).unwrap_or_default();
        let source = text
            .get(record.item.start..record.item.end)
            .unwrap_or_default()
            .to_string();
        let rejected: Vec<Rejected> = if same {
            self.state
                .rejected
                .iter()
                .rev()
                .take(2)
                .rev()
                .map(|r| Rejected {
                    body_digest: r.body_digest.clone(),
                    body: r.body.clone(),
                    error: r.error.clone(),
                })
                .collect()
        } else {
            Vec::new()
        };
        let context_left = self
            .cfg
            .budgets
            .max_context_requests
            .saturating_sub(self.state.context_requests_used);
        let mut allowed = vec![Op::ReplaceSymbolBody, Op::NoChange];
        if context_left > 0 {
            allowed.push(Op::NeedSymbol);
        }
        let packet = TaskPacket {
            version: contract::PACKET_VERSION.to_string(),
            goal: self.task.goal.clone(),
            acceptance: self.acceptance_view(),
            symbol: SymbolRef {
                id: record.id.clone(),
                path: record.path.clone(),
                name: record.name.clone(),
                start_line: record.start_line,
                end_line: record.end_line,
                fingerprint: record.fingerprint.clone(),
                source,
            },
            neighbors: neighbors(
                index,
                &record,
                limits.max_neighbors,
                limits.max_neighbor_bytes,
            ),
            rejected,
            constraints: Constraints {
                signature_frozen: self.task.signature_frozen,
                allowed_ops: allowed,
                insertion_after: None,
                max_body_bytes: limits.max_body_bytes,
                context_requests_left: context_left,
            },
            context: if same {
                self.state.context.clone()
            } else {
                Vec::new()
            },
        };
        let bytes = match packet.checked_bytes(&limits) {
            Ok(b) => b,
            Err(e) => {
                return self.finish(termination(
                    TerminationReason::PacketTooLarge,
                    "packet_too_large",
                    e.to_string(),
                ));
            }
        };
        let digest = sha256(&bytes);
        let target = TargetState {
            id: record.id.clone(),
            fingerprint: record.fingerprint.clone(),
            attempt,
        };
        let ranking = self.ranking.take();
        self.transition(
            Phase::Decide,
            json!({"target": target, "packet_digest": digest, "packet_bytes": bytes.len(), "ranking": ranking}),
            |s| {
                if !same {
                    s.rejected.clear();
                    s.context.clear();
                }
                s.target = Some(target);
                s.packet = Some(packet);
                s.packet_digest = Some(digest);
            },
        )
    }

    async fn classify(&mut self) -> Result<(), LoopError> {
        let packet = self
            .state
            .classify_packet
            .clone()
            .expect("classify packet persisted");
        let reply = self.fx.classify(&packet).await;
        let reply = match reply {
            Ok(r) => r,
            Err(f) => {
                return self.finish(termination(
                    TerminationReason::InfrastructureFailure,
                    "infrastructure_failure",
                    f.message,
                ));
            }
        };
        self.count_usage(&reply);
        match contract::validate_classification(&reply.text, &packet, &self.cfg.budgets.limits) {
            Ok(id) => {
                let Some(index) = self.index()? else {
                    return Ok(());
                };
                self.transition(Phase::Target, json!({"classified": id}), |s| {
                    s.decisions.push(json!({"classify": id}));
                    s.classified_target = Some(id.clone());
                })?;
                self.prepare_packet(&index, &id)
            }
            Err(e) => self.finish(termination(
                TerminationReason::InvalidDecision,
                reply_class(&e),
                format!("classification: {e}"),
            )),
        }
    }

    fn count_usage(&mut self, reply: &crate::fx::FxReply) {
        match (reply.input_tokens, reply.output_tokens) {
            (Some(i), Some(o)) => {
                self.state.usage.input_tokens += i;
                self.state.usage.output_tokens += o;
            }
            _ => self.state.usage.unreported_calls += 1,
        }
    }

    async fn decide(&mut self) -> Result<(), LoopError> {
        if self.state.decisions_used >= self.cfg.budgets.max_decisions {
            return self.finish(termination(
                TerminationReason::BudgetExceeded,
                "budget_exceeded",
                format!("all {} decisions were used", self.cfg.budgets.max_decisions),
            ));
        }
        if self.remaining().is_zero() {
            return self.finish(termination(
                TerminationReason::BudgetExceeded,
                "budget_exceeded",
                "the elapsed-time budget ran out before a decision",
            ));
        }
        let digest = self.state.packet_digest.clone();
        // The intent, committed before the call: a resume here never asks again.
        self.transition(
            Phase::DecisionInFlight,
            json!({"packet_digest": digest}),
            |s| s.decisions_used += 1,
        )
    }

    async fn in_flight(&mut self) -> Result<(), LoopError> {
        let packet = self
            .state
            .packet
            .clone()
            .expect("packet persisted with the intent");
        let limits = self.cfg.budgets.limits;
        let bytes = match packet.checked_bytes(&limits) {
            Ok(b) => b,
            Err(e) => {
                return self.finish(termination(
                    TerminationReason::PacketTooLarge,
                    "packet_too_large",
                    e.to_string(),
                ));
            }
        };
        let reply = match self.fx.decide(&packet, &bytes).await {
            Ok(r) => r,
            Err(f) => {
                return self.finish(termination(
                    TerminationReason::InfrastructureFailure,
                    "infrastructure_failure",
                    f.message,
                ));
            }
        };
        self.count_usage(&reply);
        let reply_digest = sha256(reply.text.as_bytes());
        let decision = match contract::validate_decision(&reply.text, &packet, &limits) {
            Ok(d) => d,
            Err(e) => {
                let class = reply_class(&e);
                return self.reject(
                    &reply.text,
                    format!("invalid decision: {e}"),
                    class,
                    Phase::Target,
                );
            }
        };
        let record = json!({"decision": decision, "reply_digest": reply_digest});
        match decision {
            Decision::NoChange { reason } => self.reject(
                "",
                format!(
                    "no_change proposed ({}); the acceptance command still fails",
                    contract::head(&reason, 200)
                ),
                "test_still_fails",
                Phase::Target,
            ),
            Decision::InsertSymbol { after, declaration } => {
                let error =
                    edit::prepare_insert(packet.constraints.insertion_after.as_deref(), &after)
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_default();
                self.reject(&declaration, error, "bad_symbol", Phase::Target)
            }
            Decision::NeedSymbol { symbol } => {
                let Some(index) = self.index()? else {
                    return Ok(());
                };
                let authorized = packet.neighbors.iter().any(|n| n.id == symbol);
                match index.get(&symbol).filter(|_| authorized) {
                    Some(s) => {
                        let text =
                            std::fs::read_to_string(self.root.join(&s.path)).unwrap_or_default();
                        let source = contract::head(
                            text.get(s.item.start..s.item.end).unwrap_or_default(),
                            limits.max_source_bytes,
                        );
                        let ctx = ContextSymbol {
                            id: s.id.clone(),
                            source,
                        };
                        let target_id = packet.symbol.id.clone();
                        self.transition(Phase::Target, record, |st| {
                            st.decisions.push(json!({"need_symbol": ctx.id}));
                            st.context.push(ctx);
                            st.context_requests_used += 1;
                            st.packet = None;
                        })?;
                        self.prepare_packet(&index, &target_id)
                    }
                    None => self.reject(
                        &symbol,
                        format!(
                            "need_symbol: {symbol:?} is not an authorized neighbor in the index"
                        ),
                        "bad_symbol",
                        Phase::Target,
                    ),
                }
            }
            Decision::ReplaceSymbolBody { symbol, body } => {
                let Some(index) = self.index()? else {
                    return Ok(());
                };
                let Some(current) = index.get(&symbol).cloned() else {
                    return self.reject(
                        &body,
                        format!("{symbol} is not in the current checkout"),
                        "bad_symbol",
                        Phase::Target,
                    );
                };
                // The record the packet was built from: its fingerprint, not today's.
                let as_sent = SymbolRecord {
                    fingerprint: packet.symbol.fingerprint.clone(),
                    ..current
                };
                match edit::prepare_replace_body(
                    self.root,
                    &as_sent,
                    &body,
                    self.task.signature_frozen,
                ) {
                    Ok(prepared) => {
                        let text = std::fs::read_to_string(self.root.join(&prepared.path))
                            .unwrap_or_default();
                        let patch = PatchRecord {
                            patch_id: prepared.patch_id.clone(),
                            symbol: prepared.symbol.clone(),
                            path: prepared.path.clone(),
                            before_sha: prepared.before_sha.clone(),
                            after_sha: prepared.after_sha.clone(),
                            body_digest: prepared.body_digest.clone(),
                            body: body.clone(),
                            diff: prepared.diff.clone(),
                            before_text: text,
                            after_text: prepared.new_text.clone(),
                        };
                        let detail = json!({"decision": record["decision"], "patch_id": patch.patch_id, "diff": patch.diff});
                        self.transition(Phase::Apply, detail, |s| {
                            s.valid_bodies += 1;
                            s.decisions.push(record);
                            s.pending_patch = Some(patch);
                        })
                    }
                    Err(EditError::StaleSource(m)) => self.finish(termination(
                        TerminationReason::StaleSource,
                        "stale_source",
                        m,
                    )),
                    Err(EditError::Io(m)) => self.finish(termination(
                        TerminationReason::InfrastructureFailure,
                        "infrastructure_failure",
                        m,
                    )),
                    Err(e) => {
                        let class = e.class();
                        self.reject(&body, e.to_string(), class, Phase::Target)
                    }
                }
            }
        }
    }

    fn apply(&mut self) -> Result<(), LoopError> {
        let patch = self
            .state
            .pending_patch
            .clone()
            .expect("patch persisted with the intent");
        match edit::write_checked(self.root, &patch.path, &patch.before_sha, &patch.after_sha, &patch.after_text) {
            Ok(outcome) => self.transition(Phase::Acceptance, json!({"applied": patch.patch_id, "already_applied": outcome == WriteOutcome::AlreadyApplied}), |s| {
                s.under_test = Some(patch);
                s.pending_patch = None;
            }),
            Err(EditError::StaleSource(m)) => self.finish(termination(TerminationReason::StaleSource, "stale_source", m)),
            Err(e) => self.finish(termination(TerminationReason::InfrastructureFailure, "infrastructure_failure", e.to_string())),
        }
    }

    fn revert(&mut self) -> Result<(), LoopError> {
        let patch = self
            .state
            .under_test
            .clone()
            .expect("the patch under test is reverted");
        match edit::write_checked(
            self.root,
            &patch.path,
            &patch.after_sha,
            &patch.before_sha,
            &patch.before_text,
        ) {
            Ok(outcome) => {
                let stop = self.state.stop_after_revert.clone();
                self.transition(Phase::Target, json!({"reverted": patch.patch_id, "already_reverted": outcome == WriteOutcome::AlreadyApplied}), |s| {
                    s.under_test = None;
                    s.stop_after_revert = None;
                })?;
                match stop {
                    Some(t) => self.finish(t),
                    None => Ok(()),
                }
            }
            Err(EditError::StaleSource(m)) => self.finish(termination(
                TerminationReason::StaleSource,
                "stale_source",
                m,
            )),
            Err(e) => self.finish(termination(
                TerminationReason::InfrastructureFailure,
                "infrastructure_failure",
                e.to_string(),
            )),
        }
    }

    fn result(&self, finished: bool) -> LoopResult {
        LoopResult {
            run_id: self.state.run_id.clone(),
            termination: self.state.termination.clone(),
            finished,
            decisions: self.state.decisions_used,
            valid_bodies: self.state.valid_bodies,
            acceptance_runs: self.state.acceptance_runs,
            patch_applied: self.state.applied.clone(),
            rejections: self.state.rejection_log.clone(),
            elapsed_ms: self.state.elapsed_ms,
            usage: self.state.usage.clone(),
        }
    }
}

/// Signatures of the symbols around `target`: those it calls, then the rest of its file. Bodies are
/// never included.
fn neighbors(
    index: &SymbolIndex,
    target: &SymbolRecord,
    max: usize,
    max_bytes: usize,
) -> Vec<Neighbor> {
    let called = index
        .symbols
        .values()
        .filter(|s| !s.is_test && s.id != target.id && target.calls.contains(&s.name));
    let same_file = index
        .symbols
        .values()
        .filter(|s| !s.is_test && s.id != target.id && s.path == target.path);
    let mut out: Vec<Neighbor> = Vec::new();
    for s in called.chain(same_file) {
        if out.len() >= max {
            break;
        }
        if !out.iter().any(|n| n.id == s.id) {
            out.push(Neighbor {
                id: s.id.clone(),
                signature: contract::head(&s.declaration, max_bytes),
            });
        }
    }
    out
}

fn clean_worktree(root: &Path) -> Result<(), String> {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err("the candidate worktree is not a git checkout".into());
    }
    if !out.stdout.is_empty() {
        return Err("the candidate worktree is not clean".into());
    }
    Ok(())
}

/// Runs (or resumes) one repair task in `root`, a clean candidate worktree, until it ends or is
/// halted. Only the acceptance command can end it in success.
pub async fn run(
    task: &TaskDefinition,
    root: &Path,
    store: &LoopStore,
    fx: &dyn Fx,
    cfg: &LoopConfig,
) -> Result<LoopResult, LoopError> {
    if task.schema != TASK_SCHEMA
        || task.acceptance.is_empty()
        || task.goal.trim().is_empty()
        || task.goal.contains('\n')
    {
        return Err(LoopError::Definition(
            "schema, a one-line goal and an acceptance command are required".into(),
        ));
    }
    let loaded = store.load(&cfg.run_id)?;
    let resumed = loaded.is_some();
    let (version, state) = match loaded {
        Some((v, s)) => (Some(v), s),
        None => (
            None,
            RunState {
                schema: RUN_SCHEMA.to_string(),
                experiment_id: cfg.experiment_id.clone(),
                run_id: cfg.run_id.clone(),
                task_id: task.task_id.clone(),
                seq: 0,
                phase: Phase::Acceptance,
                elapsed_ms: 0,
                decisions_used: 0,
                classification_used: false,
                classified_target: None,
                valid_bodies: 0,
                acceptance_runs: 0,
                target: None,
                base_acceptance: None,
                last_acceptance: None,
                rejected: Vec::new(),
                rejection_log: Vec::new(),
                context: Vec::new(),
                context_requests_used: 0,
                classify_packet: None,
                packet: None,
                packet_digest: None,
                decisions: Vec::new(),
                pending_patch: None,
                under_test: None,
                applied: None,
                stop_after_revert: None,
                usage: Usage::default(),
                termination: None,
            },
        ),
    };
    let mut s = Session {
        task,
        root,
        store,
        fx,
        cfg,
        state,
        version,
        since_commit: Instant::now(),
        committed_here: 0,
        ranking: None,
    };
    if !resumed {
        let check = clean_worktree(root);
        s.transition(
            Phase::Acceptance,
            json!({"started": {"task_id": task.task_id, "experiment_id": cfg.experiment_id}}),
            |_| {},
        )?;
        if let Err(why) = check {
            s.finish(termination(
                TerminationReason::InfrastructureFailure,
                "infrastructure_failure",
                why,
            ))?;
        }
    } else if matches!(
        s.state.phase,
        Phase::DecisionInFlight | Phase::ClassifyInFlight
    ) {
        // The call may or may not have been made; it is not made again.
        s.finish(termination(
            TerminationReason::InferenceOutcomeUnknown,
            "inference_outcome_unknown",
            "resumed after a decision request whose outcome was not recorded; it is not reissued",
        ))?;
    }
    loop {
        if s.state.phase == Phase::Done {
            return Ok(s.result(true));
        }
        if s.halted() {
            return Ok(s.result(false));
        }
        match s.state.phase {
            Phase::Acceptance => s.acceptance()?,
            Phase::Target => s.target()?,
            Phase::ClassifyInFlight => s.classify().await?,
            Phase::Decide => s.decide().await?,
            Phase::DecisionInFlight => s.in_flight().await?,
            Phase::Apply => s.apply()?,
            Phase::Revert => s.revert()?,
            Phase::Done => unreachable!(),
        }
    }
}
