//! Adversarial filesystem tests: a project that fights back. Hard links to files outside the root,
//! FIFOs, and a second process swapping directories and files for symbolic links while capabilities
//! run. On Unix the walk holds directory descriptors and never follows a link, so none of these
//! reach outside the root; the race tests would fail against a check-then-open implementation.
#![cfg(unix)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chip_core::{ExecutionId, ExecutionRequest, ExecutionStatus, Executor, InputValue};
use chip_project::{PROJECT_READ, PROJECT_SEARCH, PROJECT_WRITE, ProjectExecutor};

const OUTSIDE: &str = "OUTSIDE-the-project-secret";

struct Fixture {
    root: PathBuf,
    outside: PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let base = std::env::temp_dir().join(format!("chip-confine-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (root, outside) = (base.join("project"), base.join("outside"));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("data.txt"), format!("{OUTSIDE}\n")).unwrap();
    Fixture { root, outside }
}

fn text(pairs: &[(&str, &str)]) -> BTreeMap<String, InputValue> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), InputValue::Text(v.to_string())))
        .collect()
}

/// `(succeeded, output)`, or `None` when the request was refused as invalid.
async fn run(
    p: &ProjectExecutor,
    capability: &str,
    inputs: BTreeMap<String, InputValue>,
) -> Option<(bool, String)> {
    p.execute(ExecutionRequest::new(ExecutionId::new("e"), capability).with_inputs(inputs))
        .await
        .ok()
        .map(|r| (r.status == ExecutionStatus::Success, r.output))
}

#[tokio::test]
async fn a_hard_link_to_a_file_outside_the_project_is_never_read_or_searched() {
    let f = fixture("hardlink");
    std::fs::hard_link(f.outside.join("data.txt"), f.root.join("src/notes.txt")).unwrap();
    std::fs::write(
        f.root.join("src/lib.rs"),
        format!("// {OUTSIDE} is not here\n"),
    )
    .unwrap();
    let p = ProjectExecutor::new(&f.root);
    let (ok, out) = run(&p, PROJECT_READ, text(&[("path", "src/notes.txt")]))
        .await
        .unwrap();
    assert!(!ok && out.contains("hard_linked"), "{out}");
    assert!(!out.contains(OUTSIDE));
    let (ok, out) = run(&p, PROJECT_SEARCH, text(&[("query", "OUTSIDE")]))
        .await
        .unwrap();
    assert!(ok, "{out}");
    assert!(out.contains("\"skipped_hard_linked\":1"), "{out}");
    assert!(
        out.contains("src/lib.rs:1:") && !out.contains("notes.txt"),
        "{out}"
    );
}

#[tokio::test]
async fn a_fifo_is_refused_without_blocking_the_work() {
    let f = fixture("fifo");
    let fifo = f.root.join("src/pipe");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status();
    if !made.is_ok_and(|s| s.success()) {
        eprintln!("SKIPPED: mkfifo unavailable");
        return;
    }
    let p = ProjectExecutor::new(&f.root);
    let started = Instant::now();
    // A blocking open of a FIFO with no writer would never return.
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run(&p, PROJECT_READ, text(&[("path", "src/pipe")])),
    )
    .await
    .expect("the read returned instead of waiting on the FIFO");
    let (ok, out) = result.unwrap();
    assert!(!ok && out.contains("not_a_file"), "{out}");
    let (ok, _) = tokio::time::timeout(
        Duration::from_secs(5),
        run(&p, PROJECT_SEARCH, text(&[("query", "x")])),
    )
    .await
    .expect("the search skipped the FIFO")
    .unwrap();
    assert!(ok);
    assert!(started.elapsed() < Duration::from_secs(5));
    let (ok, out) = run(
        &p,
        PROJECT_WRITE,
        text(&[("path", "src/pipe"), ("content", "x")]),
    )
    .await
    .unwrap();
    assert!(!ok && out.contains("not_a_file"), "{out}");
}

