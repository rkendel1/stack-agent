//! A capability the operator binds to a command.
//!
//! The command is the operator's, fixed in the file: the program is never taken from an input, no
//! shell is involved, and the model's values reach it only as arguments (`${name}` placeholders for
//! *required* inputs, substituted whole into one argument and never split or interpreted) or as one
//! JSON object on standard input. The child runs with a cleared environment (plus `PATH`, the names
//! the operator lists and `CHIP_CAPABILITY`/`CHIP_IDEMPOTENCY_KEY`), in the project directory, in its
//! own process group. On the deadline the whole group is killed and the outcome is *unknown*: the
//! command may already have taken effect. Output is bounded.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use chip_core::{
    CapabilityDescriptor, ExecutionError, ExecutionRequest, ExecutionResult, InputValue,
};
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use super::{MAX_OBSERVATION_BYTES, MAX_TIMEOUT_SECS, bounded, head, to_json, valid_input_name};

const DEFAULT_TIMEOUT_SECS: u64 = 60;
const DEFAULT_MAX_OUTPUT: usize = 64 * 1024;
const MAX_OUTPUT: usize = 1 << 20;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecDecl {
    #[serde(rename = "type")]
    pub kind: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
    /// `none` (default) or `inputs`: the inputs as one JSON object on standard input.
    #[serde(default)]
    pub stdin: Option<String>,
    /// Fixed values the operator sets in the child's environment.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Names copied from the agent's environment into the child's. Nothing else is inherited but PATH.
    #[serde(default)]
    pub env_inherit: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ExecBinding {
    argv: Vec<String>,
    timeout: Duration,
    max_output: usize,
    stdin_inputs: bool,
    env: BTreeMap<String, String>,
    env_inherit: Vec<String>,
    cwd: PathBuf,
}

/// The names `${...}` placeholders in `text` refer to, or an error for a malformed placeholder.
fn placeholders(text: &str) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        let after = &rest[at + 2..];
        let Some(end) = after.find('}') else {
            return Err(format!("`{text}` has an unclosed `${{`"));
        };
        let name = &after[..end];
        if !valid_input_name(name) {
            return Err(format!(
                "`{text}` has a placeholder with an invalid name `{name}`"
            ));
        }
        names.push(name.to_string());
        rest = &after[end + 1..];
    }
    Ok(names)
}

fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
}

