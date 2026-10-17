//! The inference capability boundary: exactly one attempt, driven by the executor.
//!
//! A capability is whatever actually performs inference: the Easy-LLM adapter (R3),
//! a host bridge (R4), or the deterministic fake in [`crate::testing`]. FX defines
//! this interface; implementations live outside the core.
//!
//! The shape is non-blocking and host-drivable: the executor calls [`Capability::start`]
//! once, then [`Capability::poll`] for bounded batches until the stream ends or fails,
//! and finally [`Capability::close`] exactly once. No async runtime, thread or clock
//! is required by the interface. A capability may use any of those internally.
//!
//! What a capability must not do:
//! - retry, reconnect, fall back to another model or refresh credentials *across*
//!   this attempt in a way that changes what the attempt means (provider transport
//!   reattempts while the request is still definitely unsent are allowed and stay
//!   invisible to FX);
//! - execute application tools. Tool calls are proposals ([`crate::Event::ToolCall`]);
//!   only provider-side tools that the request declared with
//!   [`crate::ToolExecution::Provider`] may be reported as executed
//!   ([`crate::Event::ProviderToolRecord`]);
//! - report less delivery certainty than it has observed.

use crate::contract::{AttemptBudget, CapabilityError, DeliveryCertainty, Event, Limits, Request};

/// Everything a capability receives for one attempt.
///
/// The request has already passed [`crate::validate_request`]; the capability never
/// sees an invalid request.
#[derive(Clone, Copy, Debug)]
pub struct AttemptContext<'a> {
    /// The validated request.
    pub request: &'a Request,
    /// The bounds the executor enforces. Capabilities should respect them
    /// (in particular `max_events_per_poll`); the executor rejects output that does not.
    pub limits: &'a Limits,
    /// The attempt budget, if the caller set one.
    ///
    /// **Budget enforcement belongs to the capability**, which owns the clock and the
    /// transport: when the budget elapses it must stop and report
    /// [`crate::ErrorKind::Timeout`] with its true delivery certainty. The executor has
    /// no clock (the core forbids ambient time); the host can additionally cancel
    /// through [`crate::executor::Host`].
    pub budget: Option<AttemptBudget>,
}

/// State of the stream after a poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StreamState {
    /// The stream continues; poll again. A batch with no events and `Open` means
    /// "nothing available yet" (the synchronous driver then calls the host's idle hook).
    Open,
    /// The capability has no more events. A well-formed stream ends after a
    /// [`crate::Event::Finish`]; ending without one is an interrupted stream.
    Ended,
}

/// One bounded batch of events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PollBatch {
    /// Events in provider order; at most `limits.max_events_per_poll`.
    pub events: Vec<Event>,
    /// Delivery certainty as observed by the capability at the end of this batch.
    pub delivery: DeliveryCertainty,
    /// Whether the stream continues.
    pub state: StreamState,
}

/// What a capability can truthfully say about cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CancelStatus {
    /// Cancellation was requested locally; the remote side did not confirm it.
    Requested,
    /// The remote side confirmed it stopped. This still does not prove that the
    /// operation did not execute (see [`DeliveryCertainty::may_have_executed`]).
    Confirmed,
}

/// A started attempt.
#[derive(Debug)]
pub struct Started<H> {
    /// Per-attempt state.
    pub handle: H,
    /// Delivery certainty when `start` returned. A capability that dispatches the
    /// request inside `start` must report at least `PossiblySent` here, so that a
    /// cancellation right after `start` is not reported as definitely unsent.
    pub delivery: DeliveryCertainty,
}

/// An inference capability: performs exactly one attempt per `start`.
pub trait Capability {
    /// Per-attempt state owned by the capability.
    type Handle;

    /// Starts the attempt. On error, the capability reports how far delivery got;
    /// `close` is not called because no handle exists.
    fn start(
        &mut self,
        context: &AttemptContext<'_>,
    ) -> Result<Started<Self::Handle>, CapabilityError>;

    /// Returns the next batch of at most `max_events` events. Must not block
    /// indefinitely: return an empty `Open` batch when nothing is available.
    fn poll(
        &mut self,
        handle: &mut Self::Handle,
        max_events: usize,
    ) -> Result<PollBatch, CapabilityError>;

    /// Requests cancellation of the attempt. Must not block indefinitely.
    fn cancel(&mut self, handle: &mut Self::Handle) -> CancelStatus;

    /// Releases the attempt. Called exactly once for every successful `start`, on
    /// every terminal path.
    fn close(&mut self, handle: Self::Handle);
}
