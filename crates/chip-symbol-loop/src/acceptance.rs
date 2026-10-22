//! Running the acceptance command and reading what it reported. The command is the task's, fixed
//! by Chip before the loop starts; nothing a model says reaches it. Its result is observed, never
//! inferred: a pass needs exit status 0 *and* the evidence the task requires.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// What a passing run must also show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequiredEvidence {
    /// Every `test result:` line says `ok`, none reports a failure, and at least `min_passed` tests
    /// passed in total.
    CargoTestPassed { min_passed: u64 },
    /// Exit status 0 alone.
    ExitZero,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticKind {
    CompileError,
    Panic,
    FailedTest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub kind: DiagnosticKind,
    pub message: String,
    pub path: Option<String>,
    pub line: Option<usize>,
    /// For a failed test: its name as the harness printed it.
    pub test: Option<String>,
}

/// One observed acceptance run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceRun {
    pub exit_status: Option<i32>,
    pub timed_out: bool,
    /// The command could not be started at all: infrastructure.
    pub spawn_error: Option<String>,
    pub duration_ms: u64,
    pub passed_tests: u64,
    pub failed_tests: u64,
    /// Exit 0 and the required evidence present.
    pub passed: bool,
    pub diagnostics: Vec<Diagnostic>,
    /// The end of the combined output.
    pub output_tail: String,
    pub output_digest: String,
}

/// Variables passed to the acceptance command; nothing else from the environment is.
const PASSED_ENV: [&str; 9] = [
    "PATH",
    "HOME",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "CARGO_TARGET_DIR",
    "TMPDIR",
    "LANG",
    "SSL_CERT_FILE",
];

fn read_all(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        buf
    })
}

/// Runs `command` in `root`, stopping it at `timeout`.
pub fn run(
    root: &Path,
    command: &[String],
    timeout: Duration,
    evidence: &RequiredEvidence,
) -> AcceptanceRun {
    let started = Instant::now();
    let failed = |why: String| AcceptanceRun {
        exit_status: None,
        timed_out: false,
        spawn_error: Some(why),
        duration_ms: started.elapsed().as_millis() as u64,
        passed_tests: 0,
        failed_tests: 0,
        passed: false,
        diagnostics: Vec::new(),
        output_tail: String::new(),
        output_digest: crate::symbols::sha256(b""),
    };
    let Some((program, args)) = command.split_first() else {
        return failed("the acceptance command is empty".into());
    };
    let mut c = Command::new(program);
    c.args(args)
        .current_dir(root)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for var in PASSED_ENV {
        if let Some(v) = std::env::var_os(var) {
            c.env(var, v);
        }
    }
    let mut child = match c.spawn() {
        Ok(child) => child,
        Err(e) => return failed(format!("cannot start {program}: {e}")),
    };
    let out = read_all(child.stdout.take().expect("piped"));
    let err = read_all(child.stderr.take().expect("piped"));
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                timed_out = true;
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break None,
        }
    };
    let mut output = String::from_utf8_lossy(&out.join().unwrap_or_default()).into_owned();
    output.push_str(&String::from_utf8_lossy(&err.join().unwrap_or_default()));
    let exit_status = status.and_then(|s| s.code());
    let (passed_tests, failed_tests, results_ok) = test_counts(&output);
    let evidence_ok = match evidence {
        RequiredEvidence::ExitZero => true,
        RequiredEvidence::CargoTestPassed { min_passed } => {
            results_ok && passed_tests >= *min_passed && failed_tests == 0
        }
    };
    AcceptanceRun {
        exit_status,
        timed_out,
        spawn_error: None,
        duration_ms: started.elapsed().as_millis() as u64,
        passed_tests,
        failed_tests,
        passed: !timed_out && exit_status == Some(0) && evidence_ok,
        diagnostics: diagnostics(root, &output),
        output_tail: crate::contract::tail(&output, 16 * 1024),
        output_digest: crate::symbols::sha256(output.as_bytes()),
    }
}

