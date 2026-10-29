//! The generation policy of `chip work`: how many tokens a reply may spend, how random it may be,
//! whether the endpoint is asked to constrain the reply to a JSON object, and how long one model
//! call may take.
//!
//! Each setting has one effective value and one source, in this order:
//!
//! 1. the command-line flag (`--max-output-tokens`, `--temperature`, `--structured-output`),
//! 2. the environment variable of the same setting (`CHIP_MAX_OUTPUT_TOKENS`, `CHIP_TEMPERATURE`,
//!    `CHIP_STRUCTURED_OUTPUT`), which is the work-specific configuration,
//! 3. the application default: 2048 tokens, temperature 0.0, structured output `object`, 30 s.
//!
//! The call timeout is one wall-clock deadline for a whole provider attempt: connecting, sending,
//! the server's own queueing and prompt processing, and reading the entire streamed reply. It is
//! not a generation-only deadline. The native provider enforces it (`timeout_global`) and a call
//! that outlasts it is the typed failure `timeout`, never a partial reply.
//!
//! `ProviderConfig::max_tokens` and `temperature` are deliberately not on this list: Chip's request
//! always carries both, so they never took effect for `work`, and this module does not pretend they
//! do. Experiments that pin their own settings (the micro model, the benchmarks) build their own
//! providers and never pass through this module.
//!
//! The policy changes what is asked of the model, never what Chip accepts. Whatever the structured
//! output mode, a reply is parsed strictly, validated as a decision, authorized and executed (or
//! refused) by Chip exactly as before, and a reply cut off at the token cap is a typed failure.

use std::time::Duration;

use async_trait::async_trait;
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse};

use crate::software_work::WORK_MAX_OUTPUT_TOKENS;

pub const MAX_OUTPUT_TOKENS_FLAG: &str = "--max-output-tokens";
pub const TEMPERATURE_FLAG: &str = "--temperature";
pub const PROVIDER_PROFILE_FLAG: &str = "--provider-profile";
pub const PROVIDER_PROFILE_VARIABLE: &str = "CHIP_PROVIDER_PROFILE";
pub const TOKEN_LIMIT_FIELD_FLAG: &str = "--token-limit-field";
pub const TOKEN_LIMIT_FIELD_VARIABLE: &str = "CHIP_TOKEN_LIMIT_FIELD";
pub const STRUCTURED_OUTPUT_FLAG: &str = "--structured-output";
pub const CALL_TIMEOUT_FLAG: &str = "--call-timeout-secs";
pub const CALL_TIMEOUT_VARIABLE: &str = "CHIP_CALL_TIMEOUT_SECS";
pub const MAX_OUTPUT_TOKENS_VARIABLE: &str = "CHIP_MAX_OUTPUT_TOKENS";
pub const TEMPERATURE_VARIABLE: &str = "CHIP_TEMPERATURE";
pub const STRUCTURED_OUTPUT_VARIABLE: &str = "CHIP_STRUCTURED_OUTPUT";
pub const SEED_FLAG: &str = "--seed";
pub const SEED_VARIABLE: &str = "CHIP_SEED";
pub const STREAMING_FLAG: &str = "--streaming";
pub const STREAMING_VARIABLE: &str = "CHIP_STREAMING";
pub const STRUCTURED_OUTPUT_FALLBACK_FLAG: &str = "--structured-output-fallback";
pub const STRUCTURED_OUTPUT_FALLBACK_VARIABLE: &str = "CHIP_STRUCTURED_OUTPUT_FALLBACK";
pub const PROMPT_CACHE_FLAG: &str = "--prompt-cache";
pub const PROMPT_CACHE_VARIABLE: &str = "CHIP_PROMPT_CACHE";
pub const DECISION_PROMPT_FLAG: &str = "--decision-prompt";
pub const DECISION_PROMPT_VARIABLE: &str = "CHIP_DECISION_PROMPT";

/// The largest output budget accepted. A bound on typos, not a claim about any provider: a
/// provider or server that accepts less refuses the request, and that refusal is reported as is.
pub const MAX_OUTPUT_TOKENS_LIMIT: u32 = 131_072;
/// The longest call timeout accepted, in seconds: the 24 hours FX itself allows an attempt. FX
/// refuses a zero or longer budget, so a value outside 1 to this is refused here, by name, before
/// anything is sent.
pub const MAX_CALL_TIMEOUT_SECS: u64 = 86_400;
/// The highest temperature accepted. A provider that accepts a narrower range refuses the request.
pub const MAX_TEMPERATURE: f32 = 2.0;

/// The input names the product's capabilities declare (project.*, project.git.*, project.observe),
/// for the closed schema. They are the declarations' names, not the model's; a test pins this list
/// to the real descriptors so it cannot drift from them.
pub const CLOSED_INPUT_NAMES: &[&str] = &[
    "content",
    "count",
    "expect_absent",
    "expected_sha256",
    "length",
    "offset",
    "path",
    "query",
    "replace",
    "scope",
    "with",
];

/// Whether, and how, the endpoint is asked to constrain the reply. This is a request to the
/// server, not a check by Chip: the reply is validated by Chip either way, and nothing is ever
/// repaired. The schemas are projections of the one canonical decision contract
/// ([`chip_core::ModelDecisionBoundary::response_schema`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredOutput {
    /// A `json_schema` response format carrying the full canonical decision schema (`anyOf`, one
    /// alternative per decision). The default, for servers that enforce it (a local grammar does).
    Schema,
    /// The flat projection of the same schema, for providers that reject `anyOf` at the root
    /// (OpenAI). It cannot say that `complete` needs `summary`; the runtime still does.
    SchemaFlat,
    /// The closed projection of the same schema, for a provider whose structured output enforces
    /// only a subset of JSON Schema (Anthropic): every object closed, no length or count bounds,
    /// `inputs` limited to the input names the capabilities declare ([`CLOSED_INPUT_NAMES`]).
    /// What it cannot say (the text-length bound, which capability an input belongs to) stays with
    /// the runtime, which validates every reply strictly whatever the provider enforced.
    SchemaClosed,
    /// A `json_schema` response format whose schema is only `{"type":"object"}`: any object. Kept
    /// to compare against, and weaker than the two above.
    Object,
    /// A real `response_format: {"type":"json_object"}`: any JSON object, no schema, for servers
    /// that reject `json_schema`. OpenAI-compatible endpoints only.
    JsonObject,
    /// No `response_format` field is sent.
    None,
}

/// **Experimental.** How the decision contract is worded to the model. `full` (the default) is the
/// wording every release has used. `compact` words the same contract more briefly, from the same
/// data. It changes what the model is *told*, never what Chip accepts: a reply is parsed strictly,
/// validated, authorized and executed (or refused) identically in both. See
/// `audit/compact-prompt/` for the evidence, which does not yet support making it the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecisionPrompt {
    #[default]
    Full,
    Compact,
}

impl DecisionPrompt {
    pub fn name(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Compact => "compact",
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        match text.trim() {
            "full" => Ok(Self::Full),
            "compact" => Ok(Self::Compact),
            other => Err(format!(
                "`{other}` is not a decision-prompt mode; use `full` (the default) or `compact`"
            )),
        }
    }
}

