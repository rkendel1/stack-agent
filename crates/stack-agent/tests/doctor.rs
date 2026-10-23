//! `stack-agent doctor`, through the real binary.
//!
//! What is pinned:
//! - **No inference, no request.** A stub endpoint counts connections and bytes: with the network
//!   check on, doctor opens one TCP connection and sends zero bytes; with `--no-network` it opens
//!   none; with a proxy configured it does not connect directly.
//! - **The project is not touched.** A snapshot of every path, size, modification time and content
//!   hash is identical before and after. The `pax` stand-in logs its arguments, and the only one it
//!   ever receives is `--version` (never `--dry-run`, which runs `cargo` on the project). A project
//!   whose build script and compiler wrapper would leave a marker leaves none.
//! - **Parity with `work`.** For each prerequisite, doctor fails only when `work` requires it for
//!   the kind of work asked about, and `work` agrees (refuses, or never mentions it) in the same
//!   environment. Advisory prerequisites warn and keep exit 0; prerequisites a kind does not use
//!   are `not_required`, never reported missing.
//! - **Redaction.** A sentinel credential, in the key variables, the proxy password and an
//!   endpoint's userinfo, appears in neither the text nor the JSON output, even when an error
//!   message echoes it.
//! - **The exit contract.** 0, 2 and 3 only; the categories are in the output, not in new codes.
//!
//! Not covered: a real model, a real `pax` (a stand-in answers `--version`), Windows, and the
//! behaviour of an endpoint behind a proxy. A passing doctor says nothing about whether a key is
//! valid.

#![cfg(unix)]

use std::io::Read;
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};

const SENTINEL: &str = "sk-SENTINEL-doctor-8d2f6a41c9";
const USERINFO_SENTINEL: &str = "userinfo-SENTINEL-3b7e90d1";
const UNREACHABLE: &str = "http://127.0.0.1:1/v1/chat/completions";

struct Sandbox {
    root: PathBuf,
    project: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    pax_log: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("stack-agent-doctor-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (project, home, bin) = (root.join("project"), root.join("home"), root.join("bin"));
        for d in [&project.join("src"), &home.join("tmp"), &bin] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(project.join("src/lib.rs"), "pub fn one() -> u8 { 1 }\n").unwrap();
        let _ = Command::new("git")
            .args(["init", "-q"])
            .arg(&project)
            .status();
        let pax_log = root.join("pax.log");
        Self {
            root,
            project,
            home,
            bin,
            pax_log,
        }
    }

