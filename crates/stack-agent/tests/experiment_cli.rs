//! `stack-agent experiment` through the real binary, and the guarantee that it is the only way in:
//! with experimentation not invoked, `work` behaves exactly as before, whatever the environment says.
//! No model API key and no network are used.

#![cfg(unix)]

#[path = "support/gated.rs"]
mod gated;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use gated::{Model, PASSED, Pax, Reply, command, project, unique};
use serde_json::Value;

const SENTINEL: &str = "sk-SENTINEL-experiment-cli-0123456789";

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_stack-agent"));
    // Nothing inherited selects a provider; the sentinel stands in for a real key.
    c.env_remove("CHIP_PROVIDER")
        .env_remove("CHIP_MODEL")
        .env_remove("CHIP_ENDPOINT")
        .env_remove("OPENAI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env("CHIP_API_KEY", SENTINEL);
    c
}

fn experiment(args: &[&str], work_dir: &Path) -> Output {
    let mut c = bin();
    c.arg("experiment").args(args);
    if args.first() == Some(&"run") {
        c.arg("--work-dir").arg(work_dir);
    }
    c.output().unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(out)))
}

fn definition_file(tag: &str, edit: impl FnOnce(&mut Value)) -> PathBuf {
    let mut v = chip_experiments::reference::isolated_experiment().to_json();
    edit(&mut v);
    let path = unique(&format!("definition-{tag}")).join("experiment.json");
    std::fs::write(&path, v.to_string()).unwrap();
    path
}

#[test]
fn list_validate_and_run_the_reference_experiment() {
    let wd = unique("cli-run");
    let out = experiment(&["list", "--json"], &wd);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let listed = json(&out);
    let ids: Vec<&str> = listed["experiments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "autoconfig.change-context",
            "reference.retries-isolated",
            "reference.retries-replay",
            "stack-agent.observe-change"
        ]
    );
    assert!(
        listed["strategies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "stack-agent.change" && s["production"] == true)
    );

    let out = experiment(&["validate", "reference.retries-isolated", "--json"], &wd);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(json(&out)["valid"], true);

    let report_path = wd.join("report.json");
    let out = experiment(
        &[
            "run",
            "reference.retries-isolated",
            "--json",
            "--report",
            report_path.to_str().unwrap(),
        ],
        &wd,
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let printed = json(&out);
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&report_path).unwrap()).unwrap();
    assert_eq!(printed, written);
    assert_eq!(written["schema"], "chip.experiment-report.v1");
    assert_eq!(written["comparison"]["winner"], "baseline");
    assert_eq!(
        written["comparison"]["verified"],
        serde_json::json!({"baseline": 2, "candidate": 1, "tasks": 2})
    );
    assert_eq!(
        written["provenance"]["host"],
        format!("stack-agent {}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(written["cleanup"]["ok"], true);
    let left: Vec<_> = std::fs::read_dir(&wd)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(
        left,
        vec![std::ffi::OsString::from("report.json")],
        "no disposable directory is left"
    );
    assert!(!text(&out).contains(SENTINEL));
}

#[test]
fn observe_runs_the_production_policy_only_and_replay_executes_nothing() {
    let wd = unique("cli-observe");
    let out = experiment(&["run", "stack-agent.observe-change", "--json"], &wd);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let r = json(&out);
    assert_eq!(r["runs"].as_array().unwrap().len(), 1);
    assert_eq!(r["runs"][0]["side"], "baseline");
    assert_eq!(r["strategies"]["baseline"]["production"], true);
    assert_eq!(r["runs"][0]["verdict"]["class"], "verified");
    assert_eq!(r["comparison"]["conclusion"]["name"], "not_applicable");

    let wd = unique("cli-replay");
    let out = experiment(&["run", "reference.retries-replay"], &wd);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(
        text(&out).contains("Conclusion: baseline verifies more"),
        "{}",
        text(&out)
    );
    assert!(
        std::fs::read_dir(&wd).unwrap().next().is_none(),
        "replay created nothing"
    );
}

#[test]
fn invalid_definitions_fail_clearly_with_the_shared_exit_codes() {
    let wd = unique("cli-invalid");
    let out = experiment(&["run", "no.such-experiment", "--json"], &wd);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json(&out)["error"]["code"], "unknown_experiment");

    for (tag, edit, code, exit) in [
        (
            "shadow",
            Box::new(|v: &mut Value| v["mode"] = "shadow".into()) as Box<dyn FnOnce(&mut Value)>,
            "unsupported_mode",
            2,
        ),
        (
            "plugin",
            Box::new(|v: &mut Value| v["candidate"]["id"] = "nobody".into()),
            "unknown_strategy",
            2,
        ),
        (
            "git",
            Box::new(|v: &mut Value| {
                v["capabilities"]
                    .as_array_mut()
                    .unwrap()
                    .push("project.git.status".into())
            }),
            "isolation_unavailable",
            3,
        ),
    ] {
        let path = definition_file(tag, edit);
        let out = experiment(
            &["run", "--definition", path.to_str().unwrap(), "--json"],
            &wd,
        );
        assert_eq!(out.status.code(), Some(exit), "{tag}: {}", text(&out));
        assert_eq!(json(&out)["error"]["code"], code, "{tag}");
        assert!(
            std::fs::read_dir(&wd)
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
            "{tag}: nothing ran"
        );
    }
    assert_eq!(experiment(&["frobnicate"], &wd).status.code(), Some(2));
    assert_eq!(experiment(&["run"], &wd).status.code(), Some(2));
    assert_eq!(experiment(&["run", "a", "b"], &wd).status.code(), Some(2));
    let help = experiment(&["--help"], &wd);
    assert_eq!(help.status.code(), Some(0));
    for mode in [
        "observe",
        "replay",
        "isolated",
        "shadow and canary are reserved",
    ] {
        assert!(text(&help).contains(mode), "{mode}");
    }
}

