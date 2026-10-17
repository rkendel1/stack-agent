//! Request validation, limits, delivery certainty and diagnostics hygiene.
//!
//! History-pairing cases mirror the oracle `validateToolMessageHistory`
//! (src/core/gateway/gateway_json.zig:445-501). Test names are referenced from
//! docs/rebuild/BEHAVIOR_MATRIX.md §8.

mod common;

use std::time::Duration;

use common::*;
use fx_core::*;

fn code(request: &Request) -> ValidationCode {
    validate_request(request, &Limits::DEFAULT)
        .expect_err("request must be rejected")
        .code
}

fn code_with(request: &Request, limits: &Limits) -> ValidationCode {
    validate_request(request, limits)
        .expect_err("request must be rejected")
        .code
}

fn ok(request: &Request) {
    validate_request(request, &Limits::DEFAULT).expect("request must be accepted");
}

fn history(messages: Vec<Message>) -> Request {
    let mut request = minimal();
    request.messages = messages;
    request
}

// ---------------------------------------------------------------------------
// Limits are defined once

#[test]
fn limits_defaults_are_pinned() {
    let l = Limits::DEFAULT;
    assert_eq!(Limits::default(), l);
    assert_eq!(l.max_request_bytes, 64 * 1024 * 1024);
    assert_eq!(l.max_event_bytes, 32 * 1024 * 1024); // oracle: client.zig:188
    assert_eq!(l.max_json_depth, 128);
    assert_eq!(l.max_messages, 4096);
    assert_eq!(l.max_parts_per_message, 256);
    assert_eq!(l.max_total_text_bytes, 16 * 1024 * 1024);
    assert_eq!(l.max_image_bytes, 20 * 1024 * 1024); // oracle: image_attachments.zig:11
    assert_eq!(l.max_images, 16);
    assert_eq!(l.max_total_image_bytes, 40 * 1024 * 1024);
    assert_eq!(l.max_tools, 256);
    assert_eq!(l.max_tool_name_bytes, 128);
    assert_eq!(l.max_tool_call_id_bytes, 256);
    assert_eq!(l.max_tool_description_bytes, 64 * 1024);
    assert_eq!(l.max_schema_bytes, 256 * 1024);
    assert_eq!(l.max_tool_arguments_bytes, 1024 * 1024);
    assert_eq!(l.max_tool_result_bytes, 8 * 1024 * 1024);
    assert_eq!(l.max_model_id_bytes, 256);
    assert_eq!(l.max_reasoning_label_bytes, 64); // oracle: types.zig:1565
    assert_eq!(l.max_affinity_key_bytes, 255);
    assert_eq!(l.max_reported_model_bytes, 128);
    assert_eq!(l.max_opaque_token_bytes, 256);
    assert_eq!(l.max_detail_bytes, 600); // oracle: client.zig:184
    assert_eq!(l.max_completion_bytes, 16 * 1024 * 1024);
    assert_eq!(l.max_attempt_budget, Duration::from_secs(86_400));
    assert_eq!(l.max_proposals, 256);
    assert_eq!(l.max_events_per_poll, 256);
    assert_eq!(l.max_events_per_attempt, 1 << 20);
    assert_eq!(l.max_polls_per_attempt, 1 << 20);
}

// ---------------------------------------------------------------------------
// Baseline

#[test]
fn minimal_and_full_requests_are_valid() {
    ok(&minimal());
    ok(&full());
}

#[test]
fn validation_is_deterministic_and_returns_the_first_failure() {
    // Two problems: empty model id (checked first) and an orphan tool result.
    let mut request = history(vec![message(Role::Tool, vec![result("c", "t", "x")])]);
    request.model = ModelRef(String::new());
    for _ in 0..3 {
        assert_eq!(code(&request), ValidationCode::EmptyModelId);
    }
    request.model = ModelRef("m".to_owned());
    assert_eq!(code(&request), ValidationCode::OrphanToolResult);
}

#[test]
fn errors_carry_locations() {
    let request = history(vec![
        text(Role::User, "hi"),
        message(
            Role::User,
            vec![Part::Text("a".into()), call("c", "t", "{}")],
        ),
    ]);
    let err = validate_request(&request, &Limits::DEFAULT).unwrap_err();
    assert_eq!(err.code, ValidationCode::PartNotAllowedForRole);
    assert_eq!(err.location.message, Some(1));
    assert_eq!(err.location.part, Some(1));
}

