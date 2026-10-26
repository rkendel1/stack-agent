//! Stack-Agent using cli2 as its governed execution boundary, end to end and deterministic.
//!
//! The real `stack-agent` binary runs a work with a scripted model (no inference service) against
//! a real `cli2 serve` over its local protocol, in a disposable project. The test plays the
//! person at the approval prompt: it reads what is shown on stderr and answers on stdin, exactly
//! as a user would. A stand-in `pax` judges the project's actual files, so a wrong edit really
//! fails verification and a right one really passes.
//!
//! Needs a `cli2` binary: `CLI2_BIN`, else `../cli2/target/debug/cli2` next to this checkout. When
//! it is absent the tests say so loudly and stop, unless `CLI2_REQUIRED=1`, which makes that a failure.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gated::{FAILED, Model, PASSED, Reply, command, project};
use serde_json::Value;
use sha2::{Digest, Sha256};

const TASK: &str = "project-task:make:test";
const SLOW: &str = "project-task:make:slow";
const BUGGY: &str = "pub fn two() -> u8 { 1 }\n";
const WRONG: &str = "pub fn two() -> u8 { 3 }\n";
const RIGHT: &str = "pub fn two() -> u8 { 2 }\n";

fn cli2_binary() -> Option<PathBuf> {
    let found = std::env::var_os("CLI2_BIN").map(PathBuf::from).or_else(|| {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../cli2/target/debug/cli2");
        p.is_file().then_some(p)
    });
    match found {
        Some(p) if p.is_file() => Some(p),
        _ => {
            assert!(
                std::env::var_os("CLI2_REQUIRED").is_none(),
                "CLI2_REQUIRED is set but no cli2 binary was found (set CLI2_BIN)"
            );
            eprintln!(
                "SKIPPED: no cli2 binary found. Build one in a cli2 checkout (cargo build -p cli2) and set CLI2_BIN."
            );
            None
        }
    }
}

