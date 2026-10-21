//! `project.write`'s precondition (roadmap P0-09): a write based on contents that are no longer
//! there is refused as `stale_base`, and the newer contents survive byte for byte. Every case goes
//! through `Executor::execute`, the production path, against the real filesystem.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chip_core::{
    CapabilityId, CapabilityProvider, ExecutionError, ExecutionId, ExecutionRequest,
    ExecutionResult, ExecutionStatus, Executor, InputValue, Observation, ObservationKind,
};
use chip_project::{
    PROJECT_READ, PROJECT_WRITE, ProjectExecutor, STALE_BASE, checked_write, stale_write,
    write_summary,
};
use sha2::{Digest, Sha256};

fn project(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("chip-precondition-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("project/src")).unwrap();
    std::fs::create_dir_all(base.join("outside")).unwrap();
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

fn write_inputs(
    path: &str,
    content: &str,
    extra: &[(&str, InputValue)],
) -> BTreeMap<String, InputValue> {
    let mut inputs: BTreeMap<String, InputValue> = [
        ("path".to_string(), text(path)),
        ("content".to_string(), text(content)),
    ]
    .into();
    for (name, value) in extra {
        inputs.insert(name.to_string(), value.clone());
    }
    inputs
}

async fn execute(
    p: &ProjectExecutor,
    capability: &str,
    inputs: BTreeMap<String, InputValue>,
) -> Result<ExecutionResult, ExecutionError> {
    p.execute(ExecutionRequest::new(ExecutionId::new("e"), capability).with_inputs(inputs))
        .await
}

async fn write(
    p: &ProjectExecutor,
    path: &str,
    content: &str,
    extra: &[(&str, InputValue)],
) -> ExecutionResult {
    execute(p, PROJECT_WRITE, write_inputs(path, content, extra))
        .await
        .unwrap()
}

fn expecting(digest: &str) -> [(&'static str, InputValue); 1] {
    [("expected_sha256", text(digest))]
}

fn absent() -> [(&'static str, InputValue); 1] {
    [("expect_absent", InputValue::Bool(true))]
}

fn head(r: &ExecutionResult) -> serde_json::Value {
    serde_json::from_str(r.output.lines().next().unwrap()).unwrap()
}

fn no_temporaries(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .all(|e| !e.file_name().to_string_lossy().starts_with(".chip-write-"))
}

fn observed(r: &ExecutionResult) -> Observation {
    Observation {
        execution_id: ExecutionId::new("e"),
        kind: match r.status {
            ExecutionStatus::Success => ObservationKind::ExecutionCompleted,
            _ => ObservationKind::ExecutionFailed,
        },
        status: r.status.clone(),
        output: Some(r.output.clone()),
        receipt_id: None,
        evidence: None,
    }
}

/// Asserts a `stale_base` refusal that names `found` and carries nothing of the file.
fn assert_stale(r: &ExecutionResult, found: &str, file: &[u8]) {
    assert_eq!(r.status, ExecutionStatus::Failure, "{}", r.output);
    let v = head(r);
    assert_eq!(v["capability"], PROJECT_WRITE);
    assert_eq!(v["error"], STALE_BASE, "{}", r.output);
    assert_eq!(v["found"], found, "{}", r.output);
    assert!(v["hint"].as_str().unwrap().contains("project.read"));
    // Neither the newer content nor its digest: the model learns the new state by reading it.
    assert_eq!(r.output.lines().count(), 1, "{}", r.output);
    if !file.is_empty() {
        let shown = String::from_utf8_lossy(file);
        assert!(!r.output.contains(shown.trim()), "{}", r.output);
    }
    assert!(!r.output.contains(&sha(file)) || sha(file) == v["expected_sha256"]);
    assert!(v.get("bytes_written").is_none() && v.get("changed").is_none());
}

#[tokio::test]
async fn the_digest_project_read_reports_is_the_one_a_write_expects() {
    let root = project("roundtrip");
    std::fs::write(root.join("src/lib.rs"), "pub fn one() -> u32 { 1 }\n").unwrap();
    let p = ProjectExecutor::new(&root);
    let read = execute(
        &p,
        PROJECT_READ,
        [("path".to_string(), text("src/lib.rs"))].into(),
    )
    .await
    .unwrap();
    let read_head = head(&read);
    assert_eq!(read_head["complete"], true);
    let digest = read_head["sha256"].as_str().unwrap().to_string();

    let r = write(
        &p,
        "src/lib.rs",
        "pub fn one() -> u32 { 2 }\n",
        &expecting(&digest),
    )
    .await;
    assert_eq!(r.status, ExecutionStatus::Success, "{}", r.output);
    let v = head(&r);
    assert_eq!(v["precondition"], "sha256");
    assert_eq!(
        (v["operation"].as_str(), v["changed"].as_bool()),
        (Some("replaced"), Some(true))
    );
    assert_eq!(
        std::fs::read(root.join("src/lib.rs")).unwrap(),
        b"pub fn one() -> u32 { 2 }\n"
    );
    // A successful checked write is a write, and it is the checked kind.
    let o = observed(&r);
    assert_eq!(write_summary(&o), Some(("src/lib.rs".to_string(), true)));
    assert_eq!(checked_write(&o).as_deref(), Some("src/lib.rs"));
    assert_eq!(stale_write(&o), None);

    // The digest it was based on is now stale: the same request again is refused, never refreshed.
    let again = write(
        &p,
        "src/lib.rs",
        "pub fn one() -> u32 { 3 }\n",
        &expecting(&digest),
    )
    .await;
    assert_stale(&again, "different_content", b"pub fn one() -> u32 { 2 }\n");
    let twice = write(
        &p,
        "src/lib.rs",
        "pub fn one() -> u32 { 3 }\n",
        &expecting(&digest),
    )
    .await;
    assert_eq!(
        twice.output, again.output,
        "the same stale request, the same refusal"
    );
    assert_eq!(
        std::fs::read(root.join("src/lib.rs")).unwrap(),
        b"pub fn one() -> u32 { 2 }\n"
    );
}

#[tokio::test]
async fn a_write_without_a_precondition_behaves_as_it_always_did() {
    let root = project("compatible");
    let p = ProjectExecutor::new(&root);
    let r = write(&p, "a.txt", "one\n", &[]).await;
    assert_eq!(r.status, ExecutionStatus::Success);
    std::fs::write(root.join("a.txt"), "someone else\n").unwrap();
    let r = write(&p, "a.txt", "two\n", &[]).await;
    assert_eq!(r.status, ExecutionStatus::Success);
    let v = head(&r);
    // The observation is exactly the shape it was: no precondition field appears.
    assert!(v.get("precondition").is_none(), "{v}");
    let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "bytes_written",
            "capability",
            "changed",
            "operation",
            "path",
            "sha256"
        ]
    );
    assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"two\n");
    assert_eq!(checked_write(&observed(&r)), None);
}

#[tokio::test]
async fn a_stale_digest_is_refused_and_the_newer_file_is_untouched() {
    let root = project("stale");
    std::fs::write(root.join("f.txt"), "what the agent read\n").unwrap();
    let p = ProjectExecutor::new(&root);
    let base = sha(b"what the agent read\n");
    std::fs::write(root.join("f.txt"), "the user's newer edit\n").unwrap();
    let before = std::fs::metadata(root.join("f.txt")).unwrap();

    let r = write(&p, "f.txt", "the agent's change\n", &expecting(&base)).await;
    assert_stale(&r, "different_content", b"the user's newer edit\n");
    assert_eq!(head(&r)["expected_sha256"], base);
    assert_eq!(head(&r)["precondition"], "sha256");
    assert_eq!(
        std::fs::read(root.join("f.txt")).unwrap(),
        b"the user's newer edit\n"
    );
    // Not replaced at all: the same file, not an identical copy.
    let after = std::fs::metadata(root.join("f.txt")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    }
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    assert!(no_temporaries(&root));
    // What the trajectory helpers make of it: a conflict, never a write.
    let o = observed(&r);
    assert_eq!(stale_write(&o).as_deref(), Some("f.txt"));
    assert_eq!(write_summary(&o), None);
    assert_eq!(checked_write(&o), None);
}

#[tokio::test]
async fn files_are_compared_by_their_exact_bytes() {
    let root = project("bytes");
    let p = ProjectExecutor::new(&root);

    // Not UTF-8: project.read refuses it, but the precondition is about bytes, not text.
    let binary: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
    std::fs::write(root.join("blob.bin"), &binary).unwrap();
    let lossy = String::from_utf8_lossy(&binary).into_owned();
    let r = write(
        &p,
        "blob.bin",
        "text now\n",
        &expecting(&sha(lossy.as_bytes())),
    )
    .await;
    assert_stale(&r, "different_content", b"");
    assert_eq!(std::fs::read(root.join("blob.bin")).unwrap(), binary);
    let r = write(&p, "blob.bin", "text now\n", &expecting(&sha(&binary))).await;
    assert_eq!(r.status, ExecutionStatus::Success, "{}", r.output);
    assert_eq!(std::fs::read(root.join("blob.bin")).unwrap(), b"text now\n");

    // Empty: the digest of no bytes is a digest like any other, and it is not "absent".
    std::fs::write(root.join("empty.txt"), "").unwrap();
    let r = write(&p, "empty.txt", "x\n", &absent()).await;
    assert_stale(&r, "exists", b"");
    let r = write(&p, "empty.txt", "x\n", &expecting(&sha(b""))).await;
    assert_eq!(r.status, ExecutionStatus::Success, "{}", r.output);

    // Larger than anything one read or write carries: hashed whole, not a prefix of it.
    let large: Vec<u8> = (0..(1usize << 20)).map(|i| (i % 251) as u8).collect();
    std::fs::write(root.join("large.dat"), &large).unwrap();
    let mut prefix_only = large.clone();
    prefix_only.truncate(32 * 1024 + 1);
    let r = write(&p, "large.dat", "small\n", &expecting(&sha(&prefix_only))).await;
    assert_stale(&r, "different_content", b"");
    let mut last_byte = large.clone();
    *last_byte.last_mut().unwrap() ^= 1;
    let r = write(&p, "large.dat", "small\n", &expecting(&sha(&last_byte))).await;
    assert_stale(&r, "different_content", b"");
    assert_eq!(std::fs::read(root.join("large.dat")).unwrap(), large);
    let r = write(&p, "large.dat", "small\n", &expecting(&sha(&large))).await;
    assert_eq!(r.status, ExecutionStatus::Success, "{}", r.output);
    assert_eq!(std::fs::read(root.join("large.dat")).unwrap(), b"small\n");
    assert!(no_temporaries(&root));
}

#[tokio::test]
async fn expecting_absence_creates_only_while_nothing_is_there() {
    let root = project("absent");
    let p = ProjectExecutor::new(&root);
    let r = write(&p, "src/new.rs", "pub fn new() {}\n", &absent()).await;
    assert_eq!(r.status, ExecutionStatus::Success, "{}", r.output);
    let v = head(&r);
    assert_eq!(
        (v["operation"].as_str(), v["precondition"].as_str()),
        (Some("created"), Some("absent"))
    );
    // The file now exists: the same request is refused rather than overwriting what it made.
    let r = write(&p, "src/new.rs", "pub fn other() {}\n", &absent()).await;
    assert_stale(&r, "exists", b"pub fn new() {}\n");
    // Someone else's file is not overwritten either.
    std::fs::write(root.join("theirs.txt"), "their own content\n").unwrap();
    let r = write(&p, "theirs.txt", "mine\n", &absent()).await;
    assert_stale(&r, "exists", b"their own content\n");
    assert_eq!(
        std::fs::read(root.join("theirs.txt")).unwrap(),
        b"their own content\n"
    );
    // A directory there is something there: refused, and nothing written.
    let r = write(&p, "src", "x\n", &absent()).await;
    assert_stale(&r, "exists", b"");
    assert!(root.join("src").is_dir());
    // A missing directory is no conflict: the file is absent as expected, and cannot be made.
    let r = write(&p, "nowhere/new.rs", "x\n", &absent()).await;
    assert_eq!(head(&r)["error"], "parent_missing");
    assert!(!root.join("nowhere").exists());
    assert!(no_temporaries(&root) && no_temporaries(&root.join("src")));
}

#[tokio::test]
async fn a_deleted_or_replaced_file_is_judged_by_what_is_there_now() {
    let root = project("replaced");
    let p = ProjectExecutor::new(&root);
    let base = sha(b"original\n");

    // Deleted since it was read: refused, and not recreated.
    std::fs::write(root.join("gone.txt"), "original\n").unwrap();
    std::fs::remove_file(root.join("gone.txt")).unwrap();
    let r = write(&p, "gone.txt", "agent\n", &expecting(&base)).await;
    assert_stale(&r, "absent", b"");
    assert!(!root.join("gone.txt").exists());

    // Recreated with different bytes: refused.
    std::fs::write(root.join("f.txt"), "original\n").unwrap();
    std::fs::remove_file(root.join("f.txt")).unwrap();
    std::fs::write(root.join("f.txt"), "recreated\n").unwrap();
    let r = write(&p, "f.txt", "agent\n", &expecting(&base)).await;
    assert_stale(&r, "different_content", b"recreated\n");
    assert_eq!(std::fs::read(root.join("f.txt")).unwrap(), b"recreated\n");

    // Recreated with the very bytes the write was based on: the contents are what was read, so
    // the write is based on what is there. The precondition is about contents, not identity.
    std::fs::remove_file(root.join("f.txt")).unwrap();
    std::fs::write(root.join("f.txt"), "original\n").unwrap();
    let r = write(&p, "f.txt", "agent\n", &expecting(&base)).await;
    assert_eq!(r.status, ExecutionStatus::Success, "{}", r.output);
    assert_eq!(std::fs::read(root.join("f.txt")).unwrap(), b"agent\n");

    // Now a directory: not the file that was read.
    std::fs::remove_file(root.join("f.txt")).unwrap();
    std::fs::create_dir(root.join("f.txt")).unwrap();
    let r = write(&p, "f.txt", "agent\n", &expecting(&base)).await;
    assert_stale(&r, "not_a_file", b"");
    assert!(root.join("f.txt").is_dir());

    // Gone together with its directory: not recreated, and the directory is not made either.
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/g.txt"), "original\n").unwrap();
    std::fs::remove_dir_all(root.join("sub")).unwrap();
    let r = write(&p, "sub/g.txt", "agent\n", &expecting(&base)).await;
    assert_stale(&r, "absent", b"");
    assert!(!root.join("sub").exists());
    // Without a precondition that is what it always was.
    let r = write(&p, "sub/g.txt", "agent\n", &[]).await;
    assert_eq!(head(&r)["error"], "parent_missing");
}

#[tokio::test]
async fn malformed_preconditions_are_refused_before_anything_runs() {
    let root = project("malformed");
    std::fs::write(root.join("f.txt"), "original\n").unwrap();
    let p = ProjectExecutor::new(&root);
    let id = CapabilityId::new(PROJECT_WRITE).unwrap();
    let good = sha(b"original\n");
    let cases: Vec<(&str, Vec<(&str, InputValue)>)> = vec![
        (
            "uppercase",
            vec![("expected_sha256", text(&good.to_uppercase()))],
        ),
        ("short", vec![("expected_sha256", text(&good[..63]))]),
        ("long", vec![("expected_sha256", text(&format!("{good}0")))]),
        ("not hex", vec![("expected_sha256", text(&"g".repeat(64)))]),
        ("empty", vec![("expected_sha256", text(""))]),
        (
            "prefixed",
            vec![("expected_sha256", text(&format!("sha256:{good}")))],
        ),
        ("integer", vec![("expected_sha256", InputValue::Integer(1))]),
        ("false", vec![("expect_absent", InputValue::Bool(false))]),
        ("text true", vec![("expect_absent", text("true"))]),
        (
            "both",
            vec![
                ("expected_sha256", text(&good)),
                ("expect_absent", InputValue::Bool(true)),
            ],
        ),
        ("undeclared", vec![("expected_sha", text(&good))]),
    ];
    for (what, extra) in cases {
        let inputs = write_inputs("f.txt", "changed\n", &extra);
        assert!(
            p.validate_inputs(&id, &inputs).await.is_err(),
            "{what}: accepted by validation"
        );
        match execute(&p, PROJECT_WRITE, inputs).await {
            Err(ExecutionError::InvalidRequest(_)) => {}
            other => panic!("{what}: {other:?}"),
        }
        assert_eq!(
            std::fs::read(root.join("f.txt")).unwrap(),
            b"original\n",
            "{what}: the file changed"
        );
    }
    assert!(no_temporaries(&root));
    // Well-formed, each alone, is accepted.
    for extra in [expecting(&good), absent()] {
        assert!(
            p.validate_inputs(&id, &write_inputs("f.txt", "x", &extra))
                .await
                .is_ok()
        );
    }
}

#[tokio::test]
async fn a_precondition_grants_no_path_authority() {
    let root = project("authority");
    let outside = root.parent().unwrap().join("outside");
    std::fs::write(outside.join("secret.txt"), "outside secret\n").unwrap();
    let secret = sha(b"outside secret\n");
    let p = ProjectExecutor::new(&root);
    for path in [
        "../outside/secret.txt",
        "/etc/passwd",
        "src/../../outside/secret.txt",
    ] {
        for extra in [expecting(&secret), absent()] {
            match execute(&p, PROJECT_WRITE, write_inputs(path, "x\n", &extra)).await {
                Err(ExecutionError::InvalidRequest(_)) => {}
                other => panic!("{path}: {other:?}"),
            }
        }
    }
    #[cfg(unix)]
    {
        // A symlink at the name, even one whose target's digest is the one given: refused as a
        // symlink, and the file it points at is untouched. A dangling one is not created through.
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(outside.join("new.txt"), root.join("dangling.txt")).unwrap();
        for (path, extra) in [("link.txt", expecting(&secret)), ("dangling.txt", absent())] {
            match execute(&p, PROJECT_WRITE, write_inputs(path, "x\n", &extra)).await {
                Err(ExecutionError::InvalidRequest(why)) => {
                    assert!(why.contains("symbolic link"), "{why}")
                }
                other => panic!("{path}: {other:?}"),
            }
        }
        assert!(!outside.join("new.txt").exists());

        // A second name for a file outside: its content is never hashed, so a precondition cannot
        // probe it, and nothing is written.
        std::fs::hard_link(outside.join("secret.txt"), root.join("hard.txt")).unwrap();
        let r = write(&p, "hard.txt", "x\n", &expecting(&secret)).await;
        assert_stale(&r, "hard_linked", b"");
    }
    assert_eq!(
        std::fs::read(outside.join("secret.txt")).unwrap(),
        b"outside secret\n"
    );
}
