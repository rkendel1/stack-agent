//! The one-attempt FX inference executor.
//!
//! [`execute`] validates a request, starts the supplied capability at most once,
//! drives it in bounded batches, normalizes every event, forwards normalized events
//! to the host, and returns exactly one terminal [`Outcome`] with content-free
//! [`Diagnostics`].
//!
//! It never retries, falls back, selects models, refreshes credentials, reconnects,
//! plans, executes tools or recovers across turns. Those belong to Easy-LLM (inside
//! the capability) and Chip (around the executor).
//!
//! ## Cancellation and budget (what is and is not guaranteed)
//!
//! The core has no clock, thread or async runtime. Therefore:
//!
//! - The executor checks [`Host::cancel_requested`] before starting the capability and
//!   between polls, and treats an observer that returns [`Flow::Stop`] as abandonment.
//!   It cannot interrupt a capability call that is in progress: a `poll` that blocks
//!   delays cancellation until it returns. The capability contract requires `poll`
//!   and `cancel` not to block indefinitely.
//! - The attempt budget is passed to the capability ([`AttemptContext::budget`]),
//!   which owns the clock and must report [`ErrorKind::Timeout`]. A host may also
//!   enforce a deadline by returning `true` from `cancel_requested` or
//!   [`Flow::Stop`] from [`Host::idle`].
//! - Cancellation is always reported with the delivery certainty observed so far.
//!   A cancelled outcome never claims the remote operation did not execute unless
//!   delivery is still `DefinitelyUnsent` (invariant 4).

use crate::capability::{AttemptContext, CancelStatus, Capability, Started, StreamState};
use crate::contract::{
    Cancellation, CapabilityError, DeliveryCertainty, ErrorKind, Event, Failure, FailureOrigin,
    Limits, Outcome, RemoteCancellation, Request,
};
use crate::diagnostics::{Diagnostics, TerminalCause};
use crate::normalize::{Disposition, Normalizer, ProposalCheck, Pushed, classify};
use crate::validate::validate_request;
use crate::wire::{RequestRejection, parse_request};

/// Whether the host wants the attempt to continue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Flow {
    /// Keep going.
    Continue,
    /// Stop: the executor cancels the capability and returns `Cancelled`.
    Stop,
}

/// The executor's view of its host: cancellation, event delivery and idling.
///
/// All methods have defaults, so a host implements only what it needs.
pub trait Host {
    /// Polled before start and between polls. Return `true` to cancel.
    fn cancel_requested(&mut self) -> bool {
        false
    }

    /// Receives each normalized event in order, before the terminal outcome.
    /// Return [`Flow::Stop`] to abandon the attempt.
    fn on_event(&mut self, _event: &Event) -> Flow {
        Flow::Continue
    }

    /// Called when a poll returned no events and the stream is still open. A host
    /// may wait here (sleep, yield, block on a channel). Return [`Flow::Stop`] to
    /// cancel, for example when its own deadline passed.
    fn idle(&mut self) -> Flow {
        Flow::Continue
    }
}

/// A host that never cancels and ignores events.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoHost;

impl Host for NoHost {}

/// Everything an attempt produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionReport {
    /// Exactly one terminal outcome.
    pub outcome: Outcome,
    /// Per-proposal identity and argument checks, index-aligned with
    /// `Completion.proposals` (empty unless completed).
    pub proposal_checks: Vec<ProposalCheck>,
    /// Classification of a completed outcome.
    pub disposition: Option<Disposition>,
    /// Content-free diagnostics.
    pub diagnostics: Diagnostics,
}

fn executor_failure(
    kind: ErrorKind,
    delivery: DeliveryCertainty,
    partial: Option<crate::contract::PartialOutput>,
) -> Outcome {
    Outcome::Failed(Failure {
        origin: FailureOrigin::Executor,
        kind,
        delivery,
        retry_after: None,
        status_hint: None,
        detail: None,
        partial,
    })
}

fn capability_failure(
    error: CapabilityError,
    delivery: DeliveryCertainty,
    partial: Option<crate::contract::PartialOutput>,
) -> Outcome {
    Outcome::Failed(Failure {
        origin: FailureOrigin::Capability,
        kind: error.kind,
        delivery,
        retry_after: error.retry_after,
        status_hint: error.status_hint,
        detail: error.detail,
        partial,
    })
}