impl StructuredOutput {
    pub fn name(self) -> &'static str {
        match self {
            Self::Schema => "schema",
            Self::SchemaFlat => "schema-flat",
            Self::SchemaClosed => "schema-closed",
            Self::Object => "object",
            Self::JsonObject => "json-object",
            Self::None => "none",
        }
    }

    fn parse(text: &str) -> Result<Self, String> {
        match text.trim() {
            "schema" => Ok(Self::Schema),
            "schema-flat" => Ok(Self::SchemaFlat),
            "schema-closed" => Ok(Self::SchemaClosed),
            "object" => Ok(Self::Object),
            "json-object" => Ok(Self::JsonObject),
            "none" => Ok(Self::None),
            other => Err(format!(
                "`{other}` is not a structured-output mode; use `schema`, `schema-flat`, `schema-closed`, `object`, `json-object` or `none`"
            )),
        }
    }

    /// The schema text this mode asks the provider to enforce; `None` when there is no schema text
    /// (`object` is FX's permissive default object, and `none` sends nothing).
    pub fn schema_text(self) -> Option<String> {
        match self {
            Self::Schema => Some(chip_core::ModelDecisionBoundary::response_schema(
                chip_core::DecisionSchemaForm::Full,
            )),
            Self::SchemaFlat => Some(chip_core::ModelDecisionBoundary::response_schema(
                chip_core::DecisionSchemaForm::Flat,
            )),
            Self::SchemaClosed => Some(chip_core::ModelDecisionBoundary::response_schema(
                chip_core::DecisionSchemaForm::Closed(CLOSED_INPUT_NAMES),
            )),
            Self::Object | Self::JsonObject | Self::None => None,
        }
    }

    pub fn asks_for_json(self) -> bool {
        self != Self::None
    }
}

// ---- provider profiles ------------------------------------------------------------------------------------

/// A named set of request-compatibility choices for one kind of endpoint. A profile describes only
/// how the request is written (which field names the token cap, whether a temperature is sent, how
/// structured output is requested); it changes no decision schema, no authorization, no goal, no
/// coverage rule and no way success is judged. Choosing one is the operator's explicit act: nothing
/// is inferred from a model name, an endpoint or an API error, and a failed call is never retried
/// under another profile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProviderProfile {
    pub name: &'static str,
    /// The provider wire (`CHIP_PROVIDER`) this profile is written for.
    pub wire: &'static str,
    pub token_limit_field: TokenLimitField,
    /// `None` sends no temperature.
    pub temperature: Option<f32>,
    pub structured_output: StructuredOutput,
    /// The model-call timeout this kind of endpoint needs, when it differs from the default.
    pub call_timeout_secs: Option<u64>,
    /// What was measured (or only documented), in one line.
    pub note: &'static str,
}

pub const PROFILES: [ProviderProfile; 8] = [
    ProviderProfile {
        name: "openai",
        wire: chip_inference::PROVIDER_OPENAI_COMPATIBLE,
        token_limit_field: TokenLimitField::MaxCompletionTokens,
        temperature: None,
        structured_output: StructuredOutput::SchemaFlat,
        call_timeout_secs: None,
        note: "OpenAI's current models reject max_tokens and any temperature but their default, and reject anyOf at the schema root",
    },
    ProviderProfile {
        name: "anthropic",
        wire: chip_inference::PROVIDER_ANTHROPIC,
        token_limit_field: TokenLimitField::MaxTokens,
        temperature: None,
        structured_output: StructuredOutput::None,
        call_timeout_secs: None,
        note: "the Messages API rejects temperature on current models, and its structured outputs require additionalProperties:false on every object, so only `schema-closed` (inputs closed over the declared names) can be sent; the profile default stays `none` until that is measured",
    },
    ProviderProfile {
        name: "local",
        wire: chip_inference::PROVIDER_OPENAI_COMPATIBLE,
        token_limit_field: TokenLimitField::MaxTokens,
        temperature: Some(0.0),
        structured_output: StructuredOutput::Schema,
        call_timeout_secs: None,
        note: "an OpenAI-compatible server that knows max_tokens and enforces the full schema as a grammar (measured with LM Studio)",
    },
    // The profiles below are written from each server's documentation. Only `local` (LM Studio) was
    // measured in this repository; the others have not been run against a live server here, and each
    // note says so. A profile never picks the endpoint: that stays `--endpoint`.
    ProviderProfile {
        name: "lm-studio",
        wire: chip_inference::PROVIDER_OPENAI_COMPATIBLE,
        token_limit_field: TokenLimitField::MaxTokens,
        temperature: Some(0.0),
        structured_output: StructuredOutput::Schema,
        call_timeout_secs: Some(300),
        note: "LM Studio (default http://localhost:1234/v1/chat/completions); the same request as `local`, with a 300 s call timeout for a cold model load. The schema is enforced as a grammar (measured)",
    },
    ProviderProfile {
        name: "ollama",
        wire: chip_inference::PROVIDER_OPENAI_COMPATIBLE,
        token_limit_field: TokenLimitField::MaxTokens,
        temperature: Some(0.0),
        structured_output: StructuredOutput::Schema,
        call_timeout_secs: Some(300),
        note: "Ollama's OpenAI-compatible API (default http://localhost:11434/v1/chat/completions); documented, not measured here. A model that cannot take `json_schema` needs --structured-output json-object or none; the fallback ladder (--structured-output-fallback on) covers a refusal",
    },
    ProviderProfile {
        name: "llama-cpp",
        wire: chip_inference::PROVIDER_OPENAI_COMPATIBLE,
        token_limit_field: TokenLimitField::MaxTokens,
        temperature: Some(0.0),
        structured_output: StructuredOutput::JsonObject,
        call_timeout_secs: Some(300),
        note: "llama.cpp servers; documented, not measured here. Defaults to `json-object` because llama-cpp-python answers `json_schema` with HTTP 500; llama-server enforces a schema, so --structured-output schema is available there",
    },
    ProviderProfile {
        name: "vllm",
        wire: chip_inference::PROVIDER_OPENAI_COMPATIBLE,
        token_limit_field: TokenLimitField::MaxTokens,
        temperature: Some(0.0),
        structured_output: StructuredOutput::Schema,
        call_timeout_secs: Some(120),
        note: "vLLM's OpenAI-compatible server (default http://localhost:8000/v1/chat/completions); documented, not measured here. Guided decoding enforces the schema",
    },
    ProviderProfile {
        name: "openrouter",
        wire: chip_inference::PROVIDER_OPENAI_COMPATIBLE,
        token_limit_field: TokenLimitField::MaxTokens,
        temperature: Some(0.0),
        structured_output: StructuredOutput::None,
        call_timeout_secs: Some(120),
        note: "OpenRouter (https://openrouter.ai/api/v1/chat/completions); documented, not measured here. Whether `response_format` is honoured depends on the upstream model, so nothing is sent by default; choose --structured-output for a model known to take it",
    },
];