#[test]
fn a_secret_in_the_definition_never_reaches_any_output() {
    let wd = unique("cli-secret");
    let path = definition_file("secret", |v| {
        v["description"] = format!("uses key {SENTINEL}").into()
    });
    let report = wd.join("r.json");
    for args in [
        vec!["validate", "--definition", path.to_str().unwrap()],
        vec![
            "run",
            "--definition",
            path.to_str().unwrap(),
            "--report",
            report.to_str().unwrap(),
        ],
        vec!["run", "--definition", path.to_str().unwrap(), "--json"],
    ] {
        let out = experiment(&args, &wd);
        assert!(out.status.code().is_some_and(|c| c == 0), "{}", text(&out));
        assert!(!text(&out).contains(SENTINEL), "{args:?}");
    }
    let written = std::fs::read_to_string(&report).unwrap();
    assert!(!written.contains(SENTINEL));
    assert!(written.contains("uses key [redacted]"));
}

// ---- production is unchanged when experimentation is not invoked ----------------------------------

const READ: &str = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#;
const BLOCK: &str = r#"{"decision":"block","reason":"scripted"}"#;

/// `work --json` with timing fields removed, so two runs can be compared exactly.
fn stable(mut v: Value) -> Value {
    fn strip(v: &mut Value) {
        match v {
            Value::Object(o) => {
                // Timing, the endpoint's port, the learning section (whose document ids are unique per
                // run by design) and the work's own id (random per run, by design) differ between any
                // two runs.
                o.retain(|k, _| {
                    !k.contains("latency")
                        && !k.ends_with("_ms")
                        && k != "endpoint"
                        && k != "learning"
                        && k != "work_id"
                        && k != "task_id"
                        // An episode is named after its work, so its id is random per run too.
                        && k != "episode"
                });
                o.values_mut().for_each(strip);
            }
            Value::Array(a) => a.iter_mut().for_each(strip),
            _ => {}
        }
    }
    strip(&mut v);
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn work_is_unchanged_whatever_the_environment_says_about_experiments() {
    let mut outputs = Vec::new();
    for (tag, env) in [
        ("plain", vec![]),
        (
            "env",
            vec![
                ("CHIP_EXPERIMENT", "reference.retries-isolated"),
                ("STACK_AGENT_EXPERIMENT", "reference.retries-isolated"),
                ("CHIP_STRATEGY", "reference.search-then-write"),
                ("STACK_AGENT_STRATEGY", "reference.search-then-write"),
            ],
        ),
    ] {
        let model = Model::start(vec![Reply::Stream(READ), Reply::Stream(BLOCK)]).await;
        let pax = Pax::new(&format!("prod-{tag}"), PASSED, false);
        let dir = project(&format!("prod-{tag}"));
        let mut c = command(&dir, &model.url, &pax);
        c.args(["work", "--json", "Make the tests pass."]).envs(env);
        let out = tokio::task::spawn_blocking(move || c.output().unwrap())
            .await
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{}", text(&out));
        outputs.push((stable(json(&out)), model.requests()));
    }
    assert_eq!(
        outputs[0], outputs[1],
        "an experiment-looking variable changed `work`"
    );
    assert_eq!(
        outputs[0].1, 2,
        "the production path asked the model, as always"
    );
}

#[test]
fn only_the_experiment_command_reaches_the_experiment_crate() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut users = Vec::new();
    for entry in std::fs::read_dir(&src).unwrap().flatten() {
        let path = entry.path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        if text.contains("chip_experiments::") || text.contains("experiment::experiment(") {
            users.push(path.file_name().unwrap().to_string_lossy().into_owned());
        }
    }
    users.sort();
    assert_eq!(
        users,
        ["experiment.rs", "main.rs"],
        "who uses the experiment crate"
    );
    let main = std::fs::read_to_string(src.join("main.rs")).unwrap();
    assert_eq!(
        main.matches("experiment::experiment(").count(),
        1,
        "one entry point"
    );
    assert!(
        main.contains("args[1] == \"experiment\""),
        "reached only by the explicit subcommand"
    );
    // The experiment crate reads no environment variable: no hidden switch.
    let crate_src = src
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("chip-experiments/src");
    for entry in std::fs::read_dir(crate_src).unwrap().flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap();
        assert!(
            !text.contains("env::var"),
            "{} reads the environment",
            entry.path().display()
        );
    }
}