/// Decodes, validates and executes a request document. Decode and validation
/// failures become `Failed { origin: Executor, kind: InvalidRequest }` without
/// invoking the capability.
pub fn execute_document<C: Capability, H: Host>(
    capability: &mut C,
    request_bytes: &[u8],
    limits: &Limits,
    host: &mut H,
) -> ExecutionReport {
    match parse_request(request_bytes, limits) {
        Ok(request) => execute(capability, &request, limits, host),
        Err(rejection) => {
            let mut diagnostics = Diagnostics::new(None);
            diagnostics.terminal = match rejection {
                RequestRejection::Invalid(e) => TerminalCause::InvalidRequest(e.code),
                RequestRejection::Decode(_) => {
                    TerminalCause::InvalidRequest(crate::validate::ValidationCode::Undecodable)
                }
            };
            ExecutionReport {
                outcome: executor_failure(
                    ErrorKind::InvalidRequest,
                    DeliveryCertainty::DefinitelyUnsent,
                    None,
                ),
                proposal_checks: Vec::new(),
                disposition: None,
                diagnostics,
            }
        }
    }
}

/// Executes one attempt. See the module documentation for guarantees.
///
/// This is a loop over [`Execution::step`]: it calls [`Host::idle`] whenever a step
/// reports that nothing was available, and stops the attempt when the host does.
pub fn execute<C: Capability, H: Host>(
    capability: &mut C,
    request: &Request,
    limits: &Limits,
    host: &mut H,
) -> ExecutionReport {
    let mut execution = Execution::new(request, limits);
    loop {
        match execution.step(capability, host) {
            Step::Progress => {}
            Step::Idle => {
                if host.idle() == Flow::Stop
                    && let Step::Done(report) = execution.cancel(capability)
                {
                    return *report;
                }
            }
            Step::Done(report) => return *report,
            // Unreachable: `Finished` follows `Done`, and `Done` returns above. Fail
            // closed rather than loop forever if that ever changes.
            Step::Finished => {
                return ExecutionReport {
                    outcome: executor_failure(
                        ErrorKind::Internal,
                        DeliveryCertainty::DefinitelyUnsent,
                        None,
                    ),
                    proposal_checks: Vec::new(),
                    disposition: None,
                    diagnostics: Diagnostics::new(Some(request)),
                };
            }
        }
    }
}

/// What one [`Execution::step`] achieved.
#[derive(Debug)]
pub enum Step {
    /// The attempt advanced; call `step` again.
    Progress,
    /// The capability had nothing to deliver and the stream is still open. The caller
    /// decides what happens next: wait for the host's environment (an event loop, a
    /// network read) and step again, call [`Execution::cancel`], or, as [`execute`]
    /// does, consult [`Host::idle`].
    Idle,
    /// The attempt ended. Returned exactly once.
    Done(Box<ExecutionReport>),
    /// The attempt had already ended and its report was already returned.
    Finished,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Init,
    Driving,
    Ended,
}

/// How one pass through the drive loop ended the attempt.
enum Stop {
    Finished,
    Fail(Box<Outcome>, TerminalCause, bool),
    Cancel(TerminalCause),
}

fn fail(outcome: Outcome, cause: TerminalCause, cancel_remote: bool) -> Stop {
    Stop::Fail(Box::new(outcome), cause, cancel_remote)
}

/// One attempt as a resumable state machine.
///
/// [`execute`] runs it to completion in one call. A host that cannot block (a
/// WebAssembly module whose provider data arrives from an event loop) calls
/// [`Execution::step`] repeatedly and returns to its own loop on [`Step::Idle`].
/// Both paths share this code, so they have identical semantics: the same validation
/// first, the same single start, the same normalization, bounds and terminal rules.
pub struct Execution<C: Capability> {
    request: Request,
    limits: Limits,
    diagnostics: Diagnostics,
    normalizer: Option<Normalizer>,
    handle: Option<C::Handle>,
    phase: Phase,
}

impl<C: Capability> std::fmt::Debug for Execution<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Execution")
            .field("started", &self.handle.is_some())
            .field("ended", &(self.phase == Phase::Ended))
            .finish()
    }
}

impl<C: Capability> Execution<C> {
    /// Creates an attempt for `request`. Nothing happens until the first `step`.
    pub fn new(request: &Request, limits: &Limits) -> Self {
        Execution {
            request: request.clone(),
            limits: *limits,
            diagnostics: Diagnostics::new(Some(request)),
            normalizer: None,
            handle: None,
            phase: Phase::Init,
        }
    }

    /// Advances the attempt by one unit of work: validation, cancellation check and
    /// start on the first call, then one cancellation check and one poll per call.
    pub fn step<H: Host>(&mut self, capability: &mut C, host: &mut H) -> Step {
        match self.phase {
            Phase::Ended => Step::Finished,
            Phase::Init => self.init(capability, host),
            Phase::Driving => match self.drive_once(capability, host) {
                Ok(step) => step,
                Err(stop) => self.finish(capability, stop),
            },
        }
    }

