//! Independent acceptance test (written before the benchmark run; not shown to the agent).
//! Public API only: Anthropic provider -> FX executor -> `Usage.cache_write_tokens`.
mod support;

use fx_core::*;
use fx_native_provider::{Anthropic, AnthropicConfig, Secret};
use fx_single_attempt::SingleAttemptCapability;
use support::{Server, sse};

#[derive(Default)]
struct Host;
impl fx_core::Host for Host {
    fn cancel_requested(&mut self) -> bool {
        false
    }
    fn on_event(&mut self, _event: &Event) -> Flow {
        Flow::Continue
    }
}

fn request() -> Request {
    Request {
        model: ModelRef("claude-mock".to_owned()),
        messages: vec![Message {
            role: Role::User,
            parts: vec![Part::Text("say hi".to_owned())],
            cache: CacheHint::Default,
        }],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        options: GenerationOptions::default(),
        correlation: Correlation {
            request_id: RequestId([9; 16]),
            affinity_key: None,
        },
        budget: None,
    }
}

fn ev(name: &str, json: &str) -> Vec<u8> {
    format!("event: {name}\ndata: {json}\n\n").into_bytes()
}

fn stream(start_usage: &str) -> Vec<Vec<u8>> {
    vec![
        ev("message_start", &format!(r#"{{"type":"message_start","message":{{"id":"msg_1","model":"claude-mock","usage":{{{start_usage}}}}}}}"#)),
        ev("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#),
        ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#),
        ev("content_block_stop", r#"{"type":"content_block_stop","index":0}"#),
        ev("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}"#),
        ev("message_stop", r#"{"type":"message_stop"}"#),
    ]
}

fn usage_for(start_usage: &str) -> Usage {
    let server = Server::start(vec![sse(stream(start_usage))]);
    let config = AnthropicConfig::new(&server.base_url, Secret::new("sk-ant-acceptance-0123456789abcdef")).unwrap();
    let mut capability = SingleAttemptCapability::new(Anthropic::new(config));
    let report = execute(&mut capability, &request(), &Limits::DEFAULT, &mut Host);
    match report.outcome {
        Outcome::Completed(c) => c.usage.expect("usage reported"),
        other => panic!("expected completion, got {other:?}"),
    }
}

#[test]
fn ac1_cache_creation_tokens_reach_cache_write_tokens() {
    let u = usage_for(r#""input_tokens":12,"cache_creation_input_tokens":5,"cache_read_input_tokens":2"#);
    assert_eq!(u.cache_write_tokens, Some(5));
    assert_eq!(u.cache_read_tokens, Some(2));
    assert_eq!((u.input_tokens, u.output_tokens), (Some(12), Some(3)));
}

#[test]
fn ac2_an_unreported_cache_write_stays_unknown_not_zero() {
    let u = usage_for(r#""input_tokens":12,"cache_read_input_tokens":2"#);
    assert_eq!(u.cache_write_tokens, None);
}

#[test]
fn ac3_a_reported_zero_is_a_zero() {
    let u = usage_for(r#""input_tokens":12,"cache_creation_input_tokens":0"#);
    assert_eq!(u.cache_write_tokens, Some(0));
}
