//! Context efficiency under the cargo-workspace coverage contract, measured on a realistic
//! 28-member workspace, with the context budget the experiments use (24,000 bytes).
//!
//! `InspectBoundedContext` is the representation every inspection used before this change: it
//! condenses a manifest to several fact lines only when the request would not fit. The covered
//! policy condenses every parsed manifest to one line. Both are measured on the same trajectory, the
//! way a request is measured: the rendered context plus each observation sent in full, plus the
//! 5.7 KB question block (capability list and reply contract) every real request carries.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chip_cli::inspect_context::{InspectBoundedContext, InspectCoveredContext};
use chip_cli::inspect_coverage::{CoverageContract, Inventory};
use chip_cli::local_environment::{LocalEnvironment, opaque_id};
use chip_cli::software_work::{GoalKind, render_json, run_software_work_controlled};
use chip_core::{
    CapabilityId, EnvironmentDescription, EscalationContext, EscalationContextPolicy, ExecutionId,
    ExecutionRequest, ExecutionStatus, Executor, InputValue, Observation, ObservationKind,
    ObservationOrigin, WorkId, WorkLimits, WorkOutcome, WorkState, WorkTrajectory,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use chip_pax::PaxExecutor;
use chip_project::ProjectExecutor;

const MEMBERS: usize = 28;
/// What every real request carries beyond the plain goal and the evidence: the capability list and
/// reply contract (about 5.7 KB) and the inspection instructions in the goal text (about 3.4 KB).
/// Measured as the first request of a real run (9,062 bytes) less the plain goal's context.
const QUESTION_BYTES: usize = 8_600;
const BUDGET: usize = 24_000;
const GOAL: &str = "Inspect Cargo.toml, README.md, and the crate manifests under crates/. Summarize the workspace architecture, the responsibilities of the major crates, and their dependency relationships. List directories using paths without trailing slashes. Read manifests before implementation files. Once you have enough evidence, stop inspecting and provide the summary.";

fn name(i: usize) -> String {
    const WORDS: [&str; 14] = [
        "core",
        "fx",
        "session",
        "turns",
        "compute",
        "pax",
        "project",
        "graph",
        "wasm",
        "decision",
        "corpus",
        "remote",
        "native",
        "inference",
    ];
    format!("chip-{}-{}", WORDS[i % WORDS.len()], i)
}

fn workspace(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-efficiency-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let members: Vec<String> = (0..MEMBERS)
        .map(|i| format!("\"crates/{}\"", name(i)))
        .collect();
    let mut root = format!(
        "# One checkout, three groups of crates:\n#   product  crates/stack-agent\n#   Chip     crates/chip-*\n[workspace]\nmembers = [{}]\nresolver = \"2\"\n\n[workspace.dependencies]\nserde = \"1\"\ntoml = \"0.8\"\n",
        members.join(", ")
    );
    root.push_str("tokio = { version = \"1\", features = [\"macros\"] }\n");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Cargo.toml"), root).unwrap();
    std::fs::write(
        dir.join("README.md"),
        format!("# Demo workspace\n\nA workspace of {MEMBERS} crates used to measure inspection context.\n\n{}", "## Section\n\nSome prose.\n\n".repeat(40)),
    )
    .unwrap();
    for i in 0..MEMBERS {
        let crate_dir = dir.join(format!("crates/{}", name(i)));
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        std::fs::write(crate_dir.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        let mut m = format!(
            "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\ndescription = \"Crate {i}: owns one responsibility in the workspace architecture and exposes it to its dependents\"\n\n[dependencies]\nserde = {{ workspace = true }}\ntoml = \"0.8\"\nasync-trait = \"0.1\"\nsha2 = \"0.10\"\ntokio = {{ version = \"1\", features = [\"macros\", \"rt-multi-thread\", \"net\", \"io-util\"] }}\n",
            name(i)
        );
        let internal: std::collections::BTreeSet<usize> = [i / 2, i / 3, i / 5]
            .into_iter()
            .filter(|d| *d < i)
            .collect();
        for d in internal.iter() {
            m.push_str(&format!(
                "{} = {{ path = \"../{}\" }}\n",
                name(*d),
                name(*d)
            ));
        }
        m.push_str("\n[dev-dependencies]\ntempfile = \"3\"\nserde_json = \"1\"\n");
        m.push_str(&format!("{} = {{ path = \"../{}\" }}\n", name(0), name(0)));
        if i == 0 {
            // A crate does not depend on itself.
            m = m.replace(
                &format!("{} = {{ path = \"../{}\" }}\n", name(0), name(0)),
                "",
            );
        }
        std::fs::write(crate_dir.join("Cargo.toml"), m).unwrap();
    }
    dir
}

async fn observe(
    p: &ProjectExecutor,
    n: usize,
    cap: &str,
    path: &str,
) -> (Observation, ObservationOrigin) {
    let inputs: BTreeMap<String, InputValue> =
        [("path".to_string(), InputValue::Text(path.to_string()))].into();
    let id = ExecutionId::new(format!("w-exec-{n}"));
    let r = p
        .execute(ExecutionRequest::new(id.clone(), cap).with_inputs(inputs))
        .await
        .unwrap();
    (
        Observation {
            execution_id: id,
            kind: if r.status == ExecutionStatus::Success {
                ObservationKind::ExecutionCompleted
            } else {
                ObservationKind::ExecutionFailed
            },
            status: r.status,
            output: Some(r.output),
            receipt_id: Some(format!("sha256:real-{n}")),
            evidence: None,
        },
        ObservationOrigin {
            capability: CapabilityId::new(cap).unwrap(),
            invocation: format!("{cap}{path}"),
            reusable: false,
            provider_response_id: None,
        },
    )
}

/// Bytes of the request as the loop measures it, with the fixed question block.
fn request_bytes(c: &EscalationContext) -> usize {
    c.metrics().bytes + QUESTION_BYTES
}

fn context(
    policy: &dyn EscalationContextPolicy,
    obs: &[Observation],
    origins: &[ObservationOrigin],
) -> EscalationContext {
    policy.build(
        &WorkState {
            goal: GOAL,
            turn: obs.len(),
            max_turns: 45,
            executions: obs.len(),
            max_executions: 40,
            context_budget_bytes: Some(BUDGET - QUESTION_BYTES),
        },
        &WorkTrajectory {
            observations: obs,
            origins,
            decisions: &[],
            ruled_out: &[],
            evidence: &[],
            question: "",
        },
    )
}

#[tokio::test]
async fn parsed_manifests_cost_one_short_line_each_and_the_whole_workspace_fits_the_budget() {
    let dir = workspace("fits");
    let project = ProjectExecutor::new(&dir);
    let mut steps = vec![
        ("project.list", ".".to_string()),
        ("project.read", "Cargo.toml".to_string()),
        ("project.read", "README.md".to_string()),
    ];
    steps.extend((0..MEMBERS).map(|i| ("project.read", format!("crates/{}/Cargo.toml", name(i)))));
    let (mut obs, mut origins) = (Vec::new(), Vec::new());
    let (mut old_peak, mut new_peak) = (0, 0);
    let mut table = Vec::new();
    for (i, (cap, path)) in steps.iter().enumerate() {
        let (o, or) = observe(&project, i + 1, cap, path).await;
        obs.push(o);
        origins.push(or);
        let (old, new) = (
            context(&InspectBoundedContext, &obs, &origins),
            context(&InspectCoveredContext, &obs, &origins),
        );
        let (old_bytes, new_bytes) = (request_bytes(&old), request_bytes(&new));
        old_peak = old_peak.max(old_bytes);
        new_peak = new_peak.max(new_bytes);
        table.push((i + 1, old_bytes, new_bytes));
        // The runtime's own view never depends on what the model is shown.
        assert_eq!(Inventory::derive(&obs).read().len(), i.saturating_sub(2));
    }
    eprintln!(
        "observations, request bytes with the previous representation, with the covered one:"
    );
    for (n, a, b) in &table {
        if n % 4 == 3 || *n == steps.len() {
            eprintln!("  {n:>2}  {a:>6}  {b:>6}");
        }
    }

    // All 28 declared members were read; coverage is satisfied from the recorded observations.
    let inv = Inventory::derive(&obs);
    assert_eq!(inv.members.len(), MEMBERS);
    assert_eq!(inv.read().len(), MEMBERS);
    assert_eq!(inv.status().name(), "satisfied");

    // The previous representation does not fit the budget by the time the workspace is read; the
    // covered one does at every step, with room to spare.
    assert!(
        old_peak > BUDGET,
        "the old representation peaked at {old_peak}"
    );
    assert!(
        new_peak <= BUDGET,
        "the covered representation peaked at {new_peak}"
    );
    assert!(
        new_peak * 100 <= old_peak * 80,
        "a measurable reduction: {new_peak} against {old_peak}"
    );
}

#[tokio::test]
async fn what_is_sent_is_derived_from_the_manifests_and_names_its_source() {
    let dir = workspace("lines");
    let project = ProjectExecutor::new(&dir);
    let (mut obs, mut origins) = (Vec::new(), Vec::new());
    for (i, path) in [
        "Cargo.toml",
        "crates/chip-core-0/Cargo.toml",
        "crates/chip-session-2/Cargo.toml",
        "crates/chip-turns-3/Cargo.toml",
    ]
    .iter()
    .enumerate()
    {
        let (o, or) = observe(&project, i + 1, "project.read", path).await;
        obs.push(o);
        origins.push(or);
    }
    let c = context(&InspectCoveredContext, &obs, &origins);
    let lines: Vec<&str> = c
        .condensed
        .iter()
        .flat_map(|e| e.facts.iter().map(String::as_str))
        .collect();
    // The root, then the two earlier members; the latest observation is sent whole.
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(
        lines[0].starts_with("#1 Cargo.toml [workspace]: 28 members declared"),
        "{lines:?}"
    );
    assert!(
        lines[2].starts_with("#3 crates/chip-session-2 [chip-session-2 0.1.0]"),
        "{lines:?}"
    );
    // Internal dependencies by kind (a path declaration), external ones only counted.
    assert!(
        lines[2].contains("-> chip-core-0, chip-fx-1"),
        "{}",
        lines[2]
    );
    assert!(lines[2].contains("| dev: chip-core-0"), "{}", lines[2]);
    assert!(
        lines[2].contains("5 external") && lines[2].contains("2 external dev"),
        "{}",
        lines[2]
    );
    assert!(
        !lines[2].contains("tokio") && !lines[2].contains("serde_json"),
        "external names are not sent: {}",
        lines[2]
    );
    // Each condensed entry names its source observation, which stays recorded in full.
    for entry in &c.condensed {
        let source = obs
            .iter()
            .find(|o| o.execution_id == entry.execution_id)
            .expect("a recorded observation");
        assert_eq!(entry.withheld_bytes, source.render().len());
        assert!(
            source
                .output
                .as_deref()
                .unwrap()
                .contains("--- content ---")
        );
    }
    assert_eq!(
        c.relevant_observations.len(),
        1,
        "only the latest manifest is sent whole"
    );
    // The latest is whole and unsummarized.
    assert!(
        c.relevant_observations[0]
            .render()
            .contains("[dependencies]")
    );
    // Equivalent facts are not repeated: the same context built twice is the same size.
    assert_eq!(context(&InspectCoveredContext, &obs, &origins), c);
}

#[tokio::test]
async fn an_unparsed_or_partial_manifest_is_never_turned_into_a_compact_line() {
    let dir = workspace("bad");
    std::fs::write(
        dir.join("crates/chip-session-2/Cargo.toml"),
        "[package\nname = ",
    )
    .unwrap();
    // 40 KB: the read tool refuses a whole-file read over its limit, so this one fails.
    std::fs::write(
        dir.join("crates/chip-turns-3/Cargo.toml"),
        "# padding\n".repeat(5_000),
    )
    .unwrap();
    let project = ProjectExecutor::new(&dir);
    let (mut obs, mut origins) = (Vec::new(), Vec::new());
    for (i, path) in [
        "Cargo.toml",
        "crates/chip-session-2/Cargo.toml",
        "crates/chip-turns-3/Cargo.toml",
        "crates/chip-core-0/Cargo.toml",
        "README.md",
    ]
    .iter()
    .enumerate()
    {
        let (o, or) = observe(&project, i + 1, "project.read", path).await;
        obs.push(o);
        origins.push(or);
    }
    let c = context(&InspectCoveredContext, &obs, &origins);
    let lines: Vec<&str> = c
        .condensed
        .iter()
        .flat_map(|e| e.facts.iter().map(String::as_str))
        .collect();
    assert!(
        lines.iter().any(|l| l.starts_with("#4 crates/chip-core-0")),
        "{lines:?}"
    );
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("chip-session-2") || l.contains("chip-turns-3")),
        "{lines:?}"
    );
    let inv = Inventory::derive(&obs);
    assert_eq!(inv.read().len(), 1);
    assert_eq!(inv.all_unresolved().len(), 2);
    assert_eq!(inv.status().name(), "incomplete");
}

