//! The wire format carries Chip's own types across the process boundary unchanged.

use std::collections::BTreeMap;

use chip_core::{
    CapabilityAvailability, CapabilityError, CapabilityId, ExecutionError, ExecutionId,
    ExecutionResult, ExecutionStatus, InputValue,
};
use chip_remote_env::{Request, Response};

fn inputs() -> BTreeMap<String, InputValue> {
    [
        (
            "path".to_string(),
            InputValue::Text("src/\"lib\".rs\n".into()),
        ),
        ("count".to_string(), InputValue::Integer(-3)),
        ("flag".to_string(), InputValue::Bool(true)),
    ]
    .into()
}

#[test]
fn requests_round_trip() {
    for request in [
        Request::Info,
        Request::Capabilities,
        Request::Availability {
            capability: "pax.test".into(),
        },
        Request::Validate {
            capability: "project.read".into(),
            inputs: inputs(),
        },
        Request::Execute {
            execution_id: "model-1".into(),
            capability: "project.write".into(),
            inputs: inputs(),
            idempotency_key: None,
        },
        Request::Execute {
            execution_id: "model-1".into(),
            capability: "project.write".into(),
            inputs: inputs(),
            idempotency_key: Some("ak1.0123456789abcdef0123456789abcdef".into()),
        },
    ] {
        assert_eq!(Request::decode(&request.encode()).unwrap(), request);
    }
}

#[test]
fn responses_round_trip() {
    let id = ExecutionId::new("model-1");
    for response in [
        Response::Availability(CapabilityAvailability::Available),
        Response::Availability(CapabilityAvailability::Unavailable("no".into())),
        Response::Validated(Ok(())),
        Response::Validated(Err(CapabilityError::InvalidInput("bad path".into()))),
        Response::Executed(Ok(ExecutionResult {
            id: id.clone(),
            status: ExecutionStatus::Failure,
            output: "line\n\"quoted\"".into(),
            receipt_id: Some("sha256:abc".into()),
            evidence: None,
        })),
        Response::Executed(Ok(ExecutionResult::success(id, "ok"))),
        Response::Executed(Err(ExecutionError::ExecutionFailed("boom".into()))),
        Response::Executed(Err(ExecutionError::Cancelled)),
    ] {
        assert_eq!(Response::decode(&response.encode()).unwrap(), response);
    }
    let _ = CapabilityId::new("pax.test").unwrap();
}