impl ProviderProfile {
    pub fn named(name: &str) -> Result<&'static ProviderProfile, String> {
        PROFILES
            .iter()
            .find(|p| p.name == name.trim())
            .ok_or_else(|| {
                format!(
                    "`{}` is not a provider profile; use one of {}",
                    name.trim(),
                    PROFILES
                        .iter()
                        .map(|p| p.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }
}

/// Where an effective value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Default,
    /// The environment variable that set it.
    Environment(&'static str),
    /// The command-line flag that set it.
    Flag(&'static str),
    /// The provider cannot carry the setting, so it has no effect there.
    ProviderLimit,
    /// The named provider profile.
    Profile(&'static str),
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Self::Default => "default".into(),
            Self::Environment(name) => format!("environment:{name}"),
            Self::Flag(name) => format!("flag:{name}"),
            Self::ProviderLimit => "provider".into(),
            Self::Profile(name) => format!("profile:{name}"),
        }
    }
}

/// One setting's effective value and its source.
#[derive(Debug, Clone, PartialEq)]
pub struct Setting<T> {
    pub value: T,
    pub source: Source,
}

/// Raw, unvalidated settings as given on the command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flags {
    pub max_output_tokens: Option<String>,
    pub temperature: Option<String>,
    pub token_limit_field: Option<String>,
    pub structured_output: Option<String>,
    pub decision_prompt: Option<String>,
    pub streaming: Option<String>,
    pub seed: Option<String>,
    pub structured_output_fallback: Option<String>,
    pub prompt_cache: Option<String>,
    pub call_timeout_secs: Option<String>,
    pub provider_profile: Option<String>,
}

/// The effective generation policy of one runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkGeneration {
    pub max_output_tokens: Setting<u32>,
    /// `None` means the request carries no temperature at all (`omit`): some models reject any
    /// explicit value, and the report says `omitted`, never 0.
    pub temperature: Setting<Option<f32>>,
    /// The name of the output-token cap on the wire. `max_tokens` is what most OpenAI-compatible
    /// servers know; OpenAI's current models require `max_completion_tokens`. Chosen by the
    /// operator, never guessed from a model name or an endpoint.
    pub token_limit_field: Setting<TokenLimitField>,
    pub structured_output: Setting<StructuredOutput>,
    /// Experimental: how the decision contract is worded to the model (see [`DecisionPrompt`]).
    pub decision_prompt: Setting<DecisionPrompt>,
    /// `false`: ask for one complete response instead of an event stream (OpenAI-compatible
    /// endpoints that do not stream).
    pub streaming: Setting<bool>,
    /// `seed` sent with every request, when set. A request for reproducible sampling, not a guarantee.
    pub seed: Setting<Option<u64>>,
    /// On: if the endpoint refuses the structured-output request, step down one rung
    /// (`json-object`, then `none`) for the rest of the work, reported with its reason. Off by
    /// default: nothing is substituted unless the operator asked for the ladder.
    pub structured_output_fallback: Setting<bool>,
    /// On (the default for the Anthropic provider): ask the provider to cache the request's
    /// stable prefix, the instructions and the observations every request of a work resends in
    /// the same order (a `cache_control` breakpoint on the last system block). The text the model
    /// reads is unchanged; only the provider's own usage report says whether anything was read
    /// from its cache. `--prompt-cache off` (or `CHIP_PROMPT_CACHE=off`) sends the request exactly
    /// as before, for comparison. The OpenAI-compatible wire has no request field for it, so there
    /// it is `false`, source `provider`, and asking for `on` is refused.
    pub prompt_cache: Setting<bool>,
    /// How long one model call may take, end to end.
    pub call_timeout: Setting<Duration>,
    /// The provider profile selected, if any, and where the selection came from.
    pub profile: Option<Setting<&'static ProviderProfile>>,
}

impl Default for WorkGeneration {
    fn default() -> Self {
        Self {
            max_output_tokens: Setting {
                value: WORK_MAX_OUTPUT_TOKENS,
                source: Source::Default,
            },
            token_limit_field: Setting {
                value: TokenLimitField::MaxTokens,
                source: Source::Default,
            },
            temperature: Setting {
                value: Some(0.0),
                source: Source::Default,
            },
            structured_output: Setting {
                value: StructuredOutput::Schema,
                source: Source::Default,
            },
            decision_prompt: Setting {
                value: DecisionPrompt::Full,
                source: Source::Default,
            },
            streaming: Setting {
                value: true,
                source: Source::Default,
            },
            seed: Setting {
                value: None,
                source: Source::Default,
            },
            structured_output_fallback: Setting {
                value: false,
                source: Source::Default,
            },
            prompt_cache: Setting {
                value: true,
                source: Source::Default,
            },
            call_timeout: Setting {
                value: chip_inference::DEFAULT_TIMEOUT,
                source: Source::Default,
            },
            profile: None,
        }
    }
}

fn seed(text: &str, name: &str) -> Result<u64, String> {
    text.trim().parse::<u64>().map_err(|_| {
        format!(
            "{name} needs a whole number from 0 to 18446744073709551615 (got `{}`)",
            text.trim()
        )
    })
}

fn on_off(text: &str, name: &str) -> Result<bool, String> {
    match text.trim() {
        "on" => Ok(true),
        "off" => Ok(false),
        other => Err(format!("{name} takes `on` or `off` (got `{other}`)")),
    }
}

fn call_timeout(text: &str, name: &str) -> Result<Duration, String> {
    match text.trim().parse::<u64>() {
        Ok(n) if (1..=MAX_CALL_TIMEOUT_SECS).contains(&n) => Ok(Duration::from_secs(n)),
        _ => Err(format!(
            "{name} needs a whole number of seconds from 1 to {MAX_CALL_TIMEOUT_SECS} (got `{}`)",
            text.trim()
        )),
    }
}

fn max_output_tokens(text: &str, name: &str) -> Result<u32, String> {
    match text.trim().parse::<u32>() {
        Ok(n) if (1..=MAX_OUTPUT_TOKENS_LIMIT).contains(&n) => Ok(n),
        _ => Err(format!(
            "{name} needs a whole number of tokens from 1 to {MAX_OUTPUT_TOKENS_LIMIT} (got `{}`)",
            text.trim()
        )),
    }
}

/// The output-token cap's name in an OpenAI-compatible request body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenLimitField {
    MaxTokens,
    MaxCompletionTokens,
}

impl TokenLimitField {
    pub fn name(self) -> &'static str {
        match self {
            Self::MaxTokens => "max_tokens",
            Self::MaxCompletionTokens => "max_completion_tokens",
        }
    }

    fn parse(text: &str) -> Result<Self, String> {
        match text.trim() {
            "max_tokens" => Ok(Self::MaxTokens),
            "max_completion_tokens" => Ok(Self::MaxCompletionTokens),
            other => Err(format!(
                "`{other}` is not a token-limit field; use `max_tokens` or `max_completion_tokens`"
            )),
        }
    }
}

/// `omit`, or a decimal temperature from 0 to 2 with at most three decimals: FX carries thousandths, and a
/// value it would have to round is refused rather than silently changed.
fn temperature(text: &str, name: &str) -> Result<Option<f32>, String> {
    let text = text.trim();
    if text == "omit" {
        return Ok(None);
    }
    let bad = || {
        Err(format!(
            "{name} needs a number from 0 to {MAX_TEMPERATURE} with at most three decimals (got `{text}`)"
        ))
    };
    let plain = !text.is_empty()
        && text.chars().all(|c| c.is_ascii_digit() || c == '.')
        && text.matches('.').count() <= 1
        && !text.starts_with('.')
        && !text.ends_with('.')
        && text.split_once('.').is_none_or(|(_, f)| f.len() <= 3);
    match text.parse::<f32>() {
        Ok(t) if plain && (0.0..=MAX_TEMPERATURE).contains(&t) => Ok(Some(t)),
        _ => bad(),
    }
}