#[tokio::test]
async fn the_default_context_does_not_change() {
    let dir = workspace("default");
    let project = ProjectExecutor::new(&dir);
    let (mut obs, mut origins) = (Vec::new(), Vec::new());
    for (i, path) in [
        "Cargo.toml",
        "crates/chip-core-0/Cargo.toml",
        "crates/chip-fx-1/Cargo.toml",
    ]
    .iter()
    .enumerate()
    {
        let (o, or) = observe(&project, i + 1, "project.read", path).await;
        obs.push(o);
        origins.push(or);
    }
    let c = context(&InspectBoundedContext, &obs, &origins);
    // Within the budget the default sends every observation whole and condenses none.
    assert_eq!(c.relevant_observations.len(), 3);
    assert!(c.condensed.is_empty());
    let text = c.render();
    assert!(!text.contains("Workspace members declared") && !text.contains("Coverage is required"));
    assert!(
        !text.contains("[workspace]:"),
        "no compact lines in the default context"
    );
}

// ---- the real loop: all 28 members read and the answer accepted under the unchanged budget ---------------

struct Script {
    replies: Mutex<std::collections::VecDeque<String>>,
    peak: Mutex<usize>,
}

#[async_trait::async_trait]
impl ModelProvider for Script {
    async fn complete(&self, r: ModelRequest) -> Result<ModelResponse, FxError> {
        let bytes: usize = r.messages.iter().map(|m| m.content.len()).sum();
        let mut peak = self.peak.lock().unwrap();
        *peak = (*peak).max(bytes);
        drop(peak);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| FxError::Provider("the script ran out of replies".into()))?;
        Ok(ModelResponse::new("m", reply, Usage::new(1, 1)))
    }
}

