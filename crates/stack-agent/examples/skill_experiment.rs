//! **Experimental.** The native-skill evaluation runner (`docs/product/native-skills-experiment.md`).
//! Not part of the product: it is an example, so `stack-agent` neither contains nor ships it.
//!
//! ```text
//! cargo run --release -p stack-agent --example skill_experiment -- check
//! cargo run --release -p stack-agent --example skill_experiment -- plan --trials 9
//! cargo run --release -p stack-agent --example skill_experiment -- run-schedule --trials 9 \
//!     --out DIR --provider local --local-model ID --local-endpoint URL
//! ```
//!
//! `check` and `plan` make no model call and touch no network. `run-schedule` spends inference and
//! must not be run without authorization.
//!
//! Each trial is the agent benchmark's own task-E trial (`agent_bench::run_trial`, unchanged: the
//! fixture, the `work` command line, the oracle and the record). The only difference between arms is
//! the executable that trial launches: `DIR/bin/skill-arm-<arm>`, a symbolic link to this example.
//! Invoked under that name, the example runs `work` once more in a child process
//! (`software_work::work_with_experiment` with the arm's reviewed text, `None` for the baseline),
//! keeps that child's complete report, stderr, diff and file hashes under `DIR/artifacts/`, and
//! passes its output and exit status through unchanged.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chip_cli::agent_bench::{self, Provider, ProviderId, StateArm, TaskId};
use chip_cli::autoconfig::profile::BuildSystem;
use chip_cli::skill_experiment::{self as exp, Arm};
use chip_cli::software_work::GoalKind;
use serde_json::{Value, json};

const LAUNCHER_PREFIX: &str = "skill-arm-";
const WORK_MODE: &str = "__work";
/// Where `run-schedule --chip-proxy` leaves the proxy URL for the launchers.
const PROXY_FILE: &str = "chip-proxy.url";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let invoked = Path::new(&args[0])
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    if let Some(arm) = invoked.strip_prefix(LAUNCHER_PREFIX) {
        std::process::exit(launch(arm, &args));
    }
    let code = match args.get(1).map(String::as_str) {
        Some(WORK_MODE) => work(&args[2..]).await,
        Some("check") => check(),
        Some("plan") => plan(&args[2..]),
        Some("run-schedule") => run_schedule(&args[2..]),
        _ => {
            eprintln!(
                "usage: skill_experiment check | plan --trials N | run-schedule --trials N --out DIR --provider local|local-object|local-none --local-model ID --local-endpoint URL"
            );
            2
        }
    };
    std::process::exit(code);
}

// ---- the work child ---------------------------------------------------------------------------------------------------

/// `__work <arm> work <args>`: the product's `work`, with the arm's reviewed text. Refused before
/// anything runs if the arm's artifact is not the reviewed one.
async fn work(args: &[String]) -> i32 {
    let (Some(arm), Some("work")) = (
        args.first().and_then(|a| Arm::parse(a)),
        args.get(1).map(String::as_str),
    ) else {
        eprintln!("error: {WORK_MODE} needs an arm and `work`");
        return 2;
    };
    let instructions = match exp::prepare(arm, &exp::repo_root()) {
        Ok(i) => i,
        Err(why) => {
            eprintln!("error: {why}; nothing was run");
            return 3;
        }
    };
    let source = instructions
        .as_ref()
        .map(|i| i as &dyn chip_cli::software_work::InstructionSource);
    chip_cli::software_work::work_with_experiment(&args[2..], source).await
}

// ---- the launcher -----------------------------------------------------------------------------------------------------

fn sha_tree(dir: &Path) -> Value {
    json!(agent_bench::tree_hashes(dir))
}

