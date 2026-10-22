//! Decision and classification validation, and packet bounds.

use chip_symbol_loop::contract::{
    Acceptance, ClassifyPacket, Constraints, Decision, Neighbor, Op, PACKET_VERSION, PacketError,
    PacketLimits, ReplyError, SymbolRef, TaskPacket, validate_classification, validate_decision,
};
use chip_symbol_loop::edit::{EditError, prepare_insert};

fn packet() -> TaskPacket {
    TaskPacket {
        version: PACKET_VERSION.into(),
        goal: "Make the retry policy allow five retries.".into(),
        acceptance: Acceptance {
            command: vec!["cargo".into(), "test".into()],
            exit_status: Some(101),
            failure_excerpt: "assertion `left == right` failed".into(),
        },
        symbol: SymbolRef {
            id: "symbol:src/lib.rs:max_retries".into(),
            path: "src/lib.rs".into(),
            name: "max_retries".into(),
            start_line: 2,
            end_line: 4,
            fingerprint: "sha256:00".into(),
            source: "pub fn max_retries() -> u32 {\n    3\n}".into(),
        },
        neighbors: vec![Neighbor {
            id: "symbol:src/lib.rs:backoff_ms".into(),
            signature: "pub fn backoff_ms(attempt: u32) -> u64".into(),
        }],
        rejected: vec![],
        constraints: Constraints {
            signature_frozen: true,
            allowed_ops: vec![Op::ReplaceSymbolBody, Op::NoChange, Op::NeedSymbol],
            insertion_after: None,
            max_body_bytes: 64,
            context_requests_left: 1,
        },
        context: vec![],
    }
}

/// A reply and the rule its refusal must name.
type Case = (&'static str, fn(&ReplyError) -> bool);

fn limits() -> PacketLimits {
    PacketLimits::default()
}

#[test]
fn a_valid_decision_for_the_authorized_symbol_is_accepted() {
    let reply =
        r#"{"op":"replace_symbol_body","symbol":"symbol:src/lib.rs:max_retries","body":"{ 5 }"}"#;
    assert_eq!(
        validate_decision(reply, &packet(), &limits()).unwrap(),
        Decision::ReplaceSymbolBody {
            symbol: "symbol:src/lib.rs:max_retries".into(),
            body: "{ 5 }".into()
        }
    );
    // One surrounding code fence is tolerated, as chip.work-decision.v1 tolerates it.
    let fenced = format!("```json\n{reply}\n```");
    assert!(validate_decision(&fenced, &packet(), &limits()).is_ok());
}

#[test]
fn malformed_replies_and_unknown_operations_are_refused() {
    let p = packet();
    let cases: &[Case] = &[
        ("not json", |e| matches!(e, ReplyError::NotOneJsonObject(_))),
        (r#"Sure! {"op":"no_change","reason":"x"}"#, |e| {
            matches!(e, ReplyError::NotOneJsonObject(_))
        }),
        (r#"{"op":"no_change","reason":"x"} trailing"#, |e| {
            matches!(e, ReplyError::NotOneJsonObject(_))
        }),
        (
            r#"{"op":"no_change","reason":"x"}{"op":"no_change","reason":"y"}"#,
            |e| matches!(e, ReplyError::NotOneJsonObject(_)),
        ),
        (r#"{"op":"replace_symbol_body","symbol":"#, |e| {
            matches!(e, ReplyError::NotOneJsonObject(_))
        }),
        (r#"["op"]"#, |e| {
            matches!(e, ReplyError::NotOneJsonObject(_))
        }),
        (r#"{"op":"done","reason":"finished"}"#, |e| {
            matches!(e, ReplyError::UnknownOperation(_))
        }),
        (r#"{"op":"run_command","command":"cargo test"}"#, |e| {
            matches!(e, ReplyError::UnknownOperation(_))
        }),
        (r#"{"op":"no_change","op":"no_change","reason":"x"}"#, |e| {
            matches!(e, ReplyError::DuplicateKey(_))
        }),
    ];
    for (reply, expected) in cases {
        let e = validate_decision(reply, &p, &limits()).unwrap_err();
        assert!(expected(&e), "{reply}: {e:?}");
    }
}

