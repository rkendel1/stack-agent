//! The project lock: one work per directory across processes, and the removal of temporary files a
//! crashed write left behind.

use std::path::PathBuf;

use chip_project::{LockError, ProjectLock};

fn project(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("chip-lock-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("project/src")).unwrap();
    base.join("project")
}

#[test]
fn a_second_holder_is_refused_until_the_first_releases() {
    let root = project("exclusive");
    let first = ProjectLock::acquire(&root).unwrap();
    // A separate open of the lock file, as a second process would make: the OS lock refuses it.
    assert!(matches!(ProjectLock::acquire(&root), Err(LockError::Held)));
    let message = ProjectLock::acquire(&root).unwrap_err().to_string();
    assert!(message.contains("another stack-agent work"), "{message}");
    assert!(
        !message.contains(root.to_str().unwrap()),
        "names no host path"
    );
    drop(first);
    ProjectLock::acquire(&root).expect("released when dropped");
}

#[cfg(unix)]
#[test]
fn locking_creates_nothing_so_a_broken_stack_agent_directory_does_not_stop_work() {
    let root = project("nothing-created");
    let lock = ProjectLock::acquire(&root).unwrap();
    assert!(
        !root.join(".stack-agent").exists(),
        "the root itself is locked"
    );
    drop(lock);
    // Learning storage failing (here `.stack-agent` is a file) must never change the outcome, so
    // it must not stop the lock either.
    std::fs::write(root.join(".stack-agent"), "not a directory").unwrap();
    ProjectLock::acquire(&root).expect("locked regardless of .stack-agent");
}

#[test]
fn temporary_files_an_interrupted_write_left_are_removed_and_reported() {
    let root = project("sweep");
    std::fs::create_dir_all(root.join("src/deep/er")).unwrap();
    std::fs::create_dir_all(root.join("target/debug")).unwrap();
    for stray in [".chip-write-11-0.tmp", "src/deep/er/.chip-write-12-3.tmp"] {
        std::fs::write(root.join(stray), "partial").unwrap();
    }
    // Not Chip's: kept.
    std::fs::write(root.join("src/.chip-write-notes.md"), "keep").unwrap();
    std::fs::write(
        root.join("target/debug/.chip-write-1-1.tmp"),
        "build output",
    )
    .unwrap();
    let lock = ProjectLock::acquire(&root).unwrap();
    let mut recovered = lock.recovered.clone();
    recovered.sort();
    assert_eq!(
        recovered,
        [".chip-write-11-0.tmp", "src/deep/er/.chip-write-12-3.tmp"]
    );
    assert!(lock.sweep_complete);
    assert!(!root.join(".chip-write-11-0.tmp").exists());
    assert!(root.join("src/.chip-write-notes.md").exists());
    assert!(root.join("target/debug/.chip-write-1-1.tmp").exists());
}

#[cfg(unix)]
#[test]
fn a_planted_link_cannot_move_the_lock_or_the_sweep_outside_the_project() {
    let root = project("planted");
    let outside = root.parent().unwrap().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join(".chip-write-1-1.tmp"), "not ours").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("src/linked")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join(".stack-agent")).unwrap();
    let lock = ProjectLock::acquire(&root).unwrap();
    assert!(lock.recovered.is_empty());
    assert!(
        outside.join(".chip-write-1-1.tmp").exists(),
        "the sweep did not follow the link"
    );
    let entries: Vec<_> = std::fs::read_dir(&outside).unwrap().flatten().collect();
    assert_eq!(entries.len(), 1, "nothing was created through the link");
}
