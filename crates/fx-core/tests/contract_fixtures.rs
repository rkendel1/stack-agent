//! Runs every contract fixture in tests/fixtures/contract/cases.json.
//!
//! The same fixture files are intended for the executor (R2) and the Node and WASM
//! adapters (R4), so expected results live in data, not in this runner.

use std::path::PathBuf;

use fx_core::*;
use serde_json::Value;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/contract")
}

fn snake(debug: &str) -> String {
    let mut out = String::new();
    for (i, c) in debug.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Decodes a fixture and returns "ok", "decode:<kind>" or "invalid:<code>".
fn run(document: &str, bytes: &[u8]) -> String {
    let limits = Limits::DEFAULT;
    let decoded: Result<(), DecodeError> = match document {
        "request" => match decode_request(bytes, &limits) {
            Ok(request) => {
                return match validate_request(&request, &limits) {
                    Ok(()) => {
                        // Valid requests must also round-trip exactly.
                        let again = decode_request(&encode_request(&request), &limits).unwrap();
                        assert_eq!(again, request);
                        "ok".to_owned()
                    }
                    Err(e) => format!("invalid:{}", snake(&format!("{:?}", e.code))),
                };
            }
            Err(e) => Err(e),
        },
        "event" => decode_event(bytes, &limits).map(|event| {
            assert_eq!(decode_event(&encode_event(&event), &limits).unwrap(), event);
        }),
        "outcome" => decode_outcome(bytes, &limits).map(|outcome| {
            assert_eq!(
                decode_outcome(&encode_outcome(&outcome), &limits).unwrap(),
                outcome
            );
        }),
        "capability_error" => decode_capability_error(bytes, &limits).map(|error| {
            assert_eq!(
                decode_capability_error(&encode_capability_error(&error), &limits).unwrap(),
                error
            );
        }),
        other => panic!("unknown fixture document kind {other}"),
    };
    match decoded {
        Ok(()) => "ok".to_owned(),
        Err(e) => format!("decode:{}", snake(&format!("{:?}", e.kind))),
    }
}

#[test]
fn every_contract_fixture_produces_its_expected_result() {
    let manifest = std::fs::read_to_string(fixtures_dir().join("cases.json")).unwrap();
    let cases: Vec<Value> = serde_json::from_str(&manifest).unwrap();
    assert!(cases.len() >= 40, "fixture corpus unexpectedly small");
    let mut failures = Vec::new();
    for case in &cases {
        let file = case["file"].as_str().unwrap();
        let document = case["document"].as_str().unwrap();
        let expect = case["expect"].as_str().unwrap();
        assert!(
            case["behavior"].as_str().is_some_and(|b| !b.is_empty()),
            "{file}: every fixture names the behavior it protects"
        );
        let bytes = std::fs::read(fixtures_dir().join(file)).unwrap();
        let got = run(document, &bytes);
        if got != expect {
            failures.push(format!("{file}: expected {expect}, got {got}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_fixture_file_is_listed_in_the_manifest() {
    let manifest = std::fs::read_to_string(fixtures_dir().join("cases.json")).unwrap();
    let cases: Vec<Value> = serde_json::from_str(&manifest).unwrap();
    let listed: std::collections::HashSet<&str> =
        cases.iter().map(|c| c["file"].as_str().unwrap()).collect();
    for entry in std::fs::read_dir(fixtures_dir()).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        if name != "cases.json" {
            assert!(listed.contains(name.as_str()), "unlisted fixture {name}");
        }
    }
}
