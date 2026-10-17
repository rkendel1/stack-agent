//! Shared builders for the contract test suites.
#![allow(dead_code)]

use std::time::Duration;

use fx_core::*;

/// The 8-byte PNG signature followed by a tiny IHDR-like tail; enough for sniffing.
pub const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR";
pub const JPEG: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
pub const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00";
pub const WEBP: &[u8] = b"RIFF\x24\x00\x00\x00WEBPVP8 ";

pub fn text(role: Role, body: &str) -> Message {
    Message {
        role,
        parts: vec![Part::Text(body.to_owned())],
        cache: CacheHint::Default,
    }
}

pub fn call(id: &str, name: &str, args: &str) -> Part {
    Part::ToolCall(ToolCallProposal {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments_json: args.to_owned(),
    })
}

pub fn result(id: &str, name: &str, output: &str) -> Part {
    Part::ToolResult(ToolResult {
        call_id: id.to_owned(),
        name: name.to_owned(),
        output: ToolOutput::Text(output.to_owned()),
    })
}

pub fn message(role: Role, parts: Vec<Part>) -> Message {
    Message {
        role,
        parts,
        cache: CacheHint::Default,
    }
}

pub fn tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: "List directory entries.".to_owned(),
        input_schema_json: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#
            .to_owned(),
        execution: ToolExecution::Client,
        provider_args_json: None,
    }
}

/// A minimal valid request: one user message, no tools.
pub fn minimal() -> Request {
    Request {
        model: ModelRef("example/model-1".to_owned()),
        messages: vec![text(Role::User, "say hi")],
        tools: vec![],
        tool_choice: ToolChoice::Auto,
        options: GenerationOptions::default(),
        correlation: Correlation {
            request_id: RequestId([7; 16]),
            affinity_key: None,
        },
        budget: None,
    }
}

/// A request exercising every request shape: all roles and part kinds, a full
/// tool round trip, client and provider tools, every option and correlation field.
pub fn full() -> Request {
    Request {
        model: ModelRef("example/model-1".to_owned()),
        messages: vec![
            Message {
                role: Role::System,
                parts: vec![Part::Text("You are terse.".to_owned())],
                cache: CacheHint::Breakpoint,
            },
            message(
                Role::User,
                vec![
                    Part::Text("list files and describe the image".to_owned()),
                    Part::Image(ImageData {
                        media_type: ImageMediaType::Png,
                        bytes: PNG.to_vec(),
                    }),
                ],
            ),
            message(
                Role::Assistant,
                vec![
                    Part::Text("Listing.".to_owned()),
                    call("call_1", "list_files", r#"{"path":"."}"#),
                    call("call_2", "list_files", r#"{"path":"src"}"#),
                ],
            ),
            message(
                Role::Tool,
                vec![
                    result("call_2", "list_files", "main.rs"),
                    Part::ToolResult(ToolResult {
                        call_id: "call_1".to_owned(),
                        name: "list_files".to_owned(),
                        output: ToolOutput::Json(r#"{"entries":["a.txt"]}"#.to_owned()),
                    }),
                ],
            ),
            Message {
                role: Role::User,
                parts: vec![Part::Text("thanks".to_owned())],
                cache: CacheHint::NoCache,
            },
        ],
        tools: vec![
            tool("list_files"),
            ToolDefinition {
                name: "perplexity_search".to_owned(),
                description: "Search the web.".to_owned(),
                input_schema_json: r#"{"type":"object"}"#.to_owned(),
                execution: ToolExecution::Provider,
                provider_args_json: Some(r#"{"maxResults":10}"#.to_owned()),
            },
        ],
        tool_choice: ToolChoice::Named("list_files".to_owned()),
        options: GenerationOptions {
            max_output_tokens: Some(4096),
            temperature_milli: Some(700),
            reasoning: ReasoningEffort::Named("high".to_owned()),
            speed: Speed::Fast,
            response_format: Some(ResponseFormat {
                name: "answer".to_owned(),
                description: "Structured answer".to_owned(),
                schema_json: r#"{"type":"object","properties":{"ok":{"type":"boolean"}}}"#
                    .to_owned(),
            }),
        },
        correlation: Correlation {
            request_id: RequestId([
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff,
            ]),
            affinity_key: Some("1792123056696-abc".to_owned()),
        },
        budget: Some(AttemptBudget(Duration::from_millis(30_000))),
    }
}

/// Encodes a JSON value literal (as text) for decoding tests.
pub fn bytes(json: &str) -> Vec<u8> {
    json.as_bytes().to_vec()
}

/// A minimal valid request document, for mutation in decoding tests.
pub const MINIMAL_DOC: &str = r#"{"v":1,"model":"example/model-1","messages":[{"role":"user","parts":[{"type":"text","text":"say hi"}]}],"correlation":{"request_id":"07070707070707070707070707070707"}}"#;