/// Alternates `path` between the real entry (kept at `path.parked` while swapped out) and a
/// symbolic link to `target`, holding each state for a moment, until stopped. Returns the swaps.
fn swapper(
    path: PathBuf,
    target: PathBuf,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<usize> {
    std::thread::spawn(move || {
        let parked = path.with_extension("parked");
        let mut swaps = 0;
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_micros(300));
            std::fs::rename(&path, &parked).unwrap();
            std::os::unix::fs::symlink(&target, &path).unwrap();
            std::thread::sleep(Duration::from_micros(300));
            std::fs::remove_file(&path).unwrap();
            std::fs::rename(&parked, &path).unwrap();
            swaps += 1;
        }
        swaps
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_directory_swapped_for_a_link_mid_operation_never_leads_outside_the_root() {
    let f = fixture("race-dir");
    let inner = f.root.join("src/inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("data.txt"), "inside\n").unwrap();
    let p = ProjectExecutor::new(&f.root);
    let stop = Arc::new(AtomicBool::new(false));
    let handle = swapper(inner.clone(), f.outside.clone(), stop.clone());
    let deadline = Instant::now() + Duration::from_secs(2);
    let (mut reads, mut writes, mut inside) = (0, 0, 0);
    while Instant::now() < deadline {
        if let Some((ok, out)) =
            run(&p, PROJECT_READ, text(&[("path", "src/inner/data.txt")])).await
        {
            assert!(
                !out.contains(OUTSIDE),
                "read through the swapped link: {out}"
            );
            inside += usize::from(ok);
        }
        reads += 1;
        let _ = run(
            &p,
            PROJECT_WRITE,
            text(&[("path", "src/inner/planted.txt"), ("content", "planted\n")]),
        )
        .await;
        writes += 1;
        assert!(
            !f.outside.join("planted.txt").exists(),
            "a write went through the swapped link"
        );
    }
    stop.store(true, Ordering::Relaxed);
    let swaps = handle.join().unwrap();
    assert!(swaps > 10, "the race really ran ({swaps} swaps)");
    assert!(reads > 10 && writes > 10);
    assert!(inside > 0, "reads succeeded while the directory was real");
    let leftovers: Vec<_> = std::fs::read_dir(&f.outside)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(
        leftovers,
        [std::ffi::OsString::from("data.txt")],
        "nothing landed outside"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_swapped_for_a_link_mid_read_never_returns_the_target() {
    let f = fixture("race-file");
    let file = f.root.join("src/data.txt");
    std::fs::write(&file, "inside\n").unwrap();
    let p = ProjectExecutor::new(&f.root);
    let stop = Arc::new(AtomicBool::new(false));
    let handle = swapper(file.clone(), f.outside.join("data.txt"), stop.clone());
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut reads = 0;
    while Instant::now() < deadline {
        if let Some((_, out)) = run(&p, PROJECT_READ, text(&[("path", "src/data.txt")])).await {
            assert!(
                !out.contains(OUTSIDE),
                "read through the swapped link: {out}"
            );
        }
        if let Some((_, out)) = run(&p, PROJECT_SEARCH, text(&[("query", "OUTSIDE")])).await {
            assert!(
                !out.contains("data.txt"),
                "searched through the swapped link: {out}"
            );
        }
        reads += 1;
    }
    stop.store(true, Ordering::Relaxed);
    assert!(handle.join().unwrap() > 10);
    assert!(reads > 10);
    assert_eq!(
        std::fs::read_to_string(f.outside.join("data.txt")).unwrap(),
        format!("{OUTSIDE}\n"),
        "the target was never written"
    );
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn a_replaced_file_keeps_its_permissions_and_no_temporary_file_survives() {
    let f = fixture("mode");
    let private = f.root.join("src/private.rs");
    std::fs::write(&private, "old\n").unwrap();
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600)).unwrap();
    let p = ProjectExecutor::new(&f.root);
    let (ok, out) = run(
        &p,
        PROJECT_WRITE,
        text(&[("path", "src/private.rs"), ("content", "new\n")]),
    )
    .await
    .unwrap();
    assert!(ok, "{out}");
    assert_eq!(mode(&private), 0o600, "never widened to the default");
    let (ok, _) = run(
        &p,
        PROJECT_WRITE,
        text(&[("path", "src/fresh.rs"), ("content", "x\n")]),
    )
    .await
    .unwrap();
    assert!(ok);
    assert_eq!(mode(&f.root.join("src/fresh.rs")), 0o644);
    let stray: Vec<_> = std::fs::read_dir(f.root.join("src"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(".chip-write-"))
        .collect();
    assert!(stray.is_empty());
}