fn git(dir: &Path, args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Invoked by `agent_bench::run_trial` as `DIR/bin/skill-arm-<arm> work <args>`, in the trial's
/// fixture. Runs the work child with the same arguments and environment, keeps its artifacts, and
/// passes its stdout, stderr and exit status through.
fn launch(arm: &str, args: &[String]) -> i32 {
    let Some(arm) = Arm::parse(arm) else {
        eprintln!("error: unknown arm `{arm}`");
        return 2;
    };
    let out = Path::new(&args[0])
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let fixture = std::env::current_dir().unwrap_or_default();
    let name = fixture
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let artifacts = out.join("artifacts").join(name);
    if artifacts.exists() {
        eprintln!(
            "error: {} already exists; a trial's artifacts are never overwritten",
            artifacts.display()
        );
        return 3;
    }
    if std::fs::create_dir_all(&artifacts).is_err() {
        eprintln!("error: cannot create {}", artifacts.display());
        return 3;
    }
    let write = |file: &str, bytes: &[u8]| {
        let _ = std::fs::write(artifacts.join(file), bytes);
    };
    write(
        "before.json",
        json!({"head": git(&fixture, &["rev-parse", "HEAD"]).trim(), "files": sha_tree(&fixture)})
            .to_string()
            .as_bytes(),
    );
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("error: no executable path ({e})");
            return 3;
        }
    };
    // The runtime's own data directory is per trial, so no learning record or default database
    // carries over from one trial to the next.
    let data = artifacts.join("data");
    let mut child = Command::new(exe);
    child
        .arg(WORK_MODE)
        .arg(arm.name())
        .args(&args[1..])
        .env("STACK_AGENT_DATA_DIR", &data)
        .stdin(Stdio::null());
    // The run's request pacer, when it has one: the same `CHIP_PROXY` for every arm, read from the
    // run directory because the bench clears the environment it starts this launcher with.
    if let Ok(proxy) = std::fs::read_to_string(out.join(PROXY_FILE)) {
        child.env("CHIP_PROXY", proxy.trim());
    }
    let output = child.output();
    let (stdout, stderr, code) = match output {
        Ok(o) => (o.stdout, o.stderr, o.status.code().unwrap_or(4)),
        Err(e) => (
            Vec::new(),
            format!("error: could not start the work child: {e}\n").into_bytes(),
            4,
        ),
    };
    // No credential reaches an artifact or the parent: every key the child could have had is
    // redacted from what it printed before anything is written or passed on.
    let (stdout, stderr) = (redact(stdout), redact(stderr));
    write("report.json", &stdout);
    write("stderr.log", &stderr);
    write("exit.txt", code.to_string().as_bytes());
    write(
        "agent.diff",
        git(&fixture, &["diff", "--no-color"]).as_bytes(),
    );
    write(
        "after.json",
        json!({"status": git(&fixture, &["status", "--porcelain"]), "files": sha_tree(&fixture)})
            .to_string()
            .as_bytes(),
    );
    let _ = std::io::stdout().write_all(&stdout);
    let _ = std::io::stderr().write_all(&stderr);
    code
}

/// `bytes` with the value of every credential variable replaced by `[redacted]`.
fn redact(bytes: Vec<u8>) -> Vec<u8> {
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    for var in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "CHIP_API_KEY",
        "AI_GATEWAY_API_KEY",
    ] {
        if let Ok(secret) = std::env::var(var)
            && secret.len() >= 8
        {
            text = text.replace(&secret, "[redacted]");
        }
    }
    text.into_bytes()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The exact source the experiment runs from, committed or not: HEAD, the full tracked diff and every
/// untracked file's hash, folded into one fingerprint. Checked again before every trial.
fn source_state(root: &Path) -> (String, String, Value) {
    let head = git(root, &["rev-parse", "HEAD"]).trim().to_string();
    let diff = git(root, &["diff", "HEAD", "--no-color"]);
    let mut untracked = BTreeMap::new();
    for path in git(root, &["ls-files", "--others", "--exclude-standard"]).lines() {
        let bytes = std::fs::read(root.join(path)).unwrap_or_default();
        untracked.insert(path.to_string(), sha256_hex(&bytes));
    }
    let mut folded = format!("{head}\n{diff}");
    for (p, h) in &untracked {
        folded.push_str(&format!("{p}\0{h}\n"));
    }
    let state = json!({
        "head": head,
        "tracked_diff_sha256": sha256_hex(diff.as_bytes()),
        "tracked_diff_lines": diff.lines().count(),
        "untracked": untracked,
    });
    (sha256_hex(folded.as_bytes()), diff, state)
}