#[test]
fn unauthorized_symbols_fields_and_sizes_are_refused() {
    let p = packet();
    let cases: &[Case] = &[
        (
            r#"{"op":"replace_symbol_body","symbol":"symbol:src/lib.rs:backoff_ms","body":"{ 5 }"}"#,
            |e| matches!(e, ReplyError::UnauthorizedSymbol(_)),
        ),
        (
            r#"{"op":"replace_symbol_body","symbol":"symbol:src/lib.rs:max_retries","body":"{ 5 }","path":"src/other.rs"}"#,
            |e| matches!(e, ReplyError::UnknownField(f) if f == "path"),
        ),
        (
            r#"{"op":"replace_symbol_body","symbol":"symbol:src/lib.rs:max_retries","body":"{ 5 }","line":3}"#,
            |e| matches!(e, ReplyError::UnknownField(_)),
        ),
        (
            r#"{"op":"replace_symbol_body","symbol":"symbol:src/lib.rs:max_retries"}"#,
            |e| matches!(e, ReplyError::MissingField(f) if f == "body"),
        ),
        (
            r#"{"op":"replace_symbol_body","symbol":"symbol:src/lib.rs:max_retries","body":5}"#,
            |e| matches!(e, ReplyError::WrongType(_)),
        ),
        (
            r#"{"op":"replace_symbol_body","symbol":"symbol:src/lib.rs:max_retries","body":"  "}"#,
            |e| matches!(e, ReplyError::EmptyField(_)),
        ),
        (
            r#"{"op":"insert_symbol","after":"symbol:src/lib.rs:max_retries","declaration":"fn x() {}"}"#,
            |e| matches!(e, ReplyError::OperationNotAllowed(_)),
        ),
    ];
    for (reply, expected) in cases {
        let e = validate_decision(reply, &p, &limits()).unwrap_err();
        assert!(expected(&e), "{reply}: {e:?}");
    }
    let big = format!(
        r#"{{"op":"replace_symbol_body","symbol":"symbol:src/lib.rs:max_retries","body":"{{ {} }}"}}"#,
        "1 + ".repeat(40) + "1"
    );
    assert!(matches!(
        validate_decision(&big, &p, &limits()).unwrap_err(),
        ReplyError::OversizedField(_)
    ));
    let huge = format!(r#"{{"op":"no_change","reason":"{}"}}"#, "x".repeat(20_000));
    assert!(matches!(
        validate_decision(&huge, &p, &limits()).unwrap_err(),
        ReplyError::TooLarge(_)
    ));
    // need_symbol with no context requests left is refused before any lookup.
    let mut none_left = packet();
    none_left.constraints.context_requests_left = 0;
    let e = validate_decision(
        r#"{"op":"need_symbol","symbol":"symbol:src/lib.rs:backoff_ms"}"#,
        &none_left,
        &limits(),
    )
    .unwrap_err();
    assert!(matches!(e, ReplyError::OperationNotAllowed(_)));
}

#[test]
fn a_classification_must_choose_a_supplied_candidate() {
    let c = ClassifyPacket {
        version: chip_symbol_loop::contract::CLASSIFY_VERSION.into(),
        goal: "g".into(),
        acceptance: packet().acceptance,
        candidates: vec![
            Neighbor {
                id: "symbol:src/lib.rs:a".into(),
                signature: "fn a()".into(),
            },
            Neighbor {
                id: "symbol:src/lib.rs:b".into(),
                signature: "fn b()".into(),
            },
        ],
    };
    assert_eq!(
        validate_classification(
            r#"{"op":"classify","symbol":"symbol:src/lib.rs:b"}"#,
            &c,
            &limits()
        )
        .unwrap(),
        "symbol:src/lib.rs:b"
    );
    assert!(matches!(
        validate_classification(
            r#"{"op":"classify","symbol":"symbol:src/lib.rs:c"}"#,
            &c,
            &limits()
        )
        .unwrap_err(),
        ReplyError::NotACandidate(_)
    ));
    assert!(
        validate_classification(
            r#"{"op":"classify","symbol":"symbol:src/lib.rs:a","why":"x"}"#,
            &c,
            &limits()
        )
        .is_err()
    );
}

#[test]
fn insertion_is_refused_without_an_authorized_point() {
    assert!(matches!(
        prepare_insert(None, "symbol:src/lib.rs:a"),
        Err(EditError::InsertionRefused(_))
    ));
    let outside = prepare_insert(Some("symbol:src/lib.rs:a"), "symbol:src/lib.rs:b").unwrap_err();
    assert!(
        outside
            .to_string()
            .contains("not an authorized insertion point"),
        "{outside}"
    );
    // Even at the authorized point, this implementation does not insert; it says so.
    let unsupported =
        prepare_insert(Some("symbol:src/lib.rs:a"), "symbol:src/lib.rs:a").unwrap_err();
    assert!(
        unsupported.to_string().contains("not supported"),
        "{unsupported}"
    );
}

#[test]
fn a_packet_over_its_bounds_is_never_sent() {
    let mut p = packet();
    p.symbol.source = "x".repeat(9 * 1024);
    assert!(matches!(
        p.checked_bytes(&limits()),
        Err(PacketError::Field(_))
    ));
    let mut p = packet();
    p.goal = "two\nlines".into();
    assert!(p.checked_bytes(&limits()).is_err());
    let mut p = packet();
    p.neighbors = (0..40)
        .map(|i| Neighbor {
            id: format!("n{i}"),
            signature: "fn f()".into(),
        })
        .collect();
    assert!(p.checked_bytes(&limits()).is_err());
    let tight = PacketLimits {
        max_packet_bytes: 200,
        ..limits()
    };
    assert!(matches!(
        packet().checked_bytes(&tight),
        Err(PacketError::TooLarge { .. })
    ));
    assert!(packet().checked_bytes(&limits()).is_ok());
}
