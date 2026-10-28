//! `--abort-in-flight`: a stop request while a test run is in flight abandons it and stops its whole
//! process group, so nothing is left holding the project. A shim stands in for PAX; its "test run"
//! starts a long-lived grandchild, as `cargo` starting a test binary would. Unix only.
#![cfg(unix)]

#[path = "common/mod.rs"]
mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn setup(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("chip-abort-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("project/src")).unwrap();
    std::fs::write(
        dir.join("project/Cargo.toml"),
        "[package]\nname=\"p\"\nversion=\"0.1.0\"\nedition=\"2021\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("project/src/lib.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
    let pidfile = dir.join("grandchild.pid");
    let shim = dir.join("pax");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\ncase \"$*\" in *--version*) echo 'pax 0.5.0'; exit 0;; esac\nsleep 60 &\necho $! > {}\nwait\n",
            pidfile.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, shim, pidfile)
}

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

async fn run(tag: &str, abort: bool) -> (Duration, Option<i32>, bool) {
    let (dir, shim, pidfile) = setup(tag);
    let reply = serde_json::json!({
        "id": "r",
        "choices": [{"message": {"role": "assistant",
            "content": "{\"decision\":\"request_capability\",\"capability\":\"pax.test\"}"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1},
    })
    .to_string();
    let server = common::start(200, &reply, Duration::ZERO).await;
    let url = server.url.clone();
    tokio::task::spawn_blocking(move || {
        let mut c = Command::new(env!("CARGO_BIN_EXE_stack-agent"));
        c.args([
            "work",
            "Check it.",
            "--kind",
            "verify",
            "--json",
            "--no-state",
            "--no-project-files",
        ])
        .args(["--max-turns", "4", "--test-timeout-secs", "20"])
        .current_dir(dir.join("project"))
        .env("PAX_BIN", &shim)
        .env("CHIP_PROVIDER", "openai-compatible")
        .env("CHIP_MODEL", "mock-model")
        .env("CHIP_ENDPOINT", url)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        if abort {
            c.arg("--abort-in-flight");
        }
        let mut child = c.spawn().unwrap();
        let begun = Instant::now();
        while !pidfile.exists() {
            assert!(
                begun.elapsed() < Duration::from_secs(20),
                "the test run never started"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(200));
        let grandchild: i32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let stopped = Instant::now();
        unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
        let status = child.wait().unwrap();
        let took = stopped.elapsed();
        std::thread::sleep(Duration::from_millis(300));
        let survived = alive(grandchild);
        if survived {
            unsafe { libc::kill(grandchild, libc::SIGKILL) };
        }
        (took, status.code(), survived)
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn with_the_flag_a_stop_ends_the_work_at_once_and_leaves_no_orphan() {
    let (took, code, survived) = run("on", true).await;
    assert!(took < Duration::from_secs(5), "took {took:?}");
    assert_eq!(code, Some(1), "not verified");
    assert!(!survived, "the grandchild of the test run was still alive");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_flag_the_work_waits_for_the_run_as_it_always_has() {
    let (took, _, _) = run("off", false).await;
    assert!(
        took > Duration::from_secs(10),
        "it should have waited for the 20 s limit, took {took:?}"
    );
}
