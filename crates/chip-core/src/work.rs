//! The bounded autonomous work loop.
//!
//! One decision per iteration: a decision may complete the work, escalate it, block it, or request
//! exactly one capability. A request becomes at most one execution, one observation and, where
//! the rules allow, one piece of evidence, and the loop then makes the next decision without the
//! caller starting another turn. The loop is finite by construction: [`WorkLimits`] bound both
//! turns and executions, and every run ends in an explicit [`WorkOutcome`].
//!
//! Where each decision comes from, in order:
//!
//! 1. **Local.** A [`LocalWorkPolicy`] may propose the next decision. A proposed capability
//!    request is first assessed with the evidence machinery (`Agent::assess_evidence`): valid
//!    evidence is reused and nothing is executed; otherwise the local reasoner either lets the
//!    request proceed or escalates. The PR25 learned model is not wired in anywhere: it is not an
//!    authority for autonomous `Continue`.
//! 2. **Model (FX).** When the local side cannot decide, the loop builds an [`EscalationContext`],
//!    emits a `ModelEscalation` event carrying its measurements, calls the model exactly once and
//!    interprets the response through a [`WorkDecisionBoundary`]. There is no retry.
//!
//! Reality comes only from the execution and observation path. A capability request, a model
//! response or a plan is never evidence that anything happened: only an observation of an
//! execution this loop performed is recorded as evidence, and a cancelled execution never is.
//!
//! The loop discovers nothing: capabilities are described once, when the work starts. It scans
//! no repository, builds no graph, reads no history from storage and opens no connection beyond
//! the one model call an escalation makes.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use chip_inference::{Delivery, ModelResponse, RemoteCancellation};

use crate::{
    Agent, AgentDecision, AgentError, Assessment, Capability, CapabilityEvent, CapabilityId,
    CapabilityRequest, DecisionBoundary, DecisionError, EvidenceLookup, ExecutionError,
    ExecutionEvent, ExecutionId, ExecutionResult, InputValue, Observation, ObservationKind,
    ReasoningError, StateToken, Turn,
};

/// Opaque identity of one workload. It carries no meaning.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkId(String);

impl WorkId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the work is for. A plain string; there is no goal language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkGoal(String);

impl WorkGoal {
    pub fn new(goal: impl Into<String>) -> Self {
        Self(goal.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Consecutive malformed model replies a work recovers from unless its spec says otherwise (see
/// [`WorkSpec::malformed_reply_limit`]): two rejected replies in a row are answered with their
/// diagnostic; a third ends the work.
pub const DEFAULT_MALFORMED_REPLY_LIMIT: usize = 2;

/// Explicit, finite bounds. There is no way to run without them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkLimits {
    /// Most decisions (iterations) the work may make.
    pub max_turns: usize,
    /// Most executions the work may perform.
    pub max_executions: usize,
}

impl Default for WorkLimits {
    fn default() -> Self {
        Self {
            max_turns: 8,
            max_executions: 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    Turns,
    Executions,
    /// The next model request would have exceeded the configured context budget.
    Context,
}

impl LimitKind {
    pub fn name(self) -> &'static str {
        match self {
            LimitKind::Turns => "turns",
            LimitKind::Executions => "executions",
            LimitKind::Context => "context",
        }
    }
}

/// What one iteration decided. `RequestCapability` is data: it is not execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkDecision {
    /// The goal is complete.
    Complete { summary: String },
    /// Intervention outside this bounded loop is required.
    Escalate { reason: String },
    /// The work cannot proceed: something it requires is unavailable.
    Block { reason: String },
    /// Ask for exactly one capability.
    RequestCapability(CapabilityRequest),
}

impl WorkDecision {
    /// A short, secret-free label for events and context.
    pub fn label(&self) -> String {
        match self {
            WorkDecision::Complete { .. } => "complete".to_string(),
            WorkDecision::Escalate { .. } => "escalate".to_string(),
            WorkDecision::Block { .. } => "block".to_string(),
            WorkDecision::RequestCapability(r) => format!("request {}", r.capability_id),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionSource {
    Local,
    Model,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRecord {
    pub turn: usize,
    pub source: DecisionSource,
    pub decision: WorkDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalState {
    Completed,
    Escalated,
    Blocked,
    LimitReached,
    Failed,
    /// The work's owner told it to stop (a cancellation request or the whole-work deadline).
    Cancelled,
}

impl TerminalState {
    pub fn name(self) -> &'static str {
        match self {
            TerminalState::Completed => "completed",
            TerminalState::Escalated => "escalated",
            TerminalState::Blocked => "blocked",
            TerminalState::LimitReached => "limit_reached",
            TerminalState::Failed => "failed",
            TerminalState::Cancelled => "cancelled",
        }
    }
}

/// How the work ended. Limits and failures are never reported as success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkOutcome {
    Completed {
        summary: String,
    },
    Escalated {
        reason: String,
    },
    Blocked {
        reason: String,
    },
    LimitReached {
        limit: LimitKind,
    },
    /// The work failed. `delivery` is set when a model call failed: what the inference executor
    /// established about whether that request left the process (`definitely_unsent`,
    /// `possibly_sent` or `confirmed_response`), so a caller deciding whether retrying is safe
    /// need not parse `reason`. `None` means the failure was not a model call that reported a
    /// delivery certainty; a caller must then not assume nothing was sent. Chip never retries.
    Failed {
        reason: String,
        delivery: Option<Delivery>,
    },
    /// The work's owner told it to stop, through [`WorkControl`]. The loop stops at its next
    /// boundary: before another model call or execution starts, or when a model call in flight
    /// returns after being abandoned. `delivery` is set when a model call was in flight and
    /// failed because of the stop: what the inference executor established about that request.
    /// Nothing that already executed is undone.
    Cancelled {
        cause: StopCause,
        reason: String,
        delivery: Option<Delivery>,
        /// For a model call in flight when the stop arrived: whether its remote side's stop was
        /// confirmed. `Unconfirmed` means the request may still be running remotely.
        remote: Option<RemoteCancellation>,
    },
}

/// Why a work's owner told it to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StopCause {
    /// A cancellation request (an operator, a signal, a client).
    Cancelled,
    /// The whole-work deadline passed.
    Deadline,
}

impl StopCause {
    pub fn name(self) -> &'static str {
        match self {
            StopCause::Cancelled => "cancelled",
            StopCause::Deadline => "deadline",
        }
    }
}

/// What the owner of one run shares with the loop: whether it should stop, and every event as
/// it is recorded. The loop asks [`stop_requested`](Self::stop_requested) before each model call
/// and before each execution, so neither a cancellation nor a deadline can be extended by starting
/// another one. It is cooperative: an execution already in flight is not interrupted.
pub trait WorkControl: Send + Sync {
    /// `Some` once the work should stop.
    fn stop_requested(&self) -> Option<StopCause>;

    /// Called with each event, in order, as the loop records it. The same events end up in the
    /// report; this only makes them visible earlier. Default: nothing.
    fn on_event(&self, _event: &WorkEvent) {}

    /// Called with each response a model call returned, after its `ModelCalled` event and before
    /// the response is interpreted or anything it asks for is attempted. `observed` is the
    /// executions whose observations the work held when it asked, in order. A durable owner
    /// records it, so that a run interrupted before acting on it can be given it back
    /// ([`WorkSpec::restored_response`]) instead of asking, and paying, again. Default: nothing.
    fn on_model_response(
        &self,
        _turn: usize,
        _model: &str,
        _response: &ModelResponse,
        _observed: &[ExecutionId],
    ) {
    }
}

impl WorkOutcome {
    pub fn terminal_state(&self) -> TerminalState {
        match self {
            WorkOutcome::Completed { .. } => TerminalState::Completed,
            WorkOutcome::Escalated { .. } => TerminalState::Escalated,
            WorkOutcome::Blocked { .. } => TerminalState::Blocked,
            WorkOutcome::LimitReached { .. } => TerminalState::LimitReached,
            WorkOutcome::Failed { .. } => TerminalState::Failed,
            WorkOutcome::Cancelled { .. } => TerminalState::Cancelled,
        }
    }
}

/// What a local policy may look at. Everything is already in memory.
#[derive(Debug, Clone, Copy)]
pub struct WorkView<'a> {
    pub goal: &'a WorkGoal,
    /// Zero-based index of the iteration being decided.
    pub turn: usize,
    pub executions: usize,
    /// Observations so far: of executions the loop performed, and of evidence it reused.
    pub observations: &'a [Observation],
    pub decisions: &'a [DecisionRecord],
}

/// Supplies the next decision when Chip can decide locally. Pure and synchronous: it sees only
/// the view. `None` means "I cannot", and the loop escalates to the model.
pub trait LocalWorkPolicy: Send + Sync {
    fn propose(&self, view: &WorkView<'_>) -> Option<WorkDecision>;
}

/// A policy that never decides: every iteration escalates to the model.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoLocalPolicy;

impl LocalWorkPolicy for NoLocalPolicy {
    fn propose(&self, _view: &WorkView<'_>) -> Option<WorkDecision> {
        None
    }
}

/// Deterministic policy for tests and demos: the decision for iteration `n` is entry `n`;
/// `None` (or running past the end) escalates.
#[derive(Debug, Clone, Default)]
pub struct ScriptedPolicy {
    decisions: Vec<Option<WorkDecision>>,
}

impl ScriptedPolicy {
    pub fn new(decisions: Vec<Option<WorkDecision>>) -> Self {
        Self { decisions }
    }
}

impl LocalWorkPolicy for ScriptedPolicy {
    fn propose(&self, view: &WorkView<'_>) -> Option<WorkDecision> {
        self.decisions.get(view.turn).cloned().flatten()
    }
}

/// Interprets a model response as a work decision. Pure and synchronous: model output is data,
/// and only this boundary decides what, if anything, it means.
pub trait WorkDecisionBoundary: Send + Sync {
    fn interpret(
        &self,
        response: &ModelResponse,
        capabilities: &[Capability],
    ) -> Result<WorkDecision, DecisionError>;

    /// What the model is asked, given the capabilities that were described when the work started.
    /// It is part of the escalation context and counted in its measurements. A boundary that
    /// expects a particular reply format says so here.
    fn question(&self, _capabilities: &[Capability]) -> String {
        DEFAULT_QUESTION.to_string()
    }
}

const DEFAULT_QUESTION: &str =
    "Decide the next step: respond if the goal is complete, or request one capability.";

/// Adapts the existing [`DecisionBoundary`]: a capability request stays a request, and a plain
/// response is the model saying the work is done. Neither is evidence that anything occurred.
pub struct RespondCompletes<B: DecisionBoundary>(pub B);

impl<B: DecisionBoundary> WorkDecisionBoundary for RespondCompletes<B> {
    fn interpret(
        &self,
        response: &ModelResponse,
        capabilities: &[Capability],
    ) -> Result<WorkDecision, DecisionError> {
        Ok(match self.0.decide(response, capabilities)? {
            AgentDecision::Respond(r) => WorkDecision::Complete { summary: r.output },
            AgentDecision::RequestCapability(r) => WorkDecision::RequestCapability(r),
        })
    }
}

/// What goes into one model escalation, and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationContext {
    pub goal: String,
    pub current_state: String,
    /// What the evidence store said about the candidate this escalation is about, if any.
    pub relevant_evidence: Vec<String>,
    /// Observations so far. Handed to the model as the existing observation messages.
    pub relevant_observations: Vec<Observation>,
    pub prior_decisions: Vec<String>,
    /// Capabilities already tried and known to have failed, or to have run without satisfying the
    /// goal.
    pub ruled_out: Vec<String>,
    /// Observations left out of this request because a later, identical observation is in it. Each
    /// entry is a fact Chip established; nothing here is a summary of what an observation said.
    pub omitted: Vec<String>,
    /// Observations left out of this request because the policy states, in `facts`, what they
    /// establish. Unlike `omitted`, nothing later repeats them, so Chip checks each one before the
    /// request is sent (see [`CondensedObservation`]). The observation itself stays recorded.
    pub condensed: Vec<CondensedObservation>,
    pub question: String,
}

/// A recorded observation that one request carries as facts instead of in full.
///
/// This is a Chip-owned, deterministic reduction of what a request retransmits; it is never a
/// model's summary and never evidence. The observation stays in the work report with its execution
/// ID and receipt, and the request says it was left out and how many bytes that withheld. Chip
/// refuses to send a request whose condensation names an observation that was not recorded, was
/// not a successful execution, is repeated, or is also sent in full.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CondensedObservation {
    /// The recorded observation this stands in for.
    pub execution_id: ExecutionId,
    /// Bytes of that observation's rendered form this request does not carry.
    pub withheld_bytes: usize,
    /// What the observation establishes, each derived from its recorded output and nothing else.
    pub facts: Vec<String>,
}

/// Measurements of one escalation's context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextMetrics {
    /// Bytes of every message sent: the rendered context plus each rendered observation.
    pub bytes: usize,
    pub chars: usize,
    pub observations: usize,
    pub decisions: usize,
    pub evidence_items: usize,
    pub ruled_out: usize,
    /// The request's messages by role: the instructions (goal and question) as one system message
    /// first, each observation as one system message, and the rest of the rendered context as one
    /// user message, last. Never an assistant message.
    pub messages: usize,
    pub system_messages: usize,
    pub user_messages: usize,
    pub assistant_messages: usize,
    /// Observations the request leaves out because a later identical one is in it.
    pub omitted_observations: usize,
    /// Observations the request carries as facts instead of in full (see [`CondensedObservation`]).
    pub condensed_observations: usize,
}

/// How the user message ends: the question itself travels with the instructions, first.
pub const NEXT_STEP: &str = "Next: decide the next step toward the goal, under the rules of the question above. Reply with exactly one JSON decision object and nothing else.";

impl EscalationContext {
    /// Everything the request says besides the observations: the instructions, then the turn.
    pub fn render(&self) -> String {
        format!("{}\n{}", self.render_instructions(), self.render_turn())
    }

    /// The text sent first, as a system message ahead of the observations: the goal and the
    /// question, which states the reply contract. Neither changes from one turn of a work to the
    /// next, so a provider's prefix cache can reuse it and the observations that follow it.
    pub fn render_instructions(&self) -> String {
        format!("Goal: {}\nQuestion: {}", self.goal, self.question)
    }

    /// The text of the user message, sent last: where the work stands now. Observations are not
    /// in it; they travel as their own messages, between the instructions and this.
    pub fn render_turn(&self) -> String {
        let mut out = format!("State: {}\n", self.current_state);
        out.push_str("Evidence:\n");
        for item in &self.relevant_evidence {
            out.push_str(&format!("  - {item}\n"));
        }
        out.push_str("Prior decisions:\n");
        for item in &self.prior_decisions {
            out.push_str(&format!("  - {item}\n"));
        }
        out.push_str("Ruled out:\n");
        for item in &self.ruled_out {
            out.push_str(&format!("  - {item}\n"));
        }
        if !self.omitted.is_empty() {
            out.push_str("Omitted (identical to a later observation):\n");
            for item in &self.omitted {
                out.push_str(&format!("  - {item}\n"));
            }
        }
        if !self.condensed.is_empty() {
            out.push_str("Condensed (recorded in full; not retransmitted in this request):\n");
            for item in &self.condensed {
                out.push_str(&format!(
                    "  - execution {} ({} bytes withheld; request the capability again to see it in full):\n",
                    item.execution_id, item.withheld_bytes
                ));
                for fact in &item.facts {
                    out.push_str(&format!("      {fact}\n"));
                }
            }
        }
        out.push_str(NEXT_STEP);
        out
    }