impl ExecBinding {
    pub fn from_decl(
        decl: &ExecDecl,
        required: &BTreeSet<&str>,
        project_root: &Path,
    ) -> Result<Self, String> {
        if decl.kind != "exec" {
            return Err(format!(
                "binding type `{}` is not supported (only `exec`)",
                decl.kind
            ));
        }
        let Some(program) = decl.argv.first().filter(|p| !p.is_empty()) else {
            return Err("`argv` must name a program".into());
        };
        if program.contains("${") {
            return Err("the program (argv[0]) is fixed and cannot contain a placeholder".into());
        }
        for arg in &decl.argv {
            if arg.contains('\0') {
                return Err("an argument contains a NUL byte".into());
            }
            for name in placeholders(arg)? {
                if !required.contains(name.as_str()) {
                    return Err(format!(
                        "placeholder `${{{name}}}` must name a declared *required* input (an optional one may be absent)"
                    ));
                }
            }
        }
        let timeout_secs = decl.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS);
        if timeout_secs == 0 || timeout_secs > MAX_TIMEOUT_SECS {
            return Err(format!("`timeout_secs` must be 1 to {MAX_TIMEOUT_SECS}"));
        }
        let max_output = decl.max_output_bytes.unwrap_or(DEFAULT_MAX_OUTPUT);
        if max_output == 0 || max_output > MAX_OUTPUT {
            return Err(format!("`max_output_bytes` must be 1 to {MAX_OUTPUT}"));
        }
        let stdin_inputs = match decl.stdin.as_deref() {
            None | Some("none") => false,
            Some("inputs") => true,
            Some(other) => return Err(format!("`stdin` `{other}` is not `none` or `inputs`")),
        };
        for name in decl.env.keys().chain(decl.env_inherit.iter()) {
            if !valid_env_name(name) || name == "CHIP_API_KEY" {
                return Err(format!("environment name `{name}` is not allowed"));
            }
        }
        Ok(Self {
            argv: decl.argv.clone(),
            timeout: Duration::from_secs(timeout_secs),
            max_output,
            stdin_inputs,
            env: decl.env.clone(),
            env_inherit: decl.env_inherit.clone(),
            cwd: project_root.to_path_buf(),
        })
    }

    fn render(&self, inputs: &BTreeMap<String, InputValue>) -> Result<Vec<String>, ExecutionError> {
        self.argv
            .iter()
            .map(|arg| {
                let mut out = arg.clone();
                for name in placeholders(arg).map_err(ExecutionError::InvalidRequest)? {
                    let value = inputs.get(&name).ok_or_else(|| {
                        ExecutionError::InvalidRequest(format!(
                            "required input `{name}` is missing"
                        ))
                    })?;
                    let text = match value {
                        InputValue::Text(s) => s.clone(),
                        InputValue::Integer(n) => n.to_string(),
                        InputValue::Bool(b) => b.to_string(),
                    };
                    if text.contains('\0') {
                        return Err(ExecutionError::InvalidRequest(format!(
                            "input `{name}` contains a NUL byte"
                        )));
                    }
                    out = out.replace(&format!("${{{name}}}"), &text);
                }
                Ok(out)
            })
            .collect()
    }

    pub async fn run(
        &self,
        request: &ExecutionRequest,
        key: Option<&str>,
        descriptor: &CapabilityDescriptor,
    ) -> Result<ExecutionResult, ExecutionError> {
        let argv = self.render(&request.inputs)?;
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .env_clear()
            .env("CHIP_CAPABILITY", descriptor.id.as_str())
            .current_dir(&self.cwd)
            .stdin(if self.stdin_inputs {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        for name in &self.env_inherit {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        command.envs(&self.env);
        if let Some(key) = key {
            command.env("CHIP_IDEMPOTENCY_KEY", key);
        }
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn().map_err(|e| {
            ExecutionError::ExecutorUnavailable(format!(
                "`{}` could not be started ({}); nothing ran",
                argv[0],
                e.kind()
            ))
        })?;
        let pid = child.id();
        if self.stdin_inputs {
            let body: serde_json::Map<String, serde_json::Value> = request
                .inputs
                .iter()
                .map(|(k, v)| (k.clone(), to_json(v)))
                .collect();
            if let Some(mut stdin) = child.stdin.take() {
                // A command that exits without reading its input closes the pipe; that is its business.
                let _ = stdin.write_all(json!(body).to_string().as_bytes()).await;
            }
        }
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let max = self.max_output;
        let collect = async move {
            let (out, err) = tokio::join!(read_capped(stdout, max), read_capped(stderr, max));
            let status = child.wait().await;
            (out, err, status)
        };
        match tokio::time::timeout(self.timeout, collect).await {
            Ok((out, err, status)) => {
                let status = status.map_err(|e| {
                    ExecutionError::OutcomeUnknown(format!(
                        "`{}` ended and its status could not be read ({}); its outcome is unknown",
                        argv[0],
                        e.kind()
                    ))
                })?;
                let code = status.code();
                let text = bounded(
                    &format!(
                        "{}\nstdout:\n{}\nstderr:\n{}",
                        head(
                            descriptor.id.as_str(),
                            if status.success() {
                                "success"
                            } else {
                                "failed"
                            },
                            json!({"exit": code}),
                        ),
                        out.0,
                        err.0
                    ),
                    MAX_OBSERVATION_BYTES,
                );
                if status.success() {
                    Ok(ExecutionResult::success(request.id.clone(), text))
                } else {
                    Ok(ExecutionResult::failure(request.id.clone(), text))
                }
            }
            Err(_) => {
                kill_group(pid);
                Err(ExecutionError::OutcomeUnknown(format!(
                    "`{}` did not finish within {} s and its process group was killed; it may already have taken effect, so its outcome is unknown",
                    argv[0],
                    self.timeout.as_secs()
                )))
            }
        }
    }
}

/// Reads up to `max` bytes and drains the rest, so a chatty child never blocks on a full pipe.
async fn read_capped<R: AsyncRead + Unpin>(reader: Option<R>, max: usize) -> (String, bool) {
    let Some(mut reader) = reader else {
        return (String::new(), false);
    };
    let (mut kept, mut cut) = (Vec::new(), false);
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = max.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
                cut |= n > room;
            }
        }
    }
    let mut text = String::from_utf8_lossy(&kept).into_owned();
    if cut {
        text.push_str("\n[output cut]");
    }
    (text, cut)
}

