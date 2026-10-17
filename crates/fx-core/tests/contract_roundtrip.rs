//! Wire round trips and deterministic decoding failures.
//!
//! Test names are referenced from docs/rebuild/BEHAVIOR_MATRIX.md §8.

mod common;

use std::time::Duration;

use common::*;
use fx_core::*;

fn decode_kind(doc: &str) -> DecodeErrorKind {
    decode_request(doc.as_bytes(), &Limits::DEFAULT)
        .expect_err("document must be rejected")
        .kind
}

fn with(doc: &str, from: &str, to: &str) -> String {
    assert!(doc.contains(from), "mutation anchor missing: {from}");
    doc.replacen(from, to, 1)
}

// ---------------------------------------------------------------------------
// Round trips

#[test]
fn request_full_roundtrip_preserves_every_field() {
    let request = full();
    let encoded = encode_request(&request);
    let decoded = decode_request(&encoded, &Limits::DEFAULT).unwrap();
    assert_eq!(decoded, request);
    validate_request(&decoded, &Limits::DEFAULT).unwrap();
}

#[test]
fn request_encoding_is_canonical_and_stable() {
    let first = encode_request(&full());
    let second = encode_request(&decode_request(&first, &Limits::DEFAULT).unwrap());
    assert_eq!(first, second);
}

#[test]
fn request_minimal_document_uses_documented_defaults() {
    let request = decode_request(MINIMAL_DOC.as_bytes(), &Limits::DEFAULT).unwrap();
    assert_eq!(request, minimal());
    assert_eq!(request.tool_choice, ToolChoice::Auto);
    assert_eq!(request.options, GenerationOptions::default());
    assert_eq!(request.messages[0].cache, CacheHint::Default);
    assert_eq!(request.budget, None);
}

#[test]
fn request_every_image_media_type_roundtrips() {
    for (media_type, bytes) in [
        (ImageMediaType::Png, PNG),
        (ImageMediaType::Jpeg, JPEG),
        (ImageMediaType::Gif, GIF),
        (ImageMediaType::Webp, WEBP),
    ] {
        let mut request = minimal();
        request.messages[0].parts.push(Part::Image(ImageData {
            media_type,
            bytes: bytes.to_vec(),
        }));
        let decoded = decode_request(&encode_request(&request), &Limits::DEFAULT).unwrap();
        assert_eq!(decoded, request);
        validate_request(&decoded, &Limits::DEFAULT).unwrap();
    }
}

#[test]
fn request_every_tool_choice_roundtrips() {
    for choice in [
        ToolChoice::Auto,
        ToolChoice::None,
        ToolChoice::Required,
        ToolChoice::Named("list_files".to_owned()),
    ] {
        let mut request = minimal();
        request.tools.push(tool("list_files"));
        request.tool_choice = choice;
        assert_eq!(
            decode_request(&encode_request(&request), &Limits::DEFAULT).unwrap(),
            request
        );
    }
}