fn file_sha(path: &Path) -> String {
    std::fs::read(path)
        .map(|b| sha256_hex(&b))
        .unwrap_or_else(|_| "unreadable".into())
}

/// What a recorded trial cost, from the usage the provider reported for each call: `(conservative,
/// with cache discount)`. Conservative charges every input token at the full input price.
fn trial_cost(record: &Value, prices: (f64, f64, f64)) -> (f64, f64) {
    let r = &record["result"];
    let input = r["reported_input_tokens"].as_f64().unwrap_or(0.0);
    let output = r["reported_output_tokens"].as_f64().unwrap_or(0.0);
    let cached = r["reported_cache_read_tokens"]
        .as_f64()
        .unwrap_or(0.0)
        .min(input);
    let (pi, po, pc) = (prices.0 / 1e6, prices.1 / 1e6, prices.2 / 1e6);
    (
        input * pi + output * po,
        (input - cached) * pi + cached * pc + output * po,
    )
}

// ---- check and plan ---------------------------------------------------------------------------------------------------

/// Every text arm, loaded and checked against its pin and against task E (kind `change`, a Cargo
/// project, every declared capability offered), as provenance.
fn arms() -> Result<Vec<Value>, String> {
    let root = exp::repo_root();
    let offered = exp::declared_capabilities();
    let mut out = Vec::new();
    for arm in Arm::ALL {
        match exp::prepare(arm, &root)? {
            None => out.push(json!({"arm": arm.name(), "text_bytes": 0})),
            Some(i) => out.push(
                i.select(GoalKind::Change, &[BuildSystem::Cargo], &offered)?
                    .provenance()
                    .clone(),
            ),
        }
    }
    Ok(out)
}

fn fixture_digest() -> Value {
    let dir = std::env::temp_dir().join(format!("skill-experiment-fixture-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    agent_bench::build_fixture(TaskId::E, &dir);
    let files = sha_tree(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    files
}

fn version_of(program: &str) -> String {
    Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unavailable".into())
}

fn check() -> i32 {
    match arms() {
        Ok(arms) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "arms": arms,
                    "task": "E",
                    "fixture_files": fixture_digest(),
                    "label": exp::LABEL,
                }))
                .unwrap()
            );
            0
        }
        Err(why) => {
            eprintln!("error: {why}");
            3
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn trials(args: &[String]) -> Result<u32, String> {
    flag(args, "--trials")
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|n| (1..=300).contains(n))
        .ok_or_else(|| "--trials needs a number from 1 to 300".into())
}

fn plan(args: &[String]) -> i32 {
    match trials(args) {
        Ok(n) => {
            let order: Vec<Value> = exp::schedule(n)
                .into_iter()
                .map(|(round, arm)| json!({"round": round, "arm": arm.name()}))
                .collect();
            println!("{}", serde_json::to_string_pretty(&json!(order)).unwrap());
            0
        }
        Err(why) => {
            eprintln!("error: {why}");
            2
        }
    }
}

// ---- the schedule -----------------------------------------------------------------------------------------------------

/// Why the experiment must stop after this trial, if it must: an unauthorized execution, an
/// attempted unauthorized action, or a completion the independent oracle does not confirm.
fn stop_reason(record: &Value) -> Option<String> {
    if record["unauthorized_executions"].as_u64().unwrap_or(0) > 0 {
        return Some("unauthorized execution".into());
    }
    if record["attempted_unauthorized"] == true {
        return Some("attempted unauthorized action".into());
    }
    let claimed =
        record["result"]["terminal_state"] == "completed" || record["result"]["verified"] == true;
    if claimed && record["task_complete"] != true {
        return Some(
            "the work reported completion or verification the oracle does not confirm".into(),
        );
    }
    None
}

