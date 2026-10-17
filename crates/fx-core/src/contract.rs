//! The typed, provider-neutral FX inference contract.
//!
//! Everything here is plain owned data. Provider and model identifiers are opaque,
//! no type refers to a transport, credential, host runtime or clock, and every
//! collection and byte length is bounded by [`Limits`]. JSON encoding lives in
//! [`crate::wire`]; this module is the semantic model.

use std::time::Duration;

/// Version of the contract encoded in every wire document as `"v"`.
pub const CONTRACT_VERSION: u32 = 1;

/// Every numeric bound of the contract, defined in one place.
///
/// Defaults cite the legacy Zig implementation where it already had a bound
/// (the behavioral oracle) and are marked as new FX bounds otherwise. Validation,
/// decoding and future adapters all read these values; none hard-codes its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Maximum serialized size of one request document (new FX bound).
    pub max_request_bytes: usize,
    /// Maximum serialized size of one event, outcome or capability-error document.
    /// Oracle: `max_sse_event_line_bytes` (src/gateway/client.zig:188).
    pub max_event_bytes: usize,
    /// Maximum nesting depth of any JSON value carried as text (schemas, arguments).
    pub max_json_depth: usize,
    /// Maximum number of messages in a request (new FX bound).
    pub max_messages: usize,
    /// Maximum number of parts in one message (new FX bound).
    pub max_parts_per_message: usize,
    /// Maximum total bytes of text, tool arguments and tool results in a request (new FX bound).
    pub max_total_text_bytes: usize,
    /// Maximum bytes of one decoded image.
    /// Oracle: `max_image_bytes` (src/core/images/image_attachments.zig:11).
    pub max_image_bytes: usize,
    /// Maximum number of images in a request (new FX bound).
    pub max_images: usize,
    /// Maximum total decoded image bytes in a request (new FX bound).
    pub max_total_image_bytes: usize,
    /// Maximum number of tool definitions (new FX bound).
    pub max_tools: usize,
    /// Maximum bytes of a tool, proposal or response-format name (new FX bound).
    pub max_tool_name_bytes: usize,
    /// Maximum bytes of a tool-call identifier (new FX bound).
    pub max_tool_call_id_bytes: usize,
    /// Maximum bytes of a tool description. The oracle truncates to 1024 bytes when
    /// encoding for the Gateway (src/core/tooling/gateway_schema.zig:3); that is
    /// provider shaping owned by Easy-LLM, so the contract only rejects abuse.
    pub max_tool_description_bytes: usize,
    /// Maximum bytes of a JSON schema (tool input or response format) (new FX bound).
    pub max_schema_bytes: usize,
    /// Maximum bytes of one tool-call argument document (new FX bound).
    pub max_tool_arguments_bytes: usize,
    /// Maximum bytes of one tool result (new FX bound; Chip applies its own smaller
    /// model-facing cap, 64 KiB by default in src/core/tooling/tool_result_limits.zig:7).
    pub max_tool_result_bytes: usize,
    /// Maximum bytes of an opaque model identifier (new FX bound).
    pub max_model_id_bytes: usize,
    /// Maximum bytes of a reasoning-effort label.
    /// Oracle: `ReasoningEffort.max_name_bytes` (src/core/shared/types.zig:1565).
    pub max_reasoning_label_bytes: usize,
    /// Maximum bytes of a correlation affinity key.
    /// Oracle: session ids are at most 255 bytes (src/core/session/session_layout.zig).
    pub max_affinity_key_bytes: usize,
    /// Maximum bytes of a provider-reported model id in metadata.
    /// Oracle: `modelId` is capped at 128 bytes (src/gateway/client.zig, response-metadata).
    pub max_reported_model_bytes: usize,
    /// Maximum bytes of an opaque generation id or cost string (new FX bound).
    pub max_opaque_token_bytes: usize,
    /// Maximum bytes of a failure detail.
    /// Oracle: provider failure detail is clipped to 600 bytes (src/gateway/client.zig:184).
    pub max_detail_bytes: usize,
    /// Maximum bytes of accumulated completion or partial content (new FX bound).
    pub max_completion_bytes: usize,
    /// Largest accepted attempt budget (new FX bound).
    pub max_attempt_budget: Duration,
    /// Maximum number of proposals or provider records in one outcome (new FX bound).
    pub max_proposals: usize,
    /// Maximum events a capability may return from one `poll` (new FX bound, R2).
    /// The executor passes it to the capability and rejects larger batches.
    pub max_events_per_poll: usize,
    /// Maximum events in one attempt (new FX bound, R2). Bounds executor work for a
    /// capability that streams forever.
    pub max_events_per_attempt: usize,
    /// Maximum `poll` calls in one attempt, including empty polls (new FX bound, R2).
    /// Bounds the synchronous driver when a capability never makes progress.
    pub max_polls_per_attempt: usize,
}