fn sha(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

// ------------------------------------------------------------- cli2 server

struct Cli2 {
    bin: PathBuf,
    home: PathBuf,
    ledger: PathBuf,
    socket: PathBuf,
    sock_dir: PathBuf,
    child: Option<Child>,
}

impl Cli2 {
    fn start(bin: PathBuf, tag: &str) -> Cli2 {
        let root = gated::unique(&format!("cli2-{tag}"));
        let sock_dir = PathBuf::from(format!("/tmp/c2sa-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&sock_dir);
        std::fs::create_dir_all(&sock_dir).unwrap();
        std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut c = Cli2 {
            bin,
            home: root.join("home"),
            ledger: root.join("ledger"),
            socket: sock_dir.join("s.sock"),
            sock_dir,
            child: None,
        };
        std::fs::create_dir_all(&c.home).unwrap();
        c.spawn();
        c
    }

    fn spawn(&mut self) {
        let mut child = std::process::Command::new(&self.bin)
            .args(["serve", "--socket"])
            .arg(&self.socket)
            .env("CLI2_HOME", &self.home)
            // The task processes inherit this; they record each real run here.
            .env("LEDGER", &self.ledger)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("cli2 serve starts");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(
            line.contains("ready"),
            "cli2 serve did not become ready: {line:?}"
        );
        self.child = Some(child);
    }

    fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    fn restart(&mut self) {
        self.kill();
        self.spawn();
    }

    fn runs(&self) -> usize {
        std::fs::read_to_string(&self.ledger)
            .map(|l| l.lines().count())
            .unwrap_or(0)
    }

    fn client(&self) -> chip_cli2::Client {
        chip_cli2::Client::connect(&self.socket, Duration::from_secs(10)).expect("a cli2 client")
    }
}

impl Drop for Cli2 {
    fn drop(&mut self) {
        self.kill();
        let _ = std::fs::remove_dir_all(&self.sock_dir);
    }
}

// ------------------------------------------------------------- the project

fn fixture(tag: &str) -> PathBuf {
    let dir = project(tag);
    std::fs::write(dir.join("src/lib.rs"), BUGGY).unwrap();
    std::fs::write(
        dir.join("Makefile"),
        ".PHONY: test slow\n\
         test: ## Check that two() returns 2\n\
         \t@[ -z \"$$LEDGER\" ] || echo run >> \"$$LEDGER\"; sh check.sh\n\
         slow: ## A long task\n\
         \t@[ -z \"$$LEDGER\" ] || echo run >> \"$$LEDGER\"; sleep 60\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("check.sh"),
        "if grep -q 'fn two() -> u8 { 2 }' src/lib.rs; then echo 'check passed: two() returns 2'; \
         else echo 'check failed: two() must return 2' >&2; exit 1; fi\n",
    )
    .unwrap();
    dir
}

/// A `pax` that judges the project as it actually is: passed only if `two()` returns 2.
fn pax_for(dir: &Path, tag: &str) -> gated::Pax {
    let pax = gated::Pax::new(tag, PASSED, false);
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'pax 0.4.1'; exit 0; fi\n\
         touch '{d}/started'\n\
         if grep -q 'fn two() -> u8 {{ 2 }}' '{p}/src/lib.rs'; then echo '{PASSED}'; else echo '{FAILED}'; fi\n\
         touch '{d}/finished'\nexit 0\n",
        d = pax.dir.display(),
        p = dir.display(),
    );
    let path = pax.dir.join("pax");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    pax
}

// ------------------------------------------------- the model's script

fn decision(capability: &str, inputs: Option<&str>) -> Reply {
    Reply::Stream(leak(match inputs {
        Some(i) => format!(
            r#"{{"decision":"request_capability","capability":"{capability}","inputs":{i}}}"#
        ),
        None => format!(r#"{{"decision":"request_capability","capability":"{capability}"}}"#),
    }))
}

fn read_lib() -> Reply {
    decision("project.read", Some(r#"{"path":"src/lib.rs"}"#))
}

fn edit(from: &str, to: &str) -> Reply {
    decision(
        "project.write",
        Some(&format!(
            r#"{{"path":"src/lib.rs","expected_sha256":"{}","replace":"{}","with":"{}"}}"#,
            sha(from),
            from.trim_end(),
            to.trim_end()
        )),
    )
}

fn run_task(task: &str) -> Reply {
    decision("cli2.run", Some(&format!(r#"{{"task":"{task}"}}"#)))
}

// ------------------------------------------------- the person at the prompt

#[derive(Clone)]
enum Answer {
    /// Type the digest shown: approve exactly that plan.
    Approve,
    /// Decline.
    Decline,
}

struct Work {
    code: Option<i32>,
    json: Value,
    stderr: String,
    prompts: Vec<Prompt>,
}

#[derive(Clone, Debug)]
struct Prompt {
    plan_id: String,
    digest: String,
    shown: Vec<String>,
}

/// Runs `stack-agent work` with cli2 enabled and plays the human. `during` runs on the calling
/// thread while the work is in flight (it may kill or restart cli2).
fn run_work(
    dir: &Path,
    model: &Model,
    pax: &gated::Pax,
    socket: &Path,
    extra: &[&str],
    answer: Answer,
    mut during: impl FnMut(),
) -> Work {
    let mut cmd = command(dir, &model.url, pax);
    cmd.args(["work", "--json", "--no-state"])
        .args(["--cli2-socket"])
        .arg(socket)
        .args(["--cli2-approval", "stdin"])
        .args(extra)
        .arg("Make two() return 2.")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("stack-agent starts");
    let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
    let stderr = child.stderr.take().unwrap();
    let prompts: Arc<Mutex<Vec<Prompt>>> = Arc::default();
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let (p2, l2, s2) = (prompts.clone(), lines.clone(), stdin.clone());
    let reader = std::thread::spawn(move || {
        let mut shown: Vec<String> = Vec::new();
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            l2.lock().unwrap().push(line.clone());
            if line.starts_with("cli2 needs your approval") {
                shown.clear();
            }
            shown.push(line.clone());
            if let Some(rest) = line.strip_prefix("To approve exactly this plan, type its digest (")
            {
                let short = rest.split(')').next().unwrap_or("").to_string();
                let field = |name: &str| {
                    shown
                        .iter()
                        .find_map(|l| {
                            l.trim_start()
                                .strip_prefix(name)
                                .map(|v| v.trim().to_string())
                        })
                        .unwrap_or_default()
                };
                p2.lock().unwrap().push(Prompt {
                    plan_id: field("plan "),
                    digest: field("digest "),
                    shown: shown.clone(),
                });
                let reply = match answer {
                    Answer::Approve => short,
                    Answer::Decline => "no".into(),
                };
                let mut w = s2.lock().unwrap();
                let _ = writeln!(w, "{reply}");
                let _ = w.flush();
            }
        }
    });
    let out = Arc::new(Mutex::new(Vec::new()));
    let o2 = out.clone();
    let mut stdout = child.stdout.take().unwrap();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut buf);
        *o2.lock().unwrap() = buf;
    });
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        during();
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < deadline, "the work did not finish");
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(stdin);
    reader.join().unwrap();
    out_reader.join().unwrap();
    let stdout = out.lock().unwrap().clone();
    Work {
        code: status.code(),
        json: serde_json::from_slice(&stdout).unwrap_or_else(|e| {
            panic!(
                "no JSON report ({e}): {}\n{}",
                String::from_utf8_lossy(&stdout),
                lines.lock().unwrap().join("\n")
            )
        }),
        stderr: lines.lock().unwrap().join("\n"),
        prompts: prompts.lock().unwrap().clone(),
    }
}

fn used(j: &Value, capability: &str) -> u64 {
    j["executions_by_capability"][capability]
        .as_u64()
        .unwrap_or(0)
}

// =============================================================== the scenario

#[tokio::test(flavor = "multi_thread")]
async fn a_goal_is_carried_from_discovery_through_a_wrong_edit_to_verified_completion_under_cli2() {
    let Some(bin) = cli2_binary() else { return };
    let cli2 = Cli2::start(bin, "e2e");
    let dir = fixture("e2e");
    let pax = pax_for(&dir, "e2e");
    let model = Model::start(vec![
        // 1. discover what the project can run (cli2's discovery)
        decision("cli2.tasks", None),
        // 2-3. read, then a governed edit (Stack-Agent's own, with its precondition): wrong on purpose
        read_lib(),
        edit(BUGGY, WRONG),
        // 4. run the discovered task under cli2: it fails
        run_task(TASK),
        // 5-6. read again (fresh), correct it against the file as it now is
        read_lib(),
        edit(WRONG, RIGHT),
        // 7. run it again under cli2: exits 0, and proves nothing by itself
        run_task(TASK),
        // 8. the test run that Chip, not the model, treats as verification
        decision("pax.test", None),
    ])
    .await;
    let (d, m, p) = (dir.clone(), model, pax);
    let work = tokio::task::spawn_blocking(move || {
        let w = run_work(
            &d,
            &m,
            &p,
            &cli2.socket.clone(),
            &[],
            Answer::Approve,
            || {},
        );
        (w, cli2, m)
    })
    .await
    .unwrap();
    let (w, cli2, _model) = work;
    let j = &w.json;

    if std::env::var_os("CLI2_IT_VERBOSE").is_some() {
        eprintln!(
            "--- what the person was shown for the first run ---\n{}",
            w.prompts[0].shown.join("\n")
        );
    }

    // Completed, and verified by PAX after the last change.
    assert_eq!(w.code, Some(0), "{j}\n{}", w.stderr);
    assert_eq!(j["verified"], true, "{j}");
    assert_eq!(j["terminal_state"], "completed", "{j}");
    assert_eq!(
        std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
        RIGHT
    );
    assert_eq!(j["pax"]["last_status"], "passed");

    // Every operation went through its declared capability.
    assert_eq!(used(j, "cli2.tasks"), 1);
    assert_eq!(used(j, "cli2.run"), 2);
    assert_eq!(used(j, "project.read"), 2);
    assert_eq!(used(j, "project.write"), 2);
    assert_eq!(used(j, "pax.test"), 1);

    // Nothing bypassed the authority boundary.
    let audit = &j["audit"];
    assert_eq!(audit["clean"], true, "{audit}");
    for k in [
        "unauthorized_executions",
        "unauthorized_completions",
        "false_completions",
    ] {
        assert_eq!(audit[k], 0, "{k}");
    }

    // Both runs needed approval and were put to the person, with the whole plan, before they ran.
    assert_eq!(w.prompts.len(), 2, "{}", w.stderr);
    for pr in &w.prompts {
        let text = pr.shown.join("\n");
        assert!(text.contains(TASK) && text.contains("make test"), "{text}");
        assert!(
            pr.digest.starts_with("sha256:") && pr.digest.len() == 71,
            "{}",
            pr.digest
        );
    }
    assert_ne!(
        w.prompts[0].plan_id, w.prompts[1].plan_id,
        "two actions, two plans"
    );

    // cli2's own record agrees with what the person was shown and typed, and holds the evidence.
    let mut c = cli2.client();
    let mut statuses = Vec::new();
    for pr in &w.prompts {
        let view = c
            .call("plan.get", serde_json::json!({"plan_id": pr.plan_id}))
            .unwrap();
        assert_eq!(
            view["plan"]["digest"], pr.digest,
            "the digest the person saw is the plan's"
        );
        let granted: Vec<&Value> = view["approvals"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["decision"] == "granted")
            .collect();
        assert_eq!(granted.len(), 1, "{view}");
        assert_eq!(
            granted[0]["plan_digest"], pr.digest,
            "approval is bound to the full digest"
        );
        let runs = view["executions"].as_array().unwrap();
        assert_eq!(runs.len(), 1, "one approval, one execution");
        let snap = c
            .call(
                "execution.get",
                serde_json::json!({"execution_id": runs[0]["execution_id"]}),
            )
            .unwrap();
        statuses.push(snap["task_status"].as_str().unwrap().to_string());
    }
    assert_eq!(
        statuses,
        ["failed", "succeeded"],
        "the first run failed for real, the second passed"
    );
    assert_eq!(cli2.runs(), 2, "the task ran exactly twice");

    // No evidence store of its own: only cli2's journal holds the runs, and Chip held references.
    assert!(!dir.join(".cli2").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declined_approval_runs_nothing_and_cannot_complete_the_goal() {
    let Some(bin) = cli2_binary() else { return };
    let cli2 = Cli2::start(bin, "deny");
    let dir = fixture("deny");
    let pax = pax_for(&dir, "deny");
    let model = Model::start(vec![
        decision("cli2.tasks", None),
        run_task(TASK),
        // After the refusal the model gives up; it cannot talk its way past it.
        Reply::Stream(r#"{"decision":"block","reason":"the task was not approved"}"#),
    ])
    .await;
    let (d, m, p) = (dir.clone(), model, pax);
    let (w, cli2) = tokio::task::spawn_blocking(move || {
        let w = run_work(
            &d,
            &m,
            &p,
            &cli2.socket.clone(),
            &[],
            Answer::Decline,
            || {},
        );
        (w, cli2)
    })
    .await
    .unwrap();
    assert_ne!(w.code, Some(0), "{}", w.json);
    assert_eq!(w.json["verified"], false);
    assert_eq!(cli2.runs(), 0, "nothing ran");
    assert_eq!(w.prompts.len(), 1);
    // cli2 retired the plan: it can never be approved and run later.
    let mut c = cli2.client();
    let view = c
        .call(
            "plan.get",
            serde_json::json!({"plan_id": w.prompts[0].plan_id}),
        )
        .unwrap();
    assert_eq!(view["rejected"], true, "{view}");
    assert!(view["executions"].as_array().unwrap().is_empty());
    let approve = c.call(
        "plan.approve",
        serde_json::json!({"plan_id": w.prompts[0].plan_id, "plan_digest": w.prompts[0].digest}),
    );
    assert!(
        approve.is_err(),
        "a rejected plan cannot be approved afterwards"
    );
    // The model was told it was rejected, not that it failed.
    assert_eq!(used(&w.json, "cli2.run"), 1);
    assert!(w.json["failed_observations"].as_u64().unwrap_or(0) >= 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_way_to_ask_a_person_nothing_that_needs_approval_runs() {
    let Some(bin) = cli2_binary() else { return };
    let cli2 = Cli2::start(bin, "noask");
    let dir = fixture("noask");
    let pax = pax_for(&dir, "noask");
    let model = Model::start(vec![
        run_task(TASK),
        Reply::Stream(r#"{"decision":"block","reason":"not approved"}"#),
    ])
    .await;
    let mut cmd = command(&dir, &model.url, &pax);
    // The default approval is "deny": no stdin prompt, no flag.
    cmd.args(["work", "--json", "--no-state", "--cli2-socket"])
        .arg(&cli2.socket)
        .arg("Make two() return 2.");
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    let j: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_ne!(out.status.code(), Some(0));
    assert_eq!(j["verified"], false);
    assert_eq!(cli2.runs(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_that_was_in_flight_when_cli2_died_is_unknown_and_is_never_started_again() {
    let Some(bin) = cli2_binary() else { return };
    let mut cli2 = Cli2::start(bin, "crash");
    let dir = fixture("crash");
    let pax = pax_for(&dir, "crash");
    let block = || Reply::Stream(r#"{"decision":"block","reason":"x"}"#);
    let model = Model::start(vec![run_task(SLOW), block()]).await;

    // Approve the slow task; once it has really started (its line is in the ledger), kill cli2.
    let (d, p) = (dir.clone(), pax);
    let (w, mut cli2, p) = tokio::task::spawn_blocking(move || {
        let (socket, ledger) = (cli2.socket.clone(), cli2.ledger.clone());
        let mut killed = false;
        let w = run_work(&d, &model, &p, &socket, &[], Answer::Approve, || {
            if !killed && std::fs::read_to_string(&ledger).is_ok_and(|l| !l.is_empty()) {
                killed = true;
                cli2.kill();
            }
        });
        (w, cli2, p)
    })
    .await
    .unwrap();

    // The work did not succeed, and the reason is that the outcome is unknown, not that it failed.
    assert_ne!(w.code, Some(0), "{}", w.json);
    assert_eq!(w.json["verified"], false);
    let reason = w.json["outcome_reason"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(reason.contains("unknown"), "{reason}\n{}", w.json);
    assert_eq!(cli2.runs(), 1, "the task ran once");

    // cli2 comes back. Its journal says the execution was interrupted with no outcome.
    cli2.restart();
    assert_eq!(w.prompts.len(), 1);
    let plan_id = w.prompts[0].plan_id.clone();
    let mut c = cli2.client();
    let view = c
        .call("plan.get", serde_json::json!({"plan_id": plan_id}))
        .unwrap();
    let exec = view["executions"][0]["execution_id"]
        .as_str()
        .unwrap()
        .to_string();
    let snap = c
        .call("execution.get", serde_json::json!({"execution_id": exec}))
        .unwrap();
    assert_eq!(snap["lifecycle"], "interrupted_unknown", "{snap}");

    // Asking for the same task again: the person approves again (a new action), and it is a new
    // plan. The interrupted one is never revived, and the interrupted execution is never rerun.
    let again = Model::start(vec![run_task(SLOW), block()]).await;
    let (d2, socket) = (dir.clone(), cli2.socket.clone());
    let (w2, cli2) = tokio::task::spawn_blocking(move || {
        let w2 = run_work(&d2, &again, &p, &socket, &[], Answer::Decline, || {});
        (w2, cli2)
    })
    .await
    .unwrap();
    assert_ne!(w2.code, Some(0));
    assert_eq!(cli2.runs(), 1, "the interrupted task was not run again");
}