#[cfg(unix)]
fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid {
        // SAFETY: killpg only sends a signal to the process group this binding created.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: Option<u32>) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn decl(argv: &[&str]) -> ExecDecl {
        ExecDecl {
            kind: "exec".into(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
            timeout_secs: None,
            max_output_bytes: None,
            stdin: None,
            env: BTreeMap::new(),
            env_inherit: Vec::new(),
        }
    }

    fn required(names: &[&'static str]) -> BTreeSet<&'static str> {
        names.iter().copied().collect()
    }

    #[test]
    fn a_placeholder_must_name_a_required_input_and_the_program_is_fixed() {
        let root = Path::new("/p");
        assert!(
            ExecBinding::from_decl(&decl(&["echo", "${text}"]), &required(&["text"]), root).is_ok()
        );
        for (why, d, req) in [
            (
                "an optional input",
                decl(&["echo", "${text}"]),
                required(&[]),
            ),
            (
                "an undeclared input",
                decl(&["echo", "${nope}"]),
                required(&["text"]),
            ),
            (
                "a placeholder program",
                decl(&["${text}"]),
                required(&["text"]),
            ),
            ("no program", decl(&[]), required(&[])),
            ("an empty program", decl(&[""]), required(&[])),
            (
                "an unclosed placeholder",
                decl(&["echo", "${text"]),
                required(&["text"]),
            ),
            (
                "a bad placeholder name",
                decl(&["echo", "${a b}"]),
                required(&["text"]),
            ),
        ] {
            assert!(ExecBinding::from_decl(&d, &req, root).is_err(), "{why}");
        }
        let mut bad = decl(&["echo"]);
        bad.kind = "shell".into();
        assert!(
            ExecBinding::from_decl(&bad, &required(&[]), root).is_err(),
            "only `exec` is a binding"
        );
    }

    #[test]
    fn limits_and_environment_names_are_validated() {
        let root = Path::new("/p");
        let with = |f: &dyn Fn(&mut ExecDecl)| {
            let mut d = decl(&["true"]);
            f(&mut d);
            ExecBinding::from_decl(&d, &required(&[]), root)
        };
        assert!(with(&|d| d.timeout_secs = Some(0)).is_err());
        assert!(with(&|d| d.timeout_secs = Some(601)).is_err());
        assert!(with(&|d| d.timeout_secs = Some(600)).is_ok());
        assert!(with(&|d| d.max_output_bytes = Some(0)).is_err());
        assert!(with(&|d| d.max_output_bytes = Some(2 << 20)).is_err());
        assert!(with(&|d| d.stdin = Some("tty".into())).is_err());
        assert!(with(&|d| d.stdin = Some("inputs".into())).is_ok());
        assert!(
            with(&|d| d.env_inherit = vec!["CHIP_API_KEY".into()]).is_err(),
            "the model key is never passed on"
        );
        assert!(
            with(&|d| {
                d.env.insert("1BAD".into(), "x".into());
            })
            .is_err()
        );
        assert!(
            with(&|d| {
                d.env.insert("OK_NAME".into(), "x".into());
            })
            .is_ok()
        );
    }

    #[test]
    fn a_value_is_substituted_whole_and_never_interpreted() {
        let b = ExecBinding::from_decl(
            &decl(&["echo", "--text=${text}", "${n}"]),
            &required(&["text", "n"]),
            Path::new("/p"),
        )
        .unwrap();
        let inputs = BTreeMap::from([
            (
                "text".to_string(),
                InputValue::Text("a b; rm -rf / $(x) `y` \"q\"".into()),
            ),
            ("n".to_string(), InputValue::Integer(7)),
        ]);
        let argv = b.render(&inputs).unwrap();
        assert_eq!(
            argv,
            vec![
                "echo".to_string(),
                "--text=a b; rm -rf / $(x) `y` \"q\"".to_string(),
                "7".to_string()
            ]
        );
        // A missing required input is refused, not substituted with nothing.
        assert!(b.render(&BTreeMap::new()).is_err());
        let nul = BTreeMap::from([
            ("text".to_string(), InputValue::Text("a\0b".into())),
            ("n".to_string(), InputValue::Integer(1)),
        ]);
        assert!(b.render(&nul).is_err());
    }
}