fn read(path: &str) -> String {
    format!(
        r#"{{"decision":"request_capability","capability":"project.read","inputs":{{"path":{}}}}}"#,
        serde_json::to_string(path).unwrap()
    )
}

fn pax_available(dir: &Path) -> bool {
    let ok = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(PaxExecutor::new(dir).resolve())
    })
    .is_ok();
    if !ok {
        eprintln!("SKIPPED: PAX is not installed");
    }
    ok
}

async fn run_loop(
    dir: &Path,
    replies: Vec<String>,
    limits: WorkLimits,
) -> (
    chip_cli::software_work::SoftwareWork,
    serde_json::Value,
    usize,
) {
    let env = LocalEnvironment::new(
        opaque_id(dir),
        dir,
        PaxExecutor::new(dir),
        EnvironmentDescription::default(),
    );
    let model = Arc::new(Script {
        replies: Mutex::new(replies.into()),
        peak: Mutex::new(0),
    });
    let w = run_software_work_controlled(
        GoalKind::Inspect,
        WorkId::new("efficiency"),
        model.clone(),
        "scripted".into(),
        &env,
        GOAL,
        limits,
        GoalKind::Inspect.policy(),
        Some(BUDGET),
        None,
        None,
        Some(CoverageContract::CargoWorkspace),
    )
    .await;
    let json = serde_json::from_str(&render_json(&w, &EnvironmentDescription::default())).unwrap();
    let peak = *model.peak.lock().unwrap();
    (w, json, peak)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_workspace_inspection_completes_within_the_unchanged_budget() {
    let dir = workspace("loop-full");
    if !pax_available(&dir) {
        return;
    }
    let mut replies = vec![read("Cargo.toml"), read("README.md")];
    replies.extend((0..MEMBERS).map(|i| read(&format!("crates/{}/Cargo.toml", name(i)))));
    let answer = format!(
        "Cargo.toml declares {MEMBERS} members. Most crates depend on crates/{}/Cargo.toml, the base; README.md describes the workspace. Responsibilities beyond package descriptions are not established here.",
        name(0)
    );
    replies.push(format!(
        r#"{{"decision":"complete","summary":{}}}"#,
        serde_json::to_string(&answer).unwrap()
    ));
    let limits = WorkLimits {
        max_turns: 40,
        max_executions: 40,
    };
    let (w, j, peak) = run_loop(&dir, replies, limits).await;
    assert!(
        matches!(w.report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        w.report.outcome
    );
    let c = &j["inspection_coverage"];
    assert_eq!(c["status"], "satisfied");
    assert_eq!(
        (
            c["required_candidates"].as_u64(),
            c["read_candidates"].as_u64(),
            c["outstanding_candidates"].as_u64()
        ),
        (Some(28), Some(28), Some(0))
    );
    assert_eq!(c["answer"]["status"], "grounded_complete");
    assert_eq!(j["context"]["context_limit_rejections"], 0);
    assert!(peak <= BUDGET, "the largest request was {peak} bytes");
    assert_eq!(j["verified"], false, "grounded and covered, never verified");
    w.audit.assert_clean();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_budget_is_still_enforced_before_a_request_is_sent() {
    let dir = workspace("loop-budget");
    if !pax_available(&dir) {
        return;
    }
    let mut replies = vec![read("Cargo.toml")];
    replies.extend((0..MEMBERS).map(|i| read(&format!("crates/{}/Cargo.toml", name(i)))));
    // A budget the evidence outgrows part-way through: the first request that cannot fit is refused.
    let env = LocalEnvironment::new(
        opaque_id(&dir),
        &dir,
        PaxExecutor::new(&dir),
        EnvironmentDescription::default(),
    );
    let model = Arc::new(Script {
        replies: Mutex::new(replies.into()),
        peak: Mutex::new(0),
    });
    let w = run_software_work_controlled(
        GoalKind::Inspect,
        WorkId::new("efficiency"),
        model.clone(),
        "scripted".into(),
        &env,
        GOAL,
        WorkLimits {
            max_turns: 40,
            max_executions: 40,
        },
        GoalKind::Inspect.policy(),
        Some(13_000),
        None,
        None,
        Some(CoverageContract::CargoWorkspace),
    )
    .await;
    assert_eq!(
        w.report.outcome,
        WorkOutcome::LimitReached {
            limit: chip_core::LimitKind::Context
        }
    );
    assert!(
        *model.peak.lock().unwrap() <= 13_000,
        "nothing over the budget was sent"
    );
    let j: serde_json::Value =
        serde_json::from_str(&render_json(&w, &EnvironmentDescription::default())).unwrap();
    assert_eq!(j["context"]["context_limit_rejections"], 1);
    assert!(
        j["inspection_coverage"]["outstanding_candidates"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(!matches!(w.report.outcome, WorkOutcome::Completed { .. }));
}