impl WorkGeneration {
    /// Resolves the policy: a flag wins over its environment variable, which wins over the
    /// default. An invalid value is an error naming where it came from; nothing is clamped.
    pub fn resolve(flags: &Flags, env: &dyn Fn(&str) -> Option<String>) -> Result<Self, String> {
        let from_env = |name: &str| env(name).filter(|v| !v.trim().is_empty());
        let mut policy = Self::default();

        // A profile fills in the three request-compatibility settings it describes; each can still
        // be set explicitly. Order, per setting: flag, then its variable, then the profile, then
        // the default.
        let profile = if let Some(name) = &flags.provider_profile {
            Some(Setting {
                value: ProviderProfile::named(name)
                    .map_err(|e| format!("{PROVIDER_PROFILE_FLAG}: {e}"))?,
                source: Source::Flag(PROVIDER_PROFILE_FLAG),
            })
        } else if let Some(name) = from_env(PROVIDER_PROFILE_VARIABLE) {
            Some(Setting {
                value: ProviderProfile::named(&name)
                    .map_err(|e| format!("{PROVIDER_PROFILE_VARIABLE}: {e}"))?,
                source: Source::Environment(PROVIDER_PROFILE_VARIABLE),
            })
        } else {
            None
        };
        if let Some(chosen) = &profile {
            let p = chosen.value;
            policy.token_limit_field = Setting {
                value: p.token_limit_field,
                source: Source::Profile(p.name),
            };
            policy.temperature = Setting {
                value: p.temperature,
                source: Source::Profile(p.name),
            };
            policy.structured_output = Setting {
                value: p.structured_output,
                source: Source::Profile(p.name),
            };
            if let Some(secs) = p.call_timeout_secs {
                policy.call_timeout = Setting {
                    value: Duration::from_secs(secs),
                    source: Source::Profile(p.name),
                };
            }
        }
        policy.profile = profile;

        if let Some(text) = &flags.max_output_tokens {
            policy.max_output_tokens = Setting {
                value: max_output_tokens(text, MAX_OUTPUT_TOKENS_FLAG)?,
                source: Source::Flag(MAX_OUTPUT_TOKENS_FLAG),
            };
        } else if let Some(text) = from_env(MAX_OUTPUT_TOKENS_VARIABLE) {
            policy.max_output_tokens = Setting {
                value: max_output_tokens(&text, MAX_OUTPUT_TOKENS_VARIABLE)?,
                source: Source::Environment(MAX_OUTPUT_TOKENS_VARIABLE),
            };
        }

        if let Some(text) = &flags.temperature {
            policy.temperature = Setting {
                value: temperature(text, TEMPERATURE_FLAG)?,
                source: Source::Flag(TEMPERATURE_FLAG),
            };
        } else if let Some(text) = from_env(TEMPERATURE_VARIABLE) {
            policy.temperature = Setting {
                value: temperature(&text, TEMPERATURE_VARIABLE)?,
                source: Source::Environment(TEMPERATURE_VARIABLE),
            };
        }

        if let Some(text) = &flags.structured_output {
            policy.structured_output = Setting {
                value: StructuredOutput::parse(text)
                    .map_err(|e| format!("{STRUCTURED_OUTPUT_FLAG}: {e}"))?,
                source: Source::Flag(STRUCTURED_OUTPUT_FLAG),
            };
        } else if let Some(text) = from_env(STRUCTURED_OUTPUT_VARIABLE) {
            policy.structured_output = Setting {
                value: StructuredOutput::parse(&text)
                    .map_err(|e| format!("{STRUCTURED_OUTPUT_VARIABLE}: {e}"))?,
                source: Source::Environment(STRUCTURED_OUTPUT_VARIABLE),
            };
        }

        if let Some(text) = &flags.decision_prompt {
            policy.decision_prompt = Setting {
                value: DecisionPrompt::parse(text)
                    .map_err(|e| format!("{DECISION_PROMPT_FLAG}: {e}"))?,
                source: Source::Flag(DECISION_PROMPT_FLAG),
            };
        } else if let Some(text) = from_env(DECISION_PROMPT_VARIABLE) {
            policy.decision_prompt = Setting {
                value: DecisionPrompt::parse(&text)
                    .map_err(|e| format!("{DECISION_PROMPT_VARIABLE}: {e}"))?,
                source: Source::Environment(DECISION_PROMPT_VARIABLE),
            };
        }

        if let Some(text) = &flags.seed {
            policy.seed = Setting {
                value: Some(seed(text, SEED_FLAG)?),
                source: Source::Flag(SEED_FLAG),
            };
        } else if let Some(text) = from_env(SEED_VARIABLE) {
            policy.seed = Setting {
                value: Some(seed(&text, SEED_VARIABLE)?),
                source: Source::Environment(SEED_VARIABLE),
            };
        }
        for (flag_value, flag, variable, slot) in [
            (
                &flags.streaming,
                STREAMING_FLAG,
                STREAMING_VARIABLE,
                &mut policy.streaming,
            ),
            (
                &flags.structured_output_fallback,
                STRUCTURED_OUTPUT_FALLBACK_FLAG,
                STRUCTURED_OUTPUT_FALLBACK_VARIABLE,
                &mut policy.structured_output_fallback,
            ),
            (
                &flags.prompt_cache,
                PROMPT_CACHE_FLAG,
                PROMPT_CACHE_VARIABLE,
                &mut policy.prompt_cache,
            ),
        ] {
            if let Some(text) = flag_value {
                *slot = Setting {
                    value: on_off(text, flag)?,
                    source: Source::Flag(flag),
                };
            } else if let Some(text) = from_env(variable) {
                *slot = Setting {
                    value: on_off(&text, variable)?,
                    source: Source::Environment(variable),
                };
            }
        }

        if let Some(text) = &flags.call_timeout_secs {
            policy.call_timeout = Setting {
                value: call_timeout(text, CALL_TIMEOUT_FLAG)?,
                source: Source::Flag(CALL_TIMEOUT_FLAG),
            };
        } else if let Some(text) = from_env(CALL_TIMEOUT_VARIABLE) {
            policy.call_timeout = Setting {
                value: call_timeout(&text, CALL_TIMEOUT_VARIABLE)?,
                source: Source::Environment(CALL_TIMEOUT_VARIABLE),
            };
        }

        if let Some(text) = &flags.token_limit_field {
            policy.token_limit_field = Setting {
                value: TokenLimitField::parse(text)
                    .map_err(|e| format!("{TOKEN_LIMIT_FIELD_FLAG}: {e}"))?,
                source: Source::Flag(TOKEN_LIMIT_FIELD_FLAG),
            };
        } else if let Some(text) = from_env(TOKEN_LIMIT_FIELD_VARIABLE) {
            policy.token_limit_field = Setting {
                value: TokenLimitField::parse(&text)
                    .map_err(|e| format!("{TOKEN_LIMIT_FIELD_VARIABLE}: {e}"))?,
                source: Source::Environment(TOKEN_LIMIT_FIELD_VARIABLE),
            };
        }
        Ok(policy)
    }

    /// Checks the policy against the provider it is for, before anything is sent.
    ///
    /// * A profile written for one wire is refused on another (the `anthropic` profile on an
    ///   OpenAI-compatible endpoint, say).
    /// * The Anthropic Messages API enforces only a subset of JSON Schema (every object closed, no
    ///   bounds, no root combinator), which only `schema-closed` is written for. Any other
    ///   *explicitly* requested mode is refused there rather than quietly dropped; a mode that only
    ///   came from the default is reported as `none`, source `provider`.
    pub fn for_provider(mut self, provider: &str) -> Result<Self, String> {
        if let Some(chosen) = &self.profile {
            if chosen.value.wire != provider {
                return Err(format!(
                    "the `{}` provider profile is written for the {} provider, not {provider}",
                    chosen.value.name, chosen.value.wire
                ));
            }
        }
        if provider == chip_inference::PROVIDER_ANTHROPIC
            && self.seed.value.is_some()
            && let Source::Flag(name) | Source::Environment(name) = self.seed.source
        {
            return Err(format!("{name}: the Anthropic Messages API has no seed"));
        }
        if provider == chip_inference::PROVIDER_ANTHROPIC {
            if !self.streaming.value
                && let Source::Flag(name) | Source::Environment(name) = self.streaming.source
            {
                return Err(format!(
                    "{name} off: the Anthropic provider is streaming-only here"
                ));
            }
            if self.structured_output.value == StructuredOutput::JsonObject
                && let Source::Flag(name) | Source::Environment(name) =
                    self.structured_output.source
            {
                return Err(format!(
                    "{name} asks for `json-object`, which the Anthropic provider does not have; use `schema-closed` or `none`"
                ));
            }
        }
        if provider != chip_inference::PROVIDER_ANTHROPIC && self.prompt_cache.value {
            match self.prompt_cache.source {
                Source::Flag(name) | Source::Environment(name) => {
                    return Err(format!(
                        "{name} on: the {provider} provider has no request field for prompt caching (a server that caches prefixes does so on its own); use `off` or leave it unset"
                    ));
                }
                _ => {
                    self.prompt_cache = Setting {
                        value: false,
                        source: Source::ProviderLimit,
                    };
                }
            }
        }
        if provider == chip_inference::PROVIDER_ANTHROPIC
            && !matches!(
                self.structured_output.value,
                StructuredOutput::None | StructuredOutput::SchemaClosed
            )
        {
            match self.structured_output.source {
                Source::Flag(name) | Source::Environment(name) => {
                    return Err(format!(
                        "{name} asks for `{}` structured output, which the Anthropic provider cannot enforce; use `schema-closed` or `none`",
                        self.structured_output.value.name()
                    ));
                }
                _ => {
                    self.structured_output = Setting {
                        value: StructuredOutput::None,
                        source: Source::ProviderLimit,
                    };
                }
            }
        }
        Ok(self)
    }

