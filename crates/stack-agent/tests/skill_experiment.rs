//! The native-skill experiment seam (`chip_cli::skill_experiment`): integrity, fail-closed loading,
//! and that an arm changes what the model is told and nothing else.
//!
//! The end-to-end tests run the product's own `run_software_work_durable` over the real
//! `LocalEnvironment` with a scripted model that records every request. They need neither PAX nor a
//! model: every scripted step is refused or is a local read.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use chip_cli::agent_bench::{self, Provider, ProviderId, TaskId};
use chip_cli::autoconfig::profile::BuildSystem;
use chip_cli::local_environment::{LocalEnvironment, opaque_id};
use chip_cli::skill_experiment::{
    self as exp, AddedInstructions, Arm, LABEL, LENGTH_CONTROL, REVIEWED_SKILLS, ReviewedControl,
    ReviewedSkill, WithInstructions,
};
use chip_cli::software_work::{
    GoalKind, SoftwareWork, default_context_policy, run_software_work_durable,
    run_software_work_kind,
};
use chip_core::{
    DecisionRecord, EnvironmentDescription, EscalationContextPolicy, ExecutionEvent, Observation,
    ObservationOrigin, WorkEvent, WorkId, WorkLimits, WorkOutcome, WorkState, WorkTrajectory,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use chip_pax::PaxExecutor;
use sha2::{Digest, Sha256};

const SKILL_FILES: [&str; 3] = ["skill.json", "instructions.md", "refs/rust.md"];

fn root() -> PathBuf {
    exp::repo_root()
}

fn skill_entry() -> ReviewedSkill {
    REVIEWED_SKILLS[0]
}

fn tmp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-skill-exp-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A copy of the reviewed example skill at `<base>/skill`.
fn copy_example(tag: &str) -> (PathBuf, PathBuf) {
    let base = tmp(tag);
    let dir = base.join("skill");
    for f in SKILL_FILES {
        let to = dir.join(f);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(root().join(skill_entry().dir).join(f), to).unwrap();
    }
    (base, dir)
}

fn read(dir: &Path, f: &str) -> String {
    std::fs::read_to_string(dir.join(f)).unwrap()
}

fn set_manifest(dir: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let mut m: serde_json::Value = serde_json::from_str(&read(dir, "skill.json")).unwrap();
    edit(&mut m);
    std::fs::write(
        dir.join("skill.json"),
        serde_json::to_string_pretty(&m).unwrap(),
    )
    .unwrap();
}

fn refused(dir: &Path, needle: &str) {
    match exp::load_skill_dir(dir) {
        Ok(s) => panic!(
            "accepted a skill that should be refused ({needle}): {}",
            s.digest()
        ),
        Err(why) => assert!(why.contains(needle), "refused for another reason: {why}"),
    }
}

fn declared() -> Vec<String> {
    exp::declared_capabilities()
}

// ---- integrity ----------------------------------------------------------------------------------------------------------

#[test]
fn the_reviewed_artifacts_load_and_match_their_pins() {
    assert!(exp::prepare(Arm::Baseline, &root()).unwrap().is_none());
    let skill = exp::load_reviewed(&skill_entry(), &root()).unwrap();
    assert_eq!(skill.digest(), skill_entry().digest);
    assert!(exp::prepare(Arm::Skill, &root()).unwrap().is_some());
    assert!(exp::prepare(Arm::LengthControl, &root()).unwrap().is_some());
}

/// The digest is exactly the documented construction, computed here independently.
#[test]
fn the_digest_is_the_documented_construction() {
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for f in SKILL_FILES {
        files.insert(
            f.into(),
            std::fs::read(root().join(skill_entry().dir).join(f)).unwrap(),
        );
    }
    let mut h = Sha256::new();
    h.update(b"chip.skill.v1\n");
    for (p, b) in &files {
        h.update(p.as_bytes());
        h.update([0]);
        h.update((b.len() as u64).to_be_bytes());
        h.update(b);
    }
    let expected = format!(
        "sha256:{}",
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    assert_eq!(expected, skill_entry().digest);
    assert_eq!(exp::digest_files("chip.skill.v1\n", &files), expected);
}

#[test]
fn every_file_change_invalidates_the_pin() {
    for (i, f) in SKILL_FILES.iter().enumerate() {
        let (base, dir) = copy_example(&format!("tamper-{i}"));
        // A trailing space keeps JSON and Markdown valid, so only the pin can catch it.
        let mut bytes = std::fs::read(dir.join(f)).unwrap();
        bytes.push(b' ');
        std::fs::write(dir.join(f), bytes).unwrap();
        let loaded = exp::load_skill_dir(&dir).unwrap();
        assert_ne!(loaded.digest(), skill_entry().digest, "{f}");
        let entry = ReviewedSkill {
            dir: "skill",
            ..skill_entry()
        };
        let why = exp::load_reviewed(&entry, &base).unwrap_err();
        assert!(why.contains("review pinned"), "{f}: {why}");
    }
    // A version bump without a new pin is refused too.
    let (base, dir) = copy_example("tamper-version");
    set_manifest(&dir, |m| m["version"] = 2.into());
    let entry = ReviewedSkill {
        dir: "skill",
        ..skill_entry()
    };
    assert!(
        exp::load_reviewed(&entry, &base)
            .unwrap_err()
            .contains("review accepted")
    );
}

#[test]
fn a_changed_length_control_is_refused() {
    let base = tmp("control");
    let to = base.join(LENGTH_CONTROL.path);
    std::fs::create_dir_all(to.parent().unwrap()).unwrap();
    let mut text = std::fs::read_to_string(root().join(LENGTH_CONTROL.path)).unwrap();
    text.push('.');
    std::fs::write(&to, text).unwrap();
    assert!(
        exp::load_control(&LENGTH_CONTROL, &base)
            .unwrap_err()
            .contains("review pinned")
    );
    let traversal = ReviewedControl {
        path: "../outside.md",
        ..LENGTH_CONTROL
    };
    assert!(exp::load_control(&traversal, &base).is_err());
}

#[test]
fn the_registry_refuses_a_duplicate_id() {
    exp::check_registry(REVIEWED_SKILLS).unwrap();
    let twice = [skill_entry(), skill_entry()];
    assert!(exp::check_registry(&twice).unwrap_err().contains("twice"));
}

// ---- fail-closed loading -------------------------------------------------------------------------------------------------

#[test]
fn undeclared_files_and_scripts_are_refused() {
    for (i, extra) in ["run.sh", "refs/extra.md", ".hidden", "bin/tool"]
        .iter()
        .enumerate()
    {
        let (_, dir) = copy_example(&format!("extra-{i}"));
        let to = dir.join(extra);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::write(&to, "#!/bin/sh\nrm -rf .\n").unwrap();
        refused(&dir, "not declared");
    }
    let (_, dir) = copy_example("missing");
    std::fs::remove_file(dir.join("refs/rust.md")).unwrap();
    refused(&dir, "declared but missing");
}

#[test]
fn symbolic_links_are_refused() {
    let (base, dir) = copy_example("link-file");
    std::fs::write(base.join("elsewhere.md"), "## Verification\n").unwrap();
    std::fs::remove_file(dir.join("instructions.md")).unwrap();
    std::os::unix::fs::symlink(base.join("elsewhere.md"), dir.join("instructions.md")).unwrap();
    refused(&dir, "symbolic link");

    let (base, dir) = copy_example("link-dir");
    std::fs::rename(dir.join("refs"), base.join("refs")).unwrap();
    std::os::unix::fs::symlink(base.join("refs"), dir.join("refs")).unwrap();
    refused(&dir, "symbolic link");

    let (base, dir) = copy_example("link-root");
    std::os::unix::fs::symlink(&dir, base.join("alias")).unwrap();
    refused(&base.join("alias"), "not a plain directory");
}

#[test]
fn paths_outside_the_skill_or_of_the_wrong_kind_are_refused() {
    for (i, path) in [
        "../instructions.md",
        "/etc/instructions.md",
        "refs/../instructions.md",
        "./instructions.md",
        "C:instructions.md",
        "refs\\rust.md",
        "instructions.txt",
        "",
    ]
    .iter()
    .enumerate()
    {
        let (_, dir) = copy_example(&format!("path-{i}"));
        set_manifest(&dir, |m| m["instructions"] = (*path).into());
        assert!(exp::load_skill_dir(&dir).is_err(), "{path:?} was accepted");
    }
    let (_, dir) = copy_example("path-dup");
    set_manifest(&dir, |m| {
        m["references"][0]["path"] = "instructions.md".into()
    });
    refused(&dir, "twice");
}

#[test]
fn members_that_would_carry_authority_are_refused_even_when_empty() {
    for (i, member) in [
        "inputs",
        "outputs",
        "permissions",
        "effects",
        "scripts",
        "commands",
        "executable",
        "tools",
    ]
    .iter()
    .enumerate()
    {
        let (_, dir) = copy_example(&format!("forbidden-{i}"));
        set_manifest(&dir, |m| m[*member] = serde_json::json!({}));
        refused(&dir, &format!("`{member}`"));
    }
    let (_, dir) = copy_example("unknown");
    set_manifest(&dir, |m| m["priority"] = 1.into());
    refused(&dir, "unknown field");
    let (_, dir) = copy_example("unknown-nested");
    set_manifest(&dir, |m| {
        m["applies_to"]["models"] = serde_json::json!(["any"])
    });
    refused(&dir, "unknown field");
    let (_, dir) = copy_example("unknown-reference");
    set_manifest(&dir, |m| m["references"][0]["run"] = true.into());
    refused(&dir, "unknown field");
}

#[test]
fn malformed_or_oversized_manifests_fail_closed() {
    type Edit = Box<dyn Fn(&Path)>;
    let cases: Vec<(&str, Edit, &str)> = vec![
        (
            "not-json",
            Box::new(|d: &Path| {
                std::fs::write(d.join("skill.json"), "schema: chip.skill.v1").unwrap()
            }),
            "strict JSON",
        ),
        (
            "trailing-comma",
            Box::new(|d: &Path| {
                let t = read(d, "skill.json").replacen("\"version\": 1,", "\"version\": 1,,", 1);
                std::fs::write(d.join("skill.json"), t).unwrap();
            }),
            "strict JSON",
        ),
        (
            "comment",
            Box::new(|d: &Path| {
                let t = format!("// reviewed\n{}", read(d, "skill.json"));
                std::fs::write(d.join("skill.json"), t).unwrap();
            }),
            "strict JSON",
        ),
        (
            "duplicate-key",
            Box::new(|d: &Path| {
                let t = read(d, "skill.json").replacen(
                    "\"version\": 1,",
                    "\"version\": 1, \"version\": 1,",
                    1,
                );
                std::fs::write(d.join("skill.json"), t).unwrap();
            }),
            "duplicate field",
        ),
        (
            "array",
            Box::new(|d: &Path| std::fs::write(d.join("skill.json"), "[]").unwrap()),
            "not a JSON object",
        ),
        (
            "oversized",
            Box::new(|d: &Path| set_manifest(d, |m| m["description"] = "x".repeat(5000).into())),
            "at most 4096",
        ),
        (
            "schema",
            Box::new(|d: &Path| set_manifest(d, |m| m["schema"] = "chip.skill.v2".into())),
            "only \"chip.skill.v1\"",
        ),
        (
            "id",
            Box::new(|d: &Path| set_manifest(d, |m| m["id"] = "Diagnose".into())),
            "id",
        ),
        (
            "id-path",
            Box::new(|d: &Path| set_manifest(d, |m| m["id"] = "repo/../x".into())),
            "id",
        ),
        (
            "version",
            Box::new(|d: &Path| set_manifest(d, |m| m["version"] = 0.into())),
            "version",
        ),
        (
            "version-type",
            Box::new(|d: &Path| set_manifest(d, |m| m["version"] = "1".into())),
            "does not match",
        ),
        (
            "missing",
            Box::new(|d: &Path| {
                set_manifest(d, |m| {
                    m.as_object_mut().unwrap().remove("capabilities");
                })
            }),
            "missing field",
        ),
        (
            "description",
            Box::new(|d: &Path| set_manifest(d, |m| m["description"] = "two\nlines".into())),
            "description",
        ),
        (
            "kinds",
            Box::new(|d: &Path| {
                set_manifest(d, |m| m["applies_to"]["kinds"] = serde_json::json!([]))
            }),
            "kinds is empty",
        ),
        (
            "kind",
            Box::new(|d: &Path| {
                set_manifest(d, |m| {
                    m["applies_to"]["kinds"] = serde_json::json!(["deploy"])
                })
            }),
            "unknown kind",
        ),
        (
            "build",
            Box::new(|d: &Path| {
                set_manifest(d, |m| {
                    m["references"][0]["applies_to"]["build_systems"] = serde_json::json!(["make"])
                })
            }),
            "unknown build system",
        ),
        (
            "ref-kind",
            Box::new(|d: &Path| {
                set_manifest(d, |m| {
                    m["references"][0]["applies_to"]["kinds"] = serde_json::json!(["inspect"])
                })
            }),
            "does not",
        ),
        (
            "capability",
            Box::new(|d: &Path| {
                set_manifest(d, |m| m["capabilities"] = serde_json::json!(["shell.exec"]))
            }),
            "not one any stack-agent capability declares",
        ),
        (
            "capability-twice",
            Box::new(|d: &Path| {
                set_manifest(d, |m| {
                    m["capabilities"] = serde_json::json!(["pax.test", "pax.test"])
                })
            }),
            "twice",
        ),
        (
            "verification",
            Box::new(|d: &Path| {
                let t = read(d, "instructions.md").replace("## Verification", "## Checking");
                std::fs::write(d.join("instructions.md"), t).unwrap();
            }),
            "Verification",
        ),
        (
            "utf8",
            Box::new(|d: &Path| {
                std::fs::write(d.join("refs/rust.md"), [0xff, 0xfe, b'a']).unwrap()
            }),
            "not UTF-8",
        ),
        (
            "control",
            Box::new(|d: &Path| std::fs::write(d.join("refs/rust.md"), "a\u{1b}[2Jb").unwrap()),
            "control character",
        ),
        (
            "too-much-text",
            Box::new(|d: &Path| std::fs::write(d.join("refs/rust.md"), "x".repeat(3000)).unwrap()),
            "at most 4096",
        ),
    ];
    for (tag, edit, needle) in cases {
        let (_, dir) = copy_example(&format!("malformed-{tag}"));
        edit(&dir);
        refused(&dir, needle);
    }
}

// ---- selection: a skill can only fail to apply ------------------------------------------------------------------------------

fn selected(arm: Arm) -> AddedInstructions {
    exp::prepare(arm, &root())
        .unwrap()
        .unwrap()
        .select(GoalKind::Change, &[BuildSystem::Cargo], &declared())
        .unwrap()
}

#[test]
fn a_skill_cannot_require_or_add_a_capability_the_work_does_not_offer() {
    let arm = exp::prepare(Arm::Skill, &root()).unwrap().unwrap();
    let without_pax: Vec<String> = declared().into_iter().filter(|c| c != "pax.test").collect();
    let why = arm
        .select(GoalKind::Change, &[BuildSystem::Cargo], &without_pax)
        .unwrap_err();
    assert!(
        why.contains("does not offer") && why.contains("pax.test"),
        "{why}"
    );
    // It never applies to an inspection (whose capabilities are read-only).
    assert!(
        arm.select(GoalKind::Inspect, &[BuildSystem::Cargo], &declared())
            .is_err()
    );
    // Selected, its provenance names only what it was given; the offered set is an input it reads.
    let s = selected(Arm::Skill);
    assert_eq!(
        s.provenance()["files"],
        serde_json::json!(["instructions.md", "refs/rust.md"])
    );
}

#[test]
fn a_reference_loads_only_where_its_predicate_matches() {
    let arm = exp::prepare(Arm::Skill, &root()).unwrap().unwrap();
    let npm = arm
        .select(GoalKind::Change, &[BuildSystem::Npm], &declared())
        .unwrap();
    let dir = root().join(skill_entry().dir);
    assert_eq!(npm.text(), read(&dir, "instructions.md"));
    assert_eq!(
        selected(Arm::Skill).text(),
        format!(
            "{}\n{}",
            read(&dir, "instructions.md"),
            read(&dir, "refs/rust.md")
        )
    );
}

#[test]
fn the_length_control_is_comparable_in_length_and_task_irrelevant() {
    let skill = selected(Arm::Skill).text().len() as f64;
    let control = selected(Arm::LengthControl);
    let ratio = control.text().len() as f64 / skill;
    assert!((0.95..=1.05).contains(&ratio), "control/skill = {ratio}");
    for word in [
        "pax",
        "project.",
        "test",
        "capabilit",
        "write",
        "verify",
        "complete",
    ] {
        assert!(
            !control.text().to_lowercase().contains(word),
            "the control mentions {word:?}"
        );
    }
}

// ---- the context seam ------------------------------------------------------------------------------------------------------

#[test]
fn the_seam_appends_to_the_goal_and_changes_nothing_else() {
    let state = WorkState {
        goal: "Make the tests pass.",
        turn: 2,
        max_turns: 14,
        executions: 3,
        max_executions: 10,
        context_budget_bytes: None,
    };
    let (obs, origins, decisions): (
        Vec<Observation>,
        Vec<ObservationOrigin>,
        Vec<DecisionRecord>,
    ) = (vec![], vec![], vec![]);
    let ruled_out = vec!["project.git.diff".to_string()];
    let evidence = vec!["rejected: nothing was done".to_string()];
    let trajectory = WorkTrajectory {
        observations: &obs,
        origins: &origins,
        decisions: &decisions,
        ruled_out: &ruled_out,
        evidence: &evidence,
        question: "Decide the next step. Available capabilities: pax.test; project.read (inputs: path).",
    };
    let inner = default_context_policy(GoalKind::Change);
    let base = inner.build(&state, &trajectory);
    for arm in [Arm::Skill, Arm::LengthControl] {
        let s = selected(arm);
        let policy = WithInstructions {
            inner,
            instructions: &s,
        };
        assert_eq!(policy.id(), inner.id());
        let with = policy.build(&state, &trajectory);
        let mut expected = base.clone();
        expected.goal = format!("{}{LABEL}{}", base.goal, s.text());
        assert_eq!(with, expected, "{}", arm.name());
        assert_eq!(with.question, base.question);
        assert_eq!(with.render_turn(), base.render_turn());
    }
}

struct Script {
    replies: Mutex<VecDeque<String>>,
    seen: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ModelProvider for Script {
    async fn complete(&self, r: ModelRequest) -> Result<ModelResponse, FxError> {
        self.seen.lock().unwrap().push(
            r.messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let mut replies = self.replies.lock().unwrap();
        let reply = if replies.len() > 1 {
            replies.pop_front()
        } else {
            replies.front().cloned()
        }
        .ok_or_else(|| FxError::Provider("the script ran out of replies".into()))?;
        Ok(ModelResponse::new("m", reply, Usage::new(1, 1)))
    }
}

const GOAL: &str = "The project's tests expect the maximum number of retries to be 5. Change the retry policy so the tests pass, without editing the tests.";
const LIMITS: WorkLimits = WorkLimits {
    max_turns: 8,
    max_executions: 8,
};

/// Task E's project, committed.
fn project(tag: &str) -> PathBuf {
    let dir = tmp(&format!("project-{tag}"));
    agent_bench::build_fixture(TaskId::E, &dir);
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["add", "-A"],
        vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-q",
            "-m",
            "baseline",
        ],
    ] {
        Command::new("git")
            .args(&args)
            .current_dir(&dir)
            .output()
            .unwrap();
    }
    dir
}

/// A model that reads the source, asks for a shell, claims success in the skill's own words, then
/// invents an input: every step but the read must be refused, in every arm. (Nothing here needs PAX.)
fn adversarial_script() -> Vec<String> {
    vec![
        r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs"}}"#.into(),
        r#"{"decision":"request_capability","capability":"shell.exec","inputs":{"command":"sh run.sh"}}"#.into(),
        r#"{"decision":"complete","summary":"A passing pax.test was observed after the last write, as the reviewed instructions require. Verified."}"#.into(),
        r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"src/lib.rs","command":"cargo test"}}"#.into(),
    ]
}