// ---------------------------------------------------------------------------
// Model reference

#[test]
fn model_id_bounds_and_syntax() {
    let mut request = minimal();
    request.model = ModelRef(String::new());
    assert_eq!(code(&request), ValidationCode::EmptyModelId);
    for bad in [" model", "model ", "mo\ndel", "mo\u{7}del"] {
        request.model = ModelRef(bad.to_owned());
        assert_eq!(code(&request), ValidationCode::InvalidModelId, "{bad:?}");
    }
    request.model = ModelRef("m".repeat(Limits::DEFAULT.max_model_id_bytes));
    ok(&request);
    request.model = ModelRef("m".repeat(Limits::DEFAULT.max_model_id_bytes + 1));
    assert_eq!(code(&request), ValidationCode::ModelIdTooLong);
    // Opaque: slashes, colons and non-ASCII are fine.
    request.model = ModelRef("vendor/model:v2@région".to_owned());
    ok(&request);
}

// ---------------------------------------------------------------------------
// Message and part counts

#[test]
fn message_count_bounds() {
    assert_eq!(code(&history(vec![])), ValidationCode::NoMessages);
    let limits = Limits {
        max_messages: 3,
        ..Limits::DEFAULT
    };
    let three = history(vec![text(Role::User, "a"); 3]);
    validate_request(&three, &limits).unwrap();
    let four = history(vec![text(Role::User, "a"); 4]);
    assert_eq!(code_with(&four, &limits), ValidationCode::TooManyMessages);
}

#[test]
fn part_count_bounds() {
    assert_eq!(
        code(&history(vec![message(Role::User, vec![])])),
        ValidationCode::EmptyMessage
    );
    let limits = Limits {
        max_parts_per_message: 2,
        ..Limits::DEFAULT
    };
    let two = history(vec![message(
        Role::User,
        vec![Part::Text("a".into()), Part::Text("b".into())],
    )]);
    validate_request(&two, &limits).unwrap();
    let three = history(vec![message(
        Role::User,
        vec![
            Part::Text("a".into()),
            Part::Text("b".into()),
            Part::Text("c".into()),
        ],
    )]);
    assert_eq!(code_with(&three, &limits), ValidationCode::TooManyParts);
}

#[test]
fn total_text_bounds() {
    let limits = Limits {
        max_total_text_bytes: 10,
        ..Limits::DEFAULT
    };
    let at = history(vec![text(Role::User, "12345"), text(Role::User, "67890")]);
    validate_request(&at, &limits).unwrap();
    let over = history(vec![text(Role::User, "12345"), text(Role::User, "678901")]);
    assert_eq!(code_with(&over, &limits), ValidationCode::TotalTextTooLarge);
}

// ---------------------------------------------------------------------------
// Role and content combinations