    /// Ends the attempt as cancelled. Before the first `step` nothing was started, so
    /// the outcome is `Cancelled { DefinitelyUnsent, NotStarted }`; afterwards the
    /// capability is cancelled and the outcome carries the delivery observed so far.
    /// Calling it after the attempt ended returns [`Step::Finished`].
    pub fn cancel(&mut self, capability: &mut C) -> Step {
        match self.phase {
            Phase::Ended => Step::Finished,
            Phase::Init => {
                self.phase = Phase::Ended;
                self.diagnostics.terminal = TerminalCause::CancelledBeforeStart;
                Step::Done(Box::new(cancelled_before_start(self.diagnostics.clone())))
            }
            Phase::Driving => self.finish(capability, Stop::Cancel(TerminalCause::Cancelled)),
        }
    }

    fn init<H: Host>(&mut self, capability: &mut C, host: &mut H) -> Step {
        // 1. Validate before anything else; an invalid request never reaches the capability.
        if let Err(error) = validate_request(&self.request, &self.limits) {
            self.phase = Phase::Ended;
            self.diagnostics.terminal = TerminalCause::InvalidRequest(error.code);
            return Step::Done(Box::new(ExecutionReport {
                outcome: executor_failure(
                    ErrorKind::InvalidRequest,
                    DeliveryCertainty::DefinitelyUnsent,
                    None,
                ),
                proposal_checks: Vec::new(),
                disposition: None,
                diagnostics: self.diagnostics.clone(),
            }));
        }

        // 2. Cancellation before start: nothing was sent.
        if host.cancel_requested() {
            self.phase = Phase::Ended;
            self.diagnostics.terminal = TerminalCause::CancelledBeforeStart;
            return Step::Done(Box::new(cancelled_before_start(self.diagnostics.clone())));
        }

        let mut normalizer = Normalizer::new(&self.request, &self.limits);
        let context = AttemptContext {
            request: &self.request,
            limits: &self.limits,
            budget: self.request.budget,
        };

        // 3. Exactly one start.
        self.diagnostics.starts = 1;
        match capability.start(&context) {
            Ok(Started { handle, delivery }) => {
                normalizer.observe_delivery(delivery);
                self.normalizer = Some(normalizer);
                self.handle = Some(handle);
                self.phase = Phase::Driving;
                Step::Progress
            }
            Err(error) => {
                let delivery = DeliveryCertainty::DefinitelyUnsent.advance(error.delivery);
                self.phase = Phase::Ended;
                self.diagnostics.terminal = TerminalCause::StartFailed;
                self.diagnostics.delivery = delivery;
                Step::Done(Box::new(ExecutionReport {
                    outcome: capability_failure(error, delivery, None),
                    proposal_checks: Vec::new(),
                    disposition: None,
                    diagnostics: self.diagnostics.clone(),
                }))
            }
        }
    }