    /// The effective settings and where each came from, for the work report.
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({
            "max_output_tokens": {
                "value": self.max_output_tokens.value,
                "source": self.max_output_tokens.source.describe(),
            },
            "temperature": {
                "value": self.temperature.value.map_or(serde_json::json!("omitted"), |t| serde_json::json!(t)),
                "source": self.temperature.source.describe(),
            },
            "provider_profile": self.profile.as_ref().map(|p| serde_json::json!({
                "value": p.value.name,
                "source": p.source.describe(),
            })),
            "token_limit_field": {
                "value": self.token_limit_field.value.name(),
                "source": self.token_limit_field.source.describe(),
            },
            "structured_output": {
                "value": self.structured_output.value.name(),
                "source": self.structured_output.source.describe(),
            },
            "seed": {
                "value": self.seed.value,
                "source": self.seed.source.describe(),
            },
            "streaming": {
                "value": self.streaming.value,
                "source": self.streaming.source.describe(),
            },
            "structured_output_fallback": {
                "value": self.structured_output_fallback.value,
                "source": self.structured_output_fallback.source.describe(),
            },
            "prompt_cache": {
                "value": self.prompt_cache.value,
                "source": self.prompt_cache.source.describe(),
            },
            "decision_prompt": {
                "value": self.decision_prompt.value.name(),
                "source": self.decision_prompt.source.describe(),
            },
            // The limit one provider call was allowed, not how long any call took.
            "call_timeout_secs": {
                "value": self.call_timeout.value.as_secs(),
                "source": self.call_timeout.source.describe(),
            },
        })
    }
}

/// A placeholder that refuses every call; only a [`WorkRuntime`] under construction holds it.
pub struct NoModel;

#[async_trait]
impl ModelProvider for NoModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, FxError> {
        Err(FxError::Configuration(
            "no model provider was supplied".into(),
        ))
    }
}

/// Sets the policy's token cap and temperature on every request that passes through it, so what
/// the report calls effective is what the provider is handed. It does nothing else: it does not
/// look at replies, retry, or change anything after a failure.
pub struct GenerationPolicy {
    pub inner: std::sync::Arc<dyn ModelProvider>,
    pub max_output_tokens: u32,
    pub temperature: Option<f32>,
}

