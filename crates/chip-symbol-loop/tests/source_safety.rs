//! The edit boundary on real files: what a body can and cannot change.

use std::fs;
use std::path::Path;

use chip_symbol_loop::edit::{
    EditError, WriteOutcome, prepare_replace_body, resolve_inside, write_checked,
};
use chip_symbol_loop::symbols::{SymbolIndex, index_text, sha256};

const LIB: &str = "//! A policy.\n\n/// The most times a failed call is retried.\n#[inline]\npub fn max_retries() -> u32 {\n    3\n}\n\nimpl Policy {\n    pub(crate) fn backoff<T: Copy>(&self, attempt: T) -> u64 where T: Into<u64> {\n        100 * attempt.into()\n    }\n}\n\npub struct Policy;\n";

fn tree(tag: &str) -> tempfile::TempDir {
    let d = tempfile::Builder::new()
        .prefix(&format!("chip-symbol-loop-{tag}-"))
        .tempdir()
        .unwrap();
    fs::create_dir_all(d.path().join("src")).unwrap();
    fs::write(d.path().join("src/lib.rs"), LIB).unwrap();
    d
}

fn record(root: &Path, id: &str) -> chip_symbol_loop::symbols::SymbolRecord {
    SymbolIndex::build(root)
        .unwrap()
        .get(id)
        .cloned()
        .unwrap_or_else(|| panic!("{id} not indexed"))
}

#[test]
fn the_index_resolves_exact_spans_and_ids() {
    let d = tree("index");
    let index = SymbolIndex::build(d.path()).unwrap();
    let f = index.get("symbol:src/lib.rs:max_retries").unwrap();
    assert_eq!(&LIB[f.body.start..f.body.end], "{\n    3\n}");
    assert!(LIB[f.item.start..].starts_with("/// The most times"));
    assert_eq!(
        f.signature.attributes.len(),
        2,
        "the doc comment and #[inline]"
    );
    let m = index.get("symbol:src/lib.rs:Policy::backoff").unwrap();
    assert_eq!(
        &LIB[m.body.start..m.body.end],
        "{\n        100 * attempt.into()\n    }"
    );
    assert_eq!(m.signature.visibility, "pub (crate)");
}

#[test]
fn replacing_a_body_preserves_the_declaration_and_everything_else() {
    let d = tree("replace");
    let target = record(d.path(), "symbol:src/lib.rs:max_retries");
    let edit = prepare_replace_body(d.path(), &target, "{\n    5\n}", true).unwrap();
    assert!(
        edit.new_text
            .contains("#[inline]\npub fn max_retries() -> u32 {\n    5\n}")
    );
    let after = index_text("src/lib.rs", &edit.new_text).unwrap();
    let before = index_text("src/lib.rs", LIB).unwrap();
    let find = |v: &Vec<chip_symbol_loop::symbols::SymbolRecord>, id: &str| {
        v.iter().find(|s| s.id == id).cloned().unwrap()
    };
    assert_eq!(
        find(&after, &target.id).signature,
        find(&before, &target.id).signature
    );
    let other = "symbol:src/lib.rs:Policy::backoff";
    assert_eq!(
        find(&after, other).fingerprint,
        find(&before, other).fingerprint
    );
    assert_eq!(
        edit.new_text.len() - LIB.len(),
        0,
        "same length body: only the digit changed"
    );
    assert!(edit.diff.contains("-    3") && edit.diff.contains("+    5"));
    // Nothing was written yet.
    assert_eq!(
        fs::read_to_string(d.path().join("src/lib.rs")).unwrap(),
        LIB
    );
}

#[test]
fn protected_declaration_properties_are_compared_structurally() {
    let base = index_text(
        "a.rs",
        "#[inline]\npub fn f<T: Copy>(x: T) -> u32 where T: Into<u32> { 1 }",
    )
    .unwrap()
    .remove(0)
    .signature;
    let cases = [
        (
            "pub fn f<T: Copy>(x: T) -> u32 where T: Into<u32> { 1 }",
            "attributes",
        ),
        (
            "#[inline]\nfn f<T: Copy>(x: T) -> u32 where T: Into<u32> { 1 }",
            "visibility",
        ),
        (
            "#[inline]\npub fn f<T: Clone>(x: T) -> u32 where T: Into<u32> { 1 }",
            "generics",
        ),
        (
            "#[inline]\npub fn f<T: Copy>(x: T) -> u32 where T: Into<u64> { 1 }",
            "generics",
        ),
        (
            "#[inline]\npub fn f<T: Copy>(y: T) -> u32 where T: Into<u32> { 1 }",
            "parameters",
        ),
        (
            "#[inline]\npub fn f<T: Copy>(x: T, z: u8) -> u32 where T: Into<u32> { 1 }",
            "parameters",
        ),
        (
            "#[inline]\npub fn f<T: Copy>(x: T) -> u64 where T: Into<u32> { 1 }",
            "return type",
        ),
        (
            "#[inline]\npub async fn f<T: Copy>(x: T) -> u32 where T: Into<u32> { 1 }",
            "qualifiers (const, async, unsafe, extern)",
        ),
    ];
    for (src, what) in cases {
        let other = index_text("a.rs", src).unwrap().remove(0).signature;
        assert_eq!(base.first_difference(&other), Some(what), "{src}");
    }
    // Layout and comments are not structure.
    let relaid = index_text(
        "a.rs",
        "#[inline]\npub   fn f< T : Copy >( x : T ) -> u32\n  where T: Into<u32> /* c */ { 2 }",
    )
    .unwrap()
    .remove(0)
    .signature;
    assert_eq!(base.first_difference(&relaid), None);
}

