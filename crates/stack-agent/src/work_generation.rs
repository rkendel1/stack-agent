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
pub const TOKEN_LIMIT_FIELD_FLAG: &str = "--token-limit-field";
pub const TOKEN_LIMIT_FIELD_VARIABLE: &str = "CHIP_TOKEN_LIMIT_FIELD";
pub const STRUCTURED_OUTPUT_FLAG: &str = "--structured-output";
pub const CALL_TIMEOUT_FLAG: &str = "--call-timeout-secs";
pub const CALL_TIMEOUT_VARIABLE: &str = "CHIP_CALL_TIMEOUT_SECS";
pub const MAX_OUTPUT_TOKENS_VARIABLE: &str = "CHIP_MAX_OUTPUT_TOKENS";
pub const TEMPERATURE_VARIABLE: &str = "CHIP_TEMPERATURE";
pub const STRUCTURED_OUTPUT_VARIABLE: &str = "CHIP_STRUCTURED_OUTPUT";

/// The largest output budget accepted. A bound on typos, not a claim about any provider: a
/// provider or server that accepts less refuses the request, and that refusal is reported as is.
pub const MAX_OUTPUT_TOKENS_LIMIT: u32 = 131_072;
/// The longest call timeout accepted, in seconds: the 24 hours FX itself allows an attempt. FX
/// refuses a zero or longer budget, so a value outside 1 to this is refused here, by name, before
/// anything is sent.
pub const MAX_CALL_TIMEOUT_SECS: u64 = 86_400;
/// The highest temperature accepted. A provider that accepts a narrower range refuses the request.
pub const MAX_TEMPERATURE: f32 = 2.0;

/// Whether the endpoint is asked to constrain the reply. This is a request to the server, not a
/// check by Chip: the reply is validated by Chip either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredOutput {
    /// A `json_schema` response format whose schema is `{"type":"object"}` (the default).
    Object,
    /// No `response_format` field is sent.
    None,
}

impl StructuredOutput {
    pub fn name(self) -> &'static str {
        match self {
            Self::Object => "object",
            Self::None => "none",
        }
    }

    fn parse(text: &str) -> Result<Self, String> {
        match text.trim() {
            "object" => Ok(Self::Object),
            "none" => Ok(Self::None),
            other => Err(format!(
                "`{other}` is not a structured-output mode; use `object` or `none`"
            )),
        }
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
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Self::Default => "default".into(),
            Self::Environment(name) => format!("environment:{name}"),
            Self::Flag(name) => format!("flag:{name}"),
            Self::ProviderLimit => "provider".into(),
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
    pub call_timeout_secs: Option<String>,
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
    /// How long one model call may take, end to end.
    pub call_timeout: Setting<Duration>,
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
                value: StructuredOutput::Object,
                source: Source::Default,
            },
            call_timeout: Setting {
                value: chip_inference::DEFAULT_TIMEOUT,
                source: Source::Default,
            },
        }
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

    /// The Anthropic Messages API has no response-format field, so the structured-output request
    /// cannot be sent there whatever was configured: the report says so instead of claiming it.
    pub fn for_provider(mut self, provider: &str) -> Self {
        if provider == chip_inference::PROVIDER_ANTHROPIC
            && self.structured_output.value == StructuredOutput::Object
        {
            self.structured_output = Setting {
                value: StructuredOutput::None,
                source: Source::ProviderLimit,
            };
        }
        self
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
            "token_limit_field": {
                "value": self.token_limit_field.value.name(),
                "source": self.token_limit_field.source.describe(),
            },
            "structured_output": {
                "value": self.structured_output.value.name(),
                "source": self.structured_output.source.describe(),
            },
            // The limit one provider call was allowed, not how long any call took.
            "call_timeout_secs": {
                "value": self.call_timeout.value.as_secs(),
                "source": self.call_timeout.source.describe(),
            },
        })
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
    fn with_nothing_configured_the_policy_is_what_work_has_always_sent() {
        let p = WorkGeneration::resolve(&Flags::default(), &env(&[])).unwrap();
        assert_eq!(p, WorkGeneration::default());
        assert_eq!(p.max_output_tokens.value, 2048);
        assert_eq!(p.temperature.value, Some(0.0));
        assert_eq!(p.structured_output.value, StructuredOutput::Object);
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
        for bad in ["json_object", "Object", "schema", ""] {
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
    fn a_provider_without_a_response_format_is_reported_as_having_none() {
        let p = WorkGeneration::default().for_provider("anthropic");
        assert_eq!(p.structured_output.value, StructuredOutput::None);
        assert_eq!(p.structured_output.source, Source::ProviderLimit);
        let p = WorkGeneration::default().for_provider("openai-compatible");
        assert_eq!(p.structured_output.value, StructuredOutput::Object);
    }

    #[test]
    fn the_report_names_a_stable_set_of_fields() {
        let r = WorkGeneration::default().report();
        assert_eq!(r["max_output_tokens"]["value"], 2048);
        assert_eq!(r["max_output_tokens"]["source"], "default");
        assert_eq!(r["temperature"]["value"], 0.0);
        assert_eq!(r["structured_output"]["value"], "object");
    }
}