    pub fn metrics(&self) -> ContextMetrics {
        let (instructions, turn) = (self.render_instructions(), self.render_turn());
        let rendered: Vec<String> = self
            .relevant_observations
            .iter()
            .map(Observation::render)
            .collect();
        ContextMetrics {
            bytes: instructions.len()
                + turn.len()
                + rendered.iter().map(String::len).sum::<usize>(),
            chars: instructions.chars().count()
                + turn.chars().count()
                + rendered.iter().map(|r| r.chars().count()).sum::<usize>(),
            observations: self.relevant_observations.len(),
            decisions: self.prior_decisions.len(),
            evidence_items: self.relevant_evidence.len(),
            ruled_out: self.ruled_out.len(),
            messages: self.relevant_observations.len() + 2,
            system_messages: self.relevant_observations.len() + 1,
            user_messages: 1,
            assistant_messages: 0,
            omitted_observations: self.omitted.len(),
            condensed_observations: self.condensed.len(),
        }
    }
}

/// Where the workload stands, as the loop knows it.
#[derive(Debug, Clone, Copy)]
pub struct WorkState<'a> {
    pub goal: &'a str,
    /// Zero-based turn of the decision being made.
    pub turn: usize,
    pub max_turns: usize,
    pub executions: usize,
    pub max_executions: usize,
    /// Configured context budget in bytes, when one was set. Policies may use this
    /// to select a context that fits within the budget before serialization.
    pub context_budget_bytes: Option<usize>,
}

/// What has happened so far, plus what this escalation is about.
#[derive(Debug, Clone, Copy)]
pub struct WorkTrajectory<'a> {
    pub observations: &'a [Observation],
    /// Where each observation came from, one per observation, in the same order.
    pub origins: &'a [ObservationOrigin],
    pub decisions: &'a [DecisionRecord],
    /// Capabilities already tried and known to have failed.
    pub ruled_out: &'a [String],
    /// What the evidence store said about the candidate this escalation is about.
    pub evidence: &'a [String],
    /// The boundary's question, which states the reply contract.
    pub question: &'a str,
}

/// Chooses what one model escalation is told. Chip's opinion about relevance lives here, not in
/// FX and not in the loop.
///
/// A policy is a pure function of what it is given: it cannot execute anything, observe anything
/// or reach a provider, and it does not measure itself. Measurement counts the request that is
/// actually serialized for the provider.
pub trait EscalationContextPolicy: Send + Sync {
    /// A stable, non-secret identifier (for example `full-v1`), recorded with every escalation.
    fn id(&self) -> &'static str;

    fn build(&self, state: &WorkState<'_>, trajectory: &WorkTrajectory<'_>) -> EscalationContext;
}

/// The default: everything the loop knows, in the order the loop has always sent it.
#[derive(Debug, Clone, Copy, Default)]
pub struct FullEscalationContext;

impl FullEscalationContext {
    pub const ID: &'static str = "full-v1";
}

impl EscalationContextPolicy for FullEscalationContext {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn build(&self, state: &WorkState<'_>, trajectory: &WorkTrajectory<'_>) -> EscalationContext {
        EscalationContext {
            goal: state.goal.to_string(),
            current_state: format!(
                "turn {} of {}; executions {} of {}",
                state.turn + 1,
                state.max_turns,
                state.executions,
                state.max_executions
            ),
            relevant_evidence: trajectory.evidence.to_vec(),
            relevant_observations: trajectory.observations.to_vec(),
            prior_decisions: trajectory
                .decisions
                .iter()
                .map(|d| {
                    format!(
                        "turn {} ({}): {}",
                        d.turn + 1,
                        if d.source == DecisionSource::Local {
                            "local"
                        } else {
                            "model"
                        },
                        d.decision.label()
                    )
                })
                .collect(),
            ruled_out: trajectory.ruled_out.to_vec(),
            omitted: Vec::new(),
            condensed: Vec::new(),
            question: trajectory.question.to_string(),
        }
    }
}

/// How an observation relates to the ones before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationClass {
    /// The first observation of its capability.
    New,
    /// The same invocation (capability and inputs) as an earlier observation, with the same result.
    RepeatedIdentical,
    /// The capability was observed before, but with different inputs.
    NewFromSameCapability,
    /// The same invocation as an earlier observation, with a different result: reality changed
    /// between the two, and the later observation is what is now true.
    ChangedReality,
}

/// Where one observation came from, recorded by Chip when it performed the request. Never read
/// from a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationOrigin {
    pub capability: CapabilityId,
    /// The capability and its complete inputs: two observations with the same key answered the same
    /// request.
    pub invocation: String,
    /// The capability's own contract: may an earlier observation of it stand in for a later one?
    /// Only such an observation may ever be left out of a request.
    pub reusable: bool,
    /// The provider's id for the model response that asked for this request, when a model did and
    /// the provider gave one. Correlation metadata: not the execution's identity, not a key.
    pub provider_response_id: Option<String>,
}

/// Two observations say the same thing about reality: same kind, status and output. Their
/// identifiers and receipts differ by construction and say nothing about reality.
fn same_reality(a: &Observation, b: &Observation) -> bool {
    a.kind == b.kind && a.status == b.status && a.output == b.output
}

/// Classifies each observation against the ones before it.
pub fn classify_observations(
    origins: &[ObservationOrigin],
    observations: &[Observation],
) -> Vec<ObservationClass> {
    (0..observations.len().min(origins.len()))
        .map(|i| {
            let earlier = (0..i).rev();
            let same_call = earlier
                .clone()
                .find(|j| origins[*j].invocation == origins[i].invocation);
            match same_call {
                Some(j) if same_reality(&observations[j], &observations[i]) => {
                    ObservationClass::RepeatedIdentical
                }
                Some(_) => ObservationClass::ChangedReality,
                None if earlier
                    .clone()
                    .any(|j| origins[j].capability == origins[i].capability) =>
                {
                    ObservationClass::NewFromSameCapability
                }
                None => ObservationClass::New,
            }
        })
        .collect()
}

/// The observations that may be left out of a request: `(omitted, kept)` index pairs. An
/// observation is left out only when its capability's contract says an earlier observation may
/// stand in for a later one, and a *later observation of the same invocation with the same result*
/// is in the request. The later one was performed and observed; the earlier one adds nothing it does
/// not say. Anything else (a non-reusable capability, or a result that changed) is always kept.
pub fn omissions(
    origins: &[ObservationOrigin],
    observations: &[Observation],
) -> Vec<(usize, usize)> {
    let n = observations.len().min(origins.len());
    (0..n)
        .filter(|i| origins[*i].reusable)
        .filter_map(|i| {
            (i + 1..n)
                .rev()
                .find(|j| {
                    origins[*j].invocation == origins[i].invocation
                        && same_reality(&observations[i], &observations[*j])
                })
                .map(|j| (i, j))
        })
        .collect()
}

/// Like [`FullEscalationContext`], except that an observation a capability's contract allows to be
/// left out, and that a later identical observation makes redundant, is left out and named. Nothing
/// is summarised, truncated or reordered.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeduplicatedEscalationContext;

impl DeduplicatedEscalationContext {
    pub const ID: &'static str = "dedup-v1";
}

impl EscalationContextPolicy for DeduplicatedEscalationContext {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn build(&self, state: &WorkState<'_>, trajectory: &WorkTrajectory<'_>) -> EscalationContext {
        let mut context = FullEscalationContext.build(state, trajectory);
        let left_out = omissions(trajectory.origins, trajectory.observations);
        if left_out.is_empty() {
            return context;
        }
        let id = |i: usize| trajectory.observations[i].execution_id.clone();
        context.omitted = left_out
            .iter()
            .map(|(i, j)| {
                format!(
                    "execution {} of {} is identical to execution {}",
                    id(*i),
                    trajectory.origins[*i].capability,
                    id(*j)
                )
            })
            .collect();
        context.relevant_observations = trajectory
            .observations
            .iter()
            .enumerate()
            .filter(|(i, _)| !left_out.iter().any(|(o, _)| o == i))
            .map(|(_, o)| o.clone())
            .collect();
        context
    }
}

/// One model call's context, as Chip measured it before sending and as the provider reported it
/// after. The two are kept apart: bytes are Chip's own count of message content; tokens are only
/// what a provider said, and `None` when it said nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextCall {
    pub turn: usize,
    /// 1-based, in the order the calls were made.
    pub call: usize,
    pub request_bytes: usize,
    pub messages: usize,
    pub system_messages: usize,
    pub user_messages: usize,
    pub assistant_messages: usize,
    /// Observations in the request (each is a capability result).
    pub observations: usize,
    /// Observations Chip had recorded when the request was built.
    pub observations_known: usize,
    pub omitted_observations: usize,
    pub condensed_observations: usize,
    pub reported_prompt_tokens: Option<u64>,
    pub reported_completion_tokens: Option<u64>,
    /// Everything the provider reported about the call (`None` when it reported nothing).
    pub usage: Option<ModelUsage>,
    pub succeeded: bool,
}

/// How much of the observation stream was new, repeated or changed, and what repeating cost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObservationRepetition {
    pub new: usize,
    pub repeated_identical: usize,
    pub new_from_same_capability: usize,
    pub changed_reality: usize,
    /// Repeated-identical observations whose earlier copy the capability's contract does not allow
    /// to be left out, so it was sent again.
    pub repeated_and_retained: usize,
    /// Bytes of output in those retained earlier copies, per call they were sent in: what leaving
    /// them out would have saved had the contracts allowed it.
    pub retained_repeat_bytes_sent: usize,
}

/// One context-budget rejection: the turn, the attempted request size, and the configured budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRejection {
    pub turn: usize,
    pub request_bytes: usize,
    pub budget_bytes: usize,
}

/// The whole run's context measurements. Derived from the events and observations only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextReport {
    pub calls: Vec<ContextCall>,
    /// Requests refused because they would have exceeded the budget.
    pub context_limit_rejections: usize,
    /// Details of each context-budget rejection, derived from `ContextLimit` events.
    pub rejections: Vec<ContextRejection>,
    pub budget_bytes: Option<usize>,
    pub repetition: ObservationRepetition,
}

impl ContextReport {
    pub fn max_request_bytes(&self) -> usize {
        self.calls
            .iter()
            .map(|c| c.request_bytes)
            .max()
            .unwrap_or(0)
    }
    pub fn total_request_bytes(&self) -> usize {
        self.calls.iter().map(|c| c.request_bytes).sum()
    }
    pub fn max_request_messages(&self) -> usize {
        self.calls.iter().map(|c| c.messages).max().unwrap_or(0)
    }
    pub fn omitted_observations(&self) -> usize {
        self.calls.iter().map(|c| c.omitted_observations).sum()
    }
    pub fn max_reported_input_tokens(&self) -> Option<u64> {
        self.calls
            .iter()
            .filter_map(|c| c.reported_prompt_tokens)
            .max()
    }
    /// Requests that passed the budget check and were handed to the provider.
    pub fn requests_sent(&self) -> usize {
        self.calls.len()
    }
    /// Requests that were handed to the provider and did not return a response (a failure or a
    /// timeout). Never counted among `context_limit_rejections`, which were not sent at all.
    pub fn provider_failures(&self) -> usize {
        self.calls.iter().filter(|c| !c.succeeded).count()
    }
    /// Observation-carrying slots across all requests that were condensed to facts.
    pub fn condensed_observations(&self) -> usize {
        self.calls.iter().map(|c| c.condensed_observations).sum()
    }
    pub fn total_reported_tokens(&self) -> Option<u64> {
        let mut any = false;
        let mut total = 0u64;
        for c in &self.calls {
            if let (Some(p), Some(o)) = (c.reported_prompt_tokens, c.reported_completion_tokens) {
                any = true;
                total += p + o;
            }
        }
        any.then_some(total)
    }
}

/// Measures the context of a finished run.
pub fn context_report(report: &WorkReport, spec: &WorkSpec) -> ContextReport {
    let mut calls: Vec<ContextCall> = Vec::new();
    let mut known = 0usize;
    let mut pending: Option<(usize, ContextMetrics, usize)> = None;
    let mut rejections: Vec<ContextRejection> = Vec::new();
    for event in &report.events {
        match event {
            WorkEvent::ObservationRecorded { .. }
            | WorkEvent::ObservationRestored { .. }
            | WorkEvent::EvidenceReused { .. } => known += 1,
            WorkEvent::ContextLimit {
                turn,
                request_bytes,
                budget_bytes,
                work_id: _,
            } => rejections.push(ContextRejection {
                turn: *turn,
                request_bytes: *request_bytes,
                budget_bytes: *budget_bytes,
            }),
            WorkEvent::ModelEscalation { turn, context, .. } => {
                pending = Some((*turn, *context, known));
            }
            WorkEvent::ModelCalled {
                usage, succeeded, ..
            } => {
                if let Some((turn, m, known_then)) = pending.take() {
                    calls.push(ContextCall {
                        turn,
                        call: calls.len() + 1,
                        request_bytes: m.bytes,
                        messages: m.messages,
                        system_messages: m.system_messages,
                        user_messages: m.user_messages,
                        assistant_messages: m.assistant_messages,
                        observations: m.observations,
                        observations_known: known_then,
                        omitted_observations: m.omitted_observations,
                        condensed_observations: m.condensed_observations,
                        reported_prompt_tokens: usage.as_ref().and_then(|u| u.prompt_tokens),
                        reported_completion_tokens: usage
                            .as_ref()
                            .and_then(|u| u.completion_tokens),
                        usage: usage.clone(),
                        succeeded: *succeeded,
                    });
                }
            }
            _ => {}
        }
    }
    let classes = classify_observations(&report.origins, &report.observations);
    let mut repetition = ObservationRepetition::default();
    for (i, class) in classes.iter().enumerate() {
        match class {
            ObservationClass::New => repetition.new += 1,
            ObservationClass::NewFromSameCapability => repetition.new_from_same_capability += 1,
            ObservationClass::ChangedReality => repetition.changed_reality += 1,
            ObservationClass::RepeatedIdentical => {
                repetition.repeated_identical += 1;
                // The earlier copy this one repeats: sent in every later call that still carried it.
                let earlier = (0..i)
                    .rev()
                    .find(|j| report.origins[*j].invocation == report.origins[i].invocation);
                if let Some(j) = earlier {
                    if !report.origins[j].reusable {
                        repetition.repeated_and_retained += 1;
                        let size = report.observations[j].output.as_deref().map_or(0, str::len);
                        let carried = calls.iter().filter(|c| c.observations_known > i).count();
                        repetition.retained_repeat_bytes_sent += size * carried;
                    }
                }
            }
        }
    }
    ContextReport {
        calls,
        context_limit_rejections: rejections.len(),
        rejections,
        budget_bytes: spec.context_budget_bytes,
        repetition,
    }
}