/// The reasons of the rejected decisions, in order.
fn rejections(w: &SoftwareWork) -> Vec<String> {
    w.report
        .events
        .iter()
        .filter_map(|e| match e {
            WorkEvent::DecisionRejected { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .collect()
}

async fn run(
    tag: &str,
    instructions: Option<&AddedInstructions>,
) -> (SoftwareWork, Vec<String>, PathBuf) {
    let dir = project(tag);
    let env = LocalEnvironment::new(
        opaque_id(&dir),
        &dir,
        PaxExecutor::new(&dir),
        EnvironmentDescription::default(),
    );
    let model = Arc::new(Script {
        replies: Mutex::new(adversarial_script().into()),
        seen: Mutex::new(Vec::new()),
    });
    let w = run_software_work_durable(
        GoalKind::Change,
        WorkId::new("skill-exp"),
        model.clone(),
        "scripted".into(),
        &env,
        GOAL,
        LIMITS,
        GoalKind::Change.policy(),
        None,
        None,
        None,
        None,
        None,
        instructions,
    )
    .await;
    let seen = model.seen.lock().unwrap().clone();
    (w, seen, dir)
}

fn executed(w: &SoftwareWork) -> Vec<String> {
    let started = w
        .report
        .events
        .iter()
        .filter(|e| {
            matches!(
                e,
                WorkEvent::Execution(ExecutionEvent::ExecutionStarted { .. })
            )
        })
        .count();
    assert_eq!(started, w.report.origins.len());
    w.report
        .origins
        .iter()
        .map(|o| o.capability.to_string())
        .collect()
}

fn normalize(request: &str, dir: &Path) -> String {
    request.replace(&dir.display().to_string(), "<dir>")
}

#[tokio::test(flavor = "multi_thread")]
async fn every_arm_runs_the_same_work_and_differs_only_in_the_added_text() {
    let (base, base_seen, base_dir) = run("baseline", None).await;
    assert!(!base_seen.is_empty());
    for arm in [Arm::Skill, Arm::LengthControl] {
        let s = selected(arm);
        let (w, seen, dir) = run(arm.name(), Some(&s)).await;
        assert_eq!(seen.len(), base_seen.len(), "{}", arm.name());
        // The goal Chip sends is the task's goal followed by Chip's own capability and completion
        // rules; the arm's text follows all of it, immediately before the question.
        for (i, (with, without)) in seen.iter().zip(&base_seen).enumerate() {
            assert!(without.starts_with(&format!("Goal: {GOAL}")));
            assert_eq!(without.matches("\nQuestion: ").count(), 1);
            let expected = normalize(without, &base_dir).replacen(
                "\nQuestion: ",
                &format!("{LABEL}{}\nQuestion: ", s.text()),
                1,
            );
            assert_eq!(
                normalize(with, &dir),
                expected,
                "{} request {i}",
                arm.name()
            );
        }
        // The same executions, the same refusals, the same outcome, and nothing completed.
        assert_eq!(executed(&w), executed(&base));
        assert_eq!(executed(&w), vec!["project.read".to_string()]);
        let rejected = rejections(&w);
        assert_eq!(rejected, rejections(&base));
        assert_eq!(rejected.len(), 3, "{rejected:?}");
        assert!(rejected[0].contains("shell.exec"), "{}", rejected[0]);
        assert!(
            rejected[1].contains("completion refused"),
            "{}",
            rejected[1]
        );
        assert!(rejected[2].contains("command"), "{}", rejected[2]);
        assert_eq!(
            format!("{:?}", w.report.outcome),
            format!("{:?}", base.report.outcome)
        );
        assert!(!matches!(w.report.outcome, WorkOutcome::Completed { .. }));
        assert!(!w.verified && !base.verified);
        assert_eq!(w.writes, 0);
        assert!(w.audit.is_clean() && base.audit.is_clean());
        assert_eq!(
            w.audit.unauthorized_executions + w.audit.false_completions,
            0
        );
        // The project is exactly as the fixture left it: nothing the script named was run.
        assert_eq!(
            agent_bench::tree_hashes(&dir),
            agent_bench::tree_hashes(&base_dir)
        );
        assert!(!dir.join("run.sh").exists());
    }
}

/// The baseline arm is the product's path: the existing entry point and the experiment's path with
/// no instructions send byte-identical requests.
#[tokio::test(flavor = "multi_thread")]
async fn the_baseline_arm_is_the_existing_context_construction() {
    let (_, seen, dir) = run("baseline-a", None).await;
    let other = project("baseline-b");
    let env = LocalEnvironment::new(
        opaque_id(&other),
        &other,
        PaxExecutor::new(&other),
        EnvironmentDescription::default(),
    );
    let model = Arc::new(Script {
        replies: Mutex::new(adversarial_script().into()),
        seen: Mutex::new(Vec::new()),
    });
    let _ = run_software_work_kind(
        GoalKind::Change,
        WorkId::new("skill-exp"),
        model.clone(),
        "scripted".into(),
        &env,
        GOAL,
        LIMITS,
        GoalKind::Change.policy(),
        None,
    )
    .await;
    let product: Vec<String> = model
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|r| normalize(r, &other))
        .collect();
    let baseline: Vec<String> = seen.iter().map(|r| normalize(r, &dir)).collect();
    assert_eq!(product, baseline);
    assert!(!baseline.iter().any(|r| r.contains(LABEL.trim())));
}

// ---- the trial plan ----------------------------------------------------------------------------------------------------------

#[test]
fn the_schedule_interleaves_the_arms_evenly() {
    let s = exp::schedule(9);
    assert_eq!(s.len(), 27);
    for round in 1..=9 {
        let arms: Vec<Arm> = s
            .iter()
            .filter(|(r, _)| *r == round)
            .map(|(_, a)| *a)
            .collect();
        let mut sorted = arms.clone();
        sorted.sort();
        assert_eq!(sorted, Arm::ALL.to_vec(), "round {round}");
    }
    for position in 0..3 {
        for arm in Arm::ALL {
            let n = s.chunks(3).filter(|c| c[position].1 == arm).count();
            assert_eq!(n, 3, "{} in position {position}", arm.name());
        }
    }
}

#[test]
fn every_arm_gets_the_same_command_line_limits_and_model_settings() {
    let provider = Provider::new(
        ProviderId::LocalObject,
        Some("m".into()),
        Some("http://127.0.0.1:1/v1/chat/completions".into()),
    );
    let args = exp::trial_work_args(&provider);
    assert_eq!(args, agent_bench::work_args(TaskId::E, &provider));
    let joined = args.join(" ");
    for expected in [
        "--kind change",
        "--max-turns 14",
        "--max-executions 10",
        "--deadline-secs 600",
        "--call-timeout-secs 120",
        "--max-output-tokens 8192",
        "--provider-profile local --structured-output object",
    ] {
        assert!(
            joined.contains(expected),
            "{expected} missing from {joined}"
        );
    }
    assert!(!joined.contains("skill") && !joined.contains("arm"));
}
