//! The Anthropic Messages API wire format (streaming).

use fx_core::{Request, Role};
use fx_single_attempt::{PortItem, PortSupport, PortUsage, Terminal};
use serde_json::{Value, json};

use crate::config::{AnthropicConfig, Common};
use crate::openai::{check_common, plain_text, temperature};
use crate::secret::Secret;
use crate::wire::{Decoder, Step, Wire};

pub struct AnthropicWire {
    pub(crate) config: AnthropicConfig,
}

impl Wire for AnthropicWire {
    fn support(&self) -> PortSupport {
        PortSupport {
            generation_options: true,
            response_format: true,
            ..PortSupport::default()
        }
    }

    fn url(&self) -> String {
        self.config.common.endpoint("messages")
    }

    fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![
            ("accept".to_owned(), "text/event-stream".to_owned()),
            ("content-type".to_owned(), "application/json".to_owned()),
            ("anthropic-version".to_owned(), self.config.version.clone()),
        ];
        if let Some(key) = &self.config.common.key {
            headers.push(("x-api-key".to_owned(), key.expose().to_owned()));
        }
        headers.extend(self.config.common.extra_headers.iter().cloned());
        headers
    }

    fn body(&self, request: &Request) -> Result<Vec<u8>, String> {
        check_common(request, true)?;
        if request.options.temperature_milli.is_some_and(|t| t > 1000) {
            return Err("Anthropic accepts a temperature of at most 1.0".into());
        }
        let mut system: Vec<String> = Vec::new();
        let mut messages = Vec::with_capacity(request.messages.len());
        for message in &request.messages {
            let text = plain_text(&message.parts)?;
            match message.role {
                // The Messages API takes system text as a top-level field, not a message.
                Role::System => system.push(text),
                Role::User => messages.push(json!({"role": "user", "content": text})),
                Role::Assistant => messages.push(json!({"role": "assistant", "content": text})),
                Role::Tool => return Err("tool messages are not supported".into()),
            }
        }
        if messages.is_empty() {
            return Err("the Messages API needs at least one user or assistant message".into());
        }
        let mut body = json!({
            "model": request.model.as_str(),
            "messages": messages,
            "stream": true,
            // Required by the API. The request's cap wins; otherwise the host's setting.
            "max_tokens": request
                .options
                .max_output_tokens
                .unwrap_or(self.config.default_max_tokens),
        });
        if let Some(milli) = request.options.temperature_milli {
            body["temperature"] = temperature(milli);
        }
        if !system.is_empty() {
            body["system"] = if self.config.prompt_cache {
                system_blocks(&system)
            } else {
                json!(system.join("\n\n"))
            };
        }
        if let Some(format) = &request.options.response_format {
            // Anthropic's structured output is `output_config.format` (generally available; the
            // older `output_format` needs a beta header and is deprecated). FX validated the
            // schema as a JSON object; it is passed through untouched. The provider enforces only
            // a subset of JSON Schema and answers a schema outside it with a 400, which is a
            // provider failure here, never a request quietly rewritten. A refusal or a reply cut
            // at `max_tokens` may not match the schema; those arrive as their own finish reasons.
            let schema = crate::openai::schema_text(&format.schema_json)?;
            return Ok(crate::openai::with_member(
                serde_json::to_vec(&body).map_err(|e| e.to_string())?,
                "output_config",
                &format!("{{\"format\":{{\"type\":\"json_schema\",\"schema\":{schema}}}}}"),
            ));
        }
        serde_json::to_vec(&body).map_err(|e| e.to_string())
    }

    fn decoder(&self) -> Box<dyn Decoder> {
        Box::new(AnthropicDecoder::default())
    }

    fn secret(&self) -> Option<&Secret> {
        self.config.common.key.as_ref()
    }

    fn settings(&self) -> &Common {
        &self.config.common
    }
}

/// The system text as text blocks, one per system message, with a cache breakpoint on the last.
///
/// Each block after the first starts with the `\n\n` separator the joined form puts between
/// messages, so concatenating the blocks gives exactly the joined string. A block's text depends
/// only on its own message, so a request whose system messages extend an earlier request's keeps
/// that request's blocks byte-identical: the prefix a provider cache can match.
fn system_blocks(system: &[String]) -> Value {
    let last = system.len().saturating_sub(1);
    Value::Array(
        system
            .iter()
            .enumerate()
            .map(|(i, text)| {
                let text = if i == 0 {
                    text.clone()
                } else {
                    format!("\n\n{text}")
                };
                let mut block = json!({"type": "text", "text": text});
                if i == last {
                    block["cache_control"] = json!({"type": "ephemeral"});
                }
                block
            })
            .collect(),
    )
}