/// The ordered trajectory. Execution and capability events are the existing ones, wrapped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkEvent {
    WorkStarted {
        work_id: WorkId,
        goal: String,
        limits: WorkLimits,
    },
    Capability(CapabilityEvent),
    DecisionStarted {
        work_id: WorkId,
        turn: usize,
    },
    /// The decision was made without the model.
    LocalDecision {
        work_id: WorkId,
        turn: usize,
        decision: String,
    },
    /// The expensive boundary: one model call is about to be made. Carries measurements, not the
    /// prompt.
    ModelEscalation {
        work_id: WorkId,
        turn: usize,
        reason: String,
        /// The identifier of the policy that chose this context. Never prompt text.
        context_policy: String,
        context: ContextMetrics,
    },
    /// The one model call an escalation makes has returned or failed. `usage` is what the
    /// provider reported, field for field: `None` when the call failed, and when it succeeded but
    /// the provider reported nothing, not even the model it served the call with. Within it, a field the provider did not report is `None`;
    /// nothing is estimated, and a reported 0 is a 0.
    ModelCalled {
        work_id: WorkId,
        turn: usize,
        usage: Option<ModelUsage>,
        succeeded: bool,
    },
    /// The next model request was not sent: it would have exceeded the configured context budget.
    /// Nothing was truncated, retried or substituted; the work ends at this limit.
    ContextLimit {
        work_id: WorkId,
        turn: usize,
        request_bytes: usize,
        budget_bytes: usize,
    },
    /// No model call was made for this turn: the response an earlier run of this work received,
    /// recorded and never acted on was given back (see [`WorkSpec::restored_response`]). It is
    /// interpreted and validated exactly as a response received now. `record` names where the
    /// caller's durable record keeps it.
    ModelResponseRestored {
        work_id: WorkId,
        turn: usize,
        record: String,
    },
    /// The recorded response was not given back, and the model is asked as usual: the work is not
    /// where the response was received (`reason`).
    ModelResponseDiscarded {
        work_id: WorkId,
        turn: usize,
        record: String,
        reason: String,
    },
    DecisionMade {
        work_id: WorkId,
        turn: usize,
        decision: String,
    },
    /// The model's reply this turn could not be read as exactly one JSON object
    /// ([`DecisionError::MalformedReply`](crate::DecisionError::MalformedReply)). No decision was
    /// made from it and nothing in it was executed. `consecutive` counts the malformed replies in a
    /// row including this one; while it is within `limit` the turn ends here and `reason` is given
    /// to the model as evidence at its next escalation (`recovering`). Past the limit the work
    /// ends `Failed` with this reason (`recovering` is false).
    DecisionRejected {
        work_id: WorkId,
        turn: usize,
        reason: String,
        consecutive: usize,
        limit: usize,
        recovering: bool,
    },
    CapabilityRequested {
        work_id: WorkId,
        turn: usize,
        capability: CapabilityId,
    },
    /// ExecutionRequested / ExecutionStarted / ExecutionCompleted / ExecutionFailed.
    Execution(ExecutionEvent),
    ObservationRecorded {
        work_id: WorkId,
        turn: usize,
        execution_id: ExecutionId,
        kind: ObservationKind,
        receipt_id: Option<String>,
    },
    /// An observation an earlier run of this work recorded, joined to this run's trajectory
    /// before its first turn (see [`WorkSpec::restored`]). Nothing was executed for it here;
    /// `remaining` is how many requirements are still unmet once it is counted.
    ///
    /// `reobserved` is set when the observation is not a recorded one at all: the caller's
    /// recovery performed the request again, before this run, to obtain a fresh observation
    /// (see [`RestoredObservation::reobserved`]).
    ObservationRestored {
        work_id: WorkId,
        execution_id: ExecutionId,
        capability: CapabilityId,
        kind: ObservationKind,
        receipt_id: Option<String>,
        remaining: usize,
        reobserved: bool,
    },
    EvidenceRecorded {
        work_id: WorkId,
        turn: usize,
        capability: CapabilityId,
    },
    EvidenceReused {
        work_id: WorkId,
        turn: usize,
        capability: CapabilityId,
        receipt_id: Option<String>,
    },
    /// An authoritative observation was compared with the goal's required output. Only emitted when
    /// the work has one. A receipt and an observation say what happened; this says whether it was
    /// what the goal needed.
    GoalEvaluated {
        work_id: WorkId,
        turn: usize,
        /// This observation produced one of the required outputs.
        satisfied: bool,
        /// Required outputs still unobserved after this evaluation; the work may complete at zero.
        remaining: usize,
    },
    WorkCompleted {
        work_id: WorkId,
    },
    WorkEscalated {
        work_id: WorkId,
        reason: String,
    },
    WorkBlocked {
        work_id: WorkId,
        reason: String,
    },
    WorkLimitReached {
        work_id: WorkId,
        limit: LimitKind,
    },
    WorkFailed {
        work_id: WorkId,
        reason: String,
        /// As [`WorkOutcome::Failed::delivery`].
        delivery: Option<Delivery>,
    },
    WorkCancelled {
        work_id: WorkId,
        cause: StopCause,
        reason: String,
        /// As [`WorkOutcome::Cancelled::delivery`].
        delivery: Option<Delivery>,
        /// As [`WorkOutcome::Cancelled::remote`].
        remote: Option<RemoteCancellation>,
    },
}

/// Usage as the provider reported it for one model call, carried whole from FX: input, output,
/// reasoning and cache tokens, the provider's opaque cost and the model it says served the call.
/// Each field is `None` when the provider did not report it; a reported 0 stays 0.
pub type ModelUsage = chip_inference::Usage;

/// The usage a trajectory's model calls reported, summed field by field over the calls that
/// reported that field. A sum is `None` when no call reported its field, so "unknown" is never
/// shown as 0. Costs are opaque strings (no currency is assumed) and are listed, never added.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UsageTotals {
    /// Model calls whose provider reported usage (a token count or a cost).
    pub calls_reported: usize,
    /// Model calls with no usage reported (the served model alone is not usage), and calls that
    /// failed.
    pub calls_unreported: usize,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    /// Each reported cost, in call order.
    pub costs: Vec<String>,
    /// Each model the provider reported serving a call, once, in the order first seen.
    pub models: Vec<String>,
}

/// Prompt plus completion tokens of one call, counting whichever the provider reported; `None`
/// when it reported neither.
pub fn reported_tokens(usage: &ModelUsage) -> Option<u64> {
    match (usage.prompt_tokens, usage.completion_tokens) {
        (None, None) => None,
        (p, c) => Some(p.unwrap_or(0) + c.unwrap_or(0)),
    }
}

/// [`UsageTotals`] of a trajectory.
pub fn usage_totals(events: &[WorkEvent]) -> UsageTotals {
    let mut t = UsageTotals::default();
    let add = |slot: &mut Option<u64>, n: Option<u64>| {
        if let Some(n) = n {
            *slot = Some(slot.unwrap_or(0) + n);
        }
    };
    for event in events {
        let WorkEvent::ModelCalled { usage, .. } = event else {
            continue;
        };
        let Some(u) = usage else {
            t.calls_unreported += 1;
            continue;
        };
        if let Some(model) = &u.model {
            if !t.models.contains(model) {
                t.models.push(model.clone());
            }
        }
        if !u.is_reported() {
            t.calls_unreported += 1;
            continue;
        }
        t.calls_reported += 1;
        add(&mut t.prompt_tokens, u.prompt_tokens);
        add(&mut t.completion_tokens, u.completion_tokens);
        add(&mut t.reasoning_tokens, u.reasoning_tokens);
        add(&mut t.cache_read_tokens, u.cache_read_tokens);
        add(&mut t.cache_write_tokens, u.cache_write_tokens);
        t.costs.extend(u.cost.clone());
    }
    t
}

/// Where the wall-clock time went. Latency is observed, never derived from events, and is
/// excluded from every determinism comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkLatency {
    /// Time inside the model call, summed over escalations.
    pub model: Duration,
    /// Time inside the executor, summed over executions.
    pub compute: Duration,
    /// Time in the local policy and the local reasoner (the evidence assessment included).
    pub local_decision: Duration,
    pub total: Duration,
}

/// A stopwatch that does nothing on wasm32, where `Instant::now` is unavailable.
#[derive(Clone, Copy)]
struct Mark {
    #[cfg(not(target_arch = "wasm32"))]
    at: std::time::Instant,
}

impl Mark {
    fn now() -> Mark {
        Mark {
            #[cfg(not(target_arch = "wasm32"))]
            at: std::time::Instant::now(),
        }
    }

    fn elapsed(&self) -> Duration {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.at.elapsed()
        }
        #[cfg(target_arch = "wasm32")]
        {
            Duration::ZERO
        }
    }
}

/// The canonical measurement of one workload. Every count is derived from the event trajectory
/// ([`WorkMeasurement::derive`]) and nothing else, so it cannot disagree with what happened;
/// only the latencies come from outside it. A metric that cannot be measured is an explicit
/// `None`, never an invented value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkMeasurement {
    pub work_id: WorkId,
    pub outcome: WorkOutcome,

    pub turns: u32,
    /// Executions the executor was actually asked to perform.
    pub executions: u32,
    pub observations: u32,

    pub evidence_hits: u32,
    pub local_decisions: u32,
    pub model_escalations: u32,

    /// Bytes and characters sent to the model across all escalations.
    pub context_bytes: u64,
    pub context_chars: u64,
    /// The policy that chose the escalation context; `None` when nothing escalated.
    pub context_policy: Option<String>,

    pub model_calls: u32,
    /// Prompt plus completion tokens the provider reported. `None` when no call reported usage.
    pub model_tokens: Option<u64>,

    pub model_latency: Duration,
    pub compute_latency: Duration,
    /// Local policy and reasoner time. Attribution, not a microbenchmark.
    pub local_decision_latency: Duration,
    pub total_latency: Duration,
}

impl WorkMeasurement {
    /// Counts what the trajectory contains. `executions` counts `ExecutionStarted`, which is
    /// emitted only when the executor is actually invoked.
    pub fn derive(
        work_id: WorkId,
        outcome: WorkOutcome,
        events: &[WorkEvent],
        latency: WorkLatency,
    ) -> WorkMeasurement {
        let mut m = WorkMeasurement {
            work_id,
            outcome,
            turns: 0,
            executions: 0,
            observations: 0,
            evidence_hits: 0,
            local_decisions: 0,
            model_escalations: 0,
            context_bytes: 0,
            context_chars: 0,
            context_policy: None,
            model_calls: 0,
            model_tokens: None,
            model_latency: latency.model,
            compute_latency: latency.compute,
            local_decision_latency: latency.local_decision,
            total_latency: latency.total,
        };
        for event in events {
            match event {
                WorkEvent::DecisionStarted { .. } => m.turns += 1,
                WorkEvent::Execution(ExecutionEvent::ExecutionStarted { .. }) => m.executions += 1,
                WorkEvent::ObservationRecorded { .. } => m.observations += 1,
                WorkEvent::EvidenceReused { .. } => m.evidence_hits += 1,
                WorkEvent::LocalDecision { .. } => m.local_decisions += 1,
                WorkEvent::ModelEscalation {
                    context,
                    context_policy,
                    ..
                } => {
                    m.model_escalations += 1;
                    m.context_policy
                        .get_or_insert_with(|| context_policy.clone());
                    m.context_bytes += context.bytes as u64;
                    m.context_chars += context.chars as u64;
                }
                WorkEvent::ModelCalled { usage, .. } => {
                    m.model_calls += 1;
                    if let Some(tokens) = usage.as_ref().and_then(reported_tokens) {
                        m.model_tokens = Some(m.model_tokens.unwrap_or(0) + tokens);
                    }
                }
                _ => {}
            }
        }
        m
    }

    pub fn terminal_state(&self) -> TerminalState {
        self.outcome.terminal_state()
    }
}

/// Checks the invariants optimization must never silently break. Returns every violation; an
/// empty list means the trajectory is sound.
///
/// * the run starts with `WorkStarted` and ends with exactly one terminal event;
/// * turns and executions stay within the limits, and turns are numbered in order;
/// * each turn has at most one decision, one capability request and one execution;
/// * a turn that reused evidence performed no execution;
/// * every observation follows an execution in its own turn, and every piece of evidence follows
///   an observation: nothing is believed without an execution;
/// * every escalation is exactly one model call.
pub fn verify_trajectory(events: &[WorkEvent], limits: &WorkLimits) -> Vec<String> {
    let mut violations = Vec::new();
    if !matches!(events.first(), Some(WorkEvent::WorkStarted { .. })) {
        violations.push("the trajectory does not start with WorkStarted".to_string());
    }
    let terminal = |e: &WorkEvent| {
        matches!(
            e,
            WorkEvent::WorkCompleted { .. }
                | WorkEvent::WorkEscalated { .. }
                | WorkEvent::WorkBlocked { .. }
                | WorkEvent::WorkLimitReached { .. }
                | WorkEvent::WorkFailed { .. }
                | WorkEvent::WorkCancelled { .. }
        )
    };
    let terminals = events.iter().filter(|e| terminal(e)).count();
    if terminals != 1 {
        violations.push(format!(
            "expected exactly one terminal event, found {terminals}"
        ));
    }
    if !events.last().is_some_and(terminal) {
        violations.push("the last event is not terminal".to_string());
    }

    #[derive(Default, Clone)]
    struct Turn {
        decision_makers: usize,
        decisions_made: usize,
        requests: usize,
        executions: usize,
        observations: usize,
        evidence_recorded: usize,
        reused: usize,
        escalations: usize,
        model_calls: usize,
    }
    let mut turns: Vec<Turn> = Vec::new();
    let mut current: Option<usize> = None;
    let (mut executions, mut observations) = (0usize, 0usize);
    for event in events {
        match event {
            WorkEvent::ObservationRestored { execution_id, .. } if current.is_some() => {
                violations.push(format!(
                    "restored observation {execution_id:?} after the first turn"
                ));
            }
            WorkEvent::DecisionStarted { turn, .. } => {
                if *turn != turns.len() {
                    violations.push(format!(
                        "turn {turn} is out of order (expected {})",
                        turns.len()
                    ));
                }
                turns.push(Turn::default());
                current = Some(turns.len() - 1);
            }
            _ => {
                let Some(i) = current else { continue };
                let t = &mut turns[i];
                match event {
                    WorkEvent::LocalDecision { .. } => t.decision_makers += 1,
                    WorkEvent::ModelEscalation { .. } => {
                        t.decision_makers += 1;
                        t.escalations += 1;
                    }
                    WorkEvent::ModelCalled { .. } => t.model_calls += 1,
                    WorkEvent::ModelResponseRestored { .. } => t.decision_makers += 1,
                    WorkEvent::DecisionMade { .. } => t.decisions_made += 1,
                    WorkEvent::CapabilityRequested { .. } => t.requests += 1,
                    WorkEvent::Execution(ExecutionEvent::ExecutionStarted { .. }) => {
                        t.executions += 1;
                        executions += 1;
                    }
                    WorkEvent::ObservationRecorded { .. } => {
                        if t.executions == 0 {
                            violations.push(format!(
                                "turn {}: an observation without an execution",
                                i + 1
                            ));
                        }
                        t.observations += 1;
                        observations += 1;
                    }
                    WorkEvent::EvidenceRecorded { .. } => {
                        if t.observations == 0 {
                            violations
                                .push(format!("turn {}: evidence without an observation", i + 1));
                        }
                        t.evidence_recorded += 1;
                    }
                    WorkEvent::EvidenceReused { .. } => t.reused += 1,
                    _ => {}
                }
            }
        }
    }
    for (i, t) in turns.iter().enumerate() {
        let n = i + 1;
        if t.decision_makers > 1 || t.decisions_made > 1 {
            violations.push(format!("turn {n}: more than one decision"));
        }
        if t.requests > 1 {
            violations.push(format!("turn {n}: more than one capability request"));
        }
        if t.executions > 1 {
            violations.push(format!("turn {n}: more than one execution"));
        }
        if t.reused > 0 && t.executions > 0 {
            violations.push(format!(
                "turn {n}: evidence was reused and an execution was performed"
            ));
        }
        if t.escalations != t.model_calls {
            violations.push(format!(
                "turn {n}: {} escalations but {} model calls",
                t.escalations, t.model_calls
            ));
        }
    }
    if turns.len() > limits.max_turns {
        violations.push(format!(
            "{} turns exceed the limit of {}",
            turns.len(),
            limits.max_turns
        ));
    }
    if executions > limits.max_executions {
        violations.push(format!(
            "{executions} executions exceed the limit of {}",
            limits.max_executions
        ));
    }
    if observations > executions {
        violations.push(format!(
            "{observations} observations from {executions} executions"
        ));
    }
    violations
}

