//! Deterministic fake capability for offline tests and adapter conformance.
//!
//! Test support, not a production capability. It replays a script, records
//! non-sensitive observations (call counts, the bounds and budget it was given,
//! the request's message count) and never claims more than the script says: in
//! particular, cancellation reports exactly the status the script declares, so a
//! test cannot accidentally "prove" that a dispatched request did not execute.

use crate::capability::{
    AttemptContext, CancelStatus, Capability, PollBatch, Started, StreamState,
};
use crate::contract::{
    AttemptBudget, CapabilityError, DeliveryCertainty, ErrorKind, Event, FinishReason, Usage,
};

/// One scripted poll result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Return these events.
    Batch {
        /// Events, in order.
        events: Vec<Event>,
        /// Delivery certainty reported with the batch.
        delivery: DeliveryCertainty,
        /// Stream state after the batch.
        state: StreamState,
    },
    /// Return an empty open batch (nothing available yet).
    Pending,
    /// Return empty open batches forever (a capability that never progresses).
    Stall,
    /// Fail the poll.
    Fail(CapabilityError),
}

/// What the fake does on `start`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StartBehavior {
    /// Start successfully, reporting this delivery certainty.
    Ok(DeliveryCertainty),
    /// Fail to start.
    Fail(CapabilityError),
}

/// A complete script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Script {
    /// Behavior of `start`.
    pub start: StartBehavior,
    /// Poll results, in order. After the last step the stream reports `Ended`.
    pub steps: Vec<Step>,
    /// What `cancel` truthfully reports.
    pub cancel: CancelStatus,
}

impl Script {
    /// A script that starts with `PossiblySent` and returns `events` in one batch,
    /// ending the stream.
    pub fn events(events: Vec<Event>) -> Self {
        Script {
            start: StartBehavior::Ok(DeliveryCertainty::PossiblySent),
            steps: vec![Step::Batch {
                events,
                delivery: DeliveryCertainty::ConfirmedResponse,
                state: StreamState::Ended,
            }],
            cancel: CancelStatus::Requested,
        }
    }

    /// A successful text answer split into the given deltas, with usage and `stop`.
    pub fn text(deltas: &[&str]) -> Self {
        let mut events: Vec<Event> = deltas
            .iter()
            .map(|d| Event::TextDelta((*d).to_owned()))
            .collect();
        events.push(Event::Usage(Usage {
            input_tokens: Some(12),
            output_tokens: Some(3),
            ..Usage::default()
        }));
        events.push(Event::Finish {
            reason: FinishReason::Stop,
        });
        Script::events(events)
    }

    /// A start failure before anything was sent.
    pub fn start_failure(kind: ErrorKind, delivery: DeliveryCertainty) -> Self {
        Script {
            start: StartBehavior::Fail(CapabilityError {
                kind,
                delivery,
                retry_after: None,
                status_hint: None,
                detail: None,
            }),
            steps: Vec::new(),
            cancel: CancelStatus::Requested,
        }
    }
}

/// Non-sensitive observations of how the executor used the fake.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observations {
    /// `start` calls.
    pub starts: u32,
    /// `poll` calls.
    pub polls: u64,
    /// `cancel` calls.
    pub cancels: u32,
    /// `close` calls.
    pub closes: u32,
    /// The largest `max_events` passed to `poll`.
    pub max_events_requested: usize,
    /// The attempt budget received at `start`.
    pub budget: Option<AttemptBudget>,
    /// The number of messages in the request received at `start`.
    pub messages: usize,
    /// Polls made after `close` or before `start` (must stay zero).
    pub misuse: u32,
}

/// The fake capability.
#[derive(Clone, Debug)]
pub struct FakeCapability {
    script: Script,
    cursor: usize,
    open: bool,
    /// What the executor did.
    pub observations: Observations,
}

/// Handle for one fake attempt.
#[derive(Debug)]
pub struct FakeHandle {
    _private: (),
}

impl FakeCapability {
    /// Creates a fake that replays `script`.
    pub fn new(script: Script) -> Self {
        FakeCapability {
            script,
            cursor: 0,
            open: false,
            observations: Observations::default(),
        }
    }
}

impl Capability for FakeCapability {
    type Handle = FakeHandle;

    fn start(
        &mut self,
        context: &AttemptContext<'_>,
    ) -> Result<Started<FakeHandle>, CapabilityError> {
        self.observations.starts += 1;
        self.observations.budget = context.budget;
        self.observations.messages = context.request.messages.len();
        match &self.script.start {
            StartBehavior::Ok(delivery) => {
                self.open = true;
                Ok(Started {
                    handle: FakeHandle { _private: () },
                    delivery: *delivery,
                })
            }
            StartBehavior::Fail(error) => Err(error.clone()),
        }
    }

    fn poll(
        &mut self,
        _handle: &mut FakeHandle,
        max_events: usize,
    ) -> Result<PollBatch, CapabilityError> {
        self.observations.polls += 1;
        self.observations.max_events_requested =
            self.observations.max_events_requested.max(max_events);
        if !self.open {
            self.observations.misuse += 1;
        }
        let step = self.script.steps.get(self.cursor).cloned();
        match step {
            None => Ok(PollBatch {
                events: Vec::new(),
                delivery: DeliveryCertainty::DefinitelyUnsent,
                state: StreamState::Ended,
            }),
            Some(Step::Stall) => Ok(PollBatch {
                events: Vec::new(),
                delivery: DeliveryCertainty::DefinitelyUnsent,
                state: StreamState::Open,
            }),
            Some(step) => {
                self.cursor += 1;
                match step {
                    Step::Batch {
                        events,
                        delivery,
                        state,
                    } => Ok(PollBatch {
                        events,
                        delivery,
                        state,
                    }),
                    Step::Pending | Step::Stall => Ok(PollBatch {
                        events: Vec::new(),
                        delivery: DeliveryCertainty::DefinitelyUnsent,
                        state: StreamState::Open,
                    }),
                    Step::Fail(error) => Err(error),
                }
            }
        }
    }

    fn cancel(&mut self, _handle: &mut FakeHandle) -> CancelStatus {
        self.observations.cancels += 1;
        self.script.cancel
    }

    fn close(&mut self, _handle: FakeHandle) {
        self.observations.closes += 1;
        if !self.open {
            self.observations.misuse += 1;
        }
        self.open = false;
    }
}
