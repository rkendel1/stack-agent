//! Bounded, content-free diagnostics for one attempt (invariant 8).
//!
//! Diagnostics hold identifiers, counts, enums and the request *shape* only. They
//! never hold prompts, image bytes, tool arguments, tool results, credentials,
//! model output or provider error text. Everything here is fixed-size or counted.

use crate::contract::{DeliveryCertainty, Part, Request, RequestId, Role, ToolChoice};
use crate::normalize::Violation;
use crate::validate::ValidationCode;

/// Content-free summary of a request (the oracle's `formatGatewayRequestShapeSummary`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestShape {
    /// Messages per role: system, user, assistant, tool.
    pub messages_by_role: [u32; 4],
    /// Text parts.
    pub text_parts: u32,
    /// Image parts.
    pub image_parts: u32,
    /// Tool-call parts in history.
    pub tool_call_parts: u32,
    /// Tool-result parts in history.
    pub tool_result_parts: u32,
    /// Tool definitions.
    pub tools: u32,
    /// Tool choice kind: 0 auto, 1 none, 2 required, 3 named.
    pub tool_choice: u8,
    /// Whether structured output was requested.
    pub response_format: bool,
    /// Whether an attempt budget was set.
    pub budget: bool,
}

impl RequestShape {
    /// Summarizes a request without copying any content.
    pub fn of(request: &Request) -> Self {
        let mut shape = RequestShape::default();
        for message in &request.messages {
            let role = match message.role {
                Role::System => 0,
                Role::User => 1,
                Role::Assistant => 2,
                Role::Tool => 3,
            };
            shape.messages_by_role[role] = shape.messages_by_role[role].saturating_add(1);
            for part in &message.parts {
                let counter = match part {
                    Part::Text(_) => &mut shape.text_parts,
                    Part::Image(_) => &mut shape.image_parts,
                    Part::ToolCall(_) => &mut shape.tool_call_parts,
                    Part::ToolResult(_) => &mut shape.tool_result_parts,
                };
                *counter = counter.saturating_add(1);
            }
        }
        shape.tools = u32::try_from(request.tools.len()).unwrap_or(u32::MAX);
        shape.tool_choice = match request.tool_choice {
            ToolChoice::Auto => 0,
            ToolChoice::None => 1,
            ToolChoice::Required => 2,
            ToolChoice::Named(_) => 3,
        };
        shape.response_format = request.options.response_format.is_some();
        shape.budget = request.budget.is_some();
        shape
    }
}

/// Why the attempt ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TerminalCause {
    /// A valid `finish` was accepted.
    Finished,
    /// The request failed validation; the capability was not invoked.
    InvalidRequest(ValidationCode),
    /// The host cancelled before the capability was started.
    CancelledBeforeStart,
    /// The capability failed to start.
    StartFailed,
    /// The capability failed while streaming.
    PollFailed,
    /// The stream ended without a `finish` (interrupted).
    EndedWithoutFinish,
    /// The capability returned more events in one poll than allowed.
    BatchTooLarge,
    /// The capability exceeded `max_polls_per_attempt`.
    TooManyPolls,
    /// The normalizer rejected an event.
    Violation(Violation),
    /// The host requested cancellation.
    Cancelled,
    /// The host's observer stopped consuming events.
    Abandoned,
}

/// Diagnostics for one attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostics {
    /// The request id from the correlation, when the request decoded.
    pub request_id: Option<RequestId>,
    /// Request shape, when the request decoded.
    pub request_shape: Option<RequestShape>,
    /// How the attempt ended.
    pub terminal: TerminalCause,
    /// Final delivery certainty.
    pub delivery: DeliveryCertainty,
    /// Capability `start` calls (0 or 1).
    pub starts: u32,
    /// Capability `poll` calls.
    pub polls: u64,
    /// Capability `cancel` calls (0 or 1).
    pub cancels: u32,
    /// Capability `close` calls (0 or 1; equals `starts`).
    pub closes: u32,
    /// Events accepted by the normalizer.
    pub events: u64,
    /// Events forwarded to the host.
    pub forwarded: u64,
    /// Anomalous events dropped.
    pub anomalies: u32,
    /// Text bytes received.
    pub text_bytes: u64,
    /// Reasoning bytes received.
    pub reasoning_bytes: u64,
    /// Streamed tool-input bytes received.
    pub tool_input_bytes: u64,
}

impl Diagnostics {
    pub(crate) fn new(request: Option<&Request>) -> Self {
        Diagnostics {
            request_id: request.map(|r| r.correlation.request_id),
            request_shape: request.map(RequestShape::of),
            terminal: TerminalCause::Finished,
            delivery: DeliveryCertainty::DefinitelyUnsent,
            starts: 0,
            polls: 0,
            cancels: 0,
            closes: 0,
            events: 0,
            forwarded: 0,
            anomalies: 0,
            text_bytes: 0,
            reasoning_bytes: 0,
            tool_input_bytes: 0,
        }
    }
}