/// Failures that are not the model's or the runtime's judgment: any provider failure (a rate limit,
/// a transport error, a timeout, a malformed stream), at any point in the trial, or no decision at
/// all. A provider failure ends a work without a decision (`decision-contract.md`), so the trial
/// says nothing about the model's choices after it.
fn infrastructure_failure(record: &Value) -> bool {
    let provider_failed = record["result"]["model_failures"]
        .as_array()
        .is_some_and(|f| !f.is_empty());
    let kind = record["failure_kind"].as_str().unwrap_or("");
    provider_failed
        || (record["replies_received"].as_u64().unwrap_or(0) == 0
            && !matches!(
                kind,
                "" | "completion_refused" | "invalid_decision" | "work_limit"
            ))
}

fn run_schedule(args: &[String]) -> i32 {
    let usage = |why: &str| {
        eprintln!("error: {why}");
        2
    };
    let n = match trials(args) {
        Ok(n) => n,
        Err(why) => return usage(&why),
    };
    let Some(out) = flag(args, "--out").map(PathBuf::from) else {
        return usage("--out DIR is required");
    };
    let provider_id = match flag(args, "--provider").as_deref().map(ProviderId::parse) {
        Some(Some(
            p @ (ProviderId::Local
            | ProviderId::LocalObject
            | ProviderId::LocalNone
            | ProviderId::OpenAi),
        )) => p,
        _ => return usage("--provider must be local, local-object, local-none or openai"),
    };
    let (Some(model), Some(endpoint)) = (
        flag(args, "--model").or_else(|| flag(args, "--local-model")),
        flag(args, "--endpoint").or_else(|| flag(args, "--local-endpoint")),
    ) else {
        return usage("--model and --endpoint are required (nothing is defaulted)");
    };
    // A keyed provider: the key is read from the named variable, handed only to each trial's child
    // (as the provider's own variable), and never written anywhere.
    let key_env = flag(args, "--key-env");
    let key = match (provider_id, &key_env) {
        (ProviderId::OpenAi, Some(var)) => match std::env::var(var) {
            Ok(k) if !k.trim().is_empty() => Some(k),
            _ => return usage(&format!("{var} is not set")),
        },
        (ProviderId::OpenAi, None) => return usage("--provider openai needs --key-env VAR"),
        (_, Some(_)) => return usage("--key-env applies to --provider openai only"),
        (_, None) => None,
    };
    // A request pacer (`CHIP_PROXY`), when the provider's rate limit needs one.
    let chip_proxy = flag(args, "--chip-proxy");
    if chip_proxy
        .as_deref()
        .is_some_and(|p| !p.starts_with("http://"))
    {
        return usage("--chip-proxy must be an http://host:port CONNECT proxy");
    }
    // The spend ceiling, from per-million-token prices: required for a paid provider.
    let price = |name: &str| flag(args, name).and_then(|v| v.parse::<f64>().ok());
    let budget = match (
        price("--max-cost-usd"),
        price("--worst-trial-usd"),
        price("--price-input-per-mtok"),
        price("--price-output-per-mtok"),
        price("--price-cached-per-mtok"),
    ) {
        (Some(max), Some(worst), Some(pi), Some(po), Some(pc)) => Some((max, worst, (pi, po, pc))),
        (None, None, None, None, None) if key.is_none() => None,
        _ => {
            return usage(
                "a paid provider needs --max-cost-usd, --worst-trial-usd and --price-{input,output,cached}-per-mtok",
            );
        }
    };
    if out.exists()
        && std::fs::read_dir(&out)
            .map(|mut d| d.next().is_some())
            .unwrap_or(true)
    {
        return usage("--out must be a new or empty directory; nothing is overwritten");
    }
    let root = exp::repo_root();
    let inside = |p: &Path| {
        std::fs::canonicalize(p.parent().unwrap_or(p))
            .ok()
            .zip(std::fs::canonicalize(&root).ok())
            .is_some_and(|(p, r)| p.starts_with(r))
    };
    if inside(&out) {
        return usage(
            "--out must be outside the stack-agent checkout, so it cannot change the source fingerprint",
        );
    }
    // The source may be uncommitted: it is identified by HEAD, the saved diff and the untracked
    // files' hashes, and any change during the run stops it.
    let (fingerprint, diff, source) = source_state(&root);
    let arm_provenance = match arms() {
        Ok(a) => a,
        Err(why) => {
            eprintln!("error: {why}; nothing was run");
            return 3;
        }
    };
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: no executable path ({e})");
            return 3;
        }
    };
    let bin = out.join("bin");
    if let Err(e) = std::fs::create_dir_all(&bin).and_then(|_| {
        std::fs::create_dir_all(out.join("trials"))?;
        std::fs::create_dir_all(out.join("artifacts"))
    }) {
        eprintln!("error: cannot create {}: {e}", out.display());
        return 3;
    }
    for arm in Arm::ALL {
        let link = bin.join(format!("{LAUNCHER_PREFIX}{}", arm.name()));
        if let Err(e) = std::os::unix::fs::symlink(&exe, &link) {
            eprintln!("error: cannot create {}: {e}", link.display());
            return 3;
        }
    }
    let provider = Provider::new(provider_id, Some(model), Some(endpoint));
    let schedule = exp::schedule(n);
    let plan = json!({
        "schema": "chip.skill-experiment.plan.v1",
        "stack_agent_commit": git(&root, &["rev-parse", "HEAD"]).trim(),
        "task": "E",
        "fixture_files": fixture_digest(),
        "work_args": exp::trial_work_args(&provider),
        "state_arm": StateArm::Legacy.name(),
        "provider": {
            "id": provider.id.name(),
            "model": provider.model,
            "endpoint": provider.endpoint,
            "wire": provider.wire,
            "compat_flags": provider.compat_flags,
        },
        "arms": arm_provenance,
        "label": exp::LABEL,
        "schedule": schedule.iter().map(|(r, a)| json!({"round": r, "arm": a.name()})).collect::<Vec<_>>(),
        "pax": version_of("pax"),
        "cargo": version_of("cargo"),
        "git": version_of("git"),
        "launcher": exe.display().to_string(),
        "launcher_sha256": file_sha(&exe),
        "source_fingerprint": fingerprint,
        "source": source,
        "pax_path": Command::new("sh").args(["-c", "command -v pax"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default(),
        "chip_proxy": chip_proxy,
        "credential_variable": key_env,
        "credential_supplied": key.is_some(),
        "ssl_cert_file": std::env::var("SSL_CERT_FILE").ok(),
        "budget": budget.map(|(max, worst, (pi, po, pc))| json!({
            "max_cost_usd": max,
            "worst_trial_usd": worst,
            "usd_per_million": {"input": pi, "output": po, "cached_input": pc},
            "rule": "before each trial, stop if the conservative spend so far plus one worst-case trial would exceed max_cost_usd",
        })),
    });
    let _ = std::fs::write(out.join("source.diff"), &diff);
    if let Some(proxy) = &chip_proxy {
        let _ = std::fs::write(out.join(PROXY_FILE), proxy);
    }
    let _ = std::fs::write(
        out.join("plan.json"),
        serde_json::to_string_pretty(&plan).unwrap(),
    );
    let cfg = agent_bench::Config {
        env_file: PathBuf::new(),
        providers: vec![provider_id],
        tasks: vec![TaskId::E],
        trials: n,
        out: out.clone(),
        openai_model: None,
        anthropic_model: None,
        local_model: Some(provider.model.clone()),
        local_endpoint: Some(provider.endpoint.clone()),
        openai_endpoint: None,
        anthropic_endpoint: None,
        exe: PathBuf::new(),
        state_arms: vec![StateArm::Legacy],
    };
    let mut tally: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
    let exe_sha = file_sha(&exe);
    let mut spent = (0.0f64, 0.0f64);
    let halt = |index: usize, reason: &str, file: Option<&Path>| {
        let _ = std::fs::write(
            out.join("STOPPED.json"),
            json!({"after_trial": index, "file": file.map(|f| f.display().to_string()), "reason": reason})
                .to_string(),
        );
        eprintln!("STOPPED after trial {index}: {reason}");
        1
    };
    for (index, (round, arm)) in schedule.iter().enumerate() {
        if source_state(&root).0 != fingerprint || file_sha(&exe) != exe_sha {
            return halt(
                index,
                "invalid: the source or the launcher changed during the run",
                None,
            );
        }
        if let Some((max, worst, _)) = budget
            && spent.0 + worst > max
        {
            return halt(
                index,
                &format!(
                    "spend ceiling: {:.4} USD spent (conservative) + {worst} USD worst case > {max} USD",
                    spent.0
                ),
                None,
            );
        }
        let run_id = format!("r{round:03}-{}", arm.name());
        let cfg = agent_bench::Config {
            exe: bin.join(format!("{LAUNCHER_PREFIX}{}", arm.name())),
            ..cfg.clone()
        };
        eprintln!(
            "trial {}/{}: round {round}, {}",
            index + 1,
            schedule.len(),
            arm.name()
        );
        let mut record = agent_bench::run_trial(
            &cfg,
            TaskId::E,
            &provider,
            *round,
            key.as_deref(),
            &run_id,
            StateArm::Legacy,
        );
        let artifact_name = format!(
            "chip-agent-bench-{}-E-{}-{round}-{run_id}",
            std::process::id(),
            provider.id.name()
        );
        let artifacts = out.join("artifacts").join(&artifact_name);
        let instructions = std::fs::read_to_string(artifacts.join("stderr.log"))
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("note: experiment instructions: "))
                    .and_then(|j| serde_json::from_str::<Value>(j).ok())
            });
        record["context_arm"] = json!(arm.name());
        record["round"] = json!(round);
        record["position"] = json!(index);
        record["instructions"] = instructions.unwrap_or(Value::Null);
        record["artifacts"] = json!(format!("artifacts/{artifact_name}"));
        record["infrastructure_failure"] = json!(infrastructure_failure(&record));
        record["source_fingerprint"] = json!(fingerprint);
        record["launcher_sha256"] = json!(exe_sha);
        if let Some((_, _, prices)) = budget {
            let (conservative, discounted) = trial_cost(&record, prices);
            spent.0 += conservative;
            spent.1 += discounted;
            record["cost_usd"] =
                json!({"conservative": conservative, "with_cache_discount": discounted});
            record["spent_usd"] = json!({"conservative": spent.0, "with_cache_discount": spent.1});
        }
        let stop = stop_reason(&record);
        record["stop_reason"] = json!(stop);
        let file =
            out.join("trials")
                .join(format!("{:03}-r{round:03}-{}.json", index + 1, arm.name()));
        let _ = std::fs::write(&file, serde_json::to_string_pretty(&record).unwrap());
        let t = tally.entry(arm.name()).or_default();
        t.0 += 1;
        t.1 += u32::from(record["task_complete"] == true);
        if let Some(why) = stop {
            let _ = std::fs::write(
                out.join("STOPPED.json"),
                json!({"after_trial": index + 1, "file": file.display().to_string(), "reason": why}).to_string(),
            );
            eprintln!(
                "STOPPED after trial {}: {why}; the trace is in {}",
                index + 1,
                file.display()
            );
            return 1;
        }
    }
    for (arm, (ran, complete)) in &tally {
        eprintln!(
            "{arm}: {complete}/{ran} task_complete (descriptive only; see the pre-registered analysis)"
        );
    }
    0
}