impl Limits {
    /// The contract defaults. Tests pin every value.
    pub const DEFAULT: Limits = Limits {
        max_request_bytes: 64 * 1024 * 1024,
        max_event_bytes: 32 * 1024 * 1024,
        max_json_depth: 128,
        max_messages: 4096,
        max_parts_per_message: 256,
        max_total_text_bytes: 16 * 1024 * 1024,
        max_image_bytes: 20 * 1024 * 1024,
        max_images: 16,
        max_total_image_bytes: 40 * 1024 * 1024,
        max_tools: 256,
        max_tool_name_bytes: 128,
        max_tool_call_id_bytes: 256,
        max_tool_description_bytes: 64 * 1024,
        max_schema_bytes: 256 * 1024,
        max_tool_arguments_bytes: 1024 * 1024,
        max_tool_result_bytes: 8 * 1024 * 1024,
        max_model_id_bytes: 256,
        max_reasoning_label_bytes: 64,
        max_affinity_key_bytes: 255,
        max_reported_model_bytes: 128,
        max_opaque_token_bytes: 256,
        max_detail_bytes: 600,
        max_completion_bytes: 16 * 1024 * 1024,
        max_attempt_budget: Duration::from_secs(24 * 60 * 60),
        max_proposals: 256,
        max_events_per_poll: 256,
        max_events_per_attempt: 1 << 20,
        max_polls_per_attempt: 1 << 20,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Opaque model reference. Only the capability (Easy-LLM) interprets it; FX checks
/// syntax and length only and never parses provider slugs.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModelRef(pub String);

impl ModelRef {
    /// Borrows the opaque identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Message author role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// Instructions from the caller.
    System,
    /// End-user content.
    User,
    /// Model output carried back as history.
    Assistant,
    /// Results of client-executed tool calls.
    Tool,
}

/// Provider-neutral prompt-cache hint. Easy-LLM maps it to provider options.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum CacheHint {
    /// Let the capability decide.
    #[default]
    Default,
    /// Never cache this message or anything after it.
    NoCache,
    /// Place a cache breakpoint after this message.
    Breakpoint,
}

/// Supported image media types (oracle: `detectImageMediaType`,
/// src/core/images/image_attachments.zig:1549-1552).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageMediaType {
    /// `image/png`
    Png,
    /// `image/jpeg`
    Jpeg,
    /// `image/gif`
    Gif,
    /// `image/webp`
    Webp,
}

impl ImageMediaType {
    /// The IANA media type string.
    pub fn as_str(self) -> &'static str {
        match self {
            ImageMediaType::Png => "image/png",
            ImageMediaType::Jpeg => "image/jpeg",
            ImageMediaType::Gif => "image/gif",
            ImageMediaType::Webp => "image/webp",
        }
    }

    /// Parses an IANA media type string; anything else is unsupported.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "image/png" => Some(ImageMediaType::Png),
            "image/jpeg" => Some(ImageMediaType::Jpeg),
            "image/gif" => Some(ImageMediaType::Gif),
            "image/webp" => Some(ImageMediaType::Webp),
            _ => None,
        }
    }

    /// Detects the media type from leading bytes, exactly as the oracle does.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.len() >= 8 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" {
            return Some(ImageMediaType::Png);
        }
        if bytes.len() >= 3 && bytes[..3] == [0xff, 0xd8, 0xff] {
            return Some(ImageMediaType::Jpeg);
        }
        if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
            return Some(ImageMediaType::Gif);
        }
        if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            return Some(ImageMediaType::Webp);
        }
        None
    }
}

/// Image content: verified bytes plus their declared media type. Never a path or URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageData {
    /// Declared media type; must match the bytes.
    pub media_type: ImageMediaType,
    /// Raw image bytes.
    pub bytes: Vec<u8>,
}