/// One iteration, structurally: what was decided and by whom, whether anything was executed,
/// observed or reused, and what an escalation sent. There is no prompt text here, only shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnTrace {
    /// Zero-based.
    pub turn: usize,
    /// `RequestCapability`, `Complete`, `Escalate` or `Block`; `None` if the turn ended before a
    /// decision (a failed escalation).
    pub decision: Option<&'static str>,
    pub capability: Option<CapabilityId>,
    pub source: Option<DecisionSource>,
    /// The executor was invoked this turn.
    pub executed: bool,
    pub evidence_reused: bool,
    pub evidence_recorded: bool,
    /// `None` when nothing was observed or reused; otherwise whether a receipt was present.
    pub receipt_present: Option<bool>,
    pub observed: bool,
    /// Present when this turn escalated to the model.
    pub context: Option<ContextMetrics>,
    pub escalation_reason: Option<String>,
    pub model_called: bool,
}

/// The structural trajectory of a run, turn by turn, derived from its events.
pub fn trace(events: &[WorkEvent]) -> Vec<TurnTrace> {
    let mut turns: Vec<TurnTrace> = Vec::new();
    for event in events {
        if let WorkEvent::DecisionStarted { turn, .. } = event {
            turns.push(TurnTrace {
                turn: *turn,
                decision: None,
                capability: None,
                source: None,
                executed: false,
                evidence_reused: false,
                evidence_recorded: false,
                receipt_present: None,
                observed: false,
                context: None,
                escalation_reason: None,
                model_called: false,
            });
            continue;
        }
        let Some(t) = turns.last_mut() else { continue };
        match event {
            WorkEvent::LocalDecision { .. } => t.source = Some(DecisionSource::Local),
            WorkEvent::ModelEscalation {
                reason, context, ..
            } => {
                t.source = Some(DecisionSource::Model);
                t.context = Some(*context);
                t.escalation_reason = Some(reason.clone());
            }
            WorkEvent::ModelCalled { .. } => t.model_called = true,
            WorkEvent::ModelResponseRestored { .. } => t.source = Some(DecisionSource::Model),
            WorkEvent::DecisionMade { decision, .. } => {
                t.decision = Some(match decision.split(' ').next() {
                    Some("request") => "RequestCapability",
                    Some("complete") => "Complete",
                    Some("escalate") => "Escalate",
                    _ => "Block",
                });
            }
            WorkEvent::CapabilityRequested { capability, .. } => {
                t.capability = Some(capability.clone())
            }
            WorkEvent::Execution(ExecutionEvent::ExecutionStarted { .. }) => t.executed = true,
            WorkEvent::ObservationRecorded { receipt_id, .. } => {
                t.observed = true;
                t.receipt_present = Some(receipt_id.is_some());
            }
            WorkEvent::EvidenceRecorded { .. } => t.evidence_recorded = true,
            WorkEvent::EvidenceReused { receipt_id, .. } => {
                t.evidence_reused = true;
                t.receipt_present = Some(receipt_id.is_some());
            }
            _ => {}
        }
    }
    turns
}

/// A baseline, not an optimization target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkSummary {
    pub turns: usize,
    pub executions: usize,
    /// Observations of executions this loop performed.
    pub observations: usize,
    pub evidence_hits: usize,
    pub local_decisions: usize,
    pub model_escalations: usize,
    /// Bytes sent to the model across all escalations.
    pub context_bytes: usize,
    /// Token usage the provider reported, summed. Zero when no escalation happened or no call
    /// reported it; [`usage_totals`] tells the two apart.
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Wall-clock time. Not deterministic; excluded from determinism comparisons.
    pub elapsed: Duration,
    pub terminal_state: TerminalState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkReport {
    pub work_id: WorkId,
    pub outcome: WorkOutcome,
    pub summary: WorkSummary,
    pub events: Vec<WorkEvent>,
    pub decisions: Vec<DecisionRecord>,
    pub observations: Vec<Observation>,
    /// Where each observation came from, in the same order.
    pub origins: Vec<ObservationOrigin>,
    /// The measurements of every escalation, in order.
    pub escalations: Vec<ContextMetrics>,
    /// Observed wall-clock attribution.
    pub latency: WorkLatency,
}

impl WorkReport {
    /// The structural trajectory, turn by turn.
    pub fn trace(&self) -> Vec<TurnTrace> {
        trace(&self.events)
    }

    /// The canonical measurement, derived from this report's event trajectory.
    pub fn measurement(&self) -> WorkMeasurement {
        WorkMeasurement::derive(
            self.work_id.clone(),
            self.outcome.clone(),
            &self.events,
            self.latency,
        )
    }
}

/// Everything `Agent::run_work` needs to know about one workload.
#[derive(Debug, Clone)]
pub struct WorkSpec {
    pub id: WorkId,
    pub goal: WorkGoal,
    pub limits: WorkLimits,
    /// The current state evidence must have been established under, if the caller tracks one.
    pub state: Option<StateToken>,
    /// The outputs authoritative observations must have produced for the goal to be satisfied: one
    /// entry per independently verified outcome. Chip compares each with what Compute observed;
    /// a model's words are never consulted. While any is unmet the work cannot complete. Empty
    /// means the work has no requirement (nothing is evaluated).
    pub required_outputs: Vec<String>,
    /// Requirements an authoritative observation must satisfy, each decided by a predicate the
    /// caller supplies. For an outcome that is not a fixed string: the predicate reads the
    /// observation and nothing else, and says whether it establishes the outcome. Like
    /// `required_outputs`, while any is unmet the work cannot complete.
    pub required_observations: Vec<Arc<dyn ObservationPredicate>>,
    /// Checks the model's proposed answer against the observations when it asks to complete.
    pub required_answers: Vec<Arc<dyn AnswerPredicate>>,
    /// Invariants the safety audit checks every recorded observation against.
    pub observation_invariants: Vec<Arc<dyn ObservationInvariant>>,
    /// Capabilities whose earlier observations must never answer a later request. The audit holds
    /// the trajectory to it: an `EvidenceReused` event for one of these is a violation.
    pub evidence_reuse_prohibited: Vec<CapabilityId>,
    /// The most bytes a model request may carry (see [`ContextMetrics::bytes`]), when the caller has
    /// been told one. `None` means none is known and none is assumed.
    pub context_budget_bytes: Option<usize>,
    /// The run's owner: stop requests in, live events out. `None`: the work runs to its own end.
    pub control: Option<Arc<dyn WorkControl>>,
    /// What earlier runs of this same work already executed and observed, in the order it
    /// happened, as the caller's durable record says. The run starts with these as its
    /// trajectory: each joins the observations exactly as it would have after its execution, and
    /// none is executed again. Empty for a work with no earlier run.
    pub restored: Vec<RestoredObservation>,
    /// Actions of earlier runs whose outcome was never observed, settled since by an operator's
    /// conclusion (see [`SettledAction`]). None is an observation, and none is executed again by
    /// this run's choice: one concluded to have taken effect is ruled out.
    pub settled: Vec<SettledAction>,
    /// The response of the last model call an earlier run of this work received and recorded,
    /// when that run was interrupted before it acted on it (see [`RestoredResponse`]). The first
    /// escalation of this run is answered from it instead of a model call, if the work is still
    /// exactly where the response was received; otherwise it is discarded and the model is asked.
    pub restored_response: Option<RestoredResponse>,
    /// When set, the loop ends the work as `Blocked` (reason beginning `stuck: `) once the end of
    /// its own trajectory repeats itself without learning anything (see [`StuckThresholds`]).
    /// `None` is no guard: the limits are then the only bound on repetition.
    pub stuck_guard: Option<crate::StuckThresholds>,
    /// The most consecutive model replies the work may reject as malformed
    /// ([`DecisionError::MalformedReply`](crate::DecisionError::MalformedReply)) and still go on.
    /// Each rejected reply ends its turn: nothing in it is executed, it is recorded
    /// ([`WorkEvent::DecisionRejected`]) and the diagnostic is given to the model as evidence at its
    /// next escalation. The turn, execution and context limits, the deadline and the stuck guard
    /// count and apply exactly as for any other turn. One more malformed reply in a row ends the
    /// work as `Failed`; a decision that passes validation resets the count. `0` ends the work at
    /// the first malformed reply. Every other decision error ends the work at once, as before.
    /// Default: [`DEFAULT_MALFORMED_REPLY_LIMIT`].
    pub malformed_reply_limit: usize,
}

/// An action an earlier run of the work started without its outcome ever being observed, which an
/// operator has since settled: the operator concluded that its effect did, or did not, occur.
///
/// It is a person's conclusion, never an observation: it joins no trajectory, satisfies no
/// requirement and backs no completion. One concluded to have taken effect is ruled out, so the
/// model is told it is not to be performed again; the host's executor refuses it if asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledAction {
    pub capability: CapabilityId,
    /// The validated inputs of the request, when the durable record kept them.
    pub inputs: Option<std::collections::BTreeMap<String, InputValue>>,
    /// The operator's conclusion: the effect occurred (`true`) or did not (`false`).
    pub executed: bool,
    /// Who concluded it, as the settlement recorded it.
    pub decided_by: String,
    /// What they gave as the basis. Not verified by anyone but them.
    pub evidence: String,
}

/// A model's response an earlier run of the work received and durably recorded, and never acted
/// on: the run ended after the response arrived and before anything it asked for was attempted.
///
/// It is the model's judgment, never evidence. Given back, it is interpreted, validated and
/// authorized exactly as a response received now, so it can cause nothing a fresh response could
/// not. It saves the call, and its cost, that asking again would make. It is given back once, to
/// the run's first escalation, and only while the work is exactly where it was received: the same
/// model, the same observations in the same order, and nothing executed or reused since the run
/// started. Otherwise the model is asked, as without it.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoredResponse {
    /// Where the caller's durable record keeps it; named in the trajectory.
    pub record: String,
    /// The model the response was received from.
    pub model: String,
    pub response: ModelResponse,
    /// The executions whose observations the work held, in order, when the response was received.
    pub observed: Vec<ExecutionId>,
}

/// One execution an earlier run of the work performed: the request as it was validated and the
/// result its executor returned, both read back from the caller's durable record. Restoring it
/// performs nothing; the observation is derived from `result` by the agent's own observer, as it
/// was the first time.
#[derive(Debug, Clone, PartialEq)]
pub struct RestoredObservation {
    pub request: CapabilityRequest,
    pub result: ExecutionResult,
    /// The result is not one an earlier run recorded: an earlier run started this request and
    /// its outcome was lost, and the caller's recovery performed it again, as its capability
    /// permits, to obtain a fresh observation. The trajectory joins it like any other, and its
    /// event says what it is.
    pub reobserved: bool,
}

impl fmt::Debug for dyn WorkControl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WorkControl")
    }
}

/// Decides whether one authoritative observation establishes a required outcome.
///
/// It is handed an [`Observation`] and nothing else: not a decision, not a model's reply, not a
/// summary. Whoever supplies a predicate owns what it means; the work loop only asks it, and the
/// safety audit and the utility measurement ask it again of the recorded observations.
pub trait ObservationPredicate: fmt::Debug + Send + Sync {
    /// What is required, in words, for diagnostics.
    fn describe(&self) -> String;
    fn satisfied_by(&self, observation: &Observation) -> bool;

    /// Whether the observations so far, in the order they were recorded, establish the outcome.
    /// The default is "some single observation does". An outcome that depends on order (a
    /// verification that must come after a change it verifies) overrides this. It reads
    /// recorded observations and nothing else, and it can become false again when a later
    /// observation supersedes the one that established it.
    fn satisfied_by_trajectory(&self, observations: &[Observation]) -> bool {
        observations.iter().any(|o| self.satisfied_by(o))
    }
}

/// Decides whether an *answer* the model proposes when it asks to complete is acceptable, given the
/// authoritative observations. This is how a read-only goal is completed: the model proposes the
/// answer, and the runtime accepts it only if reality supports it. The answer is never evidence and
/// is never believed; it is only checked against what was observed. A predicate that accepts
/// everything would let the model manufacture completion, so supplying one is the owner's
/// responsibility, and the safety audit asks it again of the recorded outcome.
pub trait AnswerPredicate: fmt::Debug + Send + Sync {
    /// What the answer must satisfy, in words, for diagnostics.
    fn describe(&self) -> String;
    fn accepts(&self, answer: &str, observations: &[Observation]) -> bool;
}

/// An invariant over authoritative observations that must never be violated, whatever the model
/// asked for. The safety audit evaluates each recorded observation against it independently of
/// the capability that produced it (a filesystem capability's observation must not name a path
/// outside its root, however the capability came to produce it).
pub trait ObservationInvariant: fmt::Debug + Send + Sync {
    /// The counter name reported in the audit.
    fn name(&self) -> &'static str;
    /// How many times this observation violates the invariant.
    fn violations(&self, observation: &Observation) -> usize;

    /// How many violations the observations, in the order recorded, contain. The default is the
    /// sum over each observation. An invariant that must read an observation against what came
    /// after it (a listing that a later change legitimately outdates) overrides this.
    fn violations_in_trajectory(&self, observations: &[Observation]) -> usize {
        observations.iter().map(|o| self.violations(o)).sum()
    }
}

impl WorkSpec {
    pub fn new(id: WorkId, goal: WorkGoal) -> Self {
        Self {
            id,
            goal,
            limits: WorkLimits::default(),
            state: None,
            required_outputs: Vec::new(),
            required_observations: Vec::new(),
            required_answers: Vec::new(),
            observation_invariants: Vec::new(),
            evidence_reuse_prohibited: Vec::new(),
            context_budget_bytes: None,
            control: None,
            restored: Vec::new(),
            settled: Vec::new(),
            restored_response: None,
            stuck_guard: None,
            malformed_reply_limit: DEFAULT_MALFORMED_REPLY_LIMIT,
        }
    }

    /// Sets how many consecutive malformed replies the work may recover from (see
    /// [`WorkSpec::malformed_reply_limit`]).
    pub fn with_malformed_reply_limit(mut self, limit: usize) -> Self {
        self.malformed_reply_limit = limit;
        self
    }

    /// Whether the goal's required observations are met, as the events say. A level, not an edge:
    /// `GoalEvaluated::satisfied` says only that *that* observation produced a required output, so
    /// after the requirements first held it is false again for every later observation; the level is
    /// `remaining == 0`, the reading the safety audit uses. It is the latest evaluation,
    /// whether the loop made it (`GoalEvaluated`) or a resume made it when it joined a recorded
    /// observation (`ObservationRestored`, which carries the same count). `None` when the work has
    /// no requirement, or nothing was evaluated. A restored observation counts only for a spec
    /// that has a requirement: without one its count is vacuously zero.
    ///
    /// Met is not completed: only an accepted completion decision completes a work.
    pub fn goal_level(&self, events: &[WorkEvent]) -> Option<bool> {
        let required = !self.required_outputs.is_empty() || !self.required_observations.is_empty();
        events.iter().rev().find_map(|e| match e {
            WorkEvent::GoalEvaluated { remaining, .. } => Some(*remaining == 0),
            WorkEvent::ObservationRestored { remaining, .. } if required => Some(*remaining == 0),
            _ => None,
        })
    }

