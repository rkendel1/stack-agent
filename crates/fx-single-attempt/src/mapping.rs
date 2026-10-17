//! Pure translation rules from provider-layer observations to `fx-core` values.
//!
//! These are the only places that decide what an observation means in FX terms. They
//! never invent evidence: an unknown finish reason becomes `Other`, a failure without
//! a status becomes `Transport`, and delivery is never lowered.

use fx_core::{
    CapabilityError, DeliveryCertainty, Detail, ErrorKind, FinishReason, Limits, Request,
};

use crate::port::{Observed, PortFailure, PortFailureKind, PortSupport};
use fx_core::{Part, ReasoningEffort, Speed, ToolChoice, ToolExecution};

/// `Observed` to FX delivery certainty.
pub fn delivery_of(observed: Observed) -> DeliveryCertainty {
    match observed {
        Observed::NotInvoked => DeliveryCertainty::DefinitelyUnsent,
        Observed::Invoked => DeliveryCertainty::PossiblySent,
        Observed::OutputObserved => DeliveryCertainty::ConfirmedResponse,
    }
}

/// A provider's own finish reason to FX's. Values are matched exactly; anything not
/// listed is `Other`, never guessed.
pub fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        // OpenAI-style, Ollama
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "tool_calls" => FinishReason::ToolCalls,
        "content_filter" => FinishReason::ContentFilter,
        "error" => FinishReason::ProviderError,
        // Anthropic
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "max_tokens" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCalls,
        // Google
        "STOP" => FinishReason::Stop,
        "MAX_TOKENS" => FinishReason::Length,
        "SAFETY" => FinishReason::ContentFilter,
        _ => FinishReason::Other,
    }
}

/// A provider-layer failure to an FX capability error. The message is masked and
/// bounded by [`Detail::new`].
pub fn map_failure(failure: &PortFailure, limits: &Limits) -> CapabilityError {
    let kind = match failure.kind {
        PortFailureKind::ProviderNotFound | PortFailureKind::Unsupported => {
            ErrorKind::CapabilityUnavailable
        }
        PortFailureKind::InvalidRequest => ErrorKind::InvalidRequest,
        PortFailureKind::Aborted => ErrorKind::Cancelled,
        PortFailureKind::Timeout => ErrorKind::Timeout,
        PortFailureKind::MalformedOutput => ErrorKind::ProtocolViolation,
        PortFailureKind::IncompleteStream => ErrorKind::Transport,
        PortFailureKind::ProviderError => match failure.status {
            Some(401) => ErrorKind::Unauthenticated,
            Some(403) => ErrorKind::Forbidden,
            Some(404) => ErrorKind::ModelUnavailable,
            Some(429) => ErrorKind::RateLimited,
            Some(500..=599) => ErrorKind::ProviderUnavailable,
            // Without a status the provider layer cannot tell us more; do not guess
            // from message text.
            _ => ErrorKind::Transport,
        },
    };
    CapabilityError {
        kind,
        delivery: delivery_of(failure.observed),
        retry_after: failure.retry_after,
        status_hint: failure.status,
        detail: (!failure.message.is_empty()).then(|| Detail::new(&failure.message, limits)),
    }
}

/// The first request feature the port cannot carry, or `None`. Names are static
/// strings so the refusal never echoes request content.
pub fn first_unsupported(request: &Request, support: PortSupport) -> Option<&'static str> {
    if !support.images
        && request
            .messages
            .iter()
            .any(|m| m.parts.iter().any(|p| matches!(p, Part::Image(_))))
    {
        return Some("image parts");
    }
    if !support.tool_history
        && request.messages.iter().any(|m| {
            m.parts
                .iter()
                .any(|p| matches!(p, Part::ToolCall(_) | Part::ToolResult(_)))
        })
    {
        return Some("tool-call history");
    }
    if !support.tool_schemas
        && request
            .tools
            .iter()
            .any(|t| t.execution == ToolExecution::Client)
    {
        return Some("tool definitions with schemas");
    }
    if !support.provider_tools
        && request
            .tools
            .iter()
            .any(|t| t.execution == ToolExecution::Provider)
    {
        return Some("provider-executed tools");
    }
    if !support.tool_choice && request.tool_choice != ToolChoice::Auto {
        return Some("tool choice");
    }
    let options = &request.options;
    if !support.generation_options
        && (options.max_output_tokens.is_some()
            || options.temperature_milli.is_some()
            || options.reasoning != ReasoningEffort::Default
            || options.speed != Speed::Standard)
    {
        return Some("generation options");
    }
    if !support.response_format && options.response_format.is_some() {
        return Some("structured output");
    }
    None
}