#[test]
fn request_budget_is_a_portable_duration_in_milliseconds() {
    let mut request = minimal();
    request.budget = Some(AttemptBudget(Duration::from_millis(1500)));
    let text = String::from_utf8(encode_request(&request)).unwrap();
    assert!(text.contains(r#""budget_ms":1500"#));
    assert_eq!(
        decode_request(text.as_bytes(), &Limits::DEFAULT)
            .unwrap()
            .budget,
        request.budget
    );
}

fn all_events() -> Vec<Event> {
    vec![
        Event::Metadata(Metadata {
            model_reported: Some("example/model-1".to_owned()),
            generation_id: Some("gen_01".to_owned()),
            provider_timestamp_ms: Some(1_792_123_056_696),
        }),
        Event::Metadata(Metadata::default()),
        Event::TextDelta("hello".to_owned()),
        Event::ReasoningDelta("thinking".to_owned()),
        Event::ToolInputStart {
            id: "call_1".to_owned(),
            name: "list_files".to_owned(),
        },
        Event::ToolInputDelta {
            id: "call_1".to_owned(),
            delta: r#"{"path":"#.to_owned(),
        },
        Event::ToolInputEnd {
            id: "call_1".to_owned(),
        },
        Event::ToolCall(ToolCallProposal {
            id: "call_1".to_owned(),
            name: "list_files".to_owned(),
            arguments_json: r#"{"path":"."}"#.to_owned(),
        }),
        Event::ProviderToolRecord(ProviderToolRecord {
            call_id: "ws_1".to_owned(),
            name: "perplexity_search".to_owned(),
            input_json: r#"{"query":"zig"}"#.to_owned(),
            result_json: Some(r#"{"results":[]}"#.to_owned()),
            is_final: true,
        }),
        Event::Usage(Usage {
            input_tokens: Some(12),
            output_tokens: Some(3),
            cache_read_tokens: Some(1),
            cache_write_tokens: Some(2),
            reasoning_tokens: Some(4),
            cost: Some("0.000123".to_owned()),
        }),
        Event::Usage(Usage::default()),
        Event::Finish {
            reason: FinishReason::Stop,
        },
        Event::Finish {
            reason: FinishReason::Length,
        },
        Event::Finish {
            reason: FinishReason::ContentFilter,
        },
        Event::Finish {
            reason: FinishReason::ToolCalls,
        },
        Event::Finish {
            reason: FinishReason::ProviderError,
        },
        Event::Finish {
            reason: FinishReason::Other,
        },
    ]
}

#[test]
fn event_every_variant_roundtrips() {
    for event in all_events() {
        let encoded = encode_event(&event);
        assert_eq!(
            decode_event(&encoded, &Limits::DEFAULT).unwrap(),
            event,
            "{}",
            String::from_utf8_lossy(&encoded)
        );
    }
}

#[test]
fn event_proposal_with_malformed_arguments_still_decodes() {
    // Normalization of malformed arguments is executor work (R2), not decoding.
    let event = Event::ToolCall(ToolCallProposal {
        id: String::new(),
        name: "list_files".to_owned(),
        arguments_json: "{not json".to_owned(),
    });
    assert_eq!(
        decode_event(&encode_event(&event), &Limits::DEFAULT).unwrap(),
        event
    );
}

fn all_outcomes() -> Vec<Outcome> {
    let partial = Some(PartialOutput {
        content: "partial text".to_owned(),
        content_truncated: false,
    });
    let mut outcomes = vec![Outcome::Completed(Completion {
        content: "done".to_owned(),
        content_truncated: true,
        reasoning_bytes: 42,
        proposals: vec![ToolCallProposal {
            id: "call_1".to_owned(),
            name: "list_files".to_owned(),
            arguments_json: "{}".to_owned(),
        }],
        provider_records: vec![ProviderToolRecord {
            call_id: "ws_1".to_owned(),
            name: "perplexity_search".to_owned(),
            input_json: "{}".to_owned(),
            result_json: None,
            is_final: false,
        }],
        finish_reason: FinishReason::ToolCalls,
        usage: Some(Usage {
            input_tokens: Some(1),
            ..Usage::default()
        }),
        metadata: Metadata {
            model_reported: Some("example/model-1".to_owned()),
            ..Metadata::default()
        },
    })];
    let kinds = [
        ErrorKind::InvalidRequest,
        ErrorKind::CapabilityUnavailable,
        ErrorKind::Unauthenticated,
        ErrorKind::Forbidden,
        ErrorKind::ModelUnavailable,
        ErrorKind::RateLimited,
        ErrorKind::ProviderUnavailable,
        ErrorKind::Transport,
        ErrorKind::SystemResumed,
        ErrorKind::Timeout,
        ErrorKind::Cancelled,
        ErrorKind::ProtocolViolation,
        ErrorKind::ResourceLimit,
        ErrorKind::Internal,
    ];
    for (i, kind) in kinds.into_iter().enumerate() {
        outcomes.push(Outcome::Failed(Failure {
            origin: if i % 2 == 0 {
                FailureOrigin::Executor
            } else {
                FailureOrigin::Capability
            },
            kind,
            delivery: [
                DeliveryCertainty::DefinitelyUnsent,
                DeliveryCertainty::PossiblySent,
                DeliveryCertainty::ConfirmedResponse,
            ][i % 3],
            retry_after: (kind == ErrorKind::RateLimited).then_some(Duration::from_secs(5)),
            status_hint: (kind == ErrorKind::RateLimited).then_some(429),
            detail: Some(Detail::new("slow down", &Limits::DEFAULT)),
            partial: if i % 2 == 0 { partial.clone() } else { None },
        }));
    }
    for remote in [
        RemoteCancellation::NotStarted,
        RemoteCancellation::Unconfirmed,
        RemoteCancellation::Confirmed,
    ] {
        outcomes.push(Outcome::Cancelled(Cancellation {
            delivery: DeliveryCertainty::PossiblySent,
            remote,
            partial: partial.clone(),
        }));
    }
    outcomes
}

#[test]
fn outcome_every_variant_and_error_kind_roundtrips() {
    for outcome in all_outcomes() {
        let encoded = encode_outcome(&outcome);
        assert_eq!(decode_outcome(&encoded, &Limits::DEFAULT).unwrap(), outcome);
    }
}

#[test]
fn capability_error_roundtrips() {
    let error = CapabilityError {
        kind: ErrorKind::ProviderUnavailable,
        delivery: DeliveryCertainty::PossiblySent,
        retry_after: Some(Duration::from_millis(250)),
        status_hint: Some(503),
        detail: Some(Detail::new("upstream overloaded", &Limits::DEFAULT)),
    };
    assert_eq!(
        decode_capability_error(&encode_capability_error(&error), &Limits::DEFAULT).unwrap(),
        error
    );
}

// ---------------------------------------------------------------------------
// Versions

#[test]
fn unknown_versions_are_rejected() {
    for v in ["0", "2", "4294967295"] {
        let doc = with(MINIMAL_DOC, r#""v":1"#, &format!(r#""v":{v}"#));
        assert_eq!(
            decode_kind(&doc),
            DecodeErrorKind::UnsupportedVersion,
            "v={v}"
        );
    }
    let event = br#"{"v":2,"event":{"type":"text_delta","text":"x"}}"#;
    assert_eq!(
        decode_event(event, &Limits::DEFAULT).unwrap_err().kind,
        DecodeErrorKind::UnsupportedVersion
    );
    let outcome = br#"{"v":9,"outcome":{"type":"cancelled","delivery":"possibly_sent","remote":"unconfirmed"}}"#;
    assert_eq!(
        decode_outcome(outcome, &Limits::DEFAULT).unwrap_err().kind,
        DecodeErrorKind::UnsupportedVersion
    );
}

#[test]
fn unknown_version_is_reported_before_shape_errors() {
    // A future version may have a different shape; the version is checked first.
    let doc = r#"{"v":2,"entirely":"different"}"#;
    assert_eq!(decode_kind(doc), DecodeErrorKind::UnsupportedVersion);
}

#[test]
fn missing_and_invalid_version_fields_are_rejected() {
    let missing = with(MINIMAL_DOC, r#""v":1,"#, "");
    assert_eq!(decode_kind(&missing), DecodeErrorKind::MissingField);
    let string = with(MINIMAL_DOC, r#""v":1"#, r#""v":"1""#);
    assert_eq!(decode_kind(&string), DecodeErrorKind::InvalidType);
    let negative = with(MINIMAL_DOC, r#""v":1"#, r#""v":-1"#);
    assert_eq!(decode_kind(&negative), DecodeErrorKind::InvalidValue);
    let duplicate = with(MINIMAL_DOC, r#""v":1"#, r#""v":1,"v":1"#);
    assert_eq!(decode_kind(&duplicate), DecodeErrorKind::DuplicateField);
}

// ---------------------------------------------------------------------------
// Tags and enum values

#[test]
fn unknown_part_tags_are_rejected() {
    let doc = with(MINIMAL_DOC, r#""type":"text""#, r#""type":"audio""#);
    assert_eq!(decode_kind(&doc), DecodeErrorKind::UnknownVariant);
}

#[test]
fn invalid_enum_values_are_rejected() {
    for (from, to) in [
        (r#""role":"user""#, r#""role":"robot""#),
        (r#""role":"user""#, r#""role":"User""#),
        (r#""role":"user","#, r#""role":"user","cache":"forever","#),
    ] {
        assert_eq!(
            decode_kind(&with(MINIMAL_DOC, from, to)),
            DecodeErrorKind::UnknownVariant,
            "{to}"
        );
    }
    let speed = with(
        MINIMAL_DOC,
        r#""correlation""#,
        r#""options":{"speed":"ludicrous"},"correlation""#,
    );
    assert_eq!(decode_kind(&speed), DecodeErrorKind::UnknownVariant);
    let media = with(
        MINIMAL_DOC,
        r#"{"type":"text","text":"say hi"}"#,
        r#"{"type":"image","media_type":"image/bmp","data_base64":"AAAA"}"#,
    );
    assert_eq!(decode_kind(&media), DecodeErrorKind::UnknownVariant);
}

#[test]
fn unknown_event_and_outcome_tags_are_rejected() {
    for doc in [
        r#"{"v":1,"event":{"type":"audio_delta","data":"x"}}"#,
        r#"{"v":1,"event":{"type":"finish","reason":"done"}}"#,
        r#"{"v":1,"event":{"type":"finish","reason":"content-filter"}}"#,
    ] {
        assert_eq!(
            decode_event(doc.as_bytes(), &Limits::DEFAULT)
                .unwrap_err()
                .kind,
            DecodeErrorKind::UnknownVariant,
            "{doc}"
        );
    }
    for doc in [
        r#"{"v":1,"outcome":{"type":"interrupted"}}"#,
        r#"{"v":1,"outcome":{"type":"failed","origin":"executor","kind":"teapot","delivery":"possibly_sent"}}"#,
        r#"{"v":1,"outcome":{"type":"failed","origin":"executor","kind":"timeout","delivery":"maybe"}}"#,
    ] {
        assert_eq!(
            decode_outcome(doc.as_bytes(), &Limits::DEFAULT)
                .unwrap_err()
                .kind,
            DecodeErrorKind::UnknownVariant,
            "{doc}"
        );
    }
}

#[test]
fn missing_tags_are_rejected() {
    let doc = with(MINIMAL_DOC, r#""type":"text","#, "");
    assert_eq!(decode_kind(&doc), DecodeErrorKind::MissingField);
}

// ---------------------------------------------------------------------------
// Fields

#[test]
fn unknown_fields_are_rejected_at_every_level() {
    for (from, to) in [
        (r#""v":1,"#, r#""v":1,"extra":true,"#),
        (r#""role":"user","#, r#""role":"user","name":"x","#),
        (r#""text":"say hi""#, r#""text":"say hi","lang":"en""#),
        (r#""request_id""#, r#""trace_id":"x","request_id""#),
    ] {
        assert_eq!(
            decode_kind(&with(MINIMAL_DOC, from, to)),
            DecodeErrorKind::UnknownField,
            "{to}"
        );
    }
}

#[test]
fn unknown_fields_on_empty_tagged_variants_are_rejected() {
    let doc = with(
        MINIMAL_DOC,
        r#""correlation""#,
        r#""tool_choice":{"type":"auto","force":true},"correlation""#,
    );
    assert_eq!(decode_kind(&doc), DecodeErrorKind::UnknownField);
}

#[test]
fn duplicate_fields_are_rejected() {
    for (from, to) in [
        (r#""model":"example/model-1""#, r#""model":"a","model":"b""#),
        (r#""type":"text""#, r#""type":"text","type":"text""#),
        (r#""text":"say hi""#, r#""text":"a","text":"b""#),
        (r#""role":"user""#, r#""role":"user","role":"system""#),
    ] {
        assert_eq!(
            decode_kind(&with(MINIMAL_DOC, from, to)),
            DecodeErrorKind::DuplicateField,
            "{to}"
        );
    }
}

#[test]
fn missing_required_fields_are_rejected() {
    for (from, to) in [
        (r#""model":"example/model-1","#, ""),
        (
            r#","correlation":{"request_id":"07070707070707070707070707070707"}"#,
            "",
        ),
        (r#","text":"say hi""#, ""),
        (r#""role":"user","#, ""),
    ] {
        assert_eq!(
            decode_kind(&with(MINIMAL_DOC, from, to)),
            DecodeErrorKind::MissingField,
            "removed {from}"
        );
    }
    let event = br#"{"v":1,"event":{"type":"tool_input_start","id":"call_1"}}"#;
    assert_eq!(
        decode_event(event, &Limits::DEFAULT).unwrap_err().kind,
        DecodeErrorKind::MissingField
    );
}

#[test]
fn wrong_json_types_are_rejected() {
    for (from, to) in [
        (r#""model":"example/model-1""#, r#""model":7"#),
        (r#""text":"say hi""#, r#""text":null"#),
        (r#""messages":["#, r#""messages":{"x":["#),
    ] {
        let kind = decode_kind(&with(MINIMAL_DOC, from, to));
        assert!(
            matches!(kind, DecodeErrorKind::InvalidType | DecodeErrorKind::Syntax),
            "{to}: {kind:?}"
        );
    }
    let budget = with(
        MINIMAL_DOC,
        r#""correlation""#,
        r#""budget_ms":"30s","correlation""#,
    );
    assert_eq!(decode_kind(&budget), DecodeErrorKind::InvalidType);
}

#[test]
fn out_of_range_numbers_are_rejected() {
    let tokens = with(
        MINIMAL_DOC,
        r#""correlation""#,
        r#""options":{"max_output_tokens":4294967296},"correlation""#,
    );
    assert_eq!(decode_kind(&tokens), DecodeErrorKind::InvalidValue);
    let budget = with(
        MINIMAL_DOC,
        r#""correlation""#,
        r#""budget_ms":-5,"correlation""#,
    );
    assert_eq!(decode_kind(&budget), DecodeErrorKind::InvalidValue);
}

#[test]
fn request_ids_must_be_32_lowercase_hex_digits() {
    for bad in [
        "0707070707070707070707070707070",
        "070707070707070707070707070707070",
        "0707070707070707070707070707070G",
        "0707070707070707070707070707070A",
        "",
    ] {
        let doc = with(MINIMAL_DOC, "07070707070707070707070707070707", bad);
        assert_eq!(decode_kind(&doc), DecodeErrorKind::InvalidValue, "{bad}");
    }
}

// ---------------------------------------------------------------------------
// Bytes, syntax and size

#[test]
fn invalid_utf8_is_rejected_before_parsing() {
    let mut doc = bytes(MINIMAL_DOC);
    let at = MINIMAL_DOC.find("say hi").unwrap();
    doc[at] = 0xff;
    assert_eq!(
        decode_request(&doc, &Limits::DEFAULT).unwrap_err().kind,
        DecodeErrorKind::InvalidUtf8
    );
    let event = b"{\"v\":1,\"event\":{\"type\":\"text_delta\",\"text\":\"\xc3\x28\"}}";
    assert_eq!(
        decode_event(event, &Limits::DEFAULT).unwrap_err().kind,
        DecodeErrorKind::InvalidUtf8
    );
}

#[test]
fn malformed_json_is_rejected_with_one_exact_kind_per_input() {
    for (doc, expected) in [
        ("", DecodeErrorKind::Syntax),
        ("{", DecodeErrorKind::Syntax),
        (r#"{"v":1,}"#, DecodeErrorKind::Syntax),
        (r#"{'v':1}"#, DecodeErrorKind::Syntax),
        ("nul", DecodeErrorKind::Syntax),
        (r#"{"v":1"#, DecodeErrorKind::Syntax),
        // A non-object root is a structural mismatch, reported by serde as a value error.
        ("[]", DecodeErrorKind::InvalidValue),
        ("[]x", DecodeErrorKind::InvalidValue),
        ("1", DecodeErrorKind::InvalidType),
    ] {
        for _ in 0..3 {
            assert_eq!(decode_kind(doc), expected, "{doc:?}");
        }
    }
}

#[test]
fn trailing_data_is_rejected() {
    for suffix in [" x", "{}", " {\"v\":1}"] {
        let doc = format!("{MINIMAL_DOC}{suffix}");
        assert_eq!(decode_kind(&doc), DecodeErrorKind::TrailingData, "{suffix}");
    }
    // Trailing whitespace is not data.
    let doc = format!("{MINIMAL_DOC} \n");
    assert!(decode_request(doc.as_bytes(), &Limits::DEFAULT).is_ok());
}

#[test]
fn oversized_request_is_rejected_before_parsing_at_the_exact_boundary() {
    let at_limit = Limits {
        max_request_bytes: MINIMAL_DOC.len(),
        ..Limits::DEFAULT
    };
    assert!(decode_request(MINIMAL_DOC.as_bytes(), &at_limit).is_ok());
    let below = Limits {
        max_request_bytes: MINIMAL_DOC.len() - 1,
        ..Limits::DEFAULT
    };
    assert_eq!(
        decode_request(MINIMAL_DOC.as_bytes(), &below)
            .unwrap_err()
            .kind,
        DecodeErrorKind::TooLarge
    );
    // Even invalid bytes are rejected by size first: no parsing happened.
    let garbage = vec![0xffu8; 64];
    let tiny = Limits {
        max_request_bytes: 63,
        ..Limits::DEFAULT
    };
    assert_eq!(
        decode_request(&garbage, &tiny).unwrap_err().kind,
        DecodeErrorKind::TooLarge
    );
}

#[test]
fn oversized_event_and_outcome_documents_are_rejected() {
    let event = encode_event(&Event::TextDelta("x".repeat(100)));
    let limits = Limits {
        max_event_bytes: event.len() - 1,
        ..Limits::DEFAULT
    };
    assert_eq!(
        decode_event(&event, &limits).unwrap_err().kind,
        DecodeErrorKind::TooLarge
    );
    let outcome = encode_outcome(&all_outcomes()[0]);
    let limits = Limits {
        max_event_bytes: outcome.len() - 1,
        ..Limits::DEFAULT
    };
    assert_eq!(
        decode_outcome(&outcome, &limits).unwrap_err().kind,
        DecodeErrorKind::TooLarge
    );
}

#[test]
fn event_field_bounds_are_enforced_on_decode() {
    let limits = Limits::DEFAULT;
    let long_id = "c".repeat(limits.max_tool_call_id_bytes + 1);
    let cases = [
        Event::ToolInputStart {
            id: long_id.clone(),
            name: "list_files".to_owned(),
        },
        Event::ToolCall(ToolCallProposal {
            id: "call_1".to_owned(),
            name: "n".repeat(limits.max_tool_name_bytes + 1),
            arguments_json: "{}".to_owned(),
        }),
        Event::Metadata(Metadata {
            model_reported: Some("m".repeat(limits.max_reported_model_bytes + 1)),
            ..Metadata::default()
        }),
        Event::Usage(Usage {
            cost: Some("1".repeat(limits.max_opaque_token_bytes + 1)),
            ..Usage::default()
        }),
    ];
    for event in cases {
        assert_eq!(
            decode_event(&encode_event(&event), &limits)
                .unwrap_err()
                .kind,
            DecodeErrorKind::LimitExceeded
        );
    }
    let empty = br#"{"v":1,"event":{"type":"tool_input_end","id":""}}"#;
    assert_eq!(
        decode_event(empty, &limits).unwrap_err().kind,
        DecodeErrorKind::InvalidValue
    );
}

#[test]
fn outcome_bounds_are_enforced_on_decode() {
    let limits = Limits {
        max_proposals: 0,
        ..Limits::DEFAULT
    };
    assert_eq!(
        decode_outcome(&encode_outcome(&all_outcomes()[0]), &limits)
            .unwrap_err()
            .kind,
        DecodeErrorKind::LimitExceeded
    );
    let long_detail = format!(
        r#"{{"v":1,"outcome":{{"type":"failed","origin":"capability","kind":"transport","delivery":"possibly_sent","detail":"{}"}}}}"#,
        "e".repeat(Limits::DEFAULT.max_detail_bytes + 1)
    );
    assert_eq!(
        decode_outcome(long_detail.as_bytes(), &Limits::DEFAULT)
            .unwrap_err()
            .kind,
        DecodeErrorKind::LimitExceeded
    );
}

#[test]
fn image_base64_must_be_canonical() {
    for bad in [
        "iVBORw0KGgo",   // missing padding
        "iVBO Rw0KGgo=", // whitespace
        "iVBORw0KGgo_",  // URL-safe alphabet
        "iVBORw0KGgp=",  // non-zero trailing bits
        "iV=ORw0KGgo=",  // padding inside
        "====",
    ] {
        let doc = with(
            MINIMAL_DOC,
            r#"{"type":"text","text":"say hi"}"#,
            &format!(r#"{{"type":"image","media_type":"image/png","data_base64":"{bad}"}}"#),
        );
        assert_eq!(decode_kind(&doc), DecodeErrorKind::InvalidValue, "{bad}");
    }
}

#[test]
fn image_decoded_size_is_bounded_before_allocation() {
    let limits = Limits {
        max_image_bytes: 8,
        ..Limits::DEFAULT
    };
    // 12 base64 characters decode to 9 bytes.
    let doc = with(
        MINIMAL_DOC,
        r#"{"type":"text","text":"say hi"}"#,
        r#"{"type":"image","media_type":"image/png","data_base64":"iVBORw0KGgoA"}"#,
    );
    assert_eq!(
        decode_request(doc.as_bytes(), &limits).unwrap_err().kind,
        DecodeErrorKind::LimitExceeded
    );
}

// ---------------------------------------------------------------------------
// Diagnostics hygiene (invariant 8)

#[test]
fn decode_errors_never_echo_input_content() {
    let secret = "sk-live-0123456789abcdefSECRET";
    let docs = [
        with(
            MINIMAL_DOC,
            r#""role":"user""#,
            &format!(r#""role":"{secret}""#),
        ),
        with(
            MINIMAL_DOC,
            r#""text":"say hi""#,
            &format!(r#""{secret}":1"#),
        ),
        with(
            MINIMAL_DOC,
            r#""type":"text""#,
            &format!(r#""type":"{secret}""#),
        ),
        with(MINIMAL_DOC, "07070707070707070707070707070707", secret),
    ];
    for doc in docs {
        let err = decode_request(doc.as_bytes(), &Limits::DEFAULT).unwrap_err();
        let shown = format!("{err} {err:?}");
        assert!(!shown.contains("SECRET"), "{shown}");
    }
}

#[test]
fn parse_request_decodes_then_validates() {
    assert_eq!(
        parse_request(MINIMAL_DOC.as_bytes(), &Limits::DEFAULT).unwrap(),
        minimal()
    );
    let invalid = with(MINIMAL_DOC, r#""model":"example/model-1""#, r#""model":"""#);
    match parse_request(invalid.as_bytes(), &Limits::DEFAULT) {
        Err(RequestRejection::Invalid(e)) => assert_eq!(e.code, ValidationCode::EmptyModelId),
        other => panic!("expected validation rejection, got {other:?}"),
    }
    match parse_request(b"{", &Limits::DEFAULT) {
        Err(RequestRejection::Decode(e)) => assert_eq!(e.kind, DecodeErrorKind::Syntax),
        other => panic!("expected decode rejection, got {other:?}"),
    }
}