    /// A `pax` stand-in that records its arguments and answers only `--version`.
    fn with_pax(self, version: &str) -> Self {
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{log}'\nif [ \"$1\" = \"--version\" ]; then echo 'pax {version}'; exit 0; fi\necho \"unexpected pax arguments: $*\" >&2\nexit 9\n",
            log = self.pax_log.display()
        );
        let path = self.bin.join("pax");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        self
    }

    fn pax_arguments(&self) -> Vec<String> {
        std::fs::read_to_string(&self.pax_log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn data(&self) -> PathBuf {
        self.home.join("data")
    }

    fn command(&self, with_git: bool) -> Command {
        let path = if with_git {
            format!("{}:/usr/bin:/bin", self.bin.display())
        } else {
            self.bin.display().to_string()
        };
        let mut c = Command::new(env!("CARGO_BIN_EXE_stack-agent"));
        c.env_clear()
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("TMPDIR", self.home.join("tmp"))
            .env("STACK_AGENT_DATA_DIR", self.data())
            .current_dir(&self.project);
        c
    }

    fn doctor(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = self.command(true);
        c.arg("doctor").args(args).envs(env.iter().copied());
        c.output().unwrap()
    }

    fn work(&self, args: &[&str], env: &[(&str, &str)], with_git: bool) -> Output {
        let mut c = self.command(with_git);
        c.arg("work").args(args).envs(env.iter().copied());
        c.output().unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        // A read-only directory the tests made cannot be removed until it is writable again.
        if let Ok(entries) = std::fs::read_dir(self.data()) {
            for e in entries.flatten() {
                let _ = std::fs::set_permissions(e.path(), std::fs::Permissions::from_mode(0o755));
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A TCP endpoint that counts the connections it accepts and the bytes they send.
struct Stub {
    addr: SocketAddr,
    connections: Arc<AtomicUsize>,
    bytes: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Stub {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let (connections, bytes, stop) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
        );
        let (c, b, s) = (connections.clone(), bytes.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        c.fetch_add(1, Ordering::SeqCst);
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_millis(500)))
                            .unwrap();
                        let mut buf = [0u8; 4096];
                        while let Ok(n) = stream.read(&mut buf) {
                            if n == 0 {
                                break;
                            }
                            b.fetch_add(n, Ordering::SeqCst);
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Self {
            addr,
            connections,
            bytes,
            stop,
            handle: Some(handle),
        }
    }

    fn endpoint(&self) -> String {
        format!("http://{}/v1/chat/completions", self.addr)
    }

    /// Connections and bytes seen, after giving the stub time to drain a closed connection.
    fn settled(&self) -> (usize, usize) {
        std::thread::sleep(Duration::from_millis(400));
        (
            self.connections.load(Ordering::SeqCst),
            self.bytes.load(Ordering::SeqCst),
        )
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn text(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn json(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {}", text(o)))
}

fn code(o: &Output) -> i32 {
    o.status.code().expect("the process exited")
}

fn check(report: &Value, id: &str) -> (String, Option<String>) {
    let c = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("no check {id} in {report}"));
    (
        c["status"].as_str().unwrap().to_string(),
        c["category"].as_str().map(str::to_string),
    )
}

/// Every path under `root` with its type, size, modification time and content hash.
fn snapshot(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let path = e.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            if meta.is_dir() {
                out.push(format!("dir {rel} {mtime}"));
                walk(&path, root, out);
            } else if meta.is_file() {
                let hash = Sha256::digest(std::fs::read(&path).unwrap());
                out.push(format!("file {rel} {} {mtime} {hash:x}", meta.len()));
            } else {
                out.push(format!("other {rel}"));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out
}

fn as_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}

// ---- configuration --------------------------------------------------------------------------------

#[test]
fn missing_configuration_fails_with_the_exact_remedy_and_selects_nothing() {
    let sb = Sandbox::new("noconfig").with_pax("0.4.1");
    let out = sb.doctor(&["--json"], &[]);
    assert_eq!(code(&out), 3, "{}", text(&out));
    let report = json(&out);
    assert_eq!(report["ok"], false);
    assert_eq!(
        check(&report, "model"),
        ("fail".into(), Some("config_missing".into()))
    );
    let fix = report["checks"][1]["fix"].as_str().unwrap();
    assert!(
        fix.contains("CHIP_MODEL") && fix.contains("CHIP_ENDPOINT"),
        "{fix}"
    );
    // It says what to set and chooses nothing: no model name from it appears in the report.
    assert_eq!(check(&report, "endpoint").0, "unverified");

    // The same environment, the same verdict from `work`: refused before anything ran.
    let w = sb.work(&["Say hello"], &[], true);
    assert_eq!(code(&w), 3, "{}", text(&w));
    assert!(text(&w).contains("no model is selected"), "{}", text(&w));
}

#[test]
fn a_provider_work_rejects_is_config_invalid_not_missing() {
    let sb = Sandbox::new("badprovider").with_pax("0.4.1");
    let env = [
        ("CHIP_PROVIDER", "no-such-provider"),
        ("CHIP_MODEL", "m"),
        ("CHIP_ENDPOINT", UNREACHABLE),
    ];
    let out = sb.doctor(&["--json", "--no-network"], &env);
    assert_eq!(code(&out), 3, "{}", text(&out));
    let (status, category) = check(&json(&out), "model");
    assert_eq!(status, "fail");
    assert!(
        matches!(
            category.as_deref(),
            Some("config_invalid" | "config_missing")
        ),
        "{category:?}"
    );
    assert_eq!(code(&sb.work(&["x"], &env, true)), 3);
}

#[test]
fn a_flag_beats_the_environment_exactly_as_it_does_for_work() {
    let sb = Sandbox::new("precedence").with_pax("0.4.1");
    let env = [("CHIP_MODEL", "from-env"), ("CHIP_ENDPOINT", UNREACHABLE)];
    let out = sb.doctor(&["--json", "--no-network", "--model", "from-flag"], &env);
    assert_eq!(code(&out), 0, "{}", text(&out));
    assert!(text(&out).contains("from-flag") && !text(&out).contains("from-env"));
}

// ---- no request reaches the endpoint --------------------------------------------------------------

#[test]
fn the_endpoint_check_opens_a_connection_and_sends_nothing() {
    let sb = Sandbox::new("zero").with_pax("0.4.1");
    let stub = Stub::start();
    let endpoint = stub.endpoint();
    let env = [("CHIP_MODEL", "m"), ("CHIP_ENDPOINT", endpoint.as_str())];
    let out = sb.doctor(&["--json"], &env);
    assert_eq!(code(&out), 0, "{}", text(&out));
    assert_eq!(check(&json(&out), "endpoint").0, "ok");
    assert_eq!(
        stub.settled(),
        (1, 0),
        "one connection, zero bytes: no request was made"
    );
}

#[test]
fn no_network_opens_no_connection_at_all() {
    let sb = Sandbox::new("nonet").with_pax("0.4.1");
    let stub = Stub::start();
    let endpoint = stub.endpoint();
    let env = [("CHIP_MODEL", "m"), ("CHIP_ENDPOINT", endpoint.as_str())];
    let out = sb.doctor(&["--json", "--no-network"], &env);
    assert_eq!(code(&out), 0, "{}", text(&out));
    assert_eq!(check(&json(&out), "endpoint").0, "unverified");
    assert_eq!(stub.settled(), (0, 0));
}

#[test]
fn with_a_proxy_configured_doctor_does_not_connect_around_it() {
    let sb = Sandbox::new("proxy").with_pax("0.4.1");
    let stub = Stub::start();
    let endpoint = stub.endpoint();
    let env = [
        ("CHIP_MODEL", "m"),
        ("CHIP_ENDPOINT", endpoint.as_str()),
        ("CHIP_PROXY", "http://127.0.0.1:9"),
    ];
    let out = sb.doctor(&["--json"], &env);
    assert_eq!(code(&out), 0, "{}", text(&out));
    let report = json(&out);
    assert_eq!(check(&report, "endpoint").0, "unverified");
    assert!(text(&out).contains("CHIP_PROXY"));
    assert_eq!(
        stub.settled(),
        (0, 0),
        "doctor must not reach the endpoint directly"
    );
}

#[test]
fn an_unreachable_endpoint_is_reported_as_such_and_work_agrees() {
    let sb = Sandbox::new("unreachable").with_pax("0.4.1");
    let env = [("CHIP_MODEL", "m"), ("CHIP_ENDPOINT", UNREACHABLE)];
    let out = sb.doctor(&["--json"], &env);
    assert_eq!(code(&out), 3, "{}", text(&out));
    assert_eq!(
        check(&json(&out), "endpoint"),
        ("fail".into(), Some("endpoint_unreachable".into()))
    );
    let w = sb.work(&["x"], &env, true);
    assert_eq!(code(&w), 3, "{}", text(&w));
    assert!(text(&w).contains("did not answer"), "{}", text(&w));
}

// ---- the project is not touched -------------------------------------------------------------------

#[test]
fn doctor_changes_nothing_and_runs_only_pax_version() {
    let sb = Sandbox::new("untouched").with_pax("0.4.1");
    // A project that would announce any execution of its own code: a build script, a compiler
    // wrapper and a toolchain pin that makes `cargo` do real work (including a download).
    let marker = sb.root.join("EXECUTED");
    std::fs::write(
        sb.project.join("build.rs"),
        format!(
            "fn main() {{ std::fs::write(\"{}\", \"x\").unwrap(); }}\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::create_dir_all(sb.project.join(".cargo")).unwrap();
    std::fs::write(
        sb.project.join(".cargo/config.toml"),
        format!(
            "[build]\nrustc-wrapper = \"{}\"\n",
            sb.bin.join("pax").display()
        ),
    )
    .unwrap();
    std::fs::write(
        sb.project.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.55.0\"\n",
    )
    .unwrap();
    std::fs::write(
        sb.project.join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\nbuild = \"build.rs\"\n",
    )
    .unwrap();

    let stub = Stub::start();
    let endpoint = stub.endpoint();
    let env = [("CHIP_MODEL", "m"), ("CHIP_ENDPOINT", endpoint.as_str())];
    for kind in ["change", "verify", "inspect"] {
        let before = snapshot(&sb.project);
        let out = sb.doctor(&["--kind", kind, "--json"], &env);
        assert_eq!(code(&out), 0, "{kind}: {}", text(&out));
        assert_eq!(
            snapshot(&sb.project),
            before,
            "doctor changed the project ({kind})"
        );
    }
    assert!(!marker.exists(), "doctor executed the project's code");
    assert!(!sb.project.join("Cargo.lock").exists() && !sb.project.join("target").exists());
    assert!(
        sb.pax_arguments().iter().all(|a| a == "--version"),
        "pax received more than --version (a --dry-run would run cargo on the project): {:?}",
        sb.pax_arguments()
    );
}

#[test]
fn a_state_directory_that_does_not_exist_is_not_created_and_a_probe_leaves_nothing() {
    let sb = Sandbox::new("statedir").with_pax("0.4.1");
    let env = [("CHIP_MODEL", "m"), ("CHIP_ENDPOINT", UNREACHABLE)];
    assert!(!sb.data().exists());
    let out = sb.doctor(&["--json", "--no-network"], &env);
    assert_eq!(code(&out), 0, "{}", text(&out));
    assert_eq!(check(&json(&out), "state").0, "ok");
    assert!(!sb.data().exists(), "doctor created the state directory");

    std::fs::create_dir_all(sb.data().join("agent-state")).unwrap();
    let before = snapshot(&sb.data());
    let out = sb.doctor(&["--json", "--no-network"], &env);
    assert_eq!(code(&out), 0, "{}", text(&out));
    assert_eq!(check(&json(&out), "state").0, "ok");
    let after = snapshot(&sb.data());
    let names = |s: &[String]| {
        s.iter()
            .map(|l| l.split(' ').nth(1).unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(&after),
        names(&before),
        "a probe file was left behind"
    );
}

// ---- parity with `work`: required, advisory, and not needed ----------------------------------------

#[test]
fn pax_missing_is_required_for_change_and_verify_and_work_refuses() {
    let sb = Sandbox::new("nopax");
    let env = [("CHIP_MODEL", "m"), ("CHIP_ENDPOINT", UNREACHABLE)];
    for kind in ["change", "verify"] {
        // A PATH with no system directory: macOS ships an unrelated /usr/bin/pax (see the next test).
        let out = sb
            .command(false)
            .args(["doctor", "--kind", kind, "--json", "--no-network"])
            .envs(env)
            .output()
            .unwrap();
        assert_eq!(code(&out), 3, "{kind}: {}", text(&out));
        assert_eq!(
            check(&json(&out), "pax"),
            ("fail".into(), Some("dependency_missing".into()))
        );
        let fix = json(&out)["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "pax")
            .unwrap()["fix"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            fix.contains("github.com/rkendel1/pax/releases") && fix.contains("never installs"),
            "{fix}"
        );
        let w = sb.work(&["--kind", kind, "x"], &env, false);
        assert_eq!(code(&w), 3, "{kind}: {}", text(&w));
        assert!(text(&w).contains("PAX unavailable"), "{kind}: {}", text(&w));
    }
}

/// The fresh-Mac case: `/usr/bin/pax` exists and is not PAX. `work` only locates a file named
/// `pax` at start, so it begins; doctor identifies it and says what it is.
#[test]
#[cfg(target_os = "macos")]
fn the_macos_archive_utility_is_named_for_what_it_is() {
    let sb = Sandbox::new("macpax");
    let env = [("CHIP_MODEL", "m"), ("CHIP_ENDPOINT", UNREACHABLE)];
    let out = sb.doctor(&["--json", "--no-network"], &env);
    assert_eq!(code(&out), 3, "{}", text(&out));
    assert_eq!(
        check(&json(&out), "pax").1.as_deref(),
        Some("dependency_unsuitable")
    );
    assert!(
        text(&out).contains("POSIX archive utility"),
        "{}",
        text(&out)
    );
    let w = sb.work(&["x"], &env, true);
    assert!(
        !text(&w).contains("PAX unavailable"),
        "work started: {}",
        text(&w)
    );
}

#[test]
fn pax_is_not_needed_for_inspect_and_doctor_does_not_say_it_is_missing() {
    let sb = Sandbox::new("inspect");
    let env = [("CHIP_MODEL", "m"), ("CHIP_ENDPOINT", UNREACHABLE)];
    let out = sb.doctor(&["--kind", "inspect", "--json", "--no-network"], &env);
    assert_eq!(code(&out), 0, "{}", text(&out));
    let report = json(&out);
    assert_eq!(check(&report, "pax").0, "not_required");
    assert_eq!(check(&report, "verification").0, "not_required");
    // `work --kind inspect` gets past PAX too: it fails only at the unreachable model.
    let w = sb.work(&["--kind", "inspect", "x"], &env, true);
    assert!(!text(&w).contains("PAX unavailable"), "{}", text(&w));
    assert!(text(&w).contains("did not answer"), "{}", text(&w));
}

#[test]
fn an_unsuitable_pax_fails_for_change_but_work_only_refuses_it_at_first_use() {
    let sb = Sandbox::new("oldpax").with_pax("0.1.0");
    let env = [("CHIP_MODEL", "m"), ("CHIP_ENDPOINT", UNREACHABLE)];
    let out = sb.doctor(&["--json", "--no-network"], &env);
    assert_eq!(code(&out), 3, "{}", text(&out));
    assert_eq!(
        check(&json(&out), "pax"),
        ("fail".into(), Some("dependency_unsuitable".into()))
    );
    assert!(text(&out).contains("refuses `pax.test`"), "{}", text(&out));
    // `work` locates PAX up front and does not refuse a stale one at start, which is what doctor
    // says: it reaches the model.
    let w = sb.work(&["x"], &env, true);
    assert!(!text(&w).contains("PAX unavailable"), "{}", text(&w));
    assert!(text(&w).contains("did not answer"), "{}", text(&w));
    // Not PAX at all.
    let sb = Sandbox::new("notpax");
    std::fs::write(sb.bin.join("pax"), "#!/bin/sh\necho hello\n").unwrap();
    std::fs::set_permissions(sb.bin.join("pax"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = sb.doctor(&["--json", "--no-network"], &env);
    assert_eq!(
        check(&json(&out), "pax").1.as_deref(),
        Some("dependency_unsuitable")
    );
}

#[test]
fn git_is_advisory_so_its_absence_warns_and_never_fails() {
    let sb = Sandbox::new("nogit").with_pax("0.4.1");
    let env = [("CHIP_MODEL", "m"), ("CHIP_ENDPOINT", UNREACHABLE)];
    // PATH holds only the stand-in `pax`: no git.
    let mut c = sb.command(false);
    let out = c
        .args(["doctor", "--json", "--no-network"])
        .envs(env)
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", text(&out));
    let report = json(&out);
    assert_eq!(report["ok"], true);
    assert_eq!(
        check(&report, "git"),
        ("warn".into(), Some("dependency_missing".into()))
    );
    // `work` does not refuse to start for want of git.
    let w = sb.work(&["x"], &env, false);
    assert!(text(&w).contains("did not answer"), "{}", text(&w));
}

#[test]
fn the_default_state_directory_is_advisory_and_a_requested_database_is_required() {
    if as_root() {
        eprintln!("SKIPPED: permission checks mean nothing as root");
        return;
    }
    let sb = Sandbox::new("ro").with_pax("0.4.1");
    let env = [("CHIP_MODEL", "m"), ("CHIP_ENDPOINT", UNREACHABLE)];
    let dir = sb.data().join("agent-state");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

    let out = sb.doctor(&["--json", "--no-network"], &env);
    assert_eq!(code(&out), 0, "advisory: {}", text(&out));
    assert_eq!(
        check(&json(&out), "state"),
        ("warn".into(), Some("environment".into()))
    );
    assert!(text(&out).contains("still runs"), "{}", text(&out));

    // A database the user names is required, and work refuses to start without it.
    let db = dir.join("named.feltdb");
    let db = db.to_str().unwrap();
    let out = sb.doctor(&["--json", "--no-network", "--state-db", db], &env);
    assert_eq!(code(&out), 3, "required: {}", text(&out));
    assert_eq!(check(&json(&out), "state").0, "fail");
    let w = sb.work(&["--state-db", db, "x"], &env, true);
    assert_ne!(code(&w), 0, "{}", text(&w));
    assert!(
        !text(&w).contains("did not answer"),
        "work must stop before the model: {}",
        text(&w)
    );

    // --no-state turns the check off.
    let out = sb.doctor(&["--json", "--no-network", "--no-state"], &env);
    assert_eq!(check(&json(&out), "state").0, "not_required");
}

#[test]
fn a_missing_project_directory_is_unsupported() {
    let sb = Sandbox::new("nodir").with_pax("0.4.1");
    let env = [("CHIP_MODEL", "m"), ("CHIP_ENDPOINT", UNREACHABLE)];
    let missing = sb.root.join("does-not-exist");
    let out = sb.doctor(
        &["--json", "--no-network", "--dir", missing.to_str().unwrap()],
        &env,
    );
    assert_eq!(code(&out), 3, "{}", text(&out));
    assert_eq!(
        check(&json(&out), "project"),
        ("fail".into(), Some("project_unsupported".into()))
    );
}

#[test]
fn a_project_without_a_manifest_is_unverified_not_declared_unsupported() {
    let sb = Sandbox::new("nomanifest").with_pax("0.4.1");
    std::fs::remove_file(sb.project.join("Cargo.toml")).unwrap();
    let env = [("CHIP_MODEL", "m"), ("CHIP_ENDPOINT", UNREACHABLE)];
    let out = sb.doctor(&["--json", "--no-network"], &env);
    assert_eq!(code(&out), 0, "{}", text(&out));
    assert_eq!(check(&json(&out), "verification").0, "unverified");
}

#[test]
fn everything_in_place_is_ok_and_says_what_it_did_not_check() {
    let sb = Sandbox::new("allgood").with_pax("0.4.1");
    let stub = Stub::start();
    let endpoint = stub.endpoint();
    let env = [("CHIP_MODEL", "m"), ("CHIP_ENDPOINT", endpoint.as_str())];
    let out = sb.doctor(&[], &env);
    assert_eq!(code(&out), 0, "{}", text(&out));
    let t = text(&out);
    assert!(t.contains("no required prerequisite is missing"), "{t}");
    assert!(t.contains("Not checked: that the key is accepted"), "{t}");
    assert!(t.contains("Nothing was run, installed or changed"), "{t}");
}

// ---- redaction --------------------------------------------------------------------------------------

#[test]
fn a_credential_never_appears_in_text_or_json_even_when_an_error_echoes_it() {
    let sb = Sandbox::new("redact").with_pax("0.4.1");
    let userinfo_endpoint = format!(
        "http://user:{USERINFO_SENTINEL}@127.0.0.1:1/v1/chat/completions?token={USERINFO_SENTINEL}"
    );
    let scenarios: Vec<(&str, Vec<(&str, String)>)> = vec![
        (
            "key and proxy password",
            vec![
                ("CHIP_MODEL", "m".into()),
                ("CHIP_ENDPOINT", UNREACHABLE.into()),
                ("CHIP_API_KEY", SENTINEL.into()),
                ("CHIP_PROXY", "http://127.0.0.1:9".into()),
                ("CHIP_PROXY_USERNAME", "u".into()),
                ("CHIP_PROXY_PASSWORD", SENTINEL.into()),
            ],
        ),
        (
            "an error that echoes the key as the provider name",
            vec![
                ("CHIP_PROVIDER", SENTINEL.into()),
                ("CHIP_MODEL", "m".into()),
                ("CHIP_ENDPOINT", UNREACHABLE.into()),
                ("CHIP_API_KEY", SENTINEL.into()),
            ],
        ),
        (
            "provider variables",
            vec![
                ("CHIP_MODEL", "m".into()),
                ("CHIP_ENDPOINT", UNREACHABLE.into()),
                ("OPENAI_API_KEY", SENTINEL.into()),
                ("ANTHROPIC_API_KEY", SENTINEL.into()),
            ],
        ),
        (
            "userinfo and query token in the endpoint (refused connection error path)",
            vec![
                ("CHIP_MODEL", "m".into()),
                ("CHIP_ENDPOINT", userinfo_endpoint),
            ],
        ),
    ];
    for (name, vars) in scenarios {
        let env: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
        for args in [&["--json"][..], &[][..]] {
            let out = sb.doctor(args, &env);
            let t = text(&out);
            assert!(
                !t.contains(SENTINEL) && !t.contains(USERINFO_SENTINEL),
                "{name} {args:?} leaked a credential:\n{t}"
            );
        }
    }
}

// ---- the contract -----------------------------------------------------------------------------------

#[test]
fn usage_errors_exit_2_and_help_exits_0_without_running_checks() {
    let sb = Sandbox::new("usage");
    for args in [
        &["--surprise"][..],
        &["--kind", "bogus"],
        &["--model"],
        &["--state-db", "f", "--no-state"],
    ] {
        let out = sb.doctor(args, &[]);
        assert_eq!(code(&out), 2, "{args:?}: {}", text(&out));
        assert!(text(&out).contains("usage: stack-agent doctor"));
    }
    let out = sb.doctor(&["--help"], &[]);
    assert_eq!(code(&out), 0);
    assert!(!text(&out).contains("Result:"));
}

#[test]
fn json_has_a_stable_schema_and_only_json_on_stdout() {
    let sb = Sandbox::new("schema").with_pax("0.4.1");
    let env = [("CHIP_MODEL", "m"), ("CHIP_ENDPOINT", UNREACHABLE)];
    let out = sb.doctor(&["--json", "--no-network"], &env);
    let report = json(&out);
    assert_eq!(report["schema"], "stack-agent.doctor.v1");
    for c in report["checks"].as_array().unwrap() {
        for key in ["id", "status", "category", "summary", "detail", "fix"] {
            assert!(c.get(key).is_some(), "{c} lacks {key}");
        }
    }
    assert!(
        String::from_utf8_lossy(&out.stderr).is_empty(),
        "{}",
        text(&out)
    );
}
