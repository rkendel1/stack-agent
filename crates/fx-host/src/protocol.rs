//! The host protocol: JSON command documents the host sends and the conversions into
//! the `fx-single-attempt` port vocabulary.
//!
//! These are boundary types. They carry provider observations and nothing else: the
//! FX request, events, outcomes and reports use `fx-core`'s own wire encoding.

use std::time::Duration;

use fx_single_attempt::{
    Observed, PortFailure, PortFailureKind, PortItem, PortSupport, PortUsage, Terminal,
};
use serde::Deserialize;

/// Protocol version carried in every command and response as `"v"`.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObservedDoc {
    NotInvoked,
    Invoked,
    OutputObserved,
}

impl From<ObservedDoc> for Observed {
    fn from(doc: ObservedDoc) -> Self {
        match doc {
            ObservedDoc::NotInvoked => Observed::NotInvoked,
            ObservedDoc::Invoked => Observed::Invoked,
            ObservedDoc::OutputObserved => Observed::OutputObserved,
        }
    }
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub(crate) enum KindDoc {
    ProviderNotFound,
    InvalidRequest,
    Unsupported,
    Aborted,
    Timeout,
    ProviderError,
    MalformedOutput,
    IncompleteStream,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TerminalDoc {
    ProviderMarker,
    Unverified,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FailureDoc {
    kind: KindDoc,
    #[serde(default)]
    message: String,
    observed: ObservedDoc,
    #[serde(default)]
    status: Option<u16>,
    #[serde(default)]
    retry_after_ms: Option<u64>,
}

impl From<FailureDoc> for PortFailure {
    fn from(doc: FailureDoc) -> Self {
        PortFailure {
            kind: match doc.kind {
                KindDoc::ProviderNotFound => PortFailureKind::ProviderNotFound,
                KindDoc::InvalidRequest => PortFailureKind::InvalidRequest,
                KindDoc::Unsupported => PortFailureKind::Unsupported,
                KindDoc::Aborted => PortFailureKind::Aborted,
                KindDoc::Timeout => PortFailureKind::Timeout,
                KindDoc::ProviderError => PortFailureKind::ProviderError,
                KindDoc::MalformedOutput => PortFailureKind::MalformedOutput,
                KindDoc::IncompleteStream => PortFailureKind::IncompleteStream,
            },
            message: doc.message,
            observed: doc.observed.into(),
            status: doc.status,
            retry_after: doc.retry_after_ms.map(Duration::from_millis),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsageDoc {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_read_tokens: Option<u64>,
    #[serde(default)]
    reasoning_tokens: Option<u64>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ItemDoc {
    Text {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        #[serde(default)]
        arguments_json: Option<String>,
    },
    Done {
        #[serde(default)]
        usage: Option<UsageDoc>,
        #[serde(default)]
        finish_reason: Option<String>,
        #[serde(default)]
        model: Option<String>,
        terminal: TerminalDoc,
    },
}

impl From<ItemDoc> for PortItem {
    fn from(doc: ItemDoc) -> Self {
        match doc {
            ItemDoc::Text { text } => PortItem::Text(text),
            ItemDoc::ToolCall {
                id,
                name,
                arguments_json,
            } => PortItem::ToolCall {
                id,
                name,
                arguments_json,
            },
            ItemDoc::Done {
                usage,
                finish_reason,
                model,
                terminal,
            } => PortItem::Done {
                usage: usage.map(|u| PortUsage {
                    input_tokens: u.input_tokens,
                    output_tokens: u.output_tokens,
                    cache_read_tokens: u.cache_read_tokens,
                    reasoning_tokens: u.reasoning_tokens,
                }),
                finish_reason,
                model,
                terminal: match terminal {
                    TerminalDoc::ProviderMarker => Terminal::ProviderMarker,
                    TerminalDoc::Unverified => Terminal::Unverified,
                },
            },
        }
    }
}

/// A host command. Empty struct variants, not unit variants, so unknown fields are
/// rejected (serde ignores them on internally tagged unit variants).
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CommandDoc {
    InvokeResult {
        ok: bool,
        #[serde(default)]
        failure: Option<FailureDoc>,
    },
    Feed {
        items: Vec<ItemDoc>,
        observed: ObservedDoc,
        #[serde(default)]
        ended: bool,
    },
    FeedFailure {
        failure: FailureDoc,
    },
    Cancel {
        #[serde(default)]
        transport_closed: bool,
    },
    Step {},
}

/// Request features the host's provider path can carry, as a bit set passed when a
/// session is created. A request that needs a feature whose bit is clear is refused
/// before the provider is invoked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SupportFlags(pub u32);

impl SupportFlags {
    /// Image parts.
    pub const IMAGES: u32 = 1;
    /// Tool definitions with schemas.
    pub const TOOL_SCHEMAS: u32 = 2;
    /// Tool-call history.
    pub const TOOL_HISTORY: u32 = 4;
    /// Tool choice other than `auto`.
    pub const TOOL_CHOICE: u32 = 8;
    /// Provider-executed tools.
    pub const PROVIDER_TOOLS: u32 = 16;
    /// Max output tokens, reasoning effort and speed.
    pub const GENERATION_OPTIONS: u32 = 32;
    /// Structured output.
    pub const RESPONSE_FORMAT: u32 = 64;

    /// The port support these flags describe.
    pub fn support(self) -> PortSupport {
        let has = |bit: u32| self.0 & bit != 0;
        PortSupport {
            images: has(Self::IMAGES),
            tool_schemas: has(Self::TOOL_SCHEMAS),
            tool_history: has(Self::TOOL_HISTORY),
            tool_choice: has(Self::TOOL_CHOICE),
            provider_tools: has(Self::PROVIDER_TOOLS),
            generation_options: has(Self::GENERATION_OPTIONS),
            response_format: has(Self::RESPONSE_FORMAT),
        }
    }
}