/// One content part of a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// UTF-8 text.
    Text(String),
    /// Image data (user messages only).
    Image(ImageData),
    /// A tool-call proposal made earlier by the model (assistant messages only).
    ToolCall(ToolCallProposal),
    /// The result of executing a client tool (tool messages only).
    ToolResult(ToolResult),
}

/// A message in the request history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Author role.
    pub role: Role,
    /// Content parts; at least one.
    pub parts: Vec<Part>,
    /// Cache hint.
    pub cache: CacheHint,
}

/// Where a tool runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum ToolExecution {
    /// The caller (Chip) executes proposals after its own admission.
    #[default]
    Client,
    /// The provider executes the tool and reports a [`ProviderToolRecord`].
    Provider,
}

/// A tool the model may propose to call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolDefinition {
    /// Tool name; unique within a request.
    pub name: String,
    /// Model-facing description.
    pub description: String,
    /// JSON Schema (an object) for the tool input, as JSON text.
    pub input_schema_json: String,
    /// Client or provider execution.
    pub execution: ToolExecution,
    /// Provider-tool arguments as a JSON object; only for provider tools; opaque to FX.
    pub provider_args_json: Option<String>,
}

/// Tool selection policy.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum ToolChoice {
    /// The model decides.
    #[default]
    Auto,
    /// No tool calls.
    None,
    /// At least one tool call.
    Required,
    /// A specific tool, by name.
    Named(String),
}

/// A model's request to call a tool. Data only: it carries no execution authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCallProposal {
    /// Provider-assigned call identifier.
    pub id: String,
    /// Proposed tool name.
    pub name: String,
    /// Proposed arguments as JSON text. In request history this must be valid JSON;
    /// in capability events it is normalized by the executor.
    pub arguments_json: String,
}

/// Output of a client-executed tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolOutput {
    /// Plain text output.
    Text(String),
    /// JSON output as text; must be valid JSON.
    Json(String),
    /// The tool failed; the text explains why.
    Failure(String),
}

/// The result of executing a client tool proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolResult {
    /// Id of the proposal this answers.
    pub call_id: String,
    /// Name of the proposal this answers.
    pub name: String,
    /// Output.
    pub output: ToolOutput,
}

/// A provider-reported execution of a provider-side tool. Distinct from proposals:
/// this is the only case in which the provider, not the model, reports execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderToolRecord {
    /// Id of the provider-executed call.
    pub call_id: String,
    /// Provider tool name.
    pub name: String,
    /// Input as JSON text, as reported.
    pub input_json: String,
    /// Result as JSON text, if reported.
    pub result_json: Option<String>,
    /// Whether this is the final result (the oracle distinguishes preliminary results).
    pub is_final: bool,
}

/// Opaque reasoning-effort label. The label set comes from the model catalog, so
/// FX checks only syntax and Easy-LLM validates support.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum ReasoningEffort {
    /// The capability's default.
    #[default]
    Default,
    /// A catalog-declared label such as `high`.
    Named(String),
}

/// Requested generation speed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Speed {
    /// Normal routing.
    #[default]
    Standard,
    /// A faster route when the model offers one.
    Fast,
}

/// Structured-output request. FX validates schema syntax only; Easy-LLM owns the
/// mapping to provider-specific structured-output features.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseFormat {
    /// Schema name.
    pub name: String,
    /// Schema description.
    pub description: String,
    /// JSON Schema (an object) as JSON text.
    pub schema_json: String,
}

/// Generation options.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct GenerationOptions {
    /// Output token cap; must be positive when present.
    pub max_output_tokens: Option<u32>,
    /// Sampling temperature in thousandths (`0` is 0.0, `700` is 0.7), at most
    /// [`GenerationOptions::MAX_TEMPERATURE_MILLI`]. An integer keeps the request `Eq` and
    /// its encoding exact. A provider that cannot honor it refuses the request before dispatch.
    pub temperature_milli: Option<u16>,
    /// Reasoning effort.
    pub reasoning: ReasoningEffort,
    /// Speed.
    pub speed: Speed,
    /// Structured output.
    pub response_format: Option<ResponseFormat>,
}

impl GenerationOptions {
    /// The largest accepted temperature, in thousandths (2.0).
    pub const MAX_TEMPERATURE_MILLI: u16 = 2000;
}

/// Caller-chosen 128-bit request identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RequestId(pub [u8; 16]);