    /// One pass of the drive loop: `Ok` to continue, `Err` when the attempt ended.
    fn drive_once<H: Host>(&mut self, capability: &mut C, host: &mut H) -> Result<Step, Stop> {
        let limits = self.limits;
        let (Some(normalizer), Some(handle)) = (self.normalizer.as_mut(), self.handle.as_mut())
        else {
            // Unreachable: `Driving` is entered only after a successful start.
            return Err(fail(
                executor_failure(
                    ErrorKind::Internal,
                    DeliveryCertainty::DefinitelyUnsent,
                    None,
                ),
                TerminalCause::EndedWithoutFinish,
                false,
            ));
        };
        let diagnostics = &mut self.diagnostics;

        if host.cancel_requested() {
            return Err(Stop::Cancel(TerminalCause::Cancelled));
        }
        if diagnostics.polls >= limits.max_polls_per_attempt as u64 {
            return Err(fail(
                executor_failure(
                    ErrorKind::ResourceLimit,
                    normalizer.delivery(),
                    normalizer.partial(),
                ),
                TerminalCause::TooManyPolls,
                true,
            ));
        }
        diagnostics.polls += 1;
        let batch = match capability.poll(handle, limits.max_events_per_poll) {
            Ok(batch) => batch,
            Err(error) => {
                normalizer.observe_delivery(error.delivery);
                return Err(fail(
                    capability_failure(error, normalizer.delivery(), normalizer.partial()),
                    TerminalCause::PollFailed,
                    false,
                ));
            }
        };
        normalizer.observe_delivery(batch.delivery);
        if batch.events.len() > limits.max_events_per_poll {
            return Err(fail(
                executor_failure(
                    ErrorKind::ProtocolViolation,
                    normalizer.delivery(),
                    normalizer.partial(),
                ),
                TerminalCause::BatchTooLarge,
                true,
            ));
        }
        let had_events = !batch.events.is_empty();
        for event in batch.events {
            if normalizer.is_finished() {
                normalizer.note_late_event();
                continue;
            }
            match normalizer.push(event) {
                Ok(Pushed::Dropped) => {}
                Ok(Pushed::Forward(event)) => {
                    diagnostics.forwarded += 1;
                    if host.on_event(&event) == Flow::Stop {
                        return Err(Stop::Cancel(TerminalCause::Abandoned));
                    }
                }
                Ok(Pushed::Finished(event)) => {
                    diagnostics.forwarded += 1;
                    // The outcome is decided by the finish; an observer stopping now
                    // cannot change it.
                    let _ = host.on_event(&event);
                }
                Err(violation) => {
                    return Err(fail(
                        executor_failure(
                            violation.kind(),
                            normalizer.delivery(),
                            normalizer.partial(),
                        ),
                        TerminalCause::Violation(violation),
                        true,
                    ));
                }
            }
        }
        if normalizer.is_finished() {
            return Err(Stop::Finished);
        }
        if batch.state == StreamState::Ended {
            // Interrupted: the stream ended without a finish (oracle: eof/done without finish).
            return Err(fail(
                executor_failure(
                    ErrorKind::Transport,
                    normalizer.delivery(),
                    normalizer.partial(),
                ),
                TerminalCause::EndedWithoutFinish,
                false,
            ));
        }
        Ok(if had_events {
            Step::Progress
        } else {
            Step::Idle
        })
    }

    /// Exactly one terminal outcome; close exactly once.
    fn finish(&mut self, capability: &mut C, stop: Stop) -> Step {
        self.phase = Phase::Ended;
        let (Some(normalizer), Some(mut handle)) = (self.normalizer.take(), self.handle.take())
        else {
            return Step::Finished;
        };
        let diagnostics = &mut self.diagnostics;

        let (outcome, terminal) = match stop {
            Stop::Finished => match normalizer.completion() {
                Some(completion) => (Outcome::Completed(completion), TerminalCause::Finished),
                // Unreachable: `Finished` is produced only after a finish was accepted.
                // Fail closed rather than panic if that ever changes.
                None => (
                    executor_failure(
                        ErrorKind::Internal,
                        normalizer.delivery(),
                        normalizer.partial(),
                    ),
                    TerminalCause::EndedWithoutFinish,
                ),
            },
            Stop::Fail(outcome, cause, cancel_remote) => {
                if cancel_remote {
                    diagnostics.cancels = 1;
                    let _ = capability.cancel(&mut handle);
                }
                (*outcome, cause)
            }
            Stop::Cancel(cause) => {
                diagnostics.cancels = 1;
                let remote = match capability.cancel(&mut handle) {
                    CancelStatus::Requested => RemoteCancellation::Unconfirmed,
                    CancelStatus::Confirmed => RemoteCancellation::Confirmed,
                };
                (
                    Outcome::Cancelled(Cancellation {
                        delivery: normalizer.delivery(),
                        remote,
                        partial: normalizer.partial(),
                    }),
                    cause,
                )
            }
        };
        capability.close(handle);
        diagnostics.closes = 1;

        let (text, reasoning, tool_input) = normalizer.byte_counts();
        diagnostics.terminal = terminal;
        diagnostics.delivery = normalizer.delivery();
        diagnostics.events = normalizer.event_count() as u64;
        diagnostics.anomalies = normalizer.anomalies();
        diagnostics.text_bytes = text;
        diagnostics.reasoning_bytes = reasoning;
        diagnostics.tool_input_bytes = tool_input;

        let (proposal_checks, disposition) = match &outcome {
            Outcome::Completed(completion) => {
                (normalizer.checks().to_vec(), Some(classify(completion)))
            }
            _ => (Vec::new(), None),
        };
        Step::Done(Box::new(ExecutionReport {
            outcome,
            proposal_checks,
            disposition,
            diagnostics: diagnostics.clone(),
        }))
    }
}

fn cancelled_before_start(diagnostics: Diagnostics) -> ExecutionReport {
    ExecutionReport {
        outcome: Outcome::Cancelled(Cancellation {
            delivery: DeliveryCertainty::DefinitelyUnsent,
            remote: RemoteCancellation::NotStarted,
            partial: None,
        }),
        proposal_checks: Vec::new(),
        disposition: None,
        diagnostics,
    }
}
