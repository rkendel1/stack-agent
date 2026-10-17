//! Session protocol edge cases that are not host transcripts: ordering errors, size
//! limits, caching, and how the executor used the port.

use fx_host::{MAX_COMMAND_BYTES, Session, SupportFlags};
use serde_json::{Value, json};

const REQUEST: &str = r#"{"v":1,"model":"example/model-1","messages":[{"role":"user","parts":[{"type":"text","text":"hi"}]}],"correlation":{"request_id":"00112233445566778899aabbccddeeff"}}"#;

fn parse(bytes: Vec<u8>) -> Value {
    serde_json::from_slice(&bytes).expect("responses are JSON")
}

fn command(session: &mut Session, value: Value) -> Value {
    let mut object = value.as_object().unwrap().clone();
    object.insert("v".into(), 1.into());
    parse(session.command(&serde_json::to_vec(&Value::Object(object)).unwrap()))
}

fn finished_session() -> Session {
    let mut session = Session::new(SupportFlags(0));
    assert_eq!(
        parse(session.begin(REQUEST.as_bytes()))["status"],
        "need_invoke"
    );
    command(&mut session, json!({"cmd": "invoke_result", "ok": true}));
    command(&mut session, json!({"cmd": "step"}));
    command(
        &mut session,
        json!({"cmd": "feed", "observed": "output_observed", "ended": true,
               "items": [{"type": "text", "text": "ok"}, {"type": "done", "terminal": "provider_marker", "finish_reason": "stop"}]}),
    );
    let done = command(&mut session, json!({"cmd": "step"}));
    assert_eq!(done["status"], "done");
    session
}

#[test]
fn commands_before_begin_and_a_second_begin_are_rejected() {
    let mut session = Session::new(SupportFlags(0));
    for cmd in [
        json!({"cmd": "step"}),
        json!({"cmd": "cancel"}),
        json!({"cmd": "invoke_result", "ok": true}),
        json!({"cmd": "feed", "items": [], "observed": "invoked"}),
    ] {
        assert_eq!(command(&mut session, cmd)["error"]["code"], "bad_state");
    }
    assert_eq!(
        parse(session.begin(REQUEST.as_bytes()))["status"],
        "need_invoke"
    );
    assert_eq!(
        parse(session.begin(REQUEST.as_bytes()))["error"]["code"],
        "bad_state",
        "begin is once per session"
    );
}

#[test]
fn feeding_before_the_provider_was_invoked_is_rejected() {
    let mut session = Session::new(SupportFlags(0));
    session.begin(REQUEST.as_bytes());
    let response = command(
        &mut session,
        json!({"cmd": "feed", "items": [{"type": "text", "text": "early"}], "observed": "output_observed"}),
    );
    assert_eq!(response["error"]["code"], "bad_state");
}

#[test]
fn an_oversized_command_is_rejected_without_being_parsed() {
    let mut session = Session::new(SupportFlags(0));
    session.begin(REQUEST.as_bytes());
    let huge = vec![b' '; MAX_COMMAND_BYTES + 1];
    assert_eq!(parse(session.command(&huge))["error"]["code"], "too_large");
}

#[test]
fn error_responses_never_echo_the_command() {
    let mut session = Session::new(SupportFlags(0));
    session.begin(REQUEST.as_bytes());
    let secret = "sk-SECRETSECRETSECRET0123456789";
    let raw =
        parse(session.command(format!(r#"{{"v":1,"cmd":"launch","note":"{secret}"}}"#).as_bytes()));
    assert_eq!(raw["error"]["code"], "bad_command");
    assert!(!raw.to_string().contains(secret));
}

#[test]
fn the_final_response_is_cached_and_nothing_follows_it() {
    let mut session = finished_session();
    assert!(session.is_done());
    let first = session.command(br#"{"v":1,"cmd":"step"}"#);
    let second = session.command(br#"{"v":1,"cmd":"step"}"#);
    assert_eq!(first, second, "stepping after the end repeats the report");
    let report = parse(first);
    assert_eq!(report["report"]["outcome"]["type"], "completed");
    for late in [
        r#"{"v":1,"cmd":"feed","items":[],"observed":"invoked"}"#,
        r#"{"v":1,"cmd":"cancel"}"#,
    ] {
        let response = parse(session.command(late.as_bytes()));
        assert_eq!(
            (response["status"].as_str(), response["ignored"].as_bool()),
            (Some("done"), Some(true))
        );
    }
}

#[test]
fn the_executor_invokes_the_port_once_and_releases_it_once() {
    let session = finished_session();
    let counts = session.port_counts();
    assert_eq!((counts.invokes, counts.releases, counts.aborts), (1, 1, 0));
    assert!(counts.polls >= 2);
}

#[test]
fn an_unsupported_request_never_asks_the_host_to_invoke() {
    let mut session = Session::new(SupportFlags(0));
    let request = REQUEST.replace(
        r#""correlation""#,
        r#""options":{"max_output_tokens":10},"correlation""#,
    );
    let response = parse(session.begin(request.as_bytes()));
    assert_eq!(response["status"], "done");
    assert_eq!(
        response["report"]["outcome"]["kind"],
        "capability_unavailable"
    );
    assert_eq!(
        session.port_counts().invokes,
        0,
        "the port was never invoked"
    );
}

#[test]
fn the_request_returned_for_invocation_is_the_canonical_validated_one() {
    let mut session = Session::new(SupportFlags(0));
    let padded = format!("  {REQUEST}  ");
    let response = parse(session.begin(padded.as_bytes()));
    // Surrounding whitespace is valid JSON; the host receives the canonical document.
    assert_eq!(response["status"], "need_invoke");
    assert_eq!(response["request"]["model"], "example/model-1");
    assert_eq!(response["timeout_ms"], Value::Null);
}

#[test]
fn a_budget_is_handed_to_the_host_in_milliseconds() {
    let mut session = Session::new(SupportFlags(0));
    let request = REQUEST.replace(r#""correlation""#, r#""budget_ms":1500,"correlation""#);
    assert_eq!(parse(session.begin(request.as_bytes()))["timeout_ms"], 1500);
}

#[test]
fn many_sessions_share_nothing() {
    let mut a = Session::new(SupportFlags(0));
    let mut b = Session::new(SupportFlags(0));
    a.begin(REQUEST.as_bytes());
    b.begin(REQUEST.as_bytes());
    command(&mut a, json!({"cmd": "invoke_result", "ok": true}));
    // b has not been told the provider was invoked, so stepping b is still an error.
    assert_eq!(
        command(&mut b, json!({"cmd": "step"}))["error"]["code"],
        "bad_state"
    );
    assert_eq!(
        command(&mut a, json!({"cmd": "step"}))["status"],
        "need_input"
    );
}