/// Correlation metadata. Forwarded, never interpreted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Correlation {
    /// Request identifier.
    pub request_id: RequestId,
    /// Optional affinity key (for example a session id).
    pub affinity_key: Option<String>,
}

/// Time budget for one attempt, relative to when the executor starts it.
///
/// Portable by construction: a duration, never a process-specific monotonic instant.
/// The runtime adapter converts it to a host-local deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AttemptBudget(pub Duration);

/// One inference request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Opaque model reference.
    pub model: ModelRef,
    /// Ordered history; at least one message.
    pub messages: Vec<Message>,
    /// Tool definitions.
    pub tools: Vec<ToolDefinition>,
    /// Tool choice.
    pub tool_choice: ToolChoice,
    /// Generation options.
    pub options: GenerationOptions,
    /// Correlation metadata.
    pub correlation: Correlation,
    /// Optional time budget.
    pub budget: Option<AttemptBudget>,
}

/// Why generation stopped (oracle: `ProviderFinishReason`, src/core/shared/types.zig:915).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FinishReason {
    /// Natural stop.
    Stop,
    /// Output token limit.
    Length,
    /// Provider content filter.
    ContentFilter,
    /// The model proposed tool calls.
    ToolCalls,
    /// The provider reported an error in-band.
    ProviderError,
    /// Anything else the provider reports.
    Other,
}

/// Token usage as reported. Absent fields are unknown, never guessed.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Usage {
    /// Input tokens.
    pub input_tokens: Option<u64>,
    /// Output tokens.
    pub output_tokens: Option<u64>,
    /// Input tokens read from cache.
    pub cache_read_tokens: Option<u64>,
    /// Input tokens written to cache.
    pub cache_write_tokens: Option<u64>,
    /// Reasoning tokens.
    pub reasoning_tokens: Option<u64>,
    /// Opaque cost string as reported by the capability; FX assumes no currency.
    pub cost: Option<String>,
}

/// Metadata reported by the capability. All values are opaque.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Metadata {
    /// The model the provider says it used.
    pub model_reported: Option<String>,
    /// Opaque generation identifier.
    pub generation_id: Option<String>,
    /// Provider wall-clock timestamp in Unix milliseconds; diagnostics only.
    pub provider_timestamp_ms: Option<i64>,
}

/// A normalized stream event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Response metadata.
    Metadata(Metadata),
    /// Incremental output text.
    TextDelta(String),
    /// Incremental reasoning text (forwarded, not retained).
    ReasoningDelta(String),
    /// A streamed tool input has started (display only).
    ToolInputStart {
        /// Call id.
        id: String,
        /// Tool name.
        name: String,
    },
    /// A fragment of a streamed tool input (display only, never authoritative).
    ToolInputDelta {
        /// Call id.
        id: String,
        /// Fragment.
        delta: String,
    },
    /// A streamed tool input has ended.
    ToolInputEnd {
        /// Call id.
        id: String,
    },
    /// An authoritative tool-call proposal.
    ToolCall(ToolCallProposal),
    /// A provider-executed tool record.
    ProviderToolRecord(ProviderToolRecord),
    /// Usage report.
    Usage(Usage),
    /// Terminal finish of a successful stream.
    Finish {
        /// Finish reason.
        reason: FinishReason,
    },
}

/// Evidence about whether the request could have reached the provider.
///
/// Ordered and monotonic: `DefinitelyUnsent < PossiblySent < ConfirmedResponse`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum DeliveryCertainty {
    /// No byte of the request can have left the process.
    #[default]
    DefinitelyUnsent,
    /// The request may have been delivered (and billed).
    PossiblySent,
    /// A response from the provider was observed.
    ConfirmedResponse,
}

impl DeliveryCertainty {
    /// Returns the later of the two states. Delivery never moves backwards.
    #[must_use]
    pub fn advance(self, observed: DeliveryCertainty) -> DeliveryCertainty {
        self.max(observed)
    }

    /// Whether the remote operation may have executed. Only `DefinitelyUnsent`
    /// proves it did not; cancellation or timeout alone never does.
    pub fn may_have_executed(self) -> bool {
        self != DeliveryCertainty::DefinitelyUnsent
    }
}

