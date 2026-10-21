//! DISPOSABLE EXPERIMENT (Stack-Agent evaluation), not part of LayerFS.
//! Store-only route (no daemon, no FUSE, no Docker): Init, fork, write,
//! read, checkpoint, merge/publish, process kill, cleanup.
//! Inputs: env EVAL_SOURCE (directory to import), EVAL_RUNS (repetitions).
#[path = "support/installed_store.rs"]
#[allow(dead_code)]
mod installed;
#[path = "support/sdk.rs"]
#[allow(dead_code)]
mod support;
#[path = "support/view.rs"]
#[allow(dead_code)]
mod view;

use layerfs_bridge::control::SdkValue;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;
use support::{write, Fixture};

fn t(v: &str) -> SdkValue {
    SdkValue::text(v)
}

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn dir_bytes(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for e in entries.flatten() {
            let m = e.metadata().unwrap();
            if m.is_dir() {
                total += dir_bytes(&e.path());
            } else {
                total += m.len();
            }
        }
    }
    total
}

fn copy_tree(from: &Path, to: &Path) -> usize {
    let mut n = 0;
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let ft = e.file_type().unwrap();
        let dest = to.join(e.file_name());
        n += 1;
        if ft.is_dir() {
            std::fs::create_dir(&dest).unwrap();
            n += copy_tree(&e.path(), &dest);
        } else if ft.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(e.path()).unwrap(), &dest).unwrap();
        } else {
            std::fs::copy(e.path(), &dest).unwrap();
        }
    }
    n
}

fn read(f: &Fixture, at: &str, path: &str) -> Result<String, String> {
    f.try_call("fs.read", vec![("branch", t(at)), ("path", t(path))])
        .map(|r| r.get_text("text").unwrap().to_owned())
        .map_err(|e| format!("{e:?}"))
}