#[async_trait]
impl ModelProvider for GenerationPolicy {
    async fn complete(&self, mut request: ModelRequest) -> Result<ModelResponse, FxError> {
        request.max_tokens = Some(self.max_output_tokens);
        request.temperature = self.temperature;
        self.inner.complete(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn the_decision_prompt_is_full_unless_it_is_explicitly_set() {
        let p = WorkGeneration::resolve(&Flags::default(), &env(&[])).unwrap();
        assert_eq!(p.decision_prompt.value, DecisionPrompt::Full);
        assert_eq!(p.decision_prompt.source, Source::Default);
        assert_eq!(DecisionPrompt::default(), DecisionPrompt::Full);
        // Nothing but the explicit setting selects compact: not a provider, a profile or a model.
        for profile in ["openai", "anthropic", "local"] {
            let flags = Flags {
                provider_profile: Some(profile.into()),
                ..Flags::default()
            };
            let p = WorkGeneration::resolve(&flags, &env(&[])).unwrap();
            assert_eq!(p.decision_prompt.value, DecisionPrompt::Full, "{profile}");
        }
    }

    #[test]
    fn the_decision_prompt_comes_from_the_flag_then_the_environment() {
        let v = env(&[(DECISION_PROMPT_VARIABLE, "compact")]);
        let p = WorkGeneration::resolve(&Flags::default(), &v).unwrap();
        assert_eq!(p.decision_prompt.value, DecisionPrompt::Compact);
        assert_eq!(
            p.decision_prompt.source,
            Source::Environment(DECISION_PROMPT_VARIABLE)
        );
        let flags = Flags {
            decision_prompt: Some("full".into()),
            ..Flags::default()
        };
        let p = WorkGeneration::resolve(&flags, &v).unwrap();
        assert_eq!(p.decision_prompt.value, DecisionPrompt::Full);
        assert_eq!(p.decision_prompt.source, Source::Flag(DECISION_PROMPT_FLAG));
        assert_eq!(p.report()["decision_prompt"]["value"], "full");
        assert_eq!(
            p.report()["decision_prompt"]["source"],
            "flag:--decision-prompt"
        );
    }

    #[test]
    fn an_unknown_decision_prompt_is_an_error_naming_its_source_and_never_defaulted() {
        for bad in ["tiny", "Compact", "FULL", "compact ,full", "0", "true"] {
            let flags = Flags {
                decision_prompt: Some(bad.into()),
                ..Flags::default()
            };
            let e = WorkGeneration::resolve(&flags, &env(&[])).unwrap_err();
            assert!(
                e.contains(DECISION_PROMPT_FLAG) && e.contains("`full`") && e.contains("`compact`"),
                "{bad:?}: {e}"
            );
            let e = WorkGeneration::resolve(
                &Flags::default(),
                &env(&[(DECISION_PROMPT_VARIABLE, bad)]),
            )
            .unwrap_err();
            assert!(e.contains(DECISION_PROMPT_VARIABLE), "{bad:?}: {e}");
        }
    }

    #[test]
    fn with_nothing_configured_the_policy_is_what_work_has_always_sent() {
        let p = WorkGeneration::resolve(&Flags::default(), &env(&[])).unwrap();
        assert_eq!(p, WorkGeneration::default());
        assert_eq!(p.max_output_tokens.value, 2048);
        assert_eq!(p.temperature.value, Some(0.0));
        assert_eq!(p.structured_output.value, StructuredOutput::Schema);
        assert!(
            [&p.max_output_tokens.source, &p.temperature.source]
                .into_iter()
                .all(|s| *s == Source::Default)
        );
        // Blank variables count as unset.
        let blank = env(&[
            (TEMPERATURE_VARIABLE, "  "),
            (MAX_OUTPUT_TOKENS_VARIABLE, ""),
        ]);
        assert_eq!(
            WorkGeneration::resolve(&Flags::default(), &blank).unwrap(),
            p
        );
    }

    #[test]
    fn a_flag_beats_its_variable_which_beats_the_default() {
        let vars = env(&[
            (MAX_OUTPUT_TOKENS_VARIABLE, "4096"),
            (TEMPERATURE_VARIABLE, "0.3"),
            (STRUCTURED_OUTPUT_VARIABLE, "none"),
        ]);
        let p = WorkGeneration::resolve(&Flags::default(), &vars).unwrap();
        assert_eq!(p.max_output_tokens.value, 4096);
        assert_eq!(
            p.max_output_tokens.source,
            Source::Environment(MAX_OUTPUT_TOKENS_VARIABLE)
        );
        assert_eq!(p.temperature.value, Some(0.3));
        assert_eq!(p.structured_output.value, StructuredOutput::None);

        let flags = Flags {
            max_output_tokens: Some("8192".into()),
            temperature: Some("0".into()),
            structured_output: Some("object".into()),
            ..Flags::default()
        };
        let p = WorkGeneration::resolve(&flags, &vars).unwrap();
        assert_eq!(p.max_output_tokens.value, 8192);
        assert_eq!(
            p.max_output_tokens.source,
            Source::Flag(MAX_OUTPUT_TOKENS_FLAG)
        );
        assert_eq!(p.temperature.value, Some(0.0));
        assert_eq!(p.temperature.source, Source::Flag(TEMPERATURE_FLAG));
        assert_eq!(p.structured_output.value, StructuredOutput::Object);
        assert_eq!(
            p.structured_output.source,
            Source::Flag(STRUCTURED_OUTPUT_FLAG)
        );
    }

    #[test]
    fn an_invalid_value_is_an_error_naming_its_source_and_never_clamped() {
        for bad in ["0", "-1", "abc", "1.5", "131073", "99999999999", ""] {
            let flags = Flags {
                max_output_tokens: Some(bad.into()),
                ..Flags::default()
            };
            let e = WorkGeneration::resolve(&flags, &env(&[])).unwrap_err();
            assert!(e.contains(MAX_OUTPUT_TOKENS_FLAG), "{bad:?}: {e}");
        }
        for bad in [
            "-0.1", "2.1", "hot", "0.3333", ".5", "1.", "1e-1", "nan", "inf", "",
        ] {
            let flags = Flags {
                temperature: Some(bad.into()),
                ..Flags::default()
            };
            let e = WorkGeneration::resolve(&flags, &env(&[])).unwrap_err();
            assert!(e.contains(TEMPERATURE_FLAG), "{bad:?}: {e}");
        }
        for bad in ["json_object", "Object", "Schema", "schemas", ""] {
            let flags = Flags {
                structured_output: Some(bad.into()),
                ..Flags::default()
            };
            let e = WorkGeneration::resolve(&flags, &env(&[])).unwrap_err();
            assert!(
                e.contains(STRUCTURED_OUTPUT_FLAG) && e.contains("object"),
                "{bad:?}: {e}"
            );
        }
        // The variable's error names the variable, and an invalid variable is not overridden by
        // silently using the default.
        let e = WorkGeneration::resolve(&Flags::default(), &env(&[(TEMPERATURE_VARIABLE, "9")]))
            .unwrap_err();
        assert!(e.contains(TEMPERATURE_VARIABLE), "{e}");
        // The extremes are accepted.
        let edges = Flags {
            max_output_tokens: Some("131072".into()),
            temperature: Some("2.000".into()),
            ..Flags::default()
        };
        assert!(WorkGeneration::resolve(&edges, &env(&[])).is_ok());
    }

    #[test]
    fn temperature_can_be_omitted_and_the_token_limit_field_chosen_never_guessed() {
        let f = Flags {
            temperature: Some("omit".into()),
            token_limit_field: Some("max_completion_tokens".into()),
            ..Flags::default()
        };
        let p = WorkGeneration::resolve(&f, &env(&[])).unwrap();
        assert_eq!(p.temperature.value, None);
        assert_eq!(p.report()["temperature"]["value"], "omitted");
        assert_eq!(
            p.token_limit_field.value,
            TokenLimitField::MaxCompletionTokens
        );
        assert_eq!(
            p.report()["token_limit_field"]["value"],
            "max_completion_tokens"
        );
        assert_eq!(
            p.report()["token_limit_field"]["source"],
            "flag:--token-limit-field"
        );
        // Defaults are what has always been sent.
        let d = WorkGeneration::default();
        assert_eq!(d.report()["token_limit_field"]["value"], "max_tokens");
        assert_eq!(d.report()["temperature"]["value"], 0.0);
        // The variable form, and the bad values.
        let v = env(&[
            (TEMPERATURE_VARIABLE, "omit"),
            (TOKEN_LIMIT_FIELD_VARIABLE, "max_completion_tokens"),
        ]);
        let p = WorkGeneration::resolve(&Flags::default(), &v).unwrap();
        assert_eq!(p.temperature.value, None);
        assert_eq!(
            p.token_limit_field.source,
            Source::Environment(TOKEN_LIMIT_FIELD_VARIABLE)
        );
        for bad in ["tokens", "MAX_TOKENS", "max_output_tokens", ""] {
            let f = Flags {
                token_limit_field: Some(bad.into()),
                ..Flags::default()
            };
            let e = WorkGeneration::resolve(&f, &env(&[])).unwrap_err();
            assert!(e.contains(TOKEN_LIMIT_FIELD_FLAG), "{bad:?}: {e}");
        }
        for bad in ["OMIT", "omitted", "none"] {
            let f = Flags {
                temperature: Some(bad.into()),
                ..Flags::default()
            };
            assert!(WorkGeneration::resolve(&f, &env(&[])).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_call_timeout_defaults_to_thirty_seconds_and_obeys_the_same_precedence() {
        let p = WorkGeneration::resolve(&Flags::default(), &env(&[])).unwrap();
        assert_eq!(p.call_timeout.value, Duration::from_secs(30));
        assert_eq!(p.call_timeout.source, Source::Default);
        let vars = env(&[(CALL_TIMEOUT_VARIABLE, "120")]);
        let p = WorkGeneration::resolve(&Flags::default(), &vars).unwrap();
        assert_eq!(p.call_timeout.value, Duration::from_secs(120));
        assert_eq!(
            p.call_timeout.source,
            Source::Environment(CALL_TIMEOUT_VARIABLE)
        );
        let flags = Flags {
            call_timeout_secs: Some("5".into()),
            ..Flags::default()
        };
        let p = WorkGeneration::resolve(&flags, &vars).unwrap();
        assert_eq!(p.call_timeout.value, Duration::from_secs(5));
        assert_eq!(p.call_timeout.source, Source::Flag(CALL_TIMEOUT_FLAG));
        assert_eq!(p.report()["call_timeout_secs"]["value"], 5);
        assert_eq!(
            p.report()["call_timeout_secs"]["source"],
            "flag:--call-timeout-secs"
        );
        for edge in ["1", "86400"] {
            let f = Flags {
                call_timeout_secs: Some(edge.into()),
                ..Flags::default()
            };
            assert!(WorkGeneration::resolve(&f, &env(&[])).is_ok(), "{edge}");
        }
    }

    #[test]
    fn an_invalid_call_timeout_is_refused_by_name_and_never_clamped() {
        for bad in [
            "0",
            "-1",
            "1.5",
            "30s",
            "abc",
            "86401",
            "99999999999999999999",
            "",
        ] {
            let f = Flags {
                call_timeout_secs: Some(bad.into()),
                ..Flags::default()
            };
            let e = WorkGeneration::resolve(&f, &env(&[])).unwrap_err();
            assert!(e.contains(CALL_TIMEOUT_FLAG), "{bad:?}: {e}");
        }
        let e = WorkGeneration::resolve(&Flags::default(), &env(&[(CALL_TIMEOUT_VARIABLE, "0")]))
            .unwrap_err();
        assert!(e.contains(CALL_TIMEOUT_VARIABLE), "{e}");
    }

    #[test]
    fn anthropic_cannot_carry_a_structured_output_mode_and_says_so_instead_of_dropping_it() {
        // Not chosen by the operator: reported as none, source provider.
        let p = WorkGeneration::default().for_provider("anthropic").unwrap();
        assert_eq!(p.structured_output.value, StructuredOutput::None);
        assert_eq!(p.structured_output.source, Source::ProviderLimit);
        let p = WorkGeneration::default()
            .for_provider("openai-compatible")
            .unwrap();
        assert_eq!(p.structured_output.value, StructuredOutput::Schema);
        // Chosen by the operator, by flag or variable: refused, naming where it came from.
        for flags in [
            Flags {
                structured_output: Some("schema".into()),
                ..Flags::default()
            },
            Flags {
                structured_output: Some("object".into()),
                ..Flags::default()
            },
        ] {
            let e = WorkGeneration::resolve(&flags, &env(&[]))
                .unwrap()
                .for_provider("anthropic")
                .unwrap_err();
            assert!(
                e.contains(STRUCTURED_OUTPUT_FLAG) && e.contains("Anthropic"),
                "{e}"
            );
        }
        let v = env(&[(STRUCTURED_OUTPUT_VARIABLE, "schema-flat")]);
        let e = WorkGeneration::resolve(&Flags::default(), &v)
            .unwrap()
            .for_provider("anthropic")
            .unwrap_err();
        assert!(e.contains(STRUCTURED_OUTPUT_VARIABLE), "{e}");
        // `none` is fine.
        let none = Flags {
            structured_output: Some("none".into()),
            ..Flags::default()
        };
        assert!(
            WorkGeneration::resolve(&none, &env(&[]))
                .unwrap()
                .for_provider("anthropic")
                .is_ok()
        );
    }

    #[test]
    fn anthropic_carries_only_the_closed_schema_and_refuses_the_modes_it_cannot_enforce() {
        let closed = Flags {
            structured_output: Some("schema-closed".into()),
            ..Flags::default()
        };
        let p = WorkGeneration::resolve(&closed, &env(&[]))
            .unwrap()
            .for_provider("anthropic")
            .unwrap();
        assert_eq!(p.structured_output.value, StructuredOutput::SchemaClosed);
        assert_eq!(
            p.structured_output.source,
            Source::Flag(STRUCTURED_OUTPUT_FLAG)
        );
        // The modes whose schemas carry bounds, a root combinator or an open object are refused
        // naming what Anthropic does take, never sent and never turned into another mode.
        for mode in ["schema", "schema-flat", "object"] {
            let flags = Flags {
                structured_output: Some(mode.into()),
                ..Flags::default()
            };
            let e = WorkGeneration::resolve(&flags, &env(&[]))
                .unwrap()
                .for_provider("anthropic")
                .unwrap_err();
            assert!(e.contains("schema-closed") && e.contains(mode), "{e}");
        }
        // The anthropic profile still defaults to none: sending a schema is the operator's choice.
        let profile = Flags {
            provider_profile: Some("anthropic".into()),
            ..Flags::default()
        };
        let p = WorkGeneration::resolve(&profile, &env(&[]))
            .unwrap()
            .for_provider("anthropic")
            .unwrap();
        assert_eq!(p.structured_output.value, StructuredOutput::None);
    }

    #[test]
    fn the_closed_schema_names_exactly_the_inputs_the_real_capabilities_declare() {
        use chip_core::CapabilityProvider;
        let dir = std::env::temp_dir().join(format!("chip-closed-names-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let declared = rt.block_on(async {
            let mut all = chip_project::ProjectExecutor::new(&dir)
                .capabilities()
                .await
                .unwrap();
            let pax = chip_pax::PaxExecutor::new(&dir);
            all.extend(pax.capabilities().await.unwrap());
            all.extend(chip_pax::PaxObserve::new(pax).capabilities().await.unwrap());
            all
        });
        let _ = std::fs::remove_dir_all(&dir);
        let mut names: Vec<String> = declared
            .iter()
            .flat_map(|d| d.inputs.iter().map(|i| i.name.clone()))
            .collect();
        names.sort();
        names.dedup();
        assert_eq!(
            names, CLOSED_INPUT_NAMES,
            "CLOSED_INPUT_NAMES drifted from the capability declarations"
        );
        // And the schema it produces is exactly that closed set.
        let schema: serde_json::Value =
            serde_json::from_str(&StructuredOutput::SchemaClosed.schema_text().unwrap()).unwrap();
        let request = schema["anyOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| {
                b["properties"]["decision"]["enum"] == serde_json::json!(["request_capability"])
            })
            .unwrap();
        let mut got: Vec<&str> = request["properties"]["inputs"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        got.sort();
        assert_eq!(got, CLOSED_INPUT_NAMES);
    }

    fn with_profile(name: &str) -> Flags {
        Flags {
            provider_profile: Some(name.into()),
            ..Flags::default()
        }
    }

    #[test]
    fn a_profile_fills_exactly_the_three_compatibility_settings_and_names_itself_as_their_source() {
        for (name, field, temp, mode) in [
            (
                "openai",
                TokenLimitField::MaxCompletionTokens,
                None,
                StructuredOutput::SchemaFlat,
            ),
            (
                "anthropic",
                TokenLimitField::MaxTokens,
                None,
                StructuredOutput::None,
            ),
            (
                "local",
                TokenLimitField::MaxTokens,
                Some(0.0),
                StructuredOutput::Schema,
            ),
        ] {
            let p = WorkGeneration::resolve(&with_profile(name), &env(&[])).unwrap();
            assert_eq!(
                (
                    p.token_limit_field.value,
                    p.temperature.value,
                    p.structured_output.value
                ),
                (field, temp, mode),
                "{name}"
            );
            let src = Source::Profile(PROFILES.iter().find(|x| x.name == name).unwrap().name);
            assert_eq!(
                (
                    &p.token_limit_field.source,
                    &p.temperature.source,
                    &p.structured_output.source
                ),
                (&src, &src, &src),
                "{name}"
            );
            // Everything else is untouched by a profile.
            let d = WorkGeneration::default();
            assert_eq!(
                (&p.max_output_tokens, &p.call_timeout),
                (&d.max_output_tokens, &d.call_timeout),
                "{name}"
            );
            let r = p.report();
            assert_eq!(r["provider_profile"]["value"], name);
            assert_eq!(r["provider_profile"]["source"], "flag:--provider-profile");
            assert_eq!(r["token_limit_field"]["source"], format!("profile:{name}"));
        }
        // No profile: the report says so and the defaults stand.
        assert!(WorkGeneration::default().report()["provider_profile"].is_null());
        // The variable selects one too, with its own source.
        let v = env(&[(PROVIDER_PROFILE_VARIABLE, "openai")]);
        let p = WorkGeneration::resolve(&Flags::default(), &v).unwrap();
        assert_eq!(
            p.report()["provider_profile"]["source"],
            "environment:CHIP_PROVIDER_PROFILE"
        );
        // Unknown names are refused by name, never guessed.
        for bad in ["gpt", "OpenAI", "claude", ""] {
            let e = WorkGeneration::resolve(&with_profile(bad), &env(&[])).unwrap_err();
            assert!(
                e.contains(PROVIDER_PROFILE_FLAG) && e.contains("openai"),
                "{bad:?}: {e}"
            );
        }
    }

    #[test]
    fn precedence_is_flag_then_variable_then_profile_then_default_for_each_setting() {
        let vars = env(&[
            (TEMPERATURE_VARIABLE, "0.5"),
            (STRUCTURED_OUTPUT_VARIABLE, "object"),
        ]);
        let flags = Flags {
            provider_profile: Some("openai".into()),
            token_limit_field: Some("max_tokens".into()),
            ..Flags::default()
        };
        let p = WorkGeneration::resolve(&flags, &vars).unwrap();
        // Flag beats the profile.
        assert_eq!(p.token_limit_field.value, TokenLimitField::MaxTokens);
        assert_eq!(
            p.token_limit_field.source,
            Source::Flag(TOKEN_LIMIT_FIELD_FLAG)
        );
        // The variable beats the profile.
        assert_eq!(p.temperature.value, Some(0.5));
        assert_eq!(
            p.temperature.source,
            Source::Environment(TEMPERATURE_VARIABLE)
        );
        assert_eq!(p.structured_output.value, StructuredOutput::Object);
        // With neither, the profile beats the default.
        let q = WorkGeneration::resolve(&with_profile("openai"), &env(&[])).unwrap();
        assert_eq!(q.structured_output.value, StructuredOutput::SchemaFlat);
        // A flag beats the variable beats the profile.
        let both = Flags {
            temperature: Some("0.1".into()),
            provider_profile: Some("openai".into()),
            ..Flags::default()
        };
        let r = WorkGeneration::resolve(&both, &vars).unwrap();
        assert_eq!(r.temperature.value, Some(0.1));
        assert_eq!(r.temperature.source, Source::Flag(TEMPERATURE_FLAG));
        // Unrelated settings keep their own sources.
        assert_eq!(r.max_output_tokens.source, Source::Default);
    }

    #[test]
    fn a_contradictory_configuration_is_refused_before_a_request_is_sent() {
        // A profile written for another wire.
        let e = WorkGeneration::resolve(&with_profile("anthropic"), &env(&[]))
            .unwrap()
            .for_provider("openai-compatible")
            .unwrap_err();
        assert!(
            e.contains("anthropic") && e.contains("openai-compatible"),
            "{e}"
        );
        let e = WorkGeneration::resolve(&with_profile("openai"), &env(&[]))
            .unwrap()
            .for_provider("anthropic")
            .unwrap_err();
        assert!(e.contains("openai"), "{e}");
        // The openai and local profiles are both for OpenAI-compatible endpoints.
        for name in ["openai", "local"] {
            assert!(
                WorkGeneration::resolve(&with_profile(name), &env(&[]))
                    .unwrap()
                    .for_provider("openai-compatible")
                    .is_ok()
            );
        }
        assert!(
            WorkGeneration::resolve(&with_profile("anthropic"), &env(&[]))
                .unwrap()
                .for_provider("anthropic")
                .is_ok()
        );
        // An explicit structured mode that the profile's provider cannot carry.
        let flags = Flags {
            provider_profile: Some("anthropic".into()),
            structured_output: Some("schema".into()),
            ..Flags::default()
        };
        assert!(
            WorkGeneration::resolve(&flags, &env(&[]))
                .unwrap()
                .for_provider("anthropic")
                .is_err()
        );
    }

    #[test]
    fn the_modes_ask_for_the_canonical_schema_projections() {
        let full = StructuredOutput::Schema.schema_text().unwrap();
        let flat = StructuredOutput::SchemaFlat.schema_text().unwrap();
        let (full_v, flat_v): (serde_json::Value, serde_json::Value) = (
            serde_json::from_str(&full).unwrap(),
            serde_json::from_str(&flat).unwrap(),
        );
        assert!(
            full_v.get("anyOf").is_some(),
            "the full projection has one alternative per decision"
        );
        assert!(
            flat_v.get("anyOf").is_none(),
            "the flat projection has nothing a root-restricted provider rejects"
        );
        assert!(full.contains("request_capability") && flat.contains("request_capability"));
        assert_eq!(StructuredOutput::Object.schema_text(), None);
        assert_eq!(StructuredOutput::None.schema_text(), None);
        assert!(
            StructuredOutput::Object.asks_for_json() && !StructuredOutput::None.asks_for_json()
        );
        for bad in ["json_object", "Schema", "flat", ""] {
            assert!(StructuredOutput::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_endpoint_profiles_set_their_request_choices_and_timeout_and_say_where_they_came_from() {
        for (name, field, mode, secs) in [
            (
                "lm-studio",
                TokenLimitField::MaxTokens,
                StructuredOutput::Schema,
                300,
            ),
            (
                "ollama",
                TokenLimitField::MaxTokens,
                StructuredOutput::Schema,
                300,
            ),
            (
                "llama-cpp",
                TokenLimitField::MaxTokens,
                StructuredOutput::JsonObject,
                300,
            ),
            (
                "vllm",
                TokenLimitField::MaxTokens,
                StructuredOutput::Schema,
                120,
            ),
            (
                "openrouter",
                TokenLimitField::MaxTokens,
                StructuredOutput::None,
                120,
            ),
        ] {
            let flags = Flags {
                provider_profile: Some(name.into()),
                ..Flags::default()
            };
            let p = WorkGeneration::resolve(&flags, &env(&[])).unwrap();
            assert_eq!(
                (p.token_limit_field.value, p.structured_output.value),
                (field, mode),
                "{name}"
            );
            assert_eq!(p.call_timeout.value, Duration::from_secs(secs), "{name}");
            assert_eq!(
                p.call_timeout.source,
                Source::Profile(PROFILES.iter().find(|x| x.name == name).unwrap().name)
            );
            // An explicit timeout still beats the profile's.
            let flags = Flags {
                provider_profile: Some(name.into()),
                call_timeout_secs: Some("7".into()),
                ..Flags::default()
            };
            let p = WorkGeneration::resolve(&flags, &env(&[])).unwrap();
            assert_eq!(p.call_timeout.value, Duration::from_secs(7));
            assert_eq!(p.call_timeout.source, Source::Flag(CALL_TIMEOUT_FLAG));
            // Every one is an OpenAI-compatible profile: the Anthropic provider refuses it by name.
            let flags = Flags {
                provider_profile: Some(name.into()),
                ..Flags::default()
            };
            assert!(
                WorkGeneration::resolve(&flags, &env(&[]))
                    .unwrap()
                    .for_provider("anthropic")
                    .is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn the_report_names_a_stable_set_of_fields() {
        let r = WorkGeneration::default().report();
        assert_eq!(r["max_output_tokens"]["value"], 2048);
        assert_eq!(r["max_output_tokens"]["source"], "default");
        assert_eq!(r["temperature"]["value"], 0.0);
        assert_eq!(r["structured_output"]["value"], "schema");
        assert_eq!(r["prompt_cache"]["value"], true);
        assert_eq!(r["prompt_cache"]["source"], "default");
    }

    #[test]
    fn prompt_caching_is_on_for_anthropic_switchable_off_and_never_claimed_elsewhere() {
        use chip_inference::{PROVIDER_ANTHROPIC, PROVIDER_OPENAI_COMPATIBLE};
        let resolve = |flags: &Flags, vars: &[(&str, &str)], provider: &str| {
            WorkGeneration::resolve(flags, &env(vars)).and_then(|p| p.for_provider(provider))
        };
        let none = Flags::default();
        // Anthropic: on by default.
        let p = resolve(&none, &[], PROVIDER_ANTHROPIC).unwrap();
        assert!(p.prompt_cache.value);
        assert_eq!(p.prompt_cache.source, Source::Default);
        // The opt-out, by flag or by variable, with its source named.
        let off = Flags {
            prompt_cache: Some("off".into()),
            ..Flags::default()
        };
        let p = resolve(&off, &[], PROVIDER_ANTHROPIC).unwrap();
        assert!(!p.prompt_cache.value);
        assert_eq!(p.report()["prompt_cache"]["source"], "flag:--prompt-cache");
        let p = resolve(&none, &[(PROMPT_CACHE_VARIABLE, "off")], PROVIDER_ANTHROPIC).unwrap();
        assert!(!p.prompt_cache.value);
        assert_eq!(
            p.report()["prompt_cache"]["source"],
            "environment:CHIP_PROMPT_CACHE"
        );
        // OpenAI-compatible: nothing to send, so it is reported off by the provider...
        let p = resolve(&none, &[], PROVIDER_OPENAI_COMPATIBLE).unwrap();
        assert!(!p.prompt_cache.value);
        assert_eq!(p.report()["prompt_cache"]["source"], "provider");
        // ...an explicit `off` is accepted everywhere (for uniform A/B configuration)...
        assert!(
            !resolve(&off, &[], PROVIDER_OPENAI_COMPATIBLE)
                .unwrap()
                .prompt_cache
                .value
        );
        // ...and an explicit `on` is refused by name, never silently dropped.
        let on = Flags {
            prompt_cache: Some("on".into()),
            ..Flags::default()
        };
        let e = resolve(&on, &[], PROVIDER_OPENAI_COMPATIBLE).unwrap_err();
        assert!(e.contains("--prompt-cache on"), "{e}");
        let e = resolve(
            &none,
            &[(PROMPT_CACHE_VARIABLE, "on")],
            PROVIDER_OPENAI_COMPATIBLE,
        )
        .unwrap_err();
        assert!(e.contains("CHIP_PROMPT_CACHE on"), "{e}");
        // Anything but on or off is refused.
        let bad = Flags {
            prompt_cache: Some("yes".into()),
            ..Flags::default()
        };
        assert!(WorkGeneration::resolve(&bad, &env(&[])).is_err());
    }
}