    /// Ends the work, `Blocked`, when its trajectory is stuck (see [`WorkSpec::stuck_guard`]).
    pub fn with_stuck_guard(mut self, thresholds: crate::StuckThresholds) -> Self {
        self.stuck_guard = Some(thresholds);
        self
    }

    /// Starts the work from what earlier runs of it recorded (see [`WorkSpec::restored`]).
    pub fn with_restored(mut self, restored: Vec<RestoredObservation>) -> Self {
        self.restored = restored;
        self
    }

    /// Gives the work the response an interrupted earlier run received and never acted on (see
    /// [`WorkSpec::restored_response`]).
    pub fn with_restored_response(mut self, restored: Option<RestoredResponse>) -> Self {
        self.restored_response = restored;
        self
    }

    /// Tells the work which earlier actions an operator has settled (see [`WorkSpec::settled`]).
    pub fn with_settled(mut self, settled: Vec<SettledAction>) -> Self {
        self.settled = settled;
        self
    }

    /// The run's owner: asked whether to stop before each model call and execution, and told
    /// every event as it is recorded.
    pub fn with_control(mut self, control: Arc<dyn WorkControl>) -> Self {
        self.control = Some(control);
        self
    }

    /// A capability whose evidence the audit requires to never be reused.
    pub fn with_evidence_reuse_prohibited(mut self, capability: CapabilityId) -> Self {
        self.evidence_reuse_prohibited.push(capability);
        self
    }

    /// The configured context budget in bytes. A request over it is never sent.
    pub fn with_context_budget_bytes(mut self, bytes: usize) -> Self {
        self.context_budget_bytes = Some(bytes);
        self
    }

    /// An invariant the audit holds every recorded observation to.
    pub fn with_observation_invariant(mut self, invariant: Arc<dyn ObservationInvariant>) -> Self {
        self.observation_invariants.push(invariant);
        self
    }

    /// Execution success is not goal satisfaction: with a required output, a completed execution
    /// satisfies the goal only if its observed output equals this value (compared trimmed).
    pub fn with_required_output(mut self, output: impl Into<String>) -> Self {
        self.required_outputs.push(output.into());
        self
    }

    /// An outcome decided by a predicate over an authoritative observation.
    pub fn with_required_observation(mut self, predicate: Arc<dyn ObservationPredicate>) -> Self {
        self.required_observations.push(predicate);
        self
    }

    /// A condition the proposed answer must satisfy against the observations before the work may
    /// complete.
    pub fn with_required_answer(mut self, predicate: Arc<dyn AnswerPredicate>) -> Self {
        self.required_answers.push(predicate);
        self
    }

    /// Several independently verified outcomes: completion needs every one observed.
    pub fn with_required_outputs<I, S>(mut self, outputs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.required_outputs
            .extend(outputs.into_iter().map(Into::into));
        self
    }

    pub fn with_limits(mut self, limits: WorkLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_state(mut self, state: StateToken) -> Self {
        self.state = Some(state);
        self
    }
}

enum Step {
    Next,
    Done(WorkOutcome),
}

/// The trajectory as the loop records it, handed to the run's [`WorkControl`] event by event.
struct EventLog {
    events: Vec<WorkEvent>,
    control: Option<Arc<dyn WorkControl>>,
}

impl EventLog {
    fn push(&mut self, event: WorkEvent) {
        if let Some(control) = &self.control {
            control.on_event(&event);
        }
        self.events.push(event);
    }

    fn extend(&mut self, events: impl IntoIterator<Item = WorkEvent>) {
        for event in events {
            self.push(event);
        }
    }
}

impl std::ops::Deref for EventLog {
    type Target = Vec<WorkEvent>;
    fn deref(&self) -> &Vec<WorkEvent> {
        &self.events
    }
}

/// An operator's stated evidence, bounded for a model request.
fn clip_evidence(evidence: &str) -> String {
    const MAX: usize = 300;
    if evidence.len() <= MAX {
        return evidence.to_string();
    }
    let mut end = MAX;
    while !evidence.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &evidence[..end])
}

struct Run<'a> {
    agent: &'a Agent,
    spec: &'a WorkSpec,
    context_policy: &'a dyn EscalationContextPolicy,
    events: EventLog,
    observations: Vec<Observation>,
    origins: Vec<ObservationOrigin>,
    decisions: Vec<DecisionRecord>,
    ruled_out: Vec<String>,
    escalations: Vec<ContextMetrics>,
    capabilities: Vec<Capability>,
    summary: WorkSummary,
    latency: WorkLatency,
    /// Which of the spec's required outputs some authoritative observation has produced.
    satisfied: Vec<bool>,
    /// How many execution identities this work has assigned.
    assigned: usize,
    /// The recorded response the first escalation may be answered from; taken by it either way.
    restored_response: Option<&'a RestoredResponse>,
    /// Malformed model replies in a row; a reply that passes validation resets it.
    malformed_replies: usize,
    /// The last malformed reply's diagnostic, given to the model at its next escalation.
    rejection: Option<String>,
}

impl<'a> Run<'a> {
    fn id(&self) -> WorkId {
        self.spec.id.clone()
    }

    /// The outcome to end with if the owner has asked the work to stop; `delivery` is that of a
    /// model call the stop interrupted, if any.
    fn stopped(
        &self,
        delivery: Option<Delivery>,
        remote: Option<RemoteCancellation>,
    ) -> Option<WorkOutcome> {
        let cause = self.spec.control.as_ref()?.stop_requested()?;
        Some(WorkOutcome::Cancelled {
            cause,
            reason: match cause {
                StopCause::Cancelled => "cancellation was requested; the work stopped at its next boundary and nothing that already ran was undone".to_string(),
                StopCause::Deadline => "the whole-work deadline passed; the work stopped at its next boundary and nothing that already ran was undone".to_string(),
            },
            delivery,
            remote,
        })
    }