#[derive(Default)]
struct AnthropicDecoder {
    model: Option<String>,
    input: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
    output: Option<u64>,
    stop_reason: Option<String>,
}

impl AnthropicDecoder {
    /// Reads the counts a usage object carries. Anthropic's `input_tokens` excludes both cache
    /// counts; the three are kept apart and never added here. A field that is absent leaves what
    /// was read before (the `message_delta` usage is cumulative and may repeat or update them).
    fn read_usage(&mut self, usage: &Value) {
        let count = |name: &str| usage.get(name).and_then(Value::as_u64);
        if let Some(n) = count("input_tokens") {
            self.input = Some(n);
        }
        if let Some(n) = count("cache_read_input_tokens") {
            self.cache_read = Some(n);
        }
        if let Some(n) = count("cache_creation_input_tokens") {
            self.cache_write = Some(n);
        }
        if let Some(n) = count("output_tokens") {
            self.output = Some(n);
        }
    }
}

impl Decoder for AnthropicDecoder {
    fn feed(&mut self, _event: Option<&str>, data: &str) -> Result<Step, String> {
        let value: Value = serde_json::from_str(data)
            .map_err(|_| "a stream event is not valid JSON".to_owned())?;
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .ok_or("a stream event has no type")?;
        let mut items = Vec::new();
        match kind {
            "ping" | "content_block_stop" => {}
            "message_start" => {
                let message = value.get("message").unwrap_or(&Value::Null);
                if let Some(model) = message.get("model").and_then(Value::as_str) {
                    self.model = Some(model.to_owned());
                }
                // `message_start` carries the input counts (and a provisional output count, which
                // `message_delta` replaces).
                let usage = message.get("usage").unwrap_or(&Value::Null);
                self.input = usage.get("input_tokens").and_then(Value::as_u64);
                self.cache_read = usage.get("cache_read_input_tokens").and_then(Value::as_u64);
                self.cache_write = usage
                    .get("cache_creation_input_tokens")
                    .and_then(Value::as_u64);
            }
            "content_block_start" => {
                let block = value.get("content_block").and_then(|b| b.get("type"));
                match block.and_then(Value::as_str) {
                    Some("text" | "thinking" | "redacted_thinking") | None => {}
                    Some(other) => {
                        return Err(format!(
                            "the provider started a {other} block that was not offered"
                        ));
                    }
                }
            }
            "content_block_delta" => {
                let delta = value.get("delta").ok_or("a block delta has no delta")?;
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str)
                            && !text.is_empty()
                        {
                            items.push(PortItem::Text(text.to_owned()));
                        }
                    }
                    // Not requested; carries no output text.
                    Some("thinking_delta" | "signature_delta") => {}
                    Some("input_json_delta") => {
                        return Err("the provider proposed a tool call that was not offered".into());
                    }
                    _ => return Err("an unknown block delta".into()),
                }
            }
            "message_delta" => {
                if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(reason.to_owned());
                }
                if let Some(usage) = value.get("usage").filter(|u| u.is_object()) {
                    self.read_usage(usage);
                }
            }
            "message_stop" => {
                let Some(reason) = self.stop_reason.take() else {
                    return Err("the stream ended without a stop reason".into());
                };
                if reason == "tool_use" {
                    return Err("the provider stopped for a tool call that was not offered".into());
                }
                let usage = (self.input.is_some() || self.output.is_some()).then_some(PortUsage {
                    input_tokens: self.input,
                    output_tokens: self.output,
                    cache_read_tokens: self.cache_read,
                    cache_write_tokens: self.cache_write,
                    reasoning_tokens: None,
                });
                return Ok(Step::Done(PortItem::Done {
                    usage,
                    // FX maps `refusal` to nothing; a refusal is a content-filter finish.
                    finish_reason: Some(if reason == "refusal" {
                        "content_filter".to_owned()
                    } else {
                        reason
                    }),
                    model: self.model.take(),
                    terminal: Terminal::ProviderMarker,
                }));
            }
            "error" => {
                let message = value
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("the provider reported an error");
                return Ok(Step::ProviderError(message.to_owned()));
            }
            // Unknown event types are ignored, as the API documents they may be added.
            _ => {}
        }
        Ok(Step::Items(items))
    }
}