fn digest(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

fn source() -> PathBuf {
    PathBuf::from(std::env::var("EVAL_SOURCE").expect("EVAL_SOURCE"))
}

/// Fixture::open with a process-private overlay file: a new process opening
/// an existing installed Store (the shared helper reuses one overlay path).
fn open_fresh(host: installed::Fixture, tag: &str) -> Fixture {
    use layerfs_store_service::{bootstrap::open_store_observed, Owner, OwnerConfig, StoreService};
    let config = OwnerConfig::default();
    let opened = open_store_observed(
        host.config.clone(),
        installed::BINDING,
        installed::CURSOR,
        config.namespaces,
        64 * 1024,
        layerfs_storage::ReservationBlocks::default(),
    )
    .unwrap();
    let owner = Owner::start(
        &host.directory.join(format!("overlay-{tag}-{}.sqlite", std::process::id())),
        layerfs_overlay::ProfileConfig::default(),
        config,
    )
    .unwrap();
    let service = StoreService::new(opened, &owner);
    Fixture { host, owner, service }
}

const TARGET: &str = "/crates/chip-core/src/lib.rs";

/// One full trial: every required test once. Prints one JSON line.
fn trial(run: usize) {
    let src = source();
    let t0 = Instant::now();
    let f = Fixture::new(&format!("eval-{run}"), |dest| copy_tree(&src, dest));
    let init_ms = ms(t0);
    let store_after_init = dir_bytes(&f.host.directory);
    let mut fails: Vec<String> = Vec::new();

    let original = std::fs::read_to_string(src.join(TARGET.trim_start_matches('/'))).unwrap();
    // first read
    let t1 = Instant::now();
    let first = read(&f, "main", TARGET).unwrap();
    let first_read_ms = ms(t1);
    if first != original {
        fails.push("first read differs from source".into());
    }

    // fork two candidates
    let t2 = Instant::now();
    f.fork("cand-a");
    f.fork("cand-b");
    let fork2_ms = ms(t2);

    // isolation + exact write
    let payload = format!("{original}\n// candidate-a change {run}\n");
    let t3 = Instant::now();
    f.call(
        "fs.write",
        vec![("branch", t("cand-a")), ("path", t(TARGET)), ("bytes", SdkValue::Bytes(payload.clone().into_bytes()))],
    );
    let write_ms = ms(t3);
    let t4 = Instant::now();
    let back = read(&f, "cand-a", TARGET).unwrap();
    let readback_ms = ms(t4);
    if digest(&back) != digest(&payload) {
        fails.push("exact write read-back digest mismatch".into());
    }
    if read(&f, "cand-b", TARGET).unwrap() != original {
        fails.push("cand-b contaminated".into());
    }
    if read(&f, "main", TARGET).unwrap() != original {
        fails.push("main contaminated".into());
    }
    if std::fs::read_to_string(src.join(TARGET.trim_start_matches('/'))).unwrap() != original {
        fails.push("host source contaminated".into());
    }

    // checkpoint: record head commit of cand-a, change more, fork from checkpoint
    let commits = f.call("branch.list_commits", vec![("branch", t("cand-a"))]);
    let head = &support::items(&commits)[0];
    let checkpoint = head
        .get_text("id")
        .or_else(|| head.get_text("commit"))
        .unwrap_or_else(|| panic!("commit item keys: {head:?}"))
        .to_owned();
    f.apply("cand-a", vec![write(TARGET, "BROKEN\n"), write("/new-file.txt", "junk\n")]);
    let t5 = Instant::now();
    f.call("branch.fork", vec![("name", t("restore-a")), ("from", t(&checkpoint))]);
    let restore_ms = ms(t5);
    if read(&f, "restore-a", TARGET).unwrap() != payload {
        fails.push("checkpoint restore content mismatch".into());
    }
    if read(&f, "restore-a", "/new-file.txt").is_ok() {
        fails.push("checkpoint restore still has later file".into());
    }

    // failed attempt: cand-a is now broken; next attempt forks from main
    f.fork("attempt-2");
    if read(&f, "attempt-2", TARGET).unwrap() != original || read(&f, "attempt-2", "/new-file.txt").is_ok() {
        fails.push("attempt-2 not clean".into());
    }

    // concurrent modification: user branch publishes a change to the same file
    // after the candidate forked; then the candidate tries to publish.
    f.fork("cand-c");
    f.call("fs.write", vec![("branch", t("cand-c")), ("path", t(TARGET)), ("content", t("candidate version\n"))]);
    f.fork("user");
    f.call("fs.write", vec![("branch", t("user")), ("path", t(TARGET)), ("content", t("user version\n"))]);
    let user_pub = f.try_call("branch.publish", vec![("branch", t("user"))]);
    let cand_pub = f.try_call("branch.publish", vec![("branch", t("cand-c"))]);
    f.fork("observer");
    let after = f.try_call("fs.read", vec![("path", t(TARGET)), ("branch", t("observer"))]);
    let silent_overwrite = cand_pub.is_ok() && user_pub.is_ok()
        && after.as_ref().ok().and_then(|v| v.get_text("text")) == Some("candidate version\n");
    // and a merge of the two branches
    let merged = f.try_call(
        "branch.merge",
        vec![("branch", t("cand-c")), ("with", t("user")), ("name", t("m"))],
    );
    // non-conflicting concurrent edit on a different file merges
    f.fork("cand-d");
    f.call("fs.write", vec![("branch", t("cand-d")), ("path", t("/README.md")), ("content", t("cand\n"))]);
    let merged_ok = f.try_call(
        "branch.merge",
        vec![("branch", t("cand-d")), ("with", t("user")), ("name", t("m2"))],
    );
    let publish_conflict = format!("{cand_pub:?}").contains("Conflict");
    let merge_conflict = format!("{merged:?}").contains("Conflict");
    let store_after_changes = dir_bytes(&f.host.directory);

    // cleanup
    let dir = f.host.directory.clone();
    let t6 = Instant::now();
    f.cleanup();
    let cleanup_ms = ms(t6);
    let residual = dir.exists();

    println!(
        "EVAL {{\"run\":{run},\"init_ms\":{init_ms:.1},\"first_read_ms\":{first_read_ms:.2},\"fork2_ms\":{fork2_ms:.2},\
\"write_ms\":{write_ms:.2},\"readback_ms\":{readback_ms:.2},\"restore_fork_ms\":{restore_ms:.2},\"cleanup_ms\":{cleanup_ms:.1},\
\"store_after_init\":{store_after_init},\"store_after_changes\":{store_after_changes},\"residual_dir\":{residual},\
\"publish_conflict\":{publish_conflict},\"merge_conflict\":{merge_conflict},\"publish_silent_overwrite\":{silent_overwrite},\"merge_disjoint_ok\":{},\"fails\":{:?}}}",
        merged_ok.is_ok(),
        fails
    );
    println!(
        "EVAL-DETAIL run={run} user_publish={:?} cand_publish={:?} merge_same_file={:?} read_after={:?} checkpoint={checkpoint}",
        user_pub.as_ref().map(|v| v.get_text("kind").map(str::to_owned)),
        cand_pub.as_ref().map(|v| v.get_text("kind").map(str::to_owned)),
        merged.as_ref().map(|v| v.get_text("root").map(str::to_owned)).map_err(|e| e.code.clone()),
        after.map(|v| v.get_text("text").map(str::to_owned)),
    );
}

#[test]
#[ignore]
fn eval_trials() {
    let runs: usize = std::env::var("EVAL_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    for run in 0..runs {
        trial(run);
    }
}

/// Binary payloads through the Store's read API.
#[test]
#[ignore]
fn eval_binary_probe() {
    let f = Fixture::new("eval-bin", |dest| {
        std::fs::write(dest.join("seed"), b"x").unwrap();
        1
    });
    let bin: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
    let main_write = f.try_call("fs.write", vec![("branch", t("main")), ("path", t("/x")), ("content", t("x"))]);
    println!("EVAL-MAIN-WRITE {:?}", main_write.map_err(|e| e.code));
    f.fork("b");
    f.call("fs.write", vec![("branch", t("b")), ("path", t("/bin")), ("bytes", SdkValue::Bytes(bin.clone()))]);
    let stat = f.call("fs.stat", vec![("branch", t("b")), ("path", t("/bin"))]);
    let r = f.try_call("fs.read", vec![("branch", t("b")), ("path", t("/bin"))]);
    let text = r.as_ref().ok().and_then(|v| v.get_text("text").map(str::to_owned));
    println!(
        "EVAL-BIN size={:?} read={:?} read_len={:?} exact={}",
        stat.get("size"),
        r.as_ref().map(|_| ()).map_err(|e| (e.code.clone(), e.message.clone())),
        text.as_ref().map(|s| s.len()),
        text.as_deref().map(str::as_bytes) == Some(&bin[..])
    );
    // large text: 5 MiB, 100k lines
    let big: String = (0..100_000).map(|i| format!("line {i:08} {}\n", "x".repeat(40))).collect();
    f.call("fs.write", vec![("branch", t("b")), ("path", t("/big")), ("content", t(&big))]);
    let r = f.call("fs.read", vec![("branch", t("b")), ("path", t("/big"))]);
    println!(
        "EVAL-BIG written={} returned={} endLine={:?}",
        big.len(),
        r.get_text("text").unwrap().len(),
        r.get("endLine")
    );
    f.cleanup();
}

/// Child: open an existing installed Store and write a counter forever,
/// acknowledging each returned write on stdout.
#[test]
#[ignore]
fn eval_kill_child() {
    let dir = PathBuf::from(std::env::var("EVAL_KILL_DIR").unwrap());
    let host = installed::Fixture {
        directory: dir.clone(),
        config: layerfs_persistence::PersistenceConfig::sqlite(dir.join("store.sqlite"))
            .with_sqlite_profile(layerfs_persistence::SqlitePersistenceProfile::Disposable)
            .with_sqlite_acquisition(layerfs_persistence::SqliteAcquisitionSchema::Tables),
        branch: layerfs_history::BranchId::from_authority([24; 16]),
        count: 0,
    };
    let f = open_fresh(host, "child");
    let mut out = std::io::stdout();
    for i in 0u64.. {
        let body = format!("{i}\n{}", "y".repeat(65536));
        f.call("fs.write", vec![("branch", t("k")), ("path", t("/counter")), ("content", t(&body))]);
        writeln!(out, "ACK {i}").unwrap();
        out.flush().unwrap();
    }
}

/// Parent: SIGKILL the child mid-stream, reopen, check state.
#[test]
#[ignore]
fn eval_kill_parent() {
    let runs: usize = std::env::var("EVAL_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    for run in 0..runs {
        let f = Fixture::new(&format!("eval-kill-{run}"), |dest| {
            std::fs::write(dest.join("seed"), b"x").unwrap();
            1
        });
        f.fork("k");
        let host = f.close();
        let exe = std::env::current_exe().unwrap();
        let mut child = Command::new(exe)
            .args(["--ignored", "--exact", "eval_kill_child", "--nocapture", "--test-threads=1"])
            .env("EVAL_KILL_DIR", &host.directory)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stop_at = 20 + (run as u64 * 7) % 31;
        let mut last = None;
        for line in BufReader::new(child.stdout.take().unwrap()).lines() {
            let line = line.unwrap();
            if let Some(n) = line.strip_prefix("ACK ") {
                last = Some(n.parse::<u64>().unwrap());
                if last == Some(stop_at) {
                    child.kill().unwrap(); // SIGKILL
                    break;
                }
            }
        }
        child.wait().unwrap();
        let t0 = Instant::now();
        let reopened = std::panic::catch_unwind(|| open_fresh(installed::Fixture {
            directory: host.directory.clone(),
            config: host.config.clone(),
            branch: host.branch,
            count: 0,
        }, "reopen"));
        let reopen_ms = ms(t0);
        match reopened {
            Ok(f) => {
                let value = read(&f, "k", "/counter")
                    .ok()
                    .and_then(|s| s.lines().next().and_then(|l| l.parse::<u64>().ok()));
                let size = f
                    .try_call("fs.stat", vec![("branch", t("k")), ("path", t("/counter"))])
                    .ok()
                    .and_then(|v| match v.get("size") { Some(SdkValue::Int(n)) => Some(*n as usize), _ => None });
                let well_formed = value.is_some_and(|v| size == Some(format!("{v}\n").len() + 65536));
                let writable = f
                    .try_call("fs.write", vec![("branch", t("k")), ("path", t("/after")), ("content", t("ok\n"))])
                    .is_ok();
                println!(
                    "EVAL-KILL run={run} last_ack={last:?} recovered={value:?} acked_survived={} well_formed={well_formed} writable_after={writable} reopen_ms={reopen_ms:.1}",
                    value.zip(last).is_some_and(|(v, l)| v >= l)
                );
                f.cleanup();
            }
            Err(_) => {
                println!("EVAL-KILL run={run} last_ack={last:?} REOPEN_FAILED");
                host.cleanup();
            }
        }
    }
}
