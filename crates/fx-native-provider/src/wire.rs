//! What differs between providers: the request, the headers and the stream's wire format.
//! Everything else (the worker, polling, deadlines, cancellation, delivery) is shared in
//! `provider.rs` and is the same code for every provider.

use fx_core::Request;
use fx_single_attempt::{PortItem, PortSupport};

/// What decoding one stream event produced.
pub enum Step {
    /// Output items (possibly none). An event that decoded counts as a response arriving.
    Items(Vec<PortItem>),
    /// The provider's terminal marker, with the final `done`.
    Done(PortItem),
    /// The provider reported an error inside the stream.
    ProviderError(String),
}

/// Decodes one provider's stream, event by event. Strict: anything unexpected is an `Err`.
pub trait Decoder: Send {
    /// Decodes the data of one event (`event` is the `event:` name when given).
    fn feed(&mut self, event: Option<&str>, data: &str) -> Result<Step, String>;
}

/// One provider's wire format and connection details.
pub trait Wire: Send + Sync + 'static {
    /// The request features this wire format can carry.
    fn support(&self) -> PortSupport;
    /// The full request URL.
    fn url(&self) -> String;
    /// Every header to send, credentials included.
    fn headers(&self) -> Vec<(String, String)>;
    /// The request body, or the first thing that cannot be carried.
    fn body(&self, request: &Request) -> Result<Vec<u8>, String>;
    /// A fresh stream decoder for one call.
    fn decoder(&self) -> Box<dyn Decoder>;
    /// The credential, so it can be removed from provider-derived text.
    fn secret(&self) -> Option<&crate::secret::Secret>;
    /// Connection settings shared by every provider.
    fn settings(&self) -> &crate::config::Common;
}
