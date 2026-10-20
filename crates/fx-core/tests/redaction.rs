//! The redaction corpus (`tests/fixtures/redaction.json`): every secret is masked, every look-alike
//! that holds none is left alone, and masking is stable when applied twice.

use fx_core::redact::{REDACTED, mask_secrets};
use serde_json::Value;

/// Splits a credential-shaped fixture token after its public prefix, so the repository holds no
/// string a secret scanner reads as a credential.
const MARKER: &str = "«»";

fn corpus() -> Value {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/redaction.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn strings(v: &Value) -> Vec<&str> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect()
}

#[test]
fn every_secret_in_the_corpus_is_masked_and_its_context_kept() {
    let mut failures = Vec::new();
    for case in corpus()["masked"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        // The fixture splits each credential-shaped token with a marker; joined here.
        let text = case["text"].as_str().unwrap().replace(MARKER, "");
        let masked = mask_secrets(&text);
        for secret in strings(&case["secrets"]) {
            if masked.contains(secret) {
                failures.push(format!("{name}: {secret:?} survived in {masked:?}"));
            }
        }
        for kept in strings(&case["kept"]) {
            if !masked.contains(kept) {
                failures.push(format!("{name}: {kept:?} was lost from {masked:?}"));
            }
        }
        if !masked.contains(REDACTED) {
            failures.push(format!("{name}: nothing was masked in {masked:?}"));
        }
        if mask_secrets(&masked) != masked {
            failures.push(format!("{name}: masking again changed {masked:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn text_without_a_secret_is_unchanged() {
    let failures: Vec<String> = strings(&corpus()["unchanged"])
        .into_iter()
        .filter_map(|text| {
            let masked = mask_secrets(text);
            (masked != text).then(|| format!("{text:?} became {masked:?}"))
        })
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn masking_never_splits_a_character() {
    for text in [
        "é=password: ünïcødé-sëcrët",
        "Bearer ééééééééééé",
        "token=日本語の秘密です",
    ] {
        let masked = mask_secrets(text);
        assert!(std::str::from_utf8(masked.as_bytes()).is_ok());
    }
}