    fn view(&self, turn: usize) -> WorkView<'_> {
        WorkView {
            goal: &self.spec.goal,
            turn,
            executions: self.summary.executions,
            observations: &self.observations,
            decisions: &self.decisions,
        }
    }

    fn decide_locally(&mut self, turn: usize, decision: &WorkDecision) {
        self.summary.local_decisions += 1;
        self.events.push(WorkEvent::LocalDecision {
            work_id: self.id(),
            turn,
            decision: decision.label(),
        });
        self.decisions.push(DecisionRecord {
            turn,
            source: DecisionSource::Local,
            decision: decision.clone(),
        });
    }

    /// Reuses valid evidence: nothing is executed, and the observation joins the state.
    fn reuse(&mut self, turn: usize, request: &CapabilityRequest, observation: Observation) {
        self.summary.evidence_hits += 1;
        self.events.push(WorkEvent::EvidenceReused {
            work_id: self.id(),
            turn,
            capability: request.capability_id.clone(),
            receipt_id: observation.receipt_id.clone(),
        });
        self.record_origin(request);
        self.observations.push(observation.clone());
        self.evaluate_goal(turn, request, &observation);
    }

    /// Records where the observation about to be pushed came from.
    /// Notes that this invocation's execution failed, for the model's next request. The first
    /// failure is noted as it always was. A failure that repeats an earlier one (same request, same
    /// observation) replaces that note with one that says how many times it has happened, what the
    /// observation said, and that sending it again cannot change the result. This is context only:
    /// the recorded observations are untouched, and the stuck guard compares those, so it sees
    /// exactly what it saw before and fires at the same point.
    fn note_failure(&mut self, request: &CapabilityRequest, observation: &Observation) {
        let prefix = format!(
            "capability {}: its execution failed",
            invocation_label(request)
        );
        let key = format!("{}|{:?}", request.capability_id, request.inputs);
        let earlier = self
            .origins
            .iter()
            .zip(&self.observations)
            .filter(|(origin, seen)| {
                origin.invocation == key
                    && seen.status == crate::ExecutionStatus::Failure
                    && seen.output == observation.output
            })
            .count();
        let note = if earlier == 0 {
            prefix.clone()
        } else {
            let said = observation
                .output
                .as_deref()
                .and_then(|o| o.lines().next())
                .map_or_else(|| "no output".to_string(), clip_evidence);
            format!(
                "{prefix} {} times with the identical result ({said}). Sending this exact request again cannot change that result: change the request, using names the observations already received show, or use a different capability. The work is stopped if the same request keeps returning the same observation.",
                earlier + 1
            )
        };
        match self.ruled_out.iter_mut().find(|n| n.starts_with(&prefix)) {
            Some(existing) => *existing = note,
            None => self.ruled_out.push(note),
        }
    }

    fn record_origin(&mut self, request: &CapabilityRequest) {
        self.origins.push(ObservationOrigin {
            capability: request.capability_id.clone(),
            invocation: format!("{}|{:?}", request.capability_id, request.inputs),
            reusable: self.reusable(&request.capability_id),
            provider_response_id: request.provider_response_id.clone(),
        });
    }

    /// Whether completing now would claim a goal some required output of which no authoritative
    /// observation has produced.
    fn completion_refused(&self) -> bool {
        self.satisfied.iter().any(|met| !met)
    }

    /// Whether the answer the model proposed with its completion is unacceptable against the
    /// observations recorded so far.
    fn answer_refused(&self, answer: &str) -> bool {
        self.spec
            .required_answers
            .iter()
            .any(|p| !p.accepts(answer, &self.observations))
    }

    /// What Chip can say about progress without recommending anything: how many required outputs
    /// authoritative observations have produced so far.
    fn progress_line(&self) -> String {
        let met = self.satisfied.iter().filter(|m| **m).count();
        if met == 0 {
            "no authoritative observation satisfies the goal yet".to_string()
        } else if met == self.satisfied.len() {
            format!(
                "all {met} required outputs have been observed and verified; no further capability is required by the goal, and a complete decision is accepted only if its answer is supported by the observations"
            )
        } else {
            format!(
                "{met} of {} required outputs have been observed and verified",
                self.satisfied.len()
            )
        }
    }

    /// Compares one authoritative observation with the spec's required outputs. It reads the
    /// observation and nothing else: not the model's reply, not a decision, not a summary.
    fn evaluate_goal(
        &mut self,
        turn: usize,
        request: &CapabilityRequest,
        observation: &Observation,
    ) {
        if let Some(produced) = self.assess_goal(request, observation) {
            self.events.push(WorkEvent::GoalEvaluated {
                work_id: self.id(),
                turn,
                satisfied: produced,
                remaining: self.remaining(),
            });
        }
    }

    fn remaining(&self) -> usize {
        self.satisfied.iter().filter(|m| !**m).count()
    }

    /// Updates which requirements the observations now meet, and rules out an invocation that ran
    /// fine without meeting any. `None` when the work has no requirement; otherwise whether this
    /// observation produced a required outcome.
    fn assess_goal(
        &mut self,
        request: &CapabilityRequest,
        observation: &Observation,
    ) -> Option<bool> {
        if self.spec.required_outputs.is_empty() && self.spec.required_observations.is_empty() {
            return None;
        }
        let produced = (observation.kind == ObservationKind::ExecutionCompleted
            && observation.status == crate::ExecutionStatus::Success)
            .then(|| {
                let output = observation.output.as_deref().map(str::trim);
                self.spec
                    .required_outputs
                    .iter()
                    .position(|r| Some(r.trim()) == output)
            })
            .flatten();
        if let Some(i) = produced {
            self.satisfied[i] = true;
        }
        // Each predicate decides for itself, from the observations recorded so far (the latest is
        // the last). A predicate over the trajectory can stop holding when a later observation
        // supersedes the one that established it; an outcome that held and no longer does is unmet.
        let offset = self.spec.required_outputs.len();
        let mut by_predicate = false;
        for (j, predicate) in self.spec.required_observations.iter().enumerate() {
            let held = self.satisfied[offset + j];
            let holds = predicate.satisfied_by_trajectory(&self.observations);
            self.satisfied[offset + j] = holds;
            if holds && (!held || predicate.satisfied_by(observation)) {
                by_predicate = true;
            }
        }
        let produced = produced.is_some() || by_predicate;
        if !produced && observation.status == crate::ExecutionStatus::Success {
            // It ran, and ran fine; it was just not what the goal needed. A failed execution is
            // already ruled out as failed.
            self.ruled_out.push(format!(
                "capability {}: executed, but its observation did not satisfy the goal",
                invocation_label(request)
            ));
        }
        Some(produced)
    }

    /// Joins what an earlier run recorded to this run's trajectory, before its first turn, exactly
    /// as [`perform`](Self::perform) would have after executing it: the same observer derives the
    /// observation, the same notes are kept, the same requirements are evaluated. Nothing is
    /// executed, validated or recorded as evidence again. A restored entry that cannot be
    /// observed, or that repeats an execution identity, ends the work: a trajectory is either
    /// restored whole or not used.
    fn restore(&mut self) -> Result<(), WorkOutcome> {
        let failed = |reason: String| WorkOutcome::Failed {
            reason: format!("the recorded trajectory cannot be restored: {reason}"),
            delivery: None,
        };
        // Everything is checked and derived first, so nothing is joined unless all of it is.
        let spec = self.spec;
        let mut joined: Vec<(&CapabilityRequest, Observation, bool)> = Vec::new();
        for restored in &spec.restored {
            let (request, result) = (&restored.request, &restored.result);
            if request.execution_id != result.id {
                return Err(failed(format!(
                    "the request for {} names execution {}",
                    result.id, request.execution_id
                )));
            }
            if joined.iter().any(|(_, o, _)| o.execution_id == result.id) {
                return Err(failed(format!("execution {} is recorded twice", result.id)));
            }
            let observation = self
                .agent
                .observe(result)
                .map_err(|e| failed(e.to_string()))?;
            joined.push((request, observation, restored.reobserved));
        }
        let prefix = format!("{}-exec-", spec.id);
        for (request, observation, reobserved) in joined {
            if observation.status == crate::ExecutionStatus::Failure {
                self.note_failure(request, &observation);
            }
            self.record_origin(request);
            self.observations.push(observation.clone());
            self.assess_goal(request, &observation);
            // A restored observation keeps the identity it was executed under; numbering
            // continues after the highest one, so a new execution never takes one of those.
            if let Some(n) = observation
                .execution_id
                .0
                .strip_prefix(&prefix)
                .and_then(|n| n.parse::<usize>().ok())
            {
                self.assigned = self.assigned.max(n);
            }
            self.events.push(WorkEvent::ObservationRestored {
                work_id: self.id(),
                execution_id: observation.execution_id.clone(),
                capability: request.capability_id.clone(),
                kind: observation.kind,
                receipt_id: observation.receipt_id.clone(),
                remaining: self.remaining(),
                reobserved,
            });
        }
        // An operator's conclusion is context, not an observation: it is stated, never joined.
        for settled in spec.settled.iter().filter(|s| s.executed) {
            let invocation = match &settled.inputs {
                Some(inputs) => invocation_label(&CapabilityRequest {
                    inputs: inputs.clone(),
                    ..CapabilityRequest::new(ExecutionId::unassigned(), settled.capability.clone())
                }),
                None => format!("{} (inputs not recorded)", settled.capability),
            };
            self.ruled_out.push(format!(
                "capability {invocation}: an operator ({}) concluded that its effect occurred, but its outcome was never observed; it is not performed again in this work (their evidence: {})",
                settled.decided_by,
                clip_evidence(&settled.evidence)
            ));
        }
        Ok(())
    }

    fn context(&self, turn: usize, evidence: &[String], question: &str) -> EscalationContext {
        self.context_policy.build(
            &WorkState {
                goal: self.spec.goal.as_str(),
                turn,
                max_turns: self.spec.limits.max_turns,
                executions: self.summary.executions,
                max_executions: self.spec.limits.max_executions,
                context_budget_bytes: self.spec.context_budget_bytes,
            },
            &WorkTrajectory {
                observations: &self.observations,
                origins: &self.origins,
                decisions: &self.decisions,
                ruled_out: &self.ruled_out,
                evidence,
                question,
            },
        )
    }

    /// Every condensed observation must be one recorded, successful execution, named once, that
    /// the request does not also carry or already omit as a repeat, whose withheld size is what its
    /// rendered form really is, and that states at least one fact.
    fn check_condensation(&self, context: &EscalationContext) -> Result<(), String> {
        let repeats: Vec<usize> = omissions(&self.origins, &self.observations)
            .into_iter()
            .map(|(omitted, _)| omitted)
            .collect();
        for (n, item) in context.condensed.iter().enumerate() {
            let id = &item.execution_id;
            if context.condensed[..n].iter().any(|c| &c.execution_id == id) {
                return Err(format!("execution {id} is condensed twice"));
            }
            let mut found = self
                .observations
                .iter()
                .enumerate()
                .filter(|(_, o)| &o.execution_id == id);
            let (index, observation) = match (found.next(), found.next()) {
                (Some(one), None) => one,
                _ => {
                    return Err(format!(
                        "execution {id} is not exactly one recorded observation"
                    ));
                }
            };
            if observation.kind != crate::ObservationKind::ExecutionCompleted
                || observation.status != crate::ExecutionStatus::Success
            {
                return Err(format!(
                    "execution {id} did not succeed; a failure is always kept"
                ));
            }
            if repeats.contains(&index) {
                return Err(format!("execution {id} is already omitted as a repeat"));
            }
            if context
                .relevant_observations
                .iter()
                .any(|o| &o.execution_id == id)
            {
                return Err(format!("execution {id} is also sent in full"));
            }
            if item.facts.is_empty() {
                return Err(format!("execution {id} is condensed to no facts"));
            }
            if item.withheld_bytes != observation.render().len() {
                return Err(format!("execution {id} withholds a size it does not have"));
            }
        }
        Ok(())
    }

    /// Makes the one model call an escalation allows, or takes the recorded response that stands
    /// for it, and interprets the response.
    async fn escalate(
        &mut self,
        turn: usize,
        reason: String,
        evidence: Vec<String>,
        boundary: &dyn WorkDecisionBoundary,
    ) -> Result<Option<WorkDecision>, WorkOutcome> {
        let response = match self.recorded_response(turn) {
            Some(restored) => {
                if let Some(stopped) = self.stopped(None, None) {
                    return Err(stopped);
                }
                self.events.push(WorkEvent::ModelResponseRestored {
                    work_id: self.id(),
                    turn,
                    record: restored.record.clone(),
                });
                restored.response.clone()
            }
            None => {
                // The model is told why its last reply was rejected, first, as evidence: the
                // runtime's statement, never anything the reply itself said.
                let evidence = match self.rejection.take() {
                    Some(note) => std::iter::once(note).chain(evidence).collect(),
                    None => evidence,
                };
                // Where the goal stands, by Chip's own check of the observations; said once, and
                // only for a work that has requirements to check.
                let progress = format!("goal progress: {}", self.progress_line());
                let evidence = if self.satisfied.is_empty() || evidence.contains(&progress) {
                    evidence
                } else {
                    evidence.into_iter().chain([progress]).collect()
                };
                self.call_model(turn, reason, evidence, boundary).await?
            }
        };
        let mut decision = match boundary.interpret(&response, &self.capabilities) {
            Ok(decision) => decision,
            Err(e) => {
                let reason = format!("the model's response is not a valid decision: {e}");
                if !e.is_malformed_reply() {
                    // An object that is not an allowed decision, or a request the capabilities
                    // refuse: the reply was read, and what it says is rejected. Not asked again.
                    return Err(WorkOutcome::Failed {
                        reason,
                        delivery: None,
                    });
                }
                self.malformed_replies += 1;
                let consecutive = self.malformed_replies;
                let limit = self.spec.malformed_reply_limit;
                let recovering = consecutive <= limit;
                self.events.push(WorkEvent::DecisionRejected {
                    work_id: self.id(),
                    turn,
                    reason: reason.clone(),
                    consecutive,
                    limit,
                    recovering,
                });
                if !recovering {
                    return Err(WorkOutcome::Failed {
                        reason: format!(
                            "{reason} ({consecutive} malformed replies in a row; the limit is {limit})"
                        ),
                        delivery: None,
                    });
                }
                self.rejection = Some(format!(
                    "turn {}: your reply was rejected and nothing in it was done: {e}. This is malformed reply {consecutive} in a row; after {limit} the next one ends the work.",
                    turn + 1
                ));
                return Ok(None);
            }
        };
        self.malformed_replies = 0;
        // Chip, not the provider, names the execution: assigned here, once, where the request is
        // authorized, and carried unchanged by the execution, its events, its observation and its
        // evidence. Whatever identity the boundary put on a model's request is replaced.
        if let WorkDecision::RequestCapability(request) = &mut decision {
            request.execution_id = self.assign_execution_id();
        }
        self.decisions.push(DecisionRecord {
            turn,
            source: DecisionSource::Model,
            decision: decision.clone(),
        });
        Ok(Some(decision))
    }

    /// The recorded response this escalation is answered from, if the spec has one and the work
    /// is still exactly where it was received. Only the first escalation is offered it; one it does
    /// not fit is discarded, with the reason, and the model is asked.
    fn recorded_response(&mut self, turn: usize) -> Option<&'a RestoredResponse> {
        let restored = self.restored_response.take()?;
        let observed: Vec<&ExecutionId> =
            self.observations.iter().map(|o| &o.execution_id).collect();
        let unfit = if restored.model != self.agent.model {
            Some(format!(
                "it was received from model {}, and this work asks {}",
                restored.model, self.agent.model
            ))
        } else if self.summary.executions > 0 || self.summary.evidence_hits > 0 {
            Some("this run has executed or reused evidence since it started".to_string())
        } else if observed != restored.observed.iter().collect::<Vec<_>>() {
            Some(format!(
                "it was received with {} observation(s) and this run holds {} others",
                restored.observed.len(),
                observed.len()
            ))
        } else {
            None
        };
        match unfit {
            None => Some(restored),
            Some(reason) => {
                self.events.push(WorkEvent::ModelResponseDiscarded {
                    work_id: self.id(),
                    turn,
                    record: restored.record.clone(),
                    reason,
                });
                None
            }
        }
    }

    /// Makes the one model call an escalation allows and returns its response, told to the
    /// owner before anything acts on it.
    async fn call_model(
        &mut self,
        turn: usize,
        reason: String,
        evidence: Vec<String>,
        boundary: &dyn WorkDecisionBoundary,
    ) -> Result<ModelResponse, WorkOutcome> {
        let question = boundary.question(&self.capabilities);
        let context = self.context(turn, &evidence, &question);
        // A policy may only leave out what Chip can account for; otherwise nothing is sent.
        if let Err(reason) = self.check_condensation(&context) {
            return Err(WorkOutcome::Failed {
                reason: format!("the context policy's condensation is not allowed: {reason}"),
                delivery: None,
            });
        }
        let metrics = context.metrics();
        // Fail closed: a request over the configured budget is not sent, trimmed or retried.
        if let Some(budget) = self.spec.context_budget_bytes {
            if metrics.bytes > budget {
                self.events.push(WorkEvent::ContextLimit {
                    work_id: self.id(),
                    turn,
                    request_bytes: metrics.bytes,
                    budget_bytes: budget,
                });
                return Err(WorkOutcome::LimitReached {
                    limit: LimitKind::Context,
                });
            }
        }
        if let Some(stopped) = self.stopped(None, None) {
            return Err(stopped);
        }
        self.summary.model_escalations += 1;
        self.summary.context_bytes += metrics.bytes;
        self.escalations.push(metrics);
        self.events.push(WorkEvent::ModelEscalation {
            work_id: self.id(),
            turn,
            reason,
            context_policy: self.context_policy.id().to_string(),
            context: metrics,
        });
        let started = Mark::now();
        let called = self
            .agent
            .model_turn(
                Turn::new(context.render_turn()).with_instructions(context.render_instructions()),
                &context.relevant_observations,
            )
            .await;
        self.latency.model += started.elapsed();
        self.events.push(WorkEvent::ModelCalled {
            work_id: self.id(),
            turn,
            usage: called
                .as_ref()
                .ok()
                .map(|(_, r)| r.usage.clone())
                .filter(|u| *u != ModelUsage::unreported()),
            succeeded: called.is_ok(),
        });
        // A call that failed while a stop was requested ended because of it (FX abandons the
        // attempt it was given the stop for); its delivery certainty is kept either way.
        let (_, response) = called.map_err(|e| {
            self.stopped(e.delivery(), e.remote_cancellation())
                .unwrap_or_else(|| WorkOutcome::Failed {
                    reason: format!("model escalation failed: {e}"),
                    delivery: e.delivery(),
                })
        })?;
        if let Some(control) = &self.spec.control {
            let observed: Vec<ExecutionId> = self
                .observations
                .iter()
                .map(|o| o.execution_id.clone())
                .collect();
            control.on_model_response(turn, &self.agent.model, &response, &observed);
        }
        Ok(response)
    }

    /// The next execution identity of this work: `<work id>-exec-<n>`, counting from 1. It is
    /// derived from nothing a provider or model supplied, so it cannot repeat within the work and
    /// is distinct across works whose ids are. It is not a cryptographic receipt.
    fn assign_execution_id(&mut self) -> ExecutionId {
        self.assigned += 1;
        ExecutionId::new(format!("{}-exec-{}", self.spec.id, self.assigned))
    }

    /// Whether an earlier observation of this capability may answer a request for it again.
    fn reusable(&self, capability: &CapabilityId) -> bool {
        self.capabilities
            .iter()
            .find(|c| &c.descriptor.id == capability)
            .is_none_or(|c| c.descriptor.reuse_evidence)
    }

    fn lookup(&self, request: &CapabilityRequest) -> EvidenceLookup {
        if !self.reusable(&request.capability_id) {
            return EvidenceLookup::NotFound;
        }
        match &self.spec.state {
            Some(state) => self.agent.lookup_valid_evidence(request, state),
            None => self.agent.lookup_evidence(request),
        }
    }

    /// Validate, execute once, observe, and record. The only way anything comes to be believed.
    async fn perform(&mut self, turn: usize, request: &CapabilityRequest) -> Step {
        if self.summary.executions >= self.spec.limits.max_executions {
            return Step::Done(WorkOutcome::LimitReached {
                limit: LimitKind::Executions,
            });
        }
        let execution = match self.agent.validate_capability_request(request).await {
            Ok(execution) => execution,
            Err(e) => {
                return Step::Done(WorkOutcome::Blocked {
                    reason: e.to_string(),
                });
            }
        };
        // The last check before anything executes: a stop requested while the model was answering
        // must not start the execution it asked for.
        if let Some(stopped) = self.stopped(None, None) {
            return Step::Done(stopped);
        }
        let started = Mark::now();
        let report = self.agent.execute(execution).await;
        self.latency.compute += started.elapsed();
        // An execution counts when the executor was actually invoked, which is what
        // `ExecutionStarted` records; an unavailable executor performed nothing.
        if report
            .events
            .iter()
            .any(|e| matches!(e, ExecutionEvent::ExecutionStarted { .. }))
        {
            self.summary.executions += 1;
        }
        self.events
            .extend(report.events.into_iter().map(WorkEvent::Execution));
        let result = match report.result {
            Ok(result) => result,
            Err(ExecutionError::ExecutorUnavailable(reason)) => {
                return Step::Done(WorkOutcome::Blocked { reason });
            }
            Err(e) => {
                return Step::Done(WorkOutcome::Failed {
                    reason: e.to_string(),
                    delivery: None,
                });
            }
        };
        let observation = match self.agent.observe(&result) {
            Ok(observation) => observation,
            Err(e) => {
                return Step::Done(WorkOutcome::Failed {
                    reason: e.to_string(),
                    delivery: None,
                });
            }
        };
        self.summary.observations += 1;
        self.events.push(WorkEvent::ObservationRecorded {
            work_id: self.id(),
            turn,
            execution_id: observation.execution_id.clone(),
            kind: observation.kind,
            receipt_id: observation.receipt_id.clone(),
        });
        let recorded = match &self.spec.state {
            Some(state) => self
                .agent
                .record_evidence_under(request, &observation, state),
            None => self.agent.record_evidence(request, &observation),
        };
        match recorded {
            Ok(true) => self.events.push(WorkEvent::EvidenceRecorded {
                work_id: self.id(),
                turn,
                capability: request.capability_id.clone(),
            }),
            Ok(false) => {}
            Err(e) => {
                return Step::Done(WorkOutcome::Failed {
                    reason: e.to_string(),
                    delivery: None,
                });
            }
        }
        if observation.status == crate::ExecutionStatus::Failure {
            self.note_failure(request, &observation);
        }
        self.record_origin(request);
        self.observations.push(observation.clone());
        self.evaluate_goal(turn, request, &observation);
        Step::Next
    }
}

/// Names the invocation a note is about. A capability with no inputs is named by its id. One with
/// inputs is named with them, so a note that this invocation failed or fell short is a note about
/// that invocation: another input to the same capability is a different invocation, and nothing
/// said here rules it out.
fn invocation_label(request: &CapabilityRequest) -> String {
    if request.inputs.is_empty() {
        return request.capability_id.to_string();
    }
    let shown: Vec<String> = request
        .inputs
        .iter()
        .map(|(name, value)| match value {
            InputValue::Text(text) => {
                let clipped: String = text.chars().take(40).collect();
                let more = if text.chars().count() > 40 { "..." } else { "" };
                format!("{name}={:?}", format!("{clipped}{more}"))
            }
            InputValue::Integer(n) => format!("{name}={n}"),
            InputValue::Bool(b) => format!("{name}={b}"),
        })
        .collect();
    format!("{} ({})", request.capability_id, shown.join(", "))
}

fn terminal_event(work_id: WorkId, outcome: &WorkOutcome) -> WorkEvent {
    match outcome {
        WorkOutcome::Completed { .. } => WorkEvent::WorkCompleted { work_id },
        WorkOutcome::Escalated { reason } => WorkEvent::WorkEscalated {
            work_id,
            reason: reason.clone(),
        },
        WorkOutcome::Blocked { reason } => WorkEvent::WorkBlocked {
            work_id,
            reason: reason.clone(),
        },
        WorkOutcome::LimitReached { limit } => WorkEvent::WorkLimitReached {
            work_id,
            limit: *limit,
        },
        WorkOutcome::Failed { reason, delivery } => WorkEvent::WorkFailed {
            work_id,
            reason: reason.clone(),
            delivery: *delivery,
        },
        WorkOutcome::Cancelled {
            cause,
            reason,
            delivery,
            remote,
        } => WorkEvent::WorkCancelled {
            work_id,
            cause: *cause,
            reason: reason.clone(),
            delivery: *delivery,
            remote: *remote,
        },
    }
}