#[test]
fn a_stale_fingerprint_is_refused() {
    let d = tree("stale");
    let target = record(d.path(), "symbol:src/lib.rs:max_retries");
    fs::write(
        d.path().join("src/lib.rs"),
        LIB.replace("    3\n", "    4\n"),
    )
    .unwrap();
    assert!(matches!(
        prepare_replace_body(d.path(), &target, "{ 5 }", true),
        Err(EditError::StaleSource(_))
    ));
}

#[test]
fn malformed_or_truncated_bodies_are_refused() {
    let d = tree("parse");
    let target = record(d.path(), "symbol:src/lib.rs:max_retries");
    for body in [
        "5",
        "{ let x = ",
        "{ 5 } fn other() {}",
        "{ 5 }}",
        "pub fn max_retries() -> u32 { 5 }",
        "{ 5 } }\nfn evil() {",
    ] {
        let e = prepare_replace_body(d.path(), &target, body, true).unwrap_err();
        assert!(matches!(e, EditError::NoParse(_)), "{body:?}: {e:?}");
    }
}

#[test]
fn paths_cannot_escape_the_worktree_or_follow_links() {
    let d = tree("escape");
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("x.rs"), "fn x() {}").unwrap();
    std::os::unix::fs::symlink(outside.path().join("x.rs"), d.path().join("src/link.rs")).unwrap();
    std::os::unix::fs::symlink(outside.path(), d.path().join("linked_dir")).unwrap();
    for rel in [
        "../x.rs",
        "/etc/passwd",
        "src/../../x.rs",
        "src/link.rs",
        "linked_dir/x.rs",
        "src/missing.rs",
        "src",
    ] {
        assert!(
            matches!(resolve_inside(d.path(), rel), Err(EditError::BadSymbol(_))),
            "{rel}"
        );
    }
    // The index never follows the link either.
    assert!(
        SymbolIndex::build(d.path())
            .unwrap()
            .symbols
            .keys()
            .all(|id| !id.contains("link"))
    );
}

#[test]
fn a_write_needs_the_exact_prepared_state_and_is_idempotent() {
    let d = tree("write");
    let target = record(d.path(), "symbol:src/lib.rs:max_retries");
    let edit = prepare_replace_body(d.path(), &target, "{ 5 }", true).unwrap();
    // Wrong content for the digest: refused.
    assert!(
        write_checked(
            d.path(),
            "src/lib.rs",
            &edit.before_sha,
            &edit.after_sha,
            "fn evil() {}"
        )
        .is_err()
    );
    // A different file than the one prepared: its digest does not match, refused.
    fs::write(d.path().join("src/other.rs"), "pub fn other() {}\n").unwrap();
    assert!(matches!(
        write_checked(
            d.path(),
            "src/other.rs",
            &edit.before_sha,
            &edit.after_sha,
            &edit.new_text
        ),
        Err(EditError::StaleSource(_))
    ));
    assert_eq!(
        fs::read_to_string(d.path().join("src/other.rs")).unwrap(),
        "pub fn other() {}\n"
    );
    // The prepared write, then the same write again: once.
    assert_eq!(
        write_checked(
            d.path(),
            "src/lib.rs",
            &edit.before_sha,
            &edit.after_sha,
            &edit.new_text
        )
        .unwrap(),
        WriteOutcome::Written
    );
    assert_eq!(
        write_checked(
            d.path(),
            "src/lib.rs",
            &edit.before_sha,
            &edit.after_sha,
            &edit.new_text
        )
        .unwrap(),
        WriteOutcome::AlreadyApplied
    );
    assert_eq!(
        sha256(&fs::read(d.path().join("src/lib.rs")).unwrap()),
        edit.after_sha
    );
}
