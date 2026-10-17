//! Native Rust providers behind the FX single-attempt [`ProviderPort`].
//!
//! This crate is a *host* adapter, like the Node bridge: it performs the one provider call
//! and reports observations. Validation, normalization, delivery certainty, cancellation and
//! terminal outcomes stay in `fx-core` and `fx-single-attempt`. It does not retry, fall back,
//! route between models, run tools, or loop. There is no provider selection here either: the
//! host chooses a provider and a model explicitly. See `docs/rebuild/NATIVE_PROVIDER.md`.
//!
//! Providers: [`OpenAiCompatible`] (OpenAI, OpenRouter, LM Studio and other servers that speak
//! the chat-completions stream) and [`Anthropic`] (the Messages API). Both share one worker,
//! one polling model and one set of delivery rules; only the wire format differs.
//!
//! [`ProviderPort`]: fx_single_attempt::ProviderPort

mod anthropic;
mod config;
mod openai;
mod provider;
mod secret;
mod sse;
mod wire;

pub use config::{AnthropicConfig, ConfigError, OpenAiConfig};
pub use provider::{Anthropic, Native, NativeCall, OpenAiCall, OpenAiCompatible};
pub use secret::Secret;
