//! The configuration that selects one model provider. It describes a selection and its settings; it
//! performs no I/O, and it is not an executor: the model is reached through FX (`chip-fx-native`), the only executor.

use std::fmt;
use std::time::Duration;

use crate::{ModelId, Secret};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// A server speaking the OpenAI chat-completions wire format (OpenAI, OpenRouter, LM Studio, Ollama's
/// `/v1` endpoint, vLLM, and others). The endpoint is always the caller's choice.
pub const PROVIDER_OPENAI_COMPATIBLE: &str = "openai-compatible";

/// The Anthropic Messages API.
pub const PROVIDER_ANTHROPIC: &str = "anthropic";

/// The endpoint a provider talks to when none is configured. Only a provider with a single well-known
/// endpoint has one.
pub fn default_endpoint(provider: &str) -> Option<&'static str> {
    match provider {
        PROVIDER_ANTHROPIC => Some("https://api.anthropic.com/v1/messages"),
        _ => None,
    }
}

#[derive(Clone)]
pub struct ProviderConfig {
    pub provider: String,
    pub model: ModelId,
    pub endpoint: String,
    pub api_key: Option<Secret>,
    /// Sent as `anthropic-workspace-id` for Anthropic keys that are not scoped to a workspace.
    /// Ignored by other providers.
    pub workspace_id: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub timeout: Duration,
    /// Ask the endpoint for a JSON object. A request field, never prompt text, and a request only:
    /// the reply is still untrusted output that the caller must parse strictly. OpenAI-compatible
    /// endpoints only; ignored for Anthropic.
    pub json_object_output: bool,
    /// With `json_object_output`: the JSON schema text to request in place of `{"type":"object"}`.
    pub response_schema_json: Option<String>,
    /// Sent as `"chat_template_kwargs":{"enable_thinking":<value>}` when set. Opt-in because some
    /// OpenAI-compatible servers reject fields they do not know. Refused for Anthropic by the fx
    /// executor.
    pub enable_thinking: Option<bool>,
    /// Send the output-token cap as `max_completion_tokens` instead of `max_tokens`. OpenAI-compatible
    /// endpoints only (OpenAI's current models require it; others do not know it), so it is off
    /// unless set, and refused for Anthropic.
    pub max_completion_tokens: bool,
    /// May the provider's own default key variable (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`) be
    /// consulted when no key was given explicitly? `true` by default; a host sets it `false` so a
    /// key that was not chosen for this endpoint is never sent to it.
    pub ambient_keys: bool,
    /// Ask for one complete JSON response (`stream: false`) rather than an event stream.
    /// OpenAI-compatible endpoints only.
    pub non_streaming: bool,
    /// Send a structured-output request as `{"type":"json_object"}` rather than `json_schema`.
    /// OpenAI-compatible endpoints only.
    pub plain_json_object: bool,
    /// `seed` on every request, for reproducible sampling where the server honours it.
    pub seed: Option<u64>,
    /// Ask the provider to cache the request's stable prefix where its wire has a request field
    /// for it (Anthropic: a `cache_control` breakpoint on the last system block). Off unless set;
    /// OpenAI-compatible endpoints have no such field and ignore it.
    pub prompt_cache: bool,
    /// The name (never the value) of where the key came from, for reports: `CHIP_API_KEY`, a variable
    /// named with `--api-key-env`, or the provider's default variable. `None`: no key.
    pub key_source: Option<String>,
}

impl ProviderConfig {
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: ModelId::new(model),
            endpoint: endpoint.into(),
            api_key: None,
            workspace_id: None,
            temperature: None,
            max_tokens: None,
            timeout: DEFAULT_TIMEOUT,
            json_object_output: false,
            response_schema_json: None,
            enable_thinking: None,
            max_completion_tokens: false,
            ambient_keys: true,
            non_streaming: false,
            plain_json_object: false,
            seed: None,
            prompt_cache: false,
            key_source: None,
        }
    }

    pub fn without_streaming(mut self) -> Self {
        self.non_streaming = true;
        self
    }

    pub fn with_plain_json_object(mut self) -> Self {
        self.json_object_output = true;
        self.plain_json_object = true;
        self
    }

    pub fn with_prompt_cache(mut self, on: bool) -> Self {
        self.prompt_cache = on;
        self
    }

    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    pub fn without_ambient_keys(mut self) -> Self {
        self.ambient_keys = false;
        self
    }

    pub fn with_key_source(mut self, name: impl Into<String>) -> Self {
        self.key_source = Some(name.into());
        self
    }

    pub fn with_json_object_output(mut self) -> Self {
        self.json_object_output = true;
        self
    }

    pub fn with_max_completion_tokens(mut self) -> Self {
        self.max_completion_tokens = true;
        self
    }

    /// Ask for one JSON object that conforms to `schema_json` (the text of a JSON schema).
    pub fn with_response_schema_json(mut self, schema_json: String) -> Self {
        self.json_object_output = true;
        self.response_schema_json = Some(schema_json);
        self
    }

    pub fn with_enable_thinking(mut self, enable: bool) -> Self {
        self.enable_thinking = Some(enable);
        self
    }

    pub fn with_api_key(mut self, api_key: Secret) -> Self {
        self.api_key = Some(api_key);
        self
    }

    pub fn with_workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("endpoint", &self.endpoint)
            .field("api_key", &self.api_key)
            .field("workspace_id", &self.workspace_id)
            .field("temperature", &self.temperature)
            .field("max_tokens", &self.max_tokens)
            .field("timeout", &self.timeout)
            .field("json_object_output", &self.json_object_output)
            .field(
                "response_schema_json",
                &self.response_schema_json.as_ref().map(|s| s.len()),
            )
            .field("enable_thinking", &self.enable_thinking)
            .field("max_completion_tokens", &self.max_completion_tokens)
            .field("ambient_keys", &self.ambient_keys)
            .field("non_streaming", &self.non_streaming)
            .field("plain_json_object", &self.plain_json_object)
            .field("seed", &self.seed)
            .field("prompt_cache", &self.prompt_cache)
            .field("key_source", &self.key_source)
            .finish()
    }
}