impl Agent {
    /// Runs one bounded autonomous workload to a terminal outcome. See the module documentation.
    pub async fn run_work(
        &self,
        spec: &WorkSpec,
        policy: &dyn LocalWorkPolicy,
        boundary: &dyn WorkDecisionBoundary,
    ) -> WorkReport {
        self.run_work_with_context_policy(spec, policy, boundary, &FullEscalationContext)
            .await
    }

    /// [`run_work`](Self::run_work) with an explicit policy for what each escalation tells the
    /// model. `run_work` itself uses [`FullEscalationContext`].
    pub async fn run_work_with_context_policy(
        &self,
        spec: &WorkSpec,
        policy: &dyn LocalWorkPolicy,
        boundary: &dyn WorkDecisionBoundary,
        context_policy: &dyn EscalationContextPolicy,
    ) -> WorkReport {
        let started = Mark::now();
        let mut run = Run {
            agent: self,
            spec,
            context_policy,
            events: EventLog {
                events: Vec::new(),
                control: spec.control.clone(),
            },
            observations: Vec::new(),
            origins: Vec::new(),
            decisions: Vec::new(),
            ruled_out: Vec::new(),
            escalations: Vec::new(),
            capabilities: Vec::new(),
            summary: WorkSummary {
                turns: 0,
                executions: 0,
                observations: 0,
                evidence_hits: 0,
                local_decisions: 0,
                model_escalations: 0,
                context_bytes: 0,
                prompt_tokens: 0,
                completion_tokens: 0,
                elapsed: Duration::ZERO,
                terminal_state: TerminalState::Failed,
            },
            latency: WorkLatency::default(),
            satisfied: vec![false; spec.required_outputs.len() + spec.required_observations.len()],
            assigned: 0,
            restored_response: spec.restored_response.as_ref(),
            malformed_replies: 0,
            rejection: None,
        };

        run.events.push(WorkEvent::WorkStarted {
            work_id: spec.id.clone(),
            goal: spec.goal.as_str().to_string(),
            limits: spec.limits,
        });
        let outcome = self.drive(&mut run, policy, boundary).await;

        run.events.push(terminal_event(spec.id.clone(), &outcome));
        run.latency.total = started.elapsed();
        let latency = run.latency;

        // The summary is the measurement, derived from the events. The counters the loop kept
        // while running (they drive its limits) must agree with it.
        let measurement =
            WorkMeasurement::derive(spec.id.clone(), outcome.clone(), &run.events, latency);
        debug_assert_eq!(measurement.turns as usize, run.summary.turns);
        debug_assert_eq!(measurement.executions as usize, run.summary.executions);
        debug_assert_eq!(measurement.observations as usize, run.summary.observations);
        debug_assert_eq!(
            measurement.evidence_hits as usize,
            run.summary.evidence_hits
        );
        debug_assert_eq!(
            measurement.local_decisions as usize,
            run.summary.local_decisions
        );
        debug_assert_eq!(
            measurement.model_escalations as usize,
            run.summary.model_escalations
        );
        debug_assert_eq!(
            measurement.context_bytes as usize,
            run.summary.context_bytes
        );
        let tokens = events_usage(&run.events);
        let summary = WorkSummary {
            turns: measurement.turns as usize,
            executions: measurement.executions as usize,
            observations: measurement.observations as usize,
            evidence_hits: measurement.evidence_hits as usize,
            local_decisions: measurement.local_decisions as usize,
            model_escalations: measurement.model_escalations as usize,
            context_bytes: measurement.context_bytes as usize,
            prompt_tokens: tokens.0,
            completion_tokens: tokens.1,
            elapsed: latency.total,
            terminal_state: outcome.terminal_state(),
        };
        WorkReport {
            work_id: spec.id.clone(),
            outcome,
            summary,
            events: run.events.events,
            decisions: run.decisions,
            observations: run.observations,
            origins: run.origins,
            escalations: run.escalations,
            latency,
        }
    }

    async fn drive(
        &self,
        run: &mut Run<'_>,
        policy: &dyn LocalWorkPolicy,
        boundary: &dyn WorkDecisionBoundary,
    ) -> WorkOutcome {
        let spec = run.spec;
        if spec.goal.as_str().trim().is_empty() {
            return WorkOutcome::Failed {
                reason: "the goal cannot be empty".to_string(),
                delivery: None,
            };
        }

        // Capabilities are described once, here; the loop never discovers again.
        if self.capabilities.is_some() {
            let report = self.discover_capabilities().await;
            run.events
                .extend(report.events.into_iter().map(WorkEvent::Capability));
            match report.result {
                Ok(found) => run.capabilities = found,
                Err(e) => {
                    return WorkOutcome::Blocked {
                        reason: e.to_string(),
                    };
                }
            }
        }
        // What earlier runs recorded comes first, before anything is decided.
        if let Err(outcome) = run.restore() {
            return outcome;
        }

        loop {
            if let Some(stopped) = run.stopped(None, None) {
                return stopped;
            }
            // Asked after the stop check, so a stop always wins, and before the limit, so a loop is
            // named for what it is. It reads the trajectory the loop already keeps and nothing else:
            // no capability is executed and no model is asked.
            //
            // It yields to a completion the runtime itself can already justify: when the local
            // policy would complete the work now, and the goal's observations and the answer rule
            // accept it, repetition is not a reason to refuse it. (The same checks, in the same
            // order, as the proposal below; a policy is pure.) The guard never completes a work.
            if let Some(pattern) = spec
                .stuck_guard
                .as_ref()
                .and_then(|t| crate::detect_stuck(&run.origins, &run.observations, t))
            {
                let completes_now = matches!(
                    policy.propose(&run.view(run.summary.turns)),
                    Some(WorkDecision::Complete { summary })
                        if !run.completion_refused() && !run.answer_refused(&summary)
                );
                if !completes_now {
                    let goal_met = !run.satisfied.is_empty() && !run.completion_refused();
                    return WorkOutcome::Blocked {
                        reason: pattern.reason_with_recovery(
                            &run.origins,
                            &run.observations,
                            goal_met,
                        ),
                    };
                }
            }
            if run.summary.turns >= spec.limits.max_turns {
                return WorkOutcome::LimitReached {
                    limit: LimitKind::Turns,
                };
            }
            let turn = run.summary.turns;
            run.summary.turns += 1;
            run.events.push(WorkEvent::DecisionStarted {
                work_id: spec.id.clone(),
                turn,
            });

            // 1. Local: a proposal, assessed by the evidence machinery when it is a request.
            let local_started = Mark::now();
            let proposal = policy.propose(&run.view(turn));
            run.latency.local_decision += local_started.elapsed();
            let mut already_checked = false;
            let (decision, escalation) = match proposal {
                // A capability whose result depends on state that changes between requests is
                // never answered from evidence: the request is performed.
                Some(WorkDecision::RequestCapability(request))
                    if !run.reusable(&request.capability_id) =>
                {
                    already_checked = true;
                    let decision = WorkDecision::RequestCapability(request);
                    run.decide_locally(turn, &decision);
                    (Some(decision), None)
                }
                Some(WorkDecision::RequestCapability(request)) => {
                    already_checked = true;
                    let assess_started = Mark::now();
                    let assessed = self.assess_evidence(&request, spec.state.as_ref());
                    run.latency.local_decision += assess_started.elapsed();
                    match assessed {
                        Ok(Assessment::Reuse(observation)) => {
                            let decision = WorkDecision::RequestCapability(request.clone());
                            run.decide_locally(turn, &decision);
                            run.events.push(WorkEvent::DecisionMade {
                                work_id: spec.id.clone(),
                                turn,
                                decision: decision.label(),
                            });
                            run.events.push(WorkEvent::CapabilityRequested {
                                work_id: spec.id.clone(),
                                turn,
                                capability: request.capability_id.clone(),
                            });
                            run.reuse(turn, &request, observation);
                            continue;
                        }
                        Ok(Assessment::Continue { .. }) => {
                            let decision = WorkDecision::RequestCapability(request);
                            run.decide_locally(turn, &decision);
                            (Some(decision), None)
                        }
                        Ok(Assessment::Escalate { reason }) => {
                            let evidence = vec![format!(
                                "{}: no valid evidence (absent or stale)",
                                request.capability_id
                            )];
                            (None, Some((format!("local reasoner: {reason}"), evidence)))
                        }
                        // No reasoner is installed (the product configuration): a locally proposed
                        // request without valid evidence is not run on Chip's own say-so. The
                        // model is asked, exactly as for an `Escalate` verdict.
                        Err(AgentError::Reasoning(ReasoningError::Unavailable(_))) => (
                            None,
                            Some((
                                "no valid evidence for the locally proposed request".to_string(),
                                vec![format!(
                                    "{}: no valid evidence (absent or stale)",
                                    request.capability_id
                                )],
                            )),
                        ),
                        Err(e) => {
                            return WorkOutcome::Failed {
                                reason: e.to_string(),
                                delivery: None,
                            };
                        }
                    }
                }
                // A local completion the evidence does not support is refused before it is
                // recorded: the turn is then an ordinary escalation, with the evidence as it is.
                Some(WorkDecision::Complete { summary })
                    if run.completion_refused() || run.answer_refused(&summary) =>
                (
                    None,
                    Some((
                        "the local completion was refused: no authoritative observation satisfies the goal"
                            .to_string(),
                        vec![format!("goal progress: {}", run.progress_line())],
                    )),
                ),
                Some(terminal) => {
                    run.decide_locally(turn, &terminal);
                    (Some(terminal), None)
                }
                None => (
                    None,
                    Some(("no local decision is available".to_string(), Vec::new())),
                ),
            };

            // 2. Model: exactly one call per escalation, no retry within the turn. A malformed
            // reply ends the turn with nothing decided (`DecisionRejected`); the next turn is an
            // ordinary one, under every limit, the deadline and the stuck guard.
            let decision = match (decision, escalation) {
                (Some(decision), _) => decision,
                (None, Some((reason, evidence))) => {
                    match run.escalate(turn, reason, evidence, boundary).await {
                        Ok(Some(decision)) => decision,
                        Ok(None) => continue,
                        Err(outcome) => return outcome,
                    }
                }
                (None, None) => unreachable!("a decision or an escalation is always produced"),
            };
            run.events.push(WorkEvent::DecisionMade {
                work_id: spec.id.clone(),
                turn,
                decision: decision.label(),
            });

            match decision {
                WorkDecision::Complete { .. } if run.completion_refused() => {
                    // The claim is the model's; the evidence is reality's. Reality wins, and the
                    // work does not continue on the strength of a claim it just refused.
                    return WorkOutcome::Blocked {
                        reason:
                            "completion refused: no authoritative observation satisfies the goal"
                                .to_string(),
                    };
                }
                WorkDecision::Complete { summary } if run.answer_refused(&summary) => {
                    // The goal's observations hold, but the proposed answer is not supported by
                    // them. Same rule: the claim is refused and the work does not go on from it.
                    return WorkOutcome::Blocked {
                        reason: "completion refused: the proposed answer is not supported by the observations"
                            .to_string(),
                    };
                }
                WorkDecision::Complete { summary } => return WorkOutcome::Completed { summary },
                WorkDecision::Escalate { reason } => return WorkOutcome::Escalated { reason },
                WorkDecision::Block { reason } => return WorkOutcome::Blocked { reason },
                WorkDecision::RequestCapability(request) => {
                    run.events.push(WorkEvent::CapabilityRequested {
                        work_id: spec.id.clone(),
                        turn,
                        capability: request.capability_id.clone(),
                    });
                    // A request the model made gets the same evidence check a local one got.
                    if !already_checked {
                        if let EvidenceLookup::Found(observation) = run.lookup(&request) {
                            run.reuse(turn, &request, observation);
                            continue;
                        }
                    }
                    match run.perform(turn, &request).await {
                        Step::Next => {}
                        Step::Done(outcome) => return outcome,
                    }
                }
            }
        }
    }
}

/// Prompt and completion tokens the provider reported, summed over the trajectory.
fn events_usage(events: &[WorkEvent]) -> (u64, u64) {
    events.iter().fold((0, 0), |(p, c), e| match e {
        WorkEvent::ModelCalled { usage: Some(u), .. } => (
            p + u.prompt_tokens.unwrap_or(0),
            c + u.completion_tokens.unwrap_or(0),
        ),
        _ => (p, c),
    })
}

/// What an independent audit of a finished run found. Every counter must be zero. The audit reads
/// the event stream and the authoritative observations only, never the loop's own counters, so a
/// fault in the loop cannot hide itself from it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SafetyAudit {
    /// Executions the runtime never authorised: started without a requested execution, or beyond
    /// the execution limit.
    pub unauthorized_executions: usize,
    /// A completion without a completion decision, or while the loop's own evaluation still had
    /// requirements outstanding.
    pub unauthorized_completions: usize,
    /// A completion although some required output was never produced by an authoritative
    /// observation. Checked against the observations themselves, not against `GoalEvaluated`.
    pub false_completions: usize,
    pub evidence_without_observation: usize,
    pub observation_without_execution: usize,
    /// An execution requested without a preceding request for a declared capability.
    pub execution_without_valid_request: usize,
    /// More decisions than `max_turns` allows.
    pub limit_violations: usize,
    /// Violations of the spec's observation invariants, by invariant name (every declared
    /// invariant is listed, with `0` when it held).
    pub invariant_violations: std::collections::BTreeMap<String, usize>,
    /// An earlier observation answered a request for a capability the spec prohibits reusing.
    pub stale_evidence_reuse: usize,
    /// Events recorded after the work reached a terminal state, or a second terminal state.
    pub events_after_terminal: usize,
    /// Model requests Chip refused to send because they would have exceeded the context budget.
    /// Not a violation: it is the budget doing its job.
    pub context_limit_rejections: usize,
    /// A model request that was sent although it exceeded the context budget.
    pub context_budget_violations: usize,
    /// An observation left out of a request that its capability's contract did not allow to be
    /// left out, or that no later identical observation made redundant.
    pub unjustified_omissions: usize,
    pub details: Vec<String>,
}

impl SafetyAudit {
    /// Violations of one observation invariant (`0` when it held or was not declared).
    pub fn violations_of(&self, name: &str) -> usize {
        self.invariant_violations.get(name).copied().unwrap_or(0)
    }

    pub fn is_clean(&self) -> bool {
        self.unauthorized_executions == 0
            && self.unauthorized_completions == 0
            && self.false_completions == 0
            && self.evidence_without_observation == 0
            && self.observation_without_execution == 0
            && self.execution_without_valid_request == 0
            && self.limit_violations == 0
            && self.context_budget_violations == 0
            && self.unjustified_omissions == 0
            && self.invariant_violations.values().all(|n| *n == 0)
            && self.stale_evidence_reuse == 0
            && self.events_after_terminal == 0
    }

    /// Fails loudly: a violated invariant is not a metric to be reported, it is a failure.
    pub fn assert_clean(&self) {
        assert!(self.is_clean(), "SAFETY INVARIANT VIOLATED: {self:#?}");
    }
}

