//! `project.write`'s edit form: `replace`, which must occur exactly once in the file named by
//! `expected_sha256`, becomes `with`. The case that needed it: a one-line change to a tracked,
//! generated file larger than a whole-file write may carry (AppPort's `docs/audit.json`, 53 KB, in
//! coding benchmark run 2). Every case goes through the production path against the real
//! filesystem; the whole-file limit and the stale-write rules are unchanged.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chip_core::{
    CapabilityId, CapabilityProvider, ExecutionId, ExecutionRequest, ExecutionResult,
    ExecutionStatus, Executor, InputValue, Observation, ObservationKind,
};
use chip_project::{
    MAX_EDIT_BYTES, MAX_EDIT_FILE_BYTES, MAX_WRITE_BYTES, PROJECT_READ, PROJECT_WRITE,
    ProjectExecutor, STALE_BASE, checked_write, replaced_file, stale_write, write_summary,
};
use sha2::{Digest, Sha256};

fn project(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("chip-edit-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("project/docs")).unwrap();
    base.join("project")
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn text(v: &str) -> InputValue {
    InputValue::Text(v.to_string())
}

fn inputs(pairs: &[(&str, InputValue)]) -> BTreeMap<String, InputValue> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

fn edit_inputs(
    path: &str,
    replace: &str,
    with: &str,
    digest: &str,
) -> BTreeMap<String, InputValue> {
    inputs(&[
        ("path", text(path)),
        ("replace", text(replace)),
        ("with", text(with)),
        ("expected_sha256", text(digest)),
    ])
}

async fn execute(
    p: &ProjectExecutor,
    capability: &str,
    inputs: BTreeMap<String, InputValue>,
) -> ExecutionResult {
    p.execute(ExecutionRequest::new(ExecutionId::new("e"), capability).with_inputs(inputs))
        .await
        .unwrap()
}

/// Validation, then execution: the order the work loop uses.
async fn edit(p: &ProjectExecutor, i: BTreeMap<String, InputValue>) -> ExecutionResult {
    p.validate_inputs(&CapabilityId::new(PROJECT_WRITE).unwrap(), &i)
        .await
        .unwrap();
    execute(p, PROJECT_WRITE, i).await
}

async fn refused(p: &ProjectExecutor, i: BTreeMap<String, InputValue>) -> String {
    p.validate_inputs(&CapabilityId::new(PROJECT_WRITE).unwrap(), &i)
        .await
        .unwrap_err()
        .to_string()
}

fn head(r: &ExecutionResult) -> serde_json::Value {
    serde_json::from_str(r.output.lines().next().unwrap()).unwrap()
}

fn observed(r: &ExecutionResult) -> Observation {
    Observation {
        execution_id: ExecutionId::new("e"),
        kind: match r.status {
            ExecutionStatus::Success => ObservationKind::ExecutionCompleted,
            _ => ObservationKind::ExecutionFailed,
        },
        status: r.status,
        output: Some(r.output.clone()),
        receipt_id: None,
        evidence: None,
    }
}

/// A file shaped like the one in the benchmark: a long generated JSON list in which one path
/// occurs twice, once as the last item of one list (no comma) and once inside another.
fn audit_like() -> String {
    let mut s = String::from("{\n  \"workflows\": [\n");
    for n in 0..900 {
        s.push_str(&format!(
            "        \"packages/pkg{n:03}/test/case.test.ts\",\n"
        ));
    }
    s.push_str("        \"packages/server/test/desktop-host.test.ts\"\n  ],\n  \"tests\": [\n");
    s.push_str("        \"packages/server/test/desktop-host.test.ts\",\n");
    for n in 0..300 {
        s.push_str(&format!(
            "        \"packages/zzz{n:03}/test/case.test.ts\",\n"
        ));
    }
    s.push_str("        \"end\"\n  ]\n}\n");
    s
}

const PATH: &str = "docs/audit.json";

fn setup(tag: &str, content: &str) -> (PathBuf, ProjectExecutor) {
    let root = project(tag);
    std::fs::write(root.join(PATH), content).unwrap();
    let p = ProjectExecutor::new(&root);
    (root, p)
}

fn on_disk(root: &std::path::Path) -> String {
    std::fs::read_to_string(root.join(PATH)).unwrap()
}

#[tokio::test]
async fn a_small_edit_to_a_file_over_the_write_limit_changes_exactly_that_passage() {
    let base = audit_like();
    assert!(
        base.len() > MAX_WRITE_BYTES,
        "the file is larger than a whole write"
    );
    let (root, p) = setup("small", &base);

    // The model reads a range; the range names the whole file it came from.
    let read = execute(
        &p,
        PROJECT_READ,
        inputs(&[
            ("path", text(PATH)),
            ("offset", InputValue::Integer(0)),
            ("length", InputValue::Integer(4096)),
        ]),
    )
    .await;
    let r = head(&read);
    assert_eq!(r["complete"], false);
    assert_eq!(r["file_sha256"], sha(base.as_bytes()), "{}", r);
    assert_ne!(
        r["sha256"], r["file_sha256"],
        "a range's own hash is not the file's"
    );

    let anchor = "        \"packages/server/test/desktop-host.test.ts\",\n";
    let added = "        \"packages/server/test/event-authorization.test.ts\",\n";
    let w = edit(
        &p,
        edit_inputs(
            PATH,
            anchor,
            &format!("{anchor}{added}"),
            r["file_sha256"].as_str().unwrap(),
        ),
    )
    .await;
    assert_eq!(w.status, ExecutionStatus::Success, "{}", w.output);

    // Exactly the intended change; every other byte preserved.
    let expected = base.replacen(anchor, &format!("{anchor}{added}"), 1);
    assert_eq!(on_disk(&root), expected);
    assert_eq!(on_disk(&root).len(), base.len() + added.len());

    // Observed as the write it is: a precondition-checked replacement, counted and audited as one.
    let v = head(&w);
    assert_eq!(v["operation"], "replaced");
    assert_eq!(v["changed"], true);
    assert_eq!(v["precondition"], "sha256");
    assert_eq!(v["bytes_written"], expected.len());
    assert_eq!(v["sha256"], sha(expected.as_bytes()));
    assert_eq!(v["edit"]["line"], 906);
    assert_eq!(v["edit"]["replaced_bytes"], anchor.len());
    assert_eq!(v["edit"]["with_bytes"], anchor.len() + added.len());
    assert_eq!(
        w.output.lines().count(),
        1,
        "no file content in the observation"
    );
    let o = observed(&w);
    assert_eq!(write_summary(&o), Some((PATH.to_string(), true)));
    assert_eq!(replaced_file(&o).as_deref(), Some(PATH));
    assert_eq!(checked_write(&o).as_deref(), Some(PATH));
    assert_eq!(stale_write(&o), None);
}

#[tokio::test]
async fn an_edit_based_on_an_older_file_is_refused_and_the_newer_file_survives() {
    let base = audit_like();
    let (root, p) = setup("stale", &base);
    let old = sha(base.as_bytes());
    let newer = base.replace("\"end\"", "\"a person's change\"");
    std::fs::write(root.join(PATH), &newer).unwrap();
    let w = edit(
        &p,
        edit_inputs(PATH, "\"packages/pkg000/test/case.test.ts\"", "\"x\"", &old),
    )
    .await;
    assert_eq!(w.status, ExecutionStatus::Failure);
    let v = head(&w);
    assert_eq!(v["error"], STALE_BASE, "{}", w.output);
    assert_eq!(v["found"], "different_content");
    assert_eq!(on_disk(&root), newer, "untouched");
    assert_eq!(stale_write(&observed(&w)).as_deref(), Some(PATH));
    assert_eq!(write_summary(&observed(&w)), None, "not a write");
    // A file that is gone is stale too.
    std::fs::remove_file(root.join(PATH)).unwrap();
    let w = edit(&p, edit_inputs(PATH, "x", "y", &old)).await;
    assert_eq!(head(&w)["error"], STALE_BASE);
    assert_eq!(head(&w)["found"], "absent");
    assert!(!root.join(PATH).exists(), "nothing was created");
}

#[tokio::test]
async fn an_ambiguous_or_missing_passage_is_refused_and_nothing_is_written() {
    let base = audit_like();
    let (root, p) = setup("ambiguous", &base);
    let digest = sha(base.as_bytes());
    // The path alone occurs twice: no occurrence is chosen.
    let w = edit(
        &p,
        edit_inputs(
            PATH,
            "\"packages/server/test/desktop-host.test.ts\"",
            "\"x\"",
            &digest,
        ),
    )
    .await;
    assert_eq!(w.status, ExecutionStatus::Failure);
    let v = head(&w);
    assert_eq!(v["error"], "edit_ambiguous", "{}", w.output);
    assert_eq!(v["occurrences"], 2);
    assert_eq!(v["lines"], serde_json::json!([903, 906]));
    assert!(v["hint"].as_str().unwrap().contains("exactly once"));
    assert_eq!(on_disk(&root), base);
    assert_eq!(write_summary(&observed(&w)), None);

    // Overlapping occurrences count: "aa" occurs twice in "aaa".
    std::fs::write(root.join(PATH), "aaa\n").unwrap();
    let w = edit(&p, edit_inputs(PATH, "aa", "b", &sha(b"aaa\n"))).await;
    assert_eq!(head(&w)["error"], "edit_ambiguous");
    assert_eq!(head(&w)["occurrences"], 2);
    assert_eq!(on_disk(&root), "aaa\n");

    // Not there at all, e.g. whitespace copied wrong.
    std::fs::write(root.join(PATH), &base).unwrap();
    let w = edit(
        &p,
        edit_inputs(
            PATH,
            "\t\"packages/pkg001/test/case.test.ts\",\n",
            "x",
            &digest,
        ),
    )
    .await;
    assert_eq!(head(&w)["error"], "edit_not_found", "{}", w.output);
    assert!(
        head(&w)["hint"]
            .as_str()
            .unwrap()
            .contains("exactly as given")
    );
    assert_eq!(on_disk(&root), base);
}

#[tokio::test]
async fn invalid_paths_and_malformed_edits_are_refused_before_anything_runs() {
    let base = audit_like();
    let (root, p) = setup("invalid", &base);
    let digest = sha(base.as_bytes());
    for path in [
        "../outside.json",
        "/etc/passwd",
        ".git/config",
        "docs/../../x",
        "",
    ] {
        let why = refused(&p, edit_inputs(path, "a", "b", &digest)).await;
        assert!(!why.is_empty(), "{path}");
    }
    let cases: Vec<(BTreeMap<String, InputValue>, &str)> = vec![
        // No precondition: an edit always names the file it was decided against.
        (
            inputs(&[
                ("path", text(PATH)),
                ("replace", text("a")),
                ("with", text("b")),
            ]),
            "needs expected_sha256",
        ),
        (
            inputs(&[
                ("path", text(PATH)),
                ("replace", text("a")),
                ("with", text("b")),
                ("expect_absent", InputValue::Bool(true)),
            ]),
            "needs expected_sha256",
        ),
        // Two forms at once.
        (
            inputs(&[
                ("path", text(PATH)),
                ("content", text("x")),
                ("replace", text("a")),
                ("with", text("b")),
                ("expected_sha256", text(&digest)),
            ]),
            "cannot be combined",
        ),
        (
            inputs(&[
                ("path", text(PATH)),
                ("replace", text("a")),
                ("expected_sha256", text(&digest)),
            ]),
            "both replace and with",
        ),
        (
            inputs(&[
                ("path", text(PATH)),
                ("with", text("b")),
                ("expected_sha256", text(&digest)),
            ]),
            "both replace and with",
        ),
        (edit_inputs(PATH, "", "b", &digest), "must not be empty"),
        (inputs(&[("path", text(PATH))]), "a write needs content"),
        (
            edit_inputs(PATH, "a", "b", "not-a-digest"),
            "64 lowercase hexadecimal",
        ),
        (
            edit_inputs(PATH, &"r".repeat(MAX_EDIT_BYTES + 1), "b", &digest),
            "an edit's limit is 8192",
        ),
        (
            edit_inputs(PATH, "a", &"w".repeat(MAX_EDIT_BYTES + 1), &digest),
            "an edit's limit is 8192",
        ),
    ];
    for (i, says) in cases {
        let why = refused(&p, i.clone()).await;
        assert!(why.contains(says), "{i:?}: {why}");
    }
    assert_eq!(on_disk(&root), base, "nothing ran");
}

#[tokio::test]
async fn files_and_results_past_the_edit_bound_are_refused_with_nothing_written() {
    // The file itself is past the bound: refused, unchanged, and its ranged reads name no file hash.
    let big = format!("{}\nunique\n", "x".repeat(MAX_EDIT_FILE_BYTES));
    let (root, p) = setup("too-large", &big);
    let read = execute(
        &p,
        PROJECT_READ,
        inputs(&[
            ("path", text(PATH)),
            ("offset", InputValue::Integer(0)),
            ("length", InputValue::Integer(10)),
        ]),
    )
    .await;
    assert!(head(&read).get("file_sha256").is_none(), "{}", read.output);
    let w = edit(
        &p,
        edit_inputs(PATH, "unique", "changed", &sha(big.as_bytes())),
    )
    .await;
    assert_eq!(head(&w)["error"], "too_large", "{}", w.output);
    assert_eq!(head(&w)["limit"], MAX_EDIT_FILE_BYTES);
    assert_eq!(on_disk(&root), big);

    // The file fits, the result would not.
    let near = format!("{}\nunique\n", "y".repeat(MAX_EDIT_FILE_BYTES - 100));
    std::fs::write(root.join(PATH), &near).unwrap();
    let w = edit(
        &p,
        edit_inputs(
            PATH,
            "unique",
            &"z".repeat(MAX_EDIT_BYTES),
            &sha(near.as_bytes()),
        ),
    )
    .await;
    assert_eq!(head(&w)["error"], "edit_result_too_large", "{}", w.output);
    assert_eq!(on_disk(&root), near);
}

#[tokio::test]
async fn the_whole_file_write_and_its_limit_are_unchanged() {
    let base = audit_like();
    let (root, p) = setup("whole", &base);
    // Over the limit: refused as before, and the refusal names the edit form.
    let why = refused(
        &p,
        inputs(&[
            ("path", text(PATH)),
            ("content", text(&"c".repeat(MAX_WRITE_BYTES + 1))),
        ]),
    )
    .await;
    assert!(why.contains("the limit is 32768"), "{why}");
    assert!(why.contains("replace and with"), "{why}");
    assert_eq!(on_disk(&root), base);
    // A complete read's sha256 is the file's; it carries no separate file_sha256.
    std::fs::write(root.join("docs/small.txt"), "small\n").unwrap();
    let read = execute(
        &p,
        PROJECT_READ,
        inputs(&[("path", text("docs/small.txt"))]),
    )
    .await;
    let r = head(&read);
    assert_eq!(r["complete"], true);
    assert_eq!(r["sha256"], sha(b"small\n"));
    assert!(r.get("file_sha256").is_none());
    // And an edit of a small file works through the same form.
    let w = edit(
        &p,
        edit_inputs("docs/small.txt", "small", "edited", &sha(b"small\n")),
    )
    .await;
    assert_eq!(w.status, ExecutionStatus::Success, "{}", w.output);
    assert_eq!(
        std::fs::read_to_string(root.join("docs/small.txt")).unwrap(),
        "edited\n"
    );
}