#[test]
fn malformed_or_unversioned_messages_are_refused_not_repaired() {
    for bad in [
        "",
        "not json",
        "{}",
        r#"{"v":2,"op":"info"}"#,
        r#"{"v":1,"op":"nope"}"#,
        r#"{"v":1,"op":"execute","capability":"x"}"#,
        r#"{"v":1,"op":"validate","capability":"x","inputs":{"a":{"text":1}}}"#,
        r#"{"v":1,"op":"validate","capability":"x","inputs":{"a":{"text":"s","extra":1}}}"#,
    ] {
        assert!(Request::decode(bad).is_err(), "{bad:?}");
    }
    for bad in [
        "",
        "garbage",
        r#"{"v":1,"op":"execute"}"#,
        r#"{"v":1,"op":"execute","result":{"status":"win"}}"#,
    ] {
        assert!(Response::decode(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn execution_evidence_is_additive_on_the_wire() {
    use chip_core::ExecutionEvidence;
    let id = ExecutionId::new("model-1");
    // Without evidence the message has no `execution_evidence` key at all, as before.
    let plain = Response::Executed(Ok(ExecutionResult::success(id.clone(), "ok"))).encode();
    assert!(!plain.contains("execution_evidence"), "{plain}");
    // With evidence it round-trips, carrying only the identifiers present.
    let evidence = ExecutionEvidence::from_runtime(
        "rt",
        [("executionId", "exec_789"), ("receiptId", "sha256:abc")],
    )
    .unwrap();
    let with = Response::Executed(Ok(
        ExecutionResult::success(id, "ok").with_execution_evidence(evidence)
    ));
    let encoded = with.encode();
    assert!(encoded.contains(r#""executionId":"exec_789""#), "{encoded}");
    assert!(
        !encoded.contains("jobId") && !encoded.contains("environmentId"),
        "{encoded}"
    );
    assert_eq!(Response::decode(&encoded).unwrap(), with);
}

#[test]
fn a_peer_that_sends_no_or_empty_evidence_decodes_to_none() {
    let base = r#"{"v":1,"op":"execute","result":{"execution_id":"m","status":"success","output":"ok","receipt_id":null"#;
    for tail in [
        "}}",
        r#","execution_evidence":{"rt":{}}}}"#,
        r#","execution_evidence":{"rt":{"jobId":""}}}}"#,
        r#","execution_evidence":{"rt":{"bad key":"x"}}}}"#,
        r#","execution_evidence":"nope"}}"#,
    ] {
        let Response::Executed(Ok(result)) = Response::decode(&format!("{base}{tail}")).unwrap()
        else {
            panic!("not an executed result");
        };
        assert!(result.evidence.is_none(), "{tail}");
    }
}

#[test]
fn the_idempotency_key_is_additive_on_the_wire() {
    let execute = |key: Option<&str>| Request::Execute {
        execution_id: "model-1".into(),
        capability: "world.notify".into(),
        inputs: BTreeMap::new(),
        idempotency_key: key.map(str::to_string),
    };
    // Without a key the request is byte for byte what a worker that predates keys reads.
    let plain = execute(None).encode();
    assert!(!plain.contains("idempotency_key"), "{plain}");
    let keyed = execute(Some("ak1.k")).encode();
    assert!(keyed.contains(r#""idempotency_key":"ak1.k""#), "{keyed}");
    // A key that is not text is refused, not dropped.
    assert!(
        Request::decode(
            r#"{"v":1,"op":"execute","execution_id":"m","capability":"x","idempotency_key":7}"#
        )
        .is_err()
    );
}

#[test]
fn a_capability_declares_what_it_does_with_the_key_and_silence_promises_nothing() {
    use chip_core::{CapabilityDescriptor, Idempotency};
    let declared = |idempotency| {
        CapabilityDescriptor::new(CapabilityId::new("world.notify").unwrap(), "Notify", "d")
            .with_idempotency(idempotency)
    };
    for idempotency in [
        Idempotency::NotCarried,
        Idempotency::Carried,
        Idempotency::Deduplicated,
    ] {
        let response = Response::Capabilities(vec![declared(idempotency)]);
        assert_eq!(Response::decode(&response.encode()).unwrap(), response);
    }
    // A worker that predates the declaration, or names one this build does not know, is read as
    // promising nothing: never as deduplicating.
    for idempotency in ["", r#","idempotency":"exactly_once""#] {
        let encoded = format!(
            r#"{{"v":1,"op":"capabilities","capabilities":[{{"id":"world.notify","name":"Notify","description":"d","inputs":[]{idempotency}}}]}}"#
        );
        let Response::Capabilities(d) = Response::decode(&encoded).unwrap() else {
            panic!("{encoded}");
        };
        assert_eq!(d[0].idempotency, Idempotency::NotCarried, "{encoded}");
    }
}

#[test]
fn a_capability_declares_its_effect_and_the_declaration_survives_the_wire() {
    use chip_core::{CapabilityDescriptor, Effect};
    for effect in [Effect::Observation, Effect::Consequential, Effect::Unknown] {
        let declared =
            CapabilityDescriptor::new(CapabilityId::new("world.look").unwrap(), "L", "d")
                .with_effect(effect);
        let response = Response::Capabilities(vec![declared]);
        let Response::Capabilities(d) = Response::decode(&response.encode()).unwrap() else {
            panic!("capabilities expected");
        };
        assert_eq!(d[0].effect, effect, "{}", effect.name());
    }
}

#[test]
fn a_missing_or_unrecognized_effect_is_unknown_and_never_an_observation() {
    use chip_core::Effect;
    // A worker that predates the declaration says nothing; one from a newer build may say
    // something this build cannot read; a malformed value is no declaration. None of them has
    // said that performing the capability again is safe, and `observation` must be the exact name.
    for effect in [
        "",
        r#","effect":"read_only""#,
        r#","effect":"Observation""#,
        r#","effect":"OBSERVATION""#,
        r#","effect":" observation""#,
        r#","effect":"observation ""#,
        r#","effect":"observation,consequential""#,
        r#","effect":true"#,
        r#","effect":1"#,
        r#","effect":null"#,
        r#","effect":["observation"]"#,
        r#","effect":{"observation":true}"#,
    ] {
        let encoded = format!(
            r#"{{"v":1,"op":"capabilities","capabilities":[{{"id":"project.read","name":"R","description":"d","inputs":[]{effect}}}]}}"#
        );
        let Response::Capabilities(d) = Response::decode(&encoded).unwrap() else {
            panic!("{encoded}");
        };
        assert_eq!(d[0].effect, Effect::Unknown, "{encoded}");
    }
    // The exact names are read as what they say.
    for (name, effect) in [
        ("observation", Effect::Observation),
        ("consequential", Effect::Consequential),
        ("unknown", Effect::Unknown),
    ] {
        let encoded = format!(
            r#"{{"v":1,"op":"capabilities","capabilities":[{{"id":"project.read","name":"R","description":"d","inputs":[],"effect":"{name}"}}]}}"#
        );
        let Response::Capabilities(d) = Response::decode(&encoded).unwrap() else {
            panic!("{encoded}");
        };
        assert_eq!(d[0].effect, effect, "{encoded}");
    }
}

#[test]
fn a_descriptor_built_without_a_declaration_is_unknown() {
    use chip_core::{CapabilityDescriptor, Effect};
    let d = CapabilityDescriptor::new(CapabilityId::new("world.look").unwrap(), "L", "d");
    assert_eq!(d.effect, Effect::Unknown);
    // The other declarations do not imply this one.
    assert_eq!(
        d.clone()
            .with_idempotency(chip_core::Idempotency::Deduplicated)
            .effect,
        Effect::Unknown
    );
    assert_eq!(d.without_evidence_reuse().effect, Effect::Unknown);
    assert_eq!(Effect::default(), Effect::Unknown);
    assert_eq!(Effect::from_name("observation"), Some(Effect::Observation));
    assert_eq!(Effect::from_name("Observation"), None);
}
