//! [`SingleAttemptCapability`]: a [`ProviderPort`] seen as an `fx-core` [`Capability`].
//!
//! The adapter is a translation layer. It owns no attempt state machine: validation,
//! ordering, bounds, terminal outcomes and cancellation semantics stay in the FX
//! executor. It performs no retry, fallback or tool execution, and it never lowers
//! delivery certainty.

use std::collections::VecDeque;

use fx_core::{
    AttemptContext, CancelStatus, Capability, CapabilityError, DeliveryCertainty, Detail,
    ErrorKind, Event, Limits, Metadata, PollBatch, Started, StreamState, ToolCallProposal, Usage,
};

use crate::mapping::{delivery_of, first_unsupported, map_failure, map_finish_reason};
use crate::port::{PortFailureKind, PortItem, PortUsage, ProviderPort, Terminal};
use fx_core::FinishReason;

/// Adapts a [`ProviderPort`] to the FX [`Capability`] trait.
#[derive(Debug)]
pub struct SingleAttemptCapability<P: ProviderPort> {
    port: P,
}

impl<P: ProviderPort> SingleAttemptCapability<P> {
    /// Wraps a port.
    pub fn new(port: P) -> Self {
        SingleAttemptCapability { port }
    }

    /// Borrows the port (for example to read a test double's observations).
    pub fn port(&self) -> &P {
        &self.port
    }

    /// Mutably borrows the port. A host that feeds the port between executor steps (the
    /// WebAssembly bridge) uses this; the executor itself never needs it.
    pub fn port_mut(&mut self) -> &mut P {
        &mut self.port
    }

    /// Returns the port.
    pub fn into_port(self) -> P {
        self.port
    }
}

/// Per-attempt state.
#[derive(Debug)]
pub struct Attempt<C> {
    call: C,
    limits: Limits,
    queue: VecDeque<Event>,
    delivery: DeliveryCertainty,
    ended: bool,
    pending_error: Option<CapabilityError>,
}

fn usage_event(usage: PortUsage) -> Option<Event> {
    let usage = Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        ..Usage::default()
    };
    (usage != Usage::default()).then_some(Event::Usage(usage))
}

/// Turns one port item into FX events. `Done` can produce several.
fn events_of(item: PortItem) -> Vec<Event> {
    match item {
        PortItem::Text(text) => vec![Event::TextDelta(text)],
        PortItem::ToolCall {
            id,
            name,
            arguments_json,
        } => vec![Event::ToolCall(ToolCallProposal {
            id,
            name,
            // Absent arguments become the empty string. The FX normalizer records them
            // as missing and admission refuses them; this adapter never repairs.
            arguments_json: arguments_json.unwrap_or_default(),
        })],
        PortItem::Done {
            usage,
            finish_reason,
            model,
            terminal,
        } => {
            let mut events = Vec::new();
            if let Some(model) = model {
                events.push(Event::Metadata(Metadata {
                    model_reported: Some(model),
                    ..Metadata::default()
                }));
            }
            if let Some(event) = usage.and_then(usage_event) {
                events.push(event);
            }
            let reason = match (finish_reason, terminal) {
                (Some(reason), _) => Some(map_finish_reason(&reason)),
                // The provider's own marker proved completion without naming a reason.
                (None, Terminal::ProviderMarker) => Some(FinishReason::Other),
                // A synthesized `done` with no reason is not proof of completion. Emit
                // no `Finish`, so FX reports an interrupted attempt with the partial output.
                (None, Terminal::Unverified) => None,
            };
            events.extend(reason.map(|reason| Event::Finish { reason }));
            events
        }
    }
}

impl<P: ProviderPort> Capability for SingleAttemptCapability<P> {
    type Handle = Attempt<P::Call>;

    fn start(
        &mut self,
        context: &AttemptContext<'_>,
    ) -> Result<Started<Self::Handle>, CapabilityError> {
        if let Some(feature) = first_unsupported(context.request, self.port.support()) {
            return Err(CapabilityError {
                kind: ErrorKind::CapabilityUnavailable,
                // Refused before the port was called, so nothing was sent.
                delivery: DeliveryCertainty::DefinitelyUnsent,
                retry_after: None,
                status_hint: None,
                detail: Some(Detail::new(
                    &format!("The provider port cannot carry this request: {feature}."),
                    context.limits,
                )),
            });
        }
        match self.port.invoke(context.request, context.budget) {
            Ok(call) => {
                // The provider call is made inside `invoke`, so a cancel right after
                // `start` must not be reported as definitely unsent.
                let delivery = DeliveryCertainty::PossiblySent;
                Ok(Started {
                    handle: Attempt {
                        call,
                        limits: *context.limits,
                        queue: VecDeque::new(),
                        delivery,
                        ended: false,
                        pending_error: None,
                    },
                    delivery,
                })
            }
            Err(failure) => Err(map_failure(&failure, context.limits)),
        }
    }

    fn poll(
        &mut self,
        handle: &mut Self::Handle,
        max_events: usize,
    ) -> Result<PollBatch, CapabilityError> {
        if handle.queue.len() < max_events && !handle.ended && handle.pending_error.is_none() {
            match self.port.poll(&mut handle.call, max_events) {
                Ok(poll) => {
                    handle.delivery = handle.delivery.advance(delivery_of(poll.observed));
                    for item in poll.items {
                        let done = matches!(item, PortItem::Done { .. });
                        handle.queue.extend(events_of(item));
                        if done {
                            // Nothing after the provider's terminal chunk is read.
                            handle.ended = true;
                            break;
                        }
                    }
                    handle.ended |= poll.ended;
                }
                Err(failure) => {
                    handle.delivery = handle.delivery.advance(delivery_of(failure.observed));
                    if failure.kind == PortFailureKind::IncompleteStream {
                        // FX turns an ended stream without `finish` into an interrupted
                        // attempt that keeps the partial output.
                        handle.ended = true;
                    } else {
                        let mut error = map_failure(&failure, &handle.limits);
                        error.delivery = error.delivery.max(handle.delivery);
                        handle.pending_error = Some(error);
                    }
                }
            }
        }

        let count = max_events.min(handle.queue.len());
        let events: Vec<Event> = handle.queue.drain(..count).collect();
        if events.is_empty()
            && let Some(error) = handle.pending_error.take()
        {
            return Err(error);
        }
        let finished = handle.ended && handle.queue.is_empty() && handle.pending_error.is_none();
        Ok(PollBatch {
            events,
            delivery: handle.delivery,
            state: if finished {
                StreamState::Ended
            } else {
                StreamState::Open
            },
        })
    }

    fn cancel(&mut self, handle: &mut Self::Handle) -> CancelStatus {
        if self.port.abort(&mut handle.call).transport_closed {
            CancelStatus::Confirmed
        } else {
            CancelStatus::Requested
        }
    }

    fn close(&mut self, handle: Self::Handle) {
        self.port.release(handle.call);
    }
}
