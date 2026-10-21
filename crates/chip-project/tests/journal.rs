//! What a keyed `project.write` leaves for the project to answer from, and what an unkeyed one
//! leaves (nothing).
#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;

use chip_core::{ExecutionId, ExecutionRequest, Executor, InputValue};
use chip_project::journal::{bar, inspect, temp_name};
use chip_project::{PROJECT_WRITE, ProjectExecutor, ProjectLock};

const KEY: &str = "ak1.0123456789abcdef0123456789abcdef";

fn project(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-journal-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn request(content: &str, key: Option<&str>) -> ExecutionRequest {
    let inputs: BTreeMap<String, InputValue> = [
        ("path".to_string(), InputValue::Text("a.txt".into())),
        ("content".to_string(), InputValue::Text(content.into())),
    ]
    .into();
    let mut r = ExecutionRequest::new(ExecutionId::new("e"), PROJECT_WRITE).with_inputs(inputs);
    r.idempotency_key = key.map(str::to_string);
    r
}

#[tokio::test]
async fn a_keyed_write_leaves_the_file_it_staged_as_the_target() {
    let root = project("keyed");
    let r = ProjectExecutor::new(&root)
        .execute(request("one\n", Some(KEY)))
        .await
        .unwrap();
    assert!(r.output.contains("\"created\""), "{}", r.output);
    let lock = ProjectLock::acquire(&root).unwrap();
    let state = inspect(&lock, &root, KEY, "a.txt").unwrap();
    let (prepared, target) = (state.prepared.unwrap(), state.target.unwrap());
    assert_eq!((prepared.dev, prepared.ino), (target.dev, target.ino));
    assert_eq!(prepared.sha256, target.sha256);
    assert!(!state.barred && !state.temp_present);
    // Replacing the file by anyone else breaks the identity, whatever it now contains.
    drop(lock);
    std::fs::remove_file(root.join("a.txt")).unwrap();
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    let lock = ProjectLock::acquire(&root).unwrap();
    let state = inspect(&lock, &root, KEY, "a.txt").unwrap();
    let t = state.target.unwrap();
    assert_ne!((t.dev, t.ino), (prepared.dev, prepared.ino));
    assert_eq!(t.sha256, prepared.sha256, "same bytes, not the same write");
}

#[tokio::test]
async fn an_unkeyed_write_leaves_no_journal() {
    let root = project("unkeyed");
    ProjectExecutor::new(&root)
        .execute(request("one\n", None))
        .await
        .unwrap();
    assert!(!root.join(".stack-agent").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "one\n"
    );
}

#[tokio::test]
async fn a_lock_holders_sweep_bars_the_key_of_a_staged_file_it_removes() {
    let root = project("sweep");
    let staged = root.join(temp_name(KEY));
    std::fs::write(&staged, "staged\n").unwrap();
    let lock = ProjectLock::acquire(&root).unwrap();
    assert!(!staged.exists());
    assert!(inspect(&lock, &root, KEY, "a.txt").unwrap().barred);
    drop(lock);
    // The barred operation can never apply.
    let r = ProjectExecutor::new(&root)
        .execute(request("staged\n", Some(KEY)))
        .await
        .unwrap();
    assert!(r.output.contains("operation_barred"), "{}", r.output);
    assert!(!root.join("a.txt").exists());
}

#[tokio::test]
async fn a_key_is_one_write_and_it_is_applied_once() {
    let root = project("once");
    let exec = ProjectExecutor::new(&root);
    exec.execute(request("one\n", Some(KEY))).await.unwrap();
    let inode = std::fs::metadata(root.join("a.txt")).unwrap().ino();
    let again = exec.execute(request("one\n", Some(KEY))).await.unwrap();
    assert!(again.output.contains("already_applied"), "{}", again.output);
    let other = exec.execute(request("two\n", Some(KEY))).await.unwrap();
    assert!(
        other.output.contains("key_bound_elsewhere"),
        "{}",
        other.output
    );
    let meta = std::fs::metadata(root.join("a.txt")).unwrap();
    assert_eq!(meta.ino(), inode);
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "one\n"
    );
    // Barring after the fact is idempotent and does not undo a committed write's identity.
    let lock = ProjectLock::acquire(&root).unwrap();
    bar(&lock, &root, KEY, "a.txt").unwrap();
    bar(&lock, &root, KEY, "a.txt").unwrap();
}