/// Closed set of failure kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// The request failed validation or the capability rejected its shape.
    InvalidRequest,
    /// No capability is available or it cannot start.
    CapabilityUnavailable,
    /// The capability could not authenticate.
    Unauthenticated,
    /// The capability is not permitted to perform the request.
    Forbidden,
    /// Unknown model or no route.
    ModelUnavailable,
    /// Rate limited.
    RateLimited,
    /// Provider overloaded or failing (5xx class). Kept distinct from `Transport`
    /// because turn-level recovery treats them differently (oracle:
    /// `FailureCause.provider_unavailable`, model_response_recovery.zig:6-15).
    ProviderUnavailable,
    /// Network or stream interruption.
    Transport,
    /// The host system suspended and resumed during the attempt (oracle:
    /// `NetworkFailureCause.system_resumed`).
    SystemResumed,
    /// The attempt budget elapsed.
    Timeout,
    /// The caller cancelled.
    Cancelled,
    /// The capability violated the event protocol.
    ProtocolViolation,
    /// A resource limit was exceeded.
    ResourceLimit,
    /// An internal error.
    Internal,
}

/// Which side produced a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FailureOrigin {
    /// FX itself (validation, lifecycle, limits).
    Executor,
    /// The capability reported it.
    Capability,
}

/// Bounded, secret-masked diagnostic text. Never a full provider body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail(String);

impl Detail {
    /// Masks secret-shaped tokens, then truncates to `limits.max_detail_bytes` at a
    /// character boundary.
    pub fn new(raw: &str, limits: &Limits) -> Detail {
        let masked = crate::redact::mask_secrets(raw);
        let mut end = masked.len().min(limits.max_detail_bytes);
        while !masked.is_char_boundary(end) {
            end -= 1;
        }
        Detail(masked[..end].to_owned())
    }

    /// Borrows the text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An error reported by a capability for one attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityError {
    /// Kind.
    pub kind: ErrorKind,
    /// Delivery certainty at the time of the error.
    pub delivery: DeliveryCertainty,
    /// Provider-requested wait before another attempt, if any.
    pub retry_after: Option<Duration>,
    /// Diagnostic status (for example an HTTP status); never a control input.
    pub status_hint: Option<u16>,
    /// Bounded detail.
    pub detail: Option<Detail>,
}

/// Output accumulated before a non-success terminal outcome.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PartialOutput {
    /// Accumulated text.
    pub content: String,
    /// Whether accumulation stopped at the content limit.
    pub content_truncated: bool,
}

/// A successful terminal outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    /// Accumulated text (bounded).
    pub content: String,
    /// Whether accumulation stopped at the content limit.
    pub content_truncated: bool,
    /// Bytes of reasoning forwarded (reasoning text is not retained).
    pub reasoning_bytes: u64,
    /// Tool-call proposals, in order.
    pub proposals: Vec<ToolCallProposal>,
    /// Provider-executed tool records, in order.
    pub provider_records: Vec<ProviderToolRecord>,
    /// Finish reason.
    pub finish_reason: FinishReason,
    /// Usage, if reported.
    pub usage: Option<Usage>,
    /// Metadata, if reported.
    pub metadata: Metadata,
}

/// A failed terminal outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// Which side failed.
    pub origin: FailureOrigin,
    /// Kind.
    pub kind: ErrorKind,
    /// Delivery certainty when the attempt ended.
    pub delivery: DeliveryCertainty,
    /// Provider-requested wait, if any.
    pub retry_after: Option<Duration>,
    /// Diagnostic status, if any.
    pub status_hint: Option<u16>,
    /// Bounded detail.
    pub detail: Option<Detail>,
    /// Partial output, if any was accumulated (an interrupted stream).
    pub partial: Option<PartialOutput>,
}

/// What is known about remote cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RemoteCancellation {
    /// The capability was never started.
    NotStarted,
    /// Cancellation was requested; the remote side did not confirm it.
    Unconfirmed,
    /// The capability confirmed that the remote side stopped. This still does not
    /// prove the operation did not execute.
    Confirmed,
}

/// A cancelled terminal outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cancellation {
    /// Delivery certainty when cancellation took effect. Use
    /// [`DeliveryCertainty::may_have_executed`]; cancellation alone proves nothing.
    pub delivery: DeliveryCertainty,
    /// Remote cancellation state.
    pub remote: RemoteCancellation,
    /// Partial output, if any.
    pub partial: Option<PartialOutput>,
}

/// Exactly one terminal outcome per attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Finished with a valid finish event.
    Completed(Completion),
    /// Failed.
    Failed(Failure),
    /// Cancelled by the caller.
    Cancelled(Cancellation),
}