/// Totals over every `test result:` line, and whether there was at least one and all said `ok`.
pub fn test_counts(output: &str) -> (u64, u64, bool) {
    let (mut passed, mut failed, mut any, mut all_ok) = (0, 0, false, true);
    for line in output
        .lines()
        .filter(|l| l.trim_start().starts_with("test result:"))
    {
        any = true;
        all_ok &= line.contains("test result: ok.");
        let number = |word: &str| {
            line.split(';')
                .find(|part| part.trim_end().ends_with(word))
                .and_then(|part| {
                    part.split_whitespace()
                        .rev()
                        .nth(1)
                        .and_then(|n| n.parse::<u64>().ok())
                })
                .unwrap_or(0)
        };
        passed += number("passed");
        failed += number("failed");
    }
    (passed, failed, any && all_ok)
}

/// A path as the output printed it, relative to `root` when it names a file there.
fn normalize(root: &Path, printed: &str) -> Option<String> {
    let printed = printed.trim().trim_start_matches("./");
    let p = Path::new(printed);
    let rel = if p.is_absolute() {
        let root = std::fs::canonicalize(root).ok()?;
        p.strip_prefix(&root).ok()?.to_str()?.to_string()
    } else {
        printed.to_string()
    };
    root.join(&rel).is_file().then_some(rel.replace('\\', "/"))
}

fn location(root: &Path, text: &str) -> (Option<String>, Option<usize>) {
    let mut parts = text.trim().rsplitn(3, ':');
    let _col = parts.next();
    let line = parts.next().and_then(|l| l.parse::<usize>().ok());
    let path = parts.next().and_then(|p| normalize(root, p));
    match (path, line) {
        (Some(p), Some(l)) => (Some(p), Some(l)),
        _ => (None, None),
    }
}

/// Compile errors (`error...` followed by ` --> path:line:col`), panics (`panicked at path:line:col`)
/// and failed tests (`test name ... FAILED`), in output order.
pub fn diagnostics(root: &Path, output: &str) -> Vec<Diagnostic> {
    let lines: Vec<&str> = output.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("error")
            && !line.starts_with("error: test failed")
            && !line.starts_with("error: could not compile")
        {
            let at = lines[i + 1..]
                .iter()
                .take(4)
                .find_map(|l| l.trim_start().strip_prefix("--> "));
            let (path, line_no) = at.map(|a| location(root, a)).unwrap_or((None, None));
            out.push(Diagnostic {
                kind: DiagnosticKind::CompileError,
                message: crate::contract::head(line, 300),
                path,
                line: line_no,
                test: None,
            });
        } else if let Some(rest) = line.split(" panicked at ").nth(1) {
            let at = rest.trim_end_matches(':');
            let (path, line_no) = location(root, at);
            let message = lines
                .get(i + 1)
                .map(|m| crate::contract::head(m, 300))
                .unwrap_or_default();
            out.push(Diagnostic {
                kind: DiagnosticKind::Panic,
                message,
                path,
                line: line_no,
                test: line.split('\'').nth(1).map(str::to_string),
            });
        } else if let Some(name) = line
            .strip_prefix("test ")
            .and_then(|r| r.strip_suffix(" ... FAILED"))
        {
            out.push(Diagnostic {
                kind: DiagnosticKind::FailedTest,
                message: crate::contract::head(line, 300),
                path: None,
                line: None,
                test: Some(name.to_string()),
            });
        }
    }
    out
}

/// The part of the output a packet carries: the diagnostics first, then the output's end.
pub fn excerpt(run: &AcceptanceRun, max: usize) -> String {
    let mut text = String::new();
    for d in &run.diagnostics {
        text.push_str(&d.message);
        if let (Some(p), Some(l)) = (&d.path, d.line) {
            text.push_str(&format!(" ({p}:{l})"));
        }
        text.push('\n');
    }
    text.push_str("--- output end ---\n");
    let room = max.saturating_sub(text.len());
    text.push_str(&crate::contract::tail(&run.output_tail, room));
    crate::contract::head(&text, max)
}