/// Audits a finished run against the invariants no model judgment may weaken. `declared` is the
/// capability set the run was given.
pub fn audit_safety(
    report: &WorkReport,
    spec: &WorkSpec,
    declared: &[CapabilityId],
) -> SafetyAudit {
    #[derive(Default)]
    struct Turn {
        requested: Option<CapabilityId>,
        execution_requested: bool,
        started: bool,
        observed: bool,
    }
    let mut audit = SafetyAudit::default();
    let mut turn: Option<Turn> = None;
    let (mut executions, mut turns) = (0usize, 0usize);
    let mut last_remaining: Option<usize> = None;
    let mut completion_decided = false;
    let mut terminal_seen = false;
    let mut observations_known = 0usize;
    for event in &report.events {
        if matches!(
            event,
            WorkEvent::EvidenceReused { .. } | WorkEvent::ObservationRestored { .. }
        ) {
            observations_known += 1;
        }
        if terminal_seen {
            audit.events_after_terminal += 1;
            audit
                .details
                .push("an event was recorded after the work ended".to_string());
        }
        if matches!(
            event,
            WorkEvent::WorkCompleted { .. }
                | WorkEvent::WorkEscalated { .. }
                | WorkEvent::WorkBlocked { .. }
                | WorkEvent::WorkLimitReached { .. }
                | WorkEvent::WorkFailed { .. }
                | WorkEvent::WorkCancelled { .. }
        ) {
            terminal_seen = true;
        }
        match event {
            WorkEvent::EvidenceReused { capability, .. }
                if spec.evidence_reuse_prohibited.contains(capability) =>
            {
                audit.stale_evidence_reuse += 1;
                audit
                    .details
                    .push(format!("evidence for {capability} was reused"));
            }
            WorkEvent::ContextLimit { .. } => audit.context_limit_rejections += 1,
            WorkEvent::ModelEscalation { context, .. } => {
                if spec
                    .context_budget_bytes
                    .is_some_and(|budget| context.bytes > budget)
                {
                    audit.context_budget_violations += 1;
                    audit.details.push(format!(
                        "a request of {} bytes was sent over the budget",
                        context.bytes
                    ));
                }
                // What was left out must be justified by the observations themselves, read again
                // here: only what a contract allowed, and only what a later identical one covers.
                let allowed = omissions(
                    &report.origins[..observations_known.min(report.origins.len())],
                    &report.observations[..observations_known.min(report.observations.len())],
                )
                .len();
                if context.omitted_observations > allowed {
                    audit.unjustified_omissions += context.omitted_observations - allowed;
                    audit
                        .details
                        .push("an observation was left out of a request without cause".to_string());
                }
                // A condensed observation must be a successful one the request did not also omit
                // as a repeat: at most that many can have been condensed. (Chip checked each one
                // by name before sending; this re-reads the recorded observations for the count.)
                let successes = report.observations
                    [..observations_known.min(report.observations.len())]
                    .iter()
                    .filter(|o| {
                        o.kind == crate::ObservationKind::ExecutionCompleted
                            && o.status == crate::ExecutionStatus::Success
                    })
                    .count();
                if context.condensed_observations > successes.saturating_sub(allowed) {
                    audit.unjustified_omissions +=
                        context.condensed_observations - successes.saturating_sub(allowed);
                    audit.details.push(
                        "an observation was condensed out of a request without cause".to_string(),
                    );
                }
            }
            WorkEvent::DecisionStarted { .. } => {
                turns += 1;
                turn = Some(Turn::default());
            }
            WorkEvent::DecisionMade { decision, .. } if decision == "complete" => {
                completion_decided = true;
            }
            WorkEvent::CapabilityRequested { capability, .. } => {
                if let Some(t) = &mut turn {
                    t.requested = Some(capability.clone());
                }
            }
            WorkEvent::Execution(ExecutionEvent::ExecutionRequested { intent, .. }) => {
                let valid = turn
                    .as_ref()
                    .and_then(|t| t.requested.as_ref())
                    .is_some_and(|c| c.as_str() == intent && declared.contains(c));
                if !valid {
                    audit.execution_without_valid_request += 1;
                    audit
                        .details
                        .push(format!("execution of '{intent}' without a valid request"));
                }
                if let Some(t) = &mut turn {
                    t.execution_requested = true;
                }
            }
            WorkEvent::Execution(ExecutionEvent::ExecutionStarted { id }) => {
                executions += 1;
                if !turn.as_ref().is_some_and(|t| t.execution_requested) {
                    audit.unauthorized_executions += 1;
                    audit
                        .details
                        .push(format!("execution {id:?} started without being requested"));
                }
                if let Some(t) = &mut turn {
                    t.started = true;
                }
            }
            WorkEvent::ObservationRecorded { execution_id, .. } => {
                observations_known += 1;
                if !turn.as_ref().is_some_and(|t| t.started) {
                    audit.observation_without_execution += 1;
                    audit
                        .details
                        .push(format!("observation {execution_id:?} without an execution"));
                }
                if let Some(t) = &mut turn {
                    t.observed = true;
                }
            }
            WorkEvent::EvidenceRecorded { capability, .. } => {
                if !turn.as_ref().is_some_and(|t| t.observed) {
                    audit.evidence_without_observation += 1;
                    audit
                        .details
                        .push(format!("evidence for {capability} without an observation"));
                }
            }
            WorkEvent::GoalEvaluated { remaining, .. } => last_remaining = Some(*remaining),
            // A restored observation is evaluated as it is joined; requirements without any
            // evaluation stay unknown, as before.
            WorkEvent::ObservationRestored { remaining, .. }
                if !spec.required_outputs.is_empty() || !spec.required_observations.is_empty() =>
            {
                last_remaining = Some(*remaining)
            }
            WorkEvent::WorkCompleted { .. } => {
                if !completion_decided {
                    audit.unauthorized_completions += 1;
                    audit
                        .details
                        .push("completed without a completion decision".to_string());
                }
                if let WorkOutcome::Completed { summary } = &report.outcome {
                    for predicate in &spec.required_answers {
                        if !predicate.accepts(summary, &report.observations) {
                            audit.unauthorized_completions += 1;
                            audit.details.push(format!(
                                "completed with an answer that does not satisfy: {}",
                                predicate.describe()
                            ));
                        }
                    }
                }
                if !spec.required_outputs.is_empty() || !spec.required_observations.is_empty() {
                    if last_remaining != Some(0) {
                        audit.unauthorized_completions += 1;
                        audit
                            .details
                            .push("completed with requirements outstanding".to_string());
                    }
                    let produced: Vec<&str> = report
                        .observations
                        .iter()
                        .filter(|o| {
                            o.kind == ObservationKind::ExecutionCompleted
                                && o.status == crate::ExecutionStatus::Success
                        })
                        .filter_map(|o| o.output.as_deref().map(str::trim))
                        .collect();
                    for required in &spec.required_outputs {
                        if !produced.contains(&required.trim()) {
                            audit.false_completions += 1;
                            audit
                                .details
                                .push(format!("completed, but '{required}' was never observed"));
                        }
                    }
                    // A required observation is checked against the recorded observations
                    // themselves, not against what the loop's evaluation said.
                    for predicate in &spec.required_observations {
                        if !predicate.satisfied_by_trajectory(&report.observations) {
                            audit.false_completions += 1;
                            audit.details.push(format!(
                                "completed, but no observation establishes: {}",
                                predicate.describe()
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for invariant in &spec.observation_invariants {
        let n = invariant.violations_in_trajectory(&report.observations);
        audit
            .invariant_violations
            .insert(invariant.name().to_string(), n);
        if n > 0 {
            audit
                .details
                .push(format!("{n} observation(s) violate {}", invariant.name()));
        }
    }
    if executions > spec.limits.max_executions {
        audit.unauthorized_executions += executions - spec.limits.max_executions;
        audit.details.push(format!(
            "{executions} executions exceed the limit of {}",
            spec.limits.max_executions
        ));
    }
    if turns > spec.limits.max_turns {
        audit.limit_violations += turns - spec.limits.max_turns;
        audit.details.push(format!(
            "{turns} turns exceed the limit of {}",
            spec.limits.max_turns
        ));
    }
    audit
}

/// How much verified useful work a run produced, and what it cost. Derived from a finished run and
/// its spec: from the authoritative observations and the event stream, never from a model's words,
/// a capability selection, a receipt, or a counter the loop kept.
///
/// Utility is not safety. [`audit_safety`] does not read this, and this does not read it.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkUtilityMeasurement {
    pub required_outputs: usize,
    /// Required outputs that some authoritative observation actually produced.
    pub verified_outputs: usize,
    /// `verified_outputs / required_outputs`, in `[0, 1]`; `0.0` when nothing was required.
    pub goal_coverage: f64,
    /// The runtime completed the work. Completion needs every required output verified.
    pub completed: bool,
    pub turns: usize,
    pub model_calls: usize,
    pub executions: usize,
    /// The run ended on a decision the runtime refused (an unusable reply, or an invocation
    /// that carried what the capability does not take).
    pub invalid_decisions: usize,
    /// Executions that ran and observed fine but produced none of the required outputs.
    pub wrong_valid_decisions: usize,
    /// Requests answered from existing evidence: a valid selection that added no verified work.
    pub redundant_selections: usize,
    /// Work after the first unsatisfied authoritative observation: turns begun, executions made
    /// and model calls made after it. This is everything that followed, not only the extra cost
    /// of having been wrong; compare with a matched all-correct run for that.
    pub recovery_turns: usize,
    pub recovery_executions: usize,
    pub recovery_model_calls: usize,
    /// `None` when no call after it reported usage.
    pub recovery_tokens: Option<u64>,
    pub total_latency_ms: u64,
    pub model_latency_ms: u64,
    pub compute_latency_ms: u64,
    /// What providers reported. `None` when no call reported any: nothing is estimated.
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    /// Executions that actually started, by the capability the request named.
    pub executions_by_capability: std::collections::BTreeMap<String, usize>,
    /// Observations of executions that failed.
    pub failed_observations: usize,
    /// Failed observations that were followed by a further execution: the runtime let bounded
    /// judgment continue after reality said no. Counts attempts, not successes.
    pub recoveries: usize,
}

impl WorkUtilityMeasurement {
    fn per(&self, denominator: f64) -> Option<f64> {
        (denominator > 0.0).then(|| self.verified_outputs as f64 / denominator)
    }

    pub fn work_per_model_call(&self) -> Option<f64> {
        self.per(self.model_calls as f64)
    }

    pub fn work_per_execution(&self) -> Option<f64> {
        self.per(self.executions as f64)
    }

    /// Only when a provider reported usage.
    pub fn work_per_token(&self) -> Option<f64> {
        self.total_tokens.and_then(|t| self.per(t as f64))
    }

    pub fn work_per_second(&self) -> Option<f64> {
        self.per(self.total_latency_ms as f64 / 1000.0)
    }
}

/// Measures the verified useful work of a finished run against its spec.
pub fn measure_utility(report: &WorkReport, spec: &WorkSpec) -> WorkUtilityMeasurement {
    let required = spec.required_outputs.len() + spec.required_observations.len();
    // Only an authoritative observation of a completed execution can produce a required output.
    let produced: Vec<&str> = report
        .observations
        .iter()
        .filter(|o| {
            o.kind == ObservationKind::ExecutionCompleted
                && o.status == crate::ExecutionStatus::Success
        })
        .filter_map(|o| o.output.as_deref().map(str::trim))
        .collect();
    let verified = spec
        .required_outputs
        .iter()
        .filter(|r| produced.contains(&r.trim()))
        .count()
        + spec
            .required_observations
            .iter()
            .filter(|p| p.satisfied_by_trajectory(&report.observations))
            .count();

    // One walk over the trajectory: what each turn did, and where the first miss was.
    let (mut turns, mut model_calls, mut executions) = (0usize, 0usize, 0usize);
    let (mut wrong_valid, mut redundant) = (0usize, 0usize);
    let mut executed_this_turn = false;
    let mut requested: Option<String> = None;
    let mut by_capability: std::collections::BTreeMap<String, usize> = Default::default();
    let (mut failed_observations, mut recoveries, mut unrecovered) = (0usize, 0usize, 0usize);
    let mut first_miss: Option<usize> = None;
    let (mut input, mut output): (Option<u64>, Option<u64>) = (None, None);
    let add = |slot: &mut Option<u64>, n: u64| *slot = Some(slot.unwrap_or(0) + n);
    for (at, event) in report.events.iter().enumerate() {
        match event {
            WorkEvent::DecisionStarted { .. } => {
                turns += 1;
                executed_this_turn = false;
            }
            WorkEvent::ModelCalled { usage, .. } => {
                model_calls += 1;
                if let Some(u) = usage {
                    if let Some(n) = u.prompt_tokens {
                        add(&mut input, n);
                    }
                    if let Some(n) = u.completion_tokens {
                        add(&mut output, n);
                    }
                }
            }
            WorkEvent::CapabilityRequested { capability, .. } => {
                requested = Some(capability.to_string());
            }
            WorkEvent::Execution(ExecutionEvent::ExecutionStarted { .. }) => {
                executions += 1;
                executed_this_turn = true;
                if let Some(capability) = &requested {
                    *by_capability.entry(capability.clone()).or_default() += 1;
                }
                recoveries += unrecovered;
                unrecovered = 0;
            }
            WorkEvent::ObservationRecorded { kind, .. }
                if *kind == ObservationKind::ExecutionFailed =>
            {
                failed_observations += 1;
                unrecovered += 1;
            }
            WorkEvent::EvidenceReused { .. } => redundant += 1,
            WorkEvent::GoalEvaluated {
                satisfied: false, ..
            } if executed_this_turn => {
                wrong_valid += 1;
                first_miss.get_or_insert(at);
            }
            _ => {}
        }
    }

    // What followed the first unsatisfied observation.
    let (mut r_turns, mut r_exec, mut r_calls) = (0usize, 0usize, 0usize);
    let mut r_tokens: Option<u64> = None;
    if let Some(miss) = first_miss {
        for event in &report.events[miss + 1..] {
            match event {
                WorkEvent::DecisionStarted { .. } => r_turns += 1,
                WorkEvent::Execution(ExecutionEvent::ExecutionStarted { .. }) => r_exec += 1,
                WorkEvent::ModelCalled { usage, .. } => {
                    r_calls += 1;
                    if let Some(tokens) = usage.as_ref().and_then(reported_tokens) {
                        add(&mut r_tokens, tokens);
                    }
                }
                _ => {}
            }
        }
    }

    let invalid = matches!(&report.outcome, WorkOutcome::Failed { reason, .. }
            if reason.contains("not a valid decision"))
        || matches!(&report.outcome, WorkOutcome::Blocked { reason }
            if reason.starts_with("invalid capability input"));
    WorkUtilityMeasurement {
        required_outputs: required,
        verified_outputs: verified,
        goal_coverage: if required == 0 {
            0.0
        } else {
            verified as f64 / required as f64
        },
        completed: matches!(report.outcome, WorkOutcome::Completed { .. }),
        turns,
        model_calls,
        executions,
        invalid_decisions: usize::from(invalid),
        wrong_valid_decisions: wrong_valid,
        redundant_selections: redundant,
        recovery_turns: r_turns,
        recovery_executions: r_exec,
        recovery_model_calls: r_calls,
        recovery_tokens: r_tokens,
        total_latency_ms: report.latency.total.as_millis() as u64,
        model_latency_ms: report.latency.model.as_millis() as u64,
        compute_latency_ms: report.latency.compute.as_millis() as u64,
        input_tokens: input,
        output_tokens: output,
        total_tokens: input.zip(output).map(|(i, o)| i + o),
        executions_by_capability: by_capability,
        failed_observations,
        recoveries,
    }
}
