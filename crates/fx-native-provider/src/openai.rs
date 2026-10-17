//! The OpenAI-compatible chat-completions wire format (streaming).
//!
//! Serves OpenAI, OpenRouter, LM Studio and other servers that speak this format.

use fx_core::{Part, ReasoningEffort, Request, Role, Speed, ToolChoice};
use fx_single_attempt::{PortItem, PortSupport, PortUsage, Terminal};
use serde_json::{Value, json};

use crate::config::{Common, OpenAiConfig};
use crate::secret::Secret;
use crate::wire::{Decoder, Step, Wire};

pub struct OpenAiWire {
    pub(crate) config: OpenAiConfig,
}

/// Options the native wire formats carry: an output token cap, a temperature, and (for the
/// OpenAI-compatible format only) a JSON-schema response format. Anything else FX could ask
/// for is refused here, before any thread starts.
pub(crate) fn check_options(request: &Request, structured_output: bool) -> Result<(), String> {
    if request.options.reasoning != ReasoningEffort::Default {
        return Err("reasoning effort is not supported by the native provider".into());
    }
    if request.options.speed != Speed::Standard {
        return Err("speed is not supported by the native provider".into());
    }
    if !structured_output && request.options.response_format.is_some() {
        return Err("structured output is not supported by this native provider".into());
    }
    Ok(())
}

/// A temperature in thousandths as the JSON number the providers expect.
pub(crate) fn temperature(milli: u16) -> Value {
    json!(f64::from(milli) / 1000.0)
}

/// Text of a message, or the first thing in it that cannot be carried.
pub(crate) fn plain_text(parts: &[Part]) -> Result<String, String> {
    let mut text = String::new();
    for part in parts {
        match part {
            Part::Text(t) => text.push_str(t),
            _ => return Err("only text parts are supported".into()),
        }
    }
    Ok(text)
}

pub(crate) fn check_common(request: &Request, structured_output: bool) -> Result<(), String> {
    if request.model.as_str().trim().is_empty() {
        return Err("the model reference is empty".into());
    }
    if !request.tools.is_empty() || !matches!(request.tool_choice, ToolChoice::Auto) {
        return Err("tools are not supported by the native provider".into());
    }
    check_options(request, structured_output)
}

impl Wire for OpenAiWire {
    fn support(&self) -> PortSupport {
        PortSupport {
            generation_options: true,
            response_format: true,
            ..PortSupport::default()
        }
    }

    fn url(&self) -> String {
        self.config.common.endpoint("chat/completions")
    }

    fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![
            ("accept".to_owned(), "text/event-stream".to_owned()),
            ("content-type".to_owned(), "application/json".to_owned()),
        ];
        if let Some(key) = &self.config.common.key {
            headers.push((
                "authorization".to_owned(),
                format!("Bearer {}", key.expose()),
            ));
        }
        headers.extend(self.config.common.extra_headers.iter().cloned());
        headers
    }

    fn body(&self, request: &Request) -> Result<Vec<u8>, String> {
        check_common(request, true)?;
        // Consecutive system messages are sent as one, in order, joined by a blank line: the
        // format allows several, but some servers' chat templates (local models served by LM
        // Studio among them) accept a system message only first and only once. Nothing is
        // dropped or reordered, and a system message that follows another role stays in place.
        let mut messages: Vec<(&'static str, String)> = Vec::with_capacity(request.messages.len());
        for message in &request.messages {
            let role = match message.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => return Err("tool messages are not supported".into()),
            };
            let text = plain_text(&message.parts)?;
            match messages.last_mut() {
                Some((last, joined)) if role == "system" && *last == "system" => {
                    joined.push_str("\n\n");
                    joined.push_str(&text);
                }
                _ => messages.push((role, text)),
            }
        }
        let messages: Vec<Value> = messages
            .into_iter()
            .map(|(role, content)| json!({"role": role, "content": content}))
            .collect();
        let mut body = json!({
            "model": request.model.as_str(),
            "messages": messages,
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        if let Some(enable) = self.config.template_thinking {
            body["chat_template_kwargs"] = json!({"enable_thinking": enable});
        }
        if let Some(max) = request.options.max_output_tokens {
            body["max_tokens"] = json!(max);
        }
        if let Some(milli) = request.options.temperature_milli {
            body["temperature"] = temperature(milli);
        }
        if let Some(format) = &request.options.response_format {
            // FX validated the schema as a JSON object; it is passed through untouched.
            let schema: Value = serde_json::from_str(&format.schema_json)
                .map_err(|_| "the response format schema is not valid JSON".to_owned())?;
            let mut named = json!({"name": format.name, "schema": schema, "strict": false});
            if !format.description.is_empty() {
                named["description"] = json!(format.description);
            }
            body["response_format"] = json!({"type": "json_schema", "json_schema": named});
        }
        serde_json::to_vec(&body).map_err(|e| e.to_string())
    }

    fn decoder(&self) -> Box<dyn Decoder> {
        Box::new(OpenAiDecoder::default())
    }

    fn secret(&self) -> Option<&Secret> {
        self.config.common.key.as_ref()
    }

    fn settings(&self) -> &Common {
        &self.config.common
    }
}

#[derive(Default)]
struct OpenAiDecoder {
    finish: Option<String>,
    model: Option<String>,
    usage: Option<PortUsage>,
    refused: bool,
}

impl Decoder for OpenAiDecoder {
    fn feed(&mut self, _event: Option<&str>, data: &str) -> Result<Step, String> {
        if data.trim() == "[DONE]" {
            if self.finish.is_none() {
                return Err("the stream ended without a finish reason".into());
            }
            return Ok(Step::Done(PortItem::Done {
                usage: self.usage.take(),
                finish_reason: if self.refused {
                    Some("content_filter".to_owned())
                } else {
                    self.finish.take()
                },
                model: self.model.take(),
                terminal: Terminal::ProviderMarker,
            }));
        }
        let value: Value = serde_json::from_str(data)
            .map_err(|_| "a stream chunk is not valid JSON".to_owned())?;
        let object = value.as_object().ok_or("a stream chunk is not an object")?;
        if let Some(error) = object.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the provider reported an error");
            return Ok(Step::ProviderError(message.to_owned()));
        }
        if let Some(model) = object.get("model").and_then(Value::as_str) {
            self.model = Some(model.to_owned());
        }
        let mut items = Vec::new();
        if let Some(choices) = object.get("choices") {
            let choices = choices.as_array().ok_or("`choices` is not an array")?;
            // One choice is requested. Another index is ignored, never merged.
            if let Some(choice) = choices
                .iter()
                .find(|c| c.get("index").and_then(Value::as_u64).unwrap_or(0) == 0)
            {
                if let Some(delta) = choice.get("delta") {
                    if delta.get("tool_calls").is_some_and(|v| !v.is_null())
                        || delta.get("function_call").is_some_and(|v| !v.is_null())
                    {
                        return Err("the provider proposed a tool call that was not offered".into());
                    }
                    if let Some(text) = delta.get("content").and_then(Value::as_str)
                        && !text.is_empty()
                    {
                        items.push(PortItem::Text(text.to_owned()));
                    }
                    if let Some(text) = delta.get("refusal").and_then(Value::as_str)
                        && !text.is_empty()
                    {
                        self.refused = true;
                        items.push(PortItem::Text(text.to_owned()));
                    }
                }
                if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                    self.finish = Some(reason.to_owned());
                }
            }
        }
        if let Some(usage) = object.get("usage").filter(|u| u.is_object()) {
            let count = |v: Option<&Value>| v.and_then(Value::as_u64);
            self.usage = Some(PortUsage {
                input_tokens: count(usage.get("prompt_tokens")),
                output_tokens: count(usage.get("completion_tokens")),
                cache_read_tokens: count(usage.pointer("/prompt_tokens_details/cached_tokens")),
                reasoning_tokens: count(
                    usage.pointer("/completion_tokens_details/reasoning_tokens"),
                ),
            });
        }
        Ok(Step::Items(items))
    }
}
