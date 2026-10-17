//! The single-attempt provider port.
//!
//! A [`ProviderPort`] is what a host (the Node or WASM adapter in R4) implements over
//! Easy-LLM's `generateOnce` and `streamOnce`. It reports *observations* about one
//! provider call. It owns no state machine and defines no FX contract type: delivery
//! certainty, failure origin, events and outcomes remain `fx-core`'s, and
//! [`crate::SingleAttemptCapability`] translates between the two.
//!
//! The vocabulary mirrors the TypeScript API one to one (`docs/rebuild/SINGLE_ATTEMPT.md`).

use std::time::Duration;

use fx_core::{AttemptBudget, Request};

/// What the provider layer observed about whether the request left the process.
///
/// `NotInvoked` is the only state that proves nothing was sent: the provider's call
/// was never made. `Invoked` must be read as possibly sent. `OutputObserved` means a
/// response or stream chunk arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Observed {
    /// The provider was never called.
    NotInvoked,
    /// The provider was called and no output has arrived.
    Invoked,
    /// At least one response or stream chunk arrived.
    OutputObserved,
}

/// Why a provider call failed, as reported by the provider layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PortFailureKind {
    /// The named provider is not available. Nothing was invoked.
    ProviderNotFound,
    /// The provider layer rejected the request. Nothing was invoked.
    InvalidRequest,
    /// The provider cannot do what was asked. Nothing was invoked.
    Unsupported,
    /// The caller's abort reached the call.
    Aborted,
    /// The attempt budget elapsed.
    Timeout,
    /// The provider threw or rejected.
    ProviderError,
    /// The provider produced output that is not a valid response or chunk.
    MalformedOutput,
    /// The stream ended without its terminal `done` chunk.
    IncompleteStream,
}

/// A failed provider call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortFailure {
    /// Failure kind.
    pub kind: PortFailureKind,
    /// Provider text. It may contain sensitive content and is masked and bounded
    /// before it leaves this crate.
    pub message: String,
    /// What was observed about delivery.
    pub observed: Observed,
    /// HTTP status when the provider layer can supply one. Today's built-in
    /// adapters cannot (see the audit), so this is usually `None`.
    pub status: Option<u16>,
    /// Provider-requested wait, when known.
    pub retry_after: Option<Duration>,
}

/// Whether the provider's own terminal marker backed a streamed `done`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Terminal {
    /// The provider declared that `done` follows its own terminal marker. A complete
    /// non-streamed response is also reported this way.
    ProviderMarker,
    /// The provider may synthesize `done` when the transport simply ends.
    Unverified,
}

/// Usage as reported. Absent fields are unknown.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PortUsage {
    /// Input tokens.
    pub input_tokens: Option<u64>,
    /// Output tokens.
    pub output_tokens: Option<u64>,
    /// Cached input tokens.
    pub cache_read_tokens: Option<u64>,
    /// Reasoning tokens.
    pub reasoning_tokens: Option<u64>,
}

/// One item produced by the provider layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortItem {
    /// Output text.
    Text(String),
    /// A tool-call proposal. `arguments_json` is `None` when the arguments could not
    /// be written as JSON. Nothing was executed or authorized.
    ToolCall {
        /// Provider call id.
        id: String,
        /// Tool name.
        name: String,
        /// Arguments as JSON text, if they serialize.
        arguments_json: Option<String>,
    },
    /// The provider's terminal chunk (or the end of a complete response).
    Done {
        /// Final usage, if reported.
        usage: Option<PortUsage>,
        /// The provider's own finish reason, verbatim, if it reported one.
        finish_reason: Option<String>,
        /// The model the provider reported, if any.
        model: Option<String>,
        /// Whether the provider's own marker backed this `done`.
        terminal: Terminal,
    },
}

/// One bounded poll of the provider layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortPoll {
    /// Items, in provider order. At most the requested maximum.
    pub items: Vec<PortItem>,
    /// What has been observed about delivery by the end of this poll.
    pub observed: Observed,
    /// Whether the provider layer has nothing more to deliver.
    pub ended: bool,
}

/// What an abort achieved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbortReport {
    /// The transport was observed closing. Even so, the remote side may have finished
    /// the work; this never proves the operation did not run.
    pub transport_closed: bool,
}

/// Request features a port can carry. A request that needs a feature the port cannot
/// carry is refused before anything is sent, rather than silently weakened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PortSupport {
    /// Image parts.
    pub images: bool,
    /// Tool definitions with their input schemas.
    pub tool_schemas: bool,
    /// Assistant tool-call parts and tool-result messages in the history.
    pub tool_history: bool,
    /// Tool choice other than `Auto`.
    pub tool_choice: bool,
    /// Provider-executed tools.
    pub provider_tools: bool,
    /// Max output tokens, reasoning effort and speed.
    pub generation_options: bool,
    /// Structured output.
    pub response_format: bool,
}

/// One provider layer that makes at most one provider call per [`ProviderPort::invoke`].
///
/// An implementation must not retry, fall back to another provider, route, or execute
/// application tools, and must not report less delivery than it observed.
pub trait ProviderPort {
    /// Per-call state.
    type Call;

    /// The request features this port can carry.
    fn support(&self) -> PortSupport;

    /// Makes the one provider call. The host translates `request` into the provider
    /// layer's own shape.
    fn invoke(
        &mut self,
        request: &Request,
        budget: Option<AttemptBudget>,
    ) -> Result<Self::Call, PortFailure>;

    /// Returns up to `max_items` items. Must not block indefinitely.
    fn poll(&mut self, call: &mut Self::Call, max_items: usize) -> Result<PortPoll, PortFailure>;

    /// Aborts the call.
    fn abort(&mut self, call: &mut Self::Call) -> AbortReport;

    /// Releases the call. Called exactly once after a successful `invoke`.
    fn release(&mut self, call: Self::Call);
}