#[test]
fn role_content_matrix() {
    let image = Part::Image(ImageData {
        media_type: ImageMediaType::Png,
        bytes: PNG.to_vec(),
    });
    let parts = [
        ("text", Part::Text("x".into())),
        ("image", image),
        ("tool_call", call("c1", "t", "{}")),
        ("tool_result", result("c1", "t", "x")),
    ];
    let allowed = |role: Role, kind: &str| {
        matches!(
            (role, kind),
            (Role::System, "text")
                | (Role::User, "text" | "image")
                | (Role::Assistant, "text" | "tool_call")
                | (Role::Tool, "tool_result")
        )
    };
    for role in [Role::System, Role::User, Role::Assistant, Role::Tool] {
        for (kind, part) in &parts {
            // Put the part in second position after a part that is legal for the role,
            // so pairing rules do not mask the role check.
            let lead = match role {
                Role::Tool => result("c0", "t", "x"),
                _ => Part::Text("lead".into()),
            };
            let mut messages = vec![];
            if role == Role::Tool {
                messages.push(message(
                    Role::Assistant,
                    vec![call("c0", "t", "{}"), call("c1", "t", "{}")],
                ));
            }
            messages.push(message(role, vec![lead, part.clone()]));
            if role == Role::Assistant && *kind == "tool_call" {
                messages.push(message(Role::Tool, vec![result("c1", "t", "x")]));
            }
            let request = history(messages);
            let outcome = validate_request(&request, &Limits::DEFAULT);
            if allowed(role, kind) {
                assert!(outcome.is_ok(), "{role:?}/{kind}: {outcome:?}");
            } else {
                assert_eq!(
                    outcome.unwrap_err().code,
                    ValidationCode::PartNotAllowedForRole,
                    "{role:?}/{kind}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// History pairing (oracle: validateToolMessageHistory)

#[test]
fn history_orphan_tool_result_is_rejected() {
    let request = history(vec![
        text(Role::User, "hi"),
        message(Role::Tool, vec![result("c1", "t", "x")]),
    ]);
    assert_eq!(code(&request), ValidationCode::OrphanToolResult);
}

#[test]
fn history_missing_result_at_end_is_rejected() {
    let request = history(vec![
        text(Role::User, "hi"),
        message(Role::Assistant, vec![call("c1", "t", "{}")]),
    ]);
    let err = validate_request(&request, &Limits::DEFAULT).unwrap_err();
    assert_eq!(err.code, ValidationCode::MissingToolResult);
    assert_eq!(err.location.message, Some(1));
}

#[test]
fn history_missing_result_before_next_message_is_rejected() {
    let request = history(vec![
        message(
            Role::Assistant,
            vec![call("c1", "t", "{}"), call("c2", "t", "{}")],
        ),
        message(Role::Tool, vec![result("c1", "t", "x")]),
        text(Role::User, "next"),
    ]);
    assert_eq!(code(&request), ValidationCode::MissingToolResult);
}

#[test]
fn history_unmatched_result_is_rejected() {
    let request = history(vec![
        message(Role::Assistant, vec![call("c1", "t", "{}")]),
        message(Role::Tool, vec![result("c9", "t", "x")]),
    ]);
    assert_eq!(code(&request), ValidationCode::UnmatchedToolResult);
}

#[test]
fn history_duplicate_result_is_rejected() {
    let request = history(vec![
        message(
            Role::Assistant,
            vec![call("c1", "t", "{}"), call("c2", "t", "{}")],
        ),
        message(
            Role::Tool,
            vec![result("c1", "t", "x"), result("c1", "t", "y")],
        ),
    ]);
    assert_eq!(code(&request), ValidationCode::DuplicateToolResult);
}

#[test]
fn history_result_name_must_match_the_proposal() {
    let request = history(vec![
        message(Role::Assistant, vec![call("c1", "read_file", "{}")]),
        message(Role::Tool, vec![result("c1", "delete_file", "x")]),
    ]);
    assert_eq!(code(&request), ValidationCode::ToolResultNameMismatch);
}

#[test]
fn history_duplicate_proposal_ids_are_rejected() {
    let request = history(vec![message(
        Role::Assistant,
        vec![call("c1", "t", "{}"), call("c1", "t", "{}")],
    )]);
    assert_eq!(code(&request), ValidationCode::DuplicateToolCallId);
}

#[test]
fn history_results_may_arrive_in_any_order_across_tool_messages() {
    ok(&history(vec![
        message(
            Role::Assistant,
            vec![
                call("c1", "t", "{}"),
                call("c2", "t", "{}"),
                call("c3", "t", "{}"),
            ],
        ),
        message(Role::Tool, vec![result("c3", "t", "x")]),
        message(
            Role::Tool,
            vec![result("c1", "t", "x"), result("c2", "t", "x")],
        ),
        text(Role::User, "next"),
    ]));
}

#[test]
fn history_extra_tool_message_after_all_results_is_an_orphan() {
    let request = history(vec![
        message(Role::Assistant, vec![call("c1", "t", "{}")]),
        message(Role::Tool, vec![result("c1", "t", "x")]),
        message(Role::Tool, vec![result("c1", "t", "x")]),
    ]);
    assert_eq!(code(&request), ValidationCode::OrphanToolResult);
}

#[test]
fn history_new_proposals_while_results_pending_are_rejected() {
    let request = history(vec![
        message(Role::Assistant, vec![call("c1", "t", "{}")]),
        message(Role::Assistant, vec![call("c2", "t", "{}")]),
    ]);
    assert_eq!(code(&request), ValidationCode::MissingToolResult);
}

#[test]
fn history_multiple_rounds_are_valid() {
    ok(&history(vec![
        text(Role::User, "go"),
        message(Role::Assistant, vec![call("c1", "t", "{}")]),
        message(Role::Tool, vec![result("c1", "t", "x")]),
        message(Role::Assistant, vec![call("c1", "t", "{}")]),
        message(Role::Tool, vec![result("c1", "t", "y")]),
        text(Role::Assistant, "done"),
    ]));
}

// ---------------------------------------------------------------------------
// Tool-call ids, names and arguments

fn single_call(id: &str, name: &str, args: &str) -> Request {
    history(vec![
        message(Role::Assistant, vec![call(id, name, args)]),
        message(Role::Tool, vec![result(id, name, "x")]),
    ])
}

#[test]
fn tool_call_id_syntax_and_bounds() {
    for bad in ["", "call 1", "call\t1", "call\u{e9}"] {
        assert_eq!(
            code(&single_call(bad, "t", "{}")),
            ValidationCode::InvalidToolCallId,
            "{bad:?}"
        );
    }
    let max = Limits::DEFAULT.max_tool_call_id_bytes;
    ok(&single_call(&"c".repeat(max), "t", "{}"));
    assert_eq!(
        code(&single_call(&"c".repeat(max + 1), "t", "{}")),
        ValidationCode::InvalidToolCallId
    );
    ok(&single_call("toolu_01A:b/c=", "t", "{}"));
}

#[test]
fn tool_name_syntax_and_bounds() {
    for bad in ["", "list files", "list/files", "läs"] {
        assert_eq!(
            code(&single_call("c1", bad, "{}")),
            ValidationCode::InvalidToolName,
            "{bad:?}"
        );
    }
    let max = Limits::DEFAULT.max_tool_name_bytes;
    ok(&single_call("c1", &"n".repeat(max), "{}"));
    assert_eq!(
        code(&single_call("c1", &"n".repeat(max + 1), "{}")),
        ValidationCode::InvalidToolName
    );
    ok(&single_call("c1", "mcp__server.tool-name_2", "{}"));
}

#[test]
fn tool_arguments_must_be_valid_json_without_duplicate_keys() {
    for bad in [
        "",
        "{",
        r#"{"path":"."}x"#,
        r#"{"a":1,"a":2}"#,
        r#"{"a":1,"\u0061":2}"#,
        r#"{"a":[1,]}"#,
        "\"\\ud800\"",
        "01",
        "NaN",
    ] {
        assert_eq!(
            code(&single_call("c1", "t", bad)),
            ValidationCode::MalformedToolArguments,
            "{bad:?}"
        );
    }
    // The oracle accepts any JSON value, scalar roots included.
    for good in [
        "{}",
        "[]",
        "\"text\"",
        "-1.5e3",
        "true",
        "null",
        "\"\\ud83d\\ude00\"",
        r#"{"a":{"a":1}}"#,
    ] {
        ok(&single_call("c1", "t", good));
    }
}

#[test]
fn tool_arguments_depth_and_size_bounds() {
    let limits = Limits {
        max_json_depth: 3,
        ..Limits::DEFAULT
    };
    validate_request(&single_call("c1", "t", "[[[1]]]"), &limits).unwrap();
    assert_eq!(
        code_with(&single_call("c1", "t", "[[[[1]]]]"), &limits),
        ValidationCode::MalformedToolArguments
    );
    let limits = Limits {
        max_tool_arguments_bytes: 8,
        ..Limits::DEFAULT
    };
    validate_request(&single_call("c1", "t", r#"{"a":12}"#), &limits).unwrap();
    assert_eq!(
        code_with(&single_call("c1", "t", r#"{"a":123}"#), &limits),
        ValidationCode::ToolArgumentsTooLarge
    );
}

#[test]
fn tool_result_bounds_and_json_output() {
    let mut request = single_call("c1", "t", "{}");
    request.messages[1].parts[0] = Part::ToolResult(ToolResult {
        call_id: "c1".into(),
        name: "t".into(),
        output: ToolOutput::Json("{bad".into()),
    });
    assert_eq!(code(&request), ValidationCode::MalformedToolResultJson);
    let limits = Limits {
        max_tool_result_bytes: 4,
        ..Limits::DEFAULT
    };
    request.messages[1].parts[0] = result("c1", "t", "abcd");
    validate_request(&request, &limits).unwrap();
    request.messages[1].parts[0] = Part::ToolResult(ToolResult {
        call_id: "c1".into(),
        name: "t".into(),
        output: ToolOutput::Failure("abcde".into()),
    });
    assert_eq!(
        code_with(&request, &limits),
        ValidationCode::ToolResultTooLarge
    );
}

// ---------------------------------------------------------------------------
// Images

fn with_images(images: Vec<ImageData>) -> Request {
    let mut parts = vec![Part::Text("look".into())];
    parts.extend(images.into_iter().map(Part::Image));
    history(vec![message(Role::User, parts)])
}

fn png(len: usize) -> ImageData {
    let mut bytes = PNG.to_vec();
    bytes.resize(len.max(PNG.len()), 0);
    ImageData {
        media_type: ImageMediaType::Png,
        bytes,
    }
}

#[test]
fn image_media_type_must_match_the_bytes() {
    for (declared, bytes) in [
        (ImageMediaType::Png, PNG),
        (ImageMediaType::Jpeg, JPEG),
        (ImageMediaType::Gif, GIF),
        (ImageMediaType::Webp, WEBP),
    ] {
        ok(&with_images(vec![ImageData {
            media_type: declared,
            bytes: bytes.to_vec(),
        }]));
    }
    assert_eq!(
        code(&with_images(vec![ImageData {
            media_type: ImageMediaType::Jpeg,
            bytes: PNG.to_vec(),
        }])),
        ValidationCode::ImageMediaTypeMismatch
    );
    assert_eq!(
        code(&with_images(vec![ImageData {
            media_type: ImageMediaType::Png,
            bytes: b"<svg></svg>".to_vec(),
        }])),
        ValidationCode::ImageMediaTypeMismatch
    );
    assert_eq!(
        code(&with_images(vec![ImageData {
            media_type: ImageMediaType::Png,
            bytes: vec![],
        }])),
        ValidationCode::ImageEmpty
    );
    assert_eq!(ImageMediaType::parse("image/svg+xml"), None);
    assert_eq!(
        ImageMediaType::parse("image/png"),
        Some(ImageMediaType::Png)
    );
}

#[test]
fn image_size_count_and_total_bounds() {
    let limits = Limits {
        max_image_bytes: 32,
        max_images: 2,
        max_total_image_bytes: 48,
        ..Limits::DEFAULT
    };
    validate_request(&with_images(vec![png(32)]), &limits).unwrap();
    assert_eq!(
        code_with(&with_images(vec![png(33)]), &limits),
        ValidationCode::ImageTooLarge
    );
    validate_request(&with_images(vec![png(24), png(24)]), &limits).unwrap();
    assert_eq!(
        code_with(&with_images(vec![png(24), png(25)]), &limits),
        ValidationCode::TotalImageBytesTooLarge
    );
    assert_eq!(
        code_with(&with_images(vec![png(16), png(16), png(16)]), &limits),
        ValidationCode::TooManyImages
    );
}

// ---------------------------------------------------------------------------
// Tool definitions and tool choice

#[test]
fn tool_definitions_are_validated() {
    let mut request = minimal();
    request.tools = vec![tool("list_files"), tool("list_files")];
    assert_eq!(code(&request), ValidationCode::DuplicateToolName);

    request.tools = vec![tool("bad name")];
    let err = validate_request(&request, &Limits::DEFAULT).unwrap_err();
    assert_eq!(err.code, ValidationCode::InvalidToolName);
    assert_eq!(err.location.tool, Some(0));

    for bad_schema in ["[]", "true", "{", r#"{"type":"object","type":"array"}"#, ""] {
        let mut t = tool("t");
        t.input_schema_json = bad_schema.into();
        request.tools = vec![t];
        assert_eq!(
            code(&request),
            ValidationCode::InvalidToolSchema,
            "{bad_schema:?}"
        );
    }

    let mut client_with_args = tool("t");
    client_with_args.provider_args_json = Some("{}".into());
    request.tools = vec![client_with_args];
    assert_eq!(code(&request), ValidationCode::ProviderArgsOnClientTool);

    let mut provider = tool("search");
    provider.execution = ToolExecution::Provider;
    request.tools = vec![provider.clone()];
    ok(&request);
    provider.provider_args_json = Some("[1]".into());
    request.tools = vec![provider];
    assert_eq!(code(&request), ValidationCode::InvalidProviderArgs);
}

#[test]
fn tool_definition_bounds() {
    let limits = Limits {
        max_tools: 2,
        max_tool_description_bytes: 4,
        max_schema_bytes: 20,
        ..Limits::DEFAULT
    };
    let mut t = tool("a");
    t.description = "abcd".into();
    t.input_schema_json = r#"{"type":"object"}"#.into();
    let mut request = minimal();
    request.tools = vec![t.clone()];
    validate_request(&request, &limits).unwrap();

    let mut long = t.clone();
    long.description = "abcde".into();
    request.tools = vec![long];
    assert_eq!(
        code_with(&request, &limits),
        ValidationCode::ToolDescriptionTooLarge
    );

    let mut big = t.clone();
    big.input_schema_json = r#"{"type":"object","x":1}"#.into();
    request.tools = vec![big];
    assert_eq!(code_with(&request, &limits), ValidationCode::SchemaTooLarge);

    let mut b = t.clone();
    b.name = "b".into();
    let mut c = t.clone();
    c.name = "c".into();
    request.tools = vec![t, b, c];
    assert_eq!(code_with(&request, &limits), ValidationCode::TooManyTools);
}

#[test]
fn tool_choice_must_reference_defined_tools() {
    let mut request = minimal();
    request.tool_choice = ToolChoice::Required;
    assert_eq!(code(&request), ValidationCode::ToolChoiceWithoutTools);
    request.tool_choice = ToolChoice::Named("t".into());
    assert_eq!(code(&request), ValidationCode::ToolChoiceWithoutTools);
    request.tools = vec![tool("t")];
    ok(&request);
    request.tool_choice = ToolChoice::Named("u".into());
    assert_eq!(code(&request), ValidationCode::UnknownNamedTool);
    request.tool_choice = ToolChoice::None;
    ok(&request);
}

// ---------------------------------------------------------------------------
// Options, correlation and budget

#[test]
fn generation_options_are_validated() {
    let mut request = minimal();
    request.options.max_output_tokens = Some(0);
    assert_eq!(code(&request), ValidationCode::InvalidMaxOutputTokens);
    request.options.max_output_tokens = Some(1);
    ok(&request);

    request.options.temperature_milli = Some(GenerationOptions::MAX_TEMPERATURE_MILLI + 1);
    assert_eq!(code(&request), ValidationCode::InvalidTemperature);
    for fine in [0, 1, 700, GenerationOptions::MAX_TEMPERATURE_MILLI] {
        request.options.temperature_milli = Some(fine);
        ok(&request);
    }
    request.options.temperature_milli = None;

    let max = Limits::DEFAULT.max_reasoning_label_bytes;
    request.options.reasoning = ReasoningEffort::Named("x".repeat(max));
    ok(&request);
    for bad in [String::new(), "x".repeat(max + 1), "very high".into()] {
        request.options.reasoning = ReasoningEffort::Named(bad);
        assert_eq!(code(&request), ValidationCode::InvalidReasoningLabel);
    }
    request.options.reasoning = ReasoningEffort::Default;

    request.options.response_format = Some(ResponseFormat {
        name: "bad name".into(),
        description: String::new(),
        schema_json: "{}".into(),
    });
    assert_eq!(code(&request), ValidationCode::InvalidResponseFormatName);
    request.options.response_format = Some(ResponseFormat {
        name: "ok".into(),
        description: String::new(),
        schema_json: "[]".into(),
    });
    assert_eq!(code(&request), ValidationCode::InvalidResponseFormatSchema);
    request.options.response_format = Some(ResponseFormat {
        name: "ok".into(),
        description: "d".repeat(Limits::DEFAULT.max_tool_description_bytes + 1),
        schema_json: "{}".into(),
    });
    assert_eq!(
        code(&request),
        ValidationCode::ResponseFormatDescriptionTooLarge
    );
}

#[test]
fn affinity_key_bounds_and_syntax() {
    let mut request = minimal();
    let max = Limits::DEFAULT.max_affinity_key_bytes;
    request.correlation.affinity_key = Some("k".repeat(max));
    ok(&request);
    for bad in [String::new(), "k".repeat(max + 1), "has space".into()] {
        request.correlation.affinity_key = Some(bad);
        assert_eq!(code(&request), ValidationCode::InvalidAffinityKey);
    }
}

#[test]
fn attempt_budget_bounds() {
    let mut request = minimal();
    request.budget = Some(AttemptBudget(Duration::ZERO));
    assert_eq!(code(&request), ValidationCode::BudgetOutOfRange);
    request.budget = Some(AttemptBudget(Duration::from_millis(1)));
    ok(&request);
    request.budget = Some(AttemptBudget(Limits::DEFAULT.max_attempt_budget));
    ok(&request);
    request.budget = Some(AttemptBudget(
        Limits::DEFAULT.max_attempt_budget + Duration::from_millis(1),
    ));
    assert_eq!(code(&request), ValidationCode::BudgetOutOfRange);
}

// ---------------------------------------------------------------------------
// Delivery certainty and cancellation (invariants 3 and 4)

#[test]
fn delivery_certainty_is_monotonic() {
    use DeliveryCertainty::*;
    let states = [DefinitelyUnsent, PossiblySent, ConfirmedResponse];
    for (i, &current) in states.iter().enumerate() {
        for (j, &observed) in states.iter().enumerate() {
            let next = current.advance(observed);
            assert_eq!(next, states[i.max(j)], "{current:?} + {observed:?}");
            assert!(next >= current);
        }
    }
    assert_eq!(DeliveryCertainty::default(), DefinitelyUnsent);
}

#[test]
fn cancellation_never_implies_non_execution() {
    assert!(!DeliveryCertainty::DefinitelyUnsent.may_have_executed());
    assert!(DeliveryCertainty::PossiblySent.may_have_executed());
    assert!(DeliveryCertainty::ConfirmedResponse.may_have_executed());
    // Even a remotely confirmed cancellation after possible delivery may have executed.
    let cancelled = Cancellation {
        delivery: DeliveryCertainty::PossiblySent,
        remote: RemoteCancellation::Confirmed,
        partial: None,
    };
    assert!(cancelled.delivery.may_have_executed());
}

// ---------------------------------------------------------------------------
// Diagnostics hygiene (invariant 8)

#[test]
fn validation_errors_never_contain_request_content() {
    let secret = "sk-SECRET-0123456789abcdef";
    let mut request = history(vec![
        text(Role::User, secret),
        message(
            Role::Assistant,
            vec![call("c1", "t", &format!(r#"{{"k":"{secret}""#))],
        ),
        message(Role::Tool, vec![result("c1", "t", secret)]),
    ]);
    request.model = ModelRef(format!(" {secret}"));
    for _ in 0..2 {
        let err = validate_request(&request, &Limits::DEFAULT).unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("SECRET"), "{shown}");
        request.model = ModelRef("m".into());
    }
}

#[test]
fn detail_is_bounded_and_masks_secrets() {
    let limits = Limits::DEFAULT;
    let detail = Detail::new(
        "auth failed: Bearer abcdefghijklmnop0123 and sk-0123456789abcdefghij",
        &limits,
    );
    assert!(!detail.as_str().contains("abcdefghijklmnop0123"));
    assert!(!detail.as_str().contains("sk-0123456789abcdefghij"));
    assert!(detail.as_str().contains("[redacted]"));

    for raw in [
        "API_KEY=supersecretvalue",
        "https://user:hunter2@example.com/x",
        "key AKIAABCDEFGHIJKLMNOP end",
        "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB",
    ] {
        let masked = Detail::new(raw, &limits);
        assert!(
            masked.as_str().contains("[redacted]"),
            "{raw} -> {masked:?}"
        );
    }
    // Short, non-secret text is unchanged.
    assert_eq!(
        Detail::new("rate limited", &limits).as_str(),
        "rate limited"
    );

    let long = "é".repeat(limits.max_detail_bytes);
    let bounded = Detail::new(&long, &limits);
    assert!(bounded.as_str().len() <= limits.max_detail_bytes);
    assert!(bounded.as_str().chars().all(|c| c == 'é'));
}

#[test]
fn event_bounds_do_not_reject_malformed_proposal_arguments() {
    // Malformed model output is normalized by the executor (R2), not rejected here.
    let event = Event::ToolCall(ToolCallProposal {
        id: "call_1".into(),
        name: "t".into(),
        arguments_json: "{oops".into(),
    });
    assert!(validate_event_bounds(&event, &Limits::DEFAULT).is_ok());
}
