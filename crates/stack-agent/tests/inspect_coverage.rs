//! Inspection coverage: what an inspection read, against what a Cargo-workspace question needs.
//!
//! The first half drives the inventory and the answer checks with the real project executor's real
//! output. The second half runs the real work loop (real files, real PAX) with only the model
//! scripted, including a reproduction of the failure that motivated this: a plausible summary after
//! reading one member manifest of several. Nothing depends on how any model words an answer.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chip_cli::inspect_coverage::{
    AnswerStatus, CoverageContract, CoveredAnswer, DepKind, Inventory, MemberSource, MemberState,
    assess,
};
use chip_cli::local_environment::{LocalEnvironment, opaque_id};
use chip_cli::software_work::{GoalKind, SoftwareWork, render_json, run_software_work_controlled};
use chip_core::{
    AnswerPredicate, CapabilityId, EnvironmentDescription, ExecutionId, ExecutionRequest,
    ExecutionStatus, Executor, InputValue, Observation, ObservationKind, WorkEvent, WorkId,
    WorkLimits, WorkOutcome,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use chip_pax::PaxExecutor;
use chip_project::ProjectExecutor;

// ---- a real workspace -------------------------------------------------------------------------------

const ROOT: &str = r#"# Members, in groups (a comment, not a declaration):
#   core   crates/core-*    pretend glob in a comment
#   tools  crates/tool-*
[workspace]
members = ["crates/alpha", "crates/beta", "crates/gamma"]
exclude = ["crates/skipped"]

[workspace.dependencies]
beta = { path = "crates/beta" }
"#;

fn workspace(tag: &str, root: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-coverage-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for c in ["alpha", "beta", "gamma", "skipped"] {
        std::fs::create_dir_all(dir.join(format!("crates/{c}/src"))).unwrap();
        std::fs::write(
            dir.join(format!("crates/{c}/src/lib.rs")),
            "pub fn f() {}\n",
        )
        .unwrap();
    }
    let w = |p: &str, t: &str| std::fs::write(dir.join(p), t).unwrap();
    w("Cargo.toml", root);
    w("README.md", "# Demo\n\nA small workspace.\n");
    w(
        "crates/alpha/Cargo.toml",
        "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nbeta = { workspace = true }\nserde = \"1\"\n\n[dev-dependencies]\ngamma = { path = \"../gamma\" }\n",
    );
    w(
        "crates/beta/Cargo.toml",
        "[package]\nname = \"beta\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    w(
        "crates/gamma/Cargo.toml",
        "[package]\nname = \"gamma\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nbeta = { path = \"../beta\" }\n",
    );
    w(
        "crates/skipped/Cargo.toml",
        "[package]\nname = \"skipped\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    dir
}

async fn observe(project: &ProjectExecutor, n: usize, capability: &str, path: &str) -> Observation {
    let inputs: BTreeMap<String, InputValue> =
        [("path".to_string(), InputValue::Text(path.to_string()))].into();
    let id = ExecutionId::new(format!("w-exec-{n}"));
    // The executor refuses a path the project rules refuse; such a step records nothing.
    let result = project
        .execute(ExecutionRequest::new(id.clone(), capability).with_inputs(inputs))
        .await
        .expect("a path the executor accepts");
    Observation {
        execution_id: id,
        kind: if result.status == ExecutionStatus::Success {
            ObservationKind::ExecutionCompleted
        } else {
            ObservationKind::ExecutionFailed
        },
        status: result.status,
        output: Some(result.output),
        receipt_id: Some(format!("sha256:real-{n}")),
        evidence: None,
    }
}

async fn trajectory(dir: &Path, steps: &[(&str, &str)]) -> Vec<Observation> {
    let project = ProjectExecutor::new(dir);
    let mut out = Vec::new();
    for (i, (capability, path)) in steps.iter().enumerate() {
        out.push(observe(&project, i + 1, capability, path).await);
    }
    out
}

const READ: &str = "project.read";
const LIST: &str = "project.list";

// ---- the inventory --------------------------------------------------------------------------------------

#[tokio::test]
async fn members_come_from_the_parsed_array_and_never_from_comments() {
    let dir = workspace("members", ROOT);
    let obs = trajectory(&dir, &[(READ, "Cargo.toml")]).await;
    let inv = Inventory::derive(&obs);
    assert_eq!(inv.root_read, Some(1));
    assert!(inv.is_workspace);
    assert_eq!(
        inv.declared,
        ["crates/alpha", "crates/beta", "crates/gamma"]
    );
    let paths: Vec<&str> = inv.members.iter().map(|m| m.path.as_str()).collect();
    assert_eq!(paths, ["crates/alpha", "crates/beta", "crates/gamma"]);
    // The comment's globs and the excluded directory are not members.
    assert!(
        !paths
            .iter()
            .any(|p| p.contains('*') || p.contains("core-") || p.contains("skipped"))
    );
    assert!(
        inv.members
            .iter()
            .all(|m| m.source == MemberSource::Declared)
    );
    // Derived, not read: nothing is claimed read.
    assert!(
        inv.members
            .iter()
            .all(|m| m.state == MemberState::Outstanding)
    );
    assert!(inv.read().is_empty());
    assert_eq!(inv.outstanding().len(), 3);
}

#[tokio::test]
async fn coverage_follows_what_was_actually_read_and_nothing_else() {
    let dir = workspace("progress", ROOT);
    let status = |obs: &[Observation]| Inventory::derive(obs).status().name();

    assert_eq!(status(&[]), "root_unread");
    let obs = trajectory(&dir, &[(READ, "Cargo.toml")]).await;
    assert_eq!(
        status(&obs),
        "incomplete",
        "the root manifest alone is not coverage"
    );

    let obs = trajectory(
        &dir,
        &[(READ, "Cargo.toml"), (READ, "crates/alpha/Cargo.toml")],
    )
    .await;
    let inv = Inventory::derive(&obs);
    assert_eq!(inv.status().name(), "incomplete");
    assert_eq!((inv.read().len(), inv.outstanding().len()), (1, 2));

    // A listing of a directory names manifests without reading them.
    let obs = trajectory(
        &dir,
        &[
            (READ, "Cargo.toml"),
            (LIST, "crates"),
            (READ, "crates/alpha/Cargo.toml"),
        ],
    )
    .await;
    assert_eq!(Inventory::derive(&obs).read().len(), 1);

    let obs = trajectory(
        &dir,
        &[
            (READ, "Cargo.toml"),
            (READ, "crates/alpha/Cargo.toml"),
            (READ, "crates/beta/Cargo.toml"),
            (READ, "crates/gamma/Cargo.toml"),
        ],
    )
    .await;
    let inv = Inventory::derive(&obs);
    assert_eq!(inv.status().name(), "satisfied");
    assert_eq!(
        (
            inv.read().len(),
            inv.outstanding().len(),
            inv.all_unresolved().len()
        ),
        (3, 0, 0)
    );
    // Reading the README is tracked, and is not a member.
    let with_readme = trajectory(&dir, &[(READ, "Cargo.toml"), (READ, "README.md")]).await;
    assert!(Inventory::derive(&with_readme).overview_read);
    assert!(!inv.overview_read);
}

#[tokio::test]
async fn dependency_kinds_stay_distinct_and_inherited_paths_resolve_through_the_root() {
    let dir = workspace("kinds", ROOT);
    let obs = trajectory(
        &dir,
        &[(READ, "Cargo.toml"), (READ, "crates/alpha/Cargo.toml")],
    )
    .await;
    let inv = Inventory::derive(&obs);
    let alpha = inv
        .members
        .iter()
        .find(|m| m.path == "crates/alpha")
        .unwrap();
    assert_eq!(alpha.package.as_deref(), Some("alpha"));
    let dep = |name: &str| alpha.dependencies.iter().find(|d| d.name == name).unwrap();
    assert_eq!(dep("beta").kind, DepKind::Normal);
    assert!(dep("beta").inherited);
    assert_eq!(dep("beta").path.as_deref(), Some("crates/beta"));
    assert_eq!(dep("serde").kind, DepKind::Normal);
    assert_eq!(dep("gamma").kind, DepKind::Dev);
    assert_eq!(dep("gamma").path.as_deref(), Some("../gamma"));
}

#[tokio::test]
async fn a_manifest_that_cannot_be_read_or_parsed_is_unresolved_and_never_read() {
    let root = "[workspace]\nmembers = [\"crates/alpha\", \"crates/ghost\", \"crates/broken\", \"../escape\", \"/abs\"]\n";
    let dir = workspace("unresolved", root);
    std::fs::create_dir_all(dir.join("crates/broken")).unwrap();
    std::fs::write(dir.join("crates/broken/Cargo.toml"), "[package\nname = ").unwrap();
    let project = ProjectExecutor::new(&dir);
    let mut obs = vec![observe(&project, 1, READ, "Cargo.toml").await];
    for (i, p) in [
        "crates/alpha/Cargo.toml",
        "crates/ghost/Cargo.toml",
        "crates/broken/Cargo.toml",
    ]
    .iter()
    .enumerate()
    {
        obs.push(observe(&project, i + 2, READ, p).await);
    }
    // The paths the project rules refuse cannot even be asked of the executor.
    for bad in ["../escape/Cargo.toml", "/abs/Cargo.toml"] {
        let inputs: BTreeMap<String, InputValue> =
            [("path".to_string(), InputValue::Text(bad.to_string()))].into();
        assert!(
            project
                .execute(ExecutionRequest::new(ExecutionId::new("x"), READ).with_inputs(inputs))
                .await
                .is_err(),
            "{bad}"
        );
    }
    let inv = Inventory::derive(&obs);
    assert_eq!(inv.read().len(), 1, "only alpha counts as read");
    let unresolved = inv.all_unresolved();
    let names: Vec<&str> = unresolved.iter().map(|u| u.candidate.as_str()).collect();
    assert!(
        names.contains(&"../escape") && names.contains(&"/abs"),
        "{names:?}"
    );
    assert!(
        names.contains(&"crates/ghost/Cargo.toml") && names.contains(&"crates/broken/Cargo.toml"),
        "{names:?}"
    );
    assert!(
        unresolved
            .iter()
            .find(|u| u.candidate == "crates/ghost/Cargo.toml")
            .unwrap()
            .reason
            .contains("not_found")
    );
    assert!(
        unresolved
            .iter()
            .find(|u| u.candidate == "crates/broken/Cargo.toml")
            .unwrap()
            .reason
            .contains("TOML")
    );
    assert!(unresolved.iter().all(|u| !u.resolvable));
    assert_eq!(
        inv.status().name(),
        "unresolved",
        "tried and failed is not satisfied"
    );
    assert!(inv.outstanding().is_empty());
}

#[tokio::test]
async fn globs_and_excludes_are_expanded_only_through_a_recorded_listing_or_left_unresolved() {
    let root = "[workspace]\nmembers = [\"crates/*\", \"tools/**/x?\"]\nexclude = [\"crates/skipped\", \"crates/[ab]\"]\n";
    let dir = workspace("glob", root);
    let obs = trajectory(&dir, &[(READ, "Cargo.toml")]).await;
    let inv = Inventory::derive(&obs);
    // No listing, no concrete members: the glob is a resolvable gap, not a guess.
    assert!(inv.members.is_empty());
    let gaps = inv.all_unresolved();
    let glob = gaps.iter().find(|u| u.candidate == "crates/*").unwrap();
    assert!(
        glob.resolvable && glob.reason.contains("project.list `crates`"),
        "{glob:?}"
    );
    assert!(
        gaps.iter()
            .any(|u| u.candidate == "tools/**/x?" && !u.resolvable)
    );
    assert!(
        gaps.iter()
            .any(|u| u.candidate.starts_with("exclude ") && !u.resolvable)
    );
    assert_eq!(inv.status().name(), "incomplete");

    // With the listing recorded, the matches are concrete members whose source names the listing.
    let obs = trajectory(&dir, &[(READ, "Cargo.toml"), (LIST, "crates")]).await;
    let inv = Inventory::derive(&obs);
    let paths: Vec<&str> = inv.members.iter().map(|m| m.path.as_str()).collect();
    assert_eq!(
        paths,
        ["crates/alpha", "crates/beta", "crates/gamma"],
        "the literal exclude removed `skipped`"
    );
    assert!(inv.excluded.contains(&"crates/skipped".to_string()));
    assert!(inv.members.iter().all(
        |m| matches!(&m.source, MemberSource::Glob { pattern, listing: 2 } if pattern == "crates/*")
    ));
    // Two entries are still not evaluated, and that stays visible.
    assert!(
        inv.all_unresolved()
            .iter()
            .any(|u| u.candidate == "tools/**/x?")
    );
}

#[tokio::test]
async fn a_root_without_a_workspace_requires_only_itself() {
    let dir = workspace(
        "single",
        "[package]\nname = \"solo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    let obs = trajectory(&dir, &[(READ, "Cargo.toml")]).await;
    let inv = Inventory::derive(&obs);
    assert!(!inv.is_workspace);
    assert_eq!(inv.status().name(), "not_a_workspace");
    assert!(inv.status().is_satisfied());
}

// ---- the answer checks ------------------------------------------------------------------------------------------

fn paths_cited() -> &'static str {
    " (Cargo.toml, crates/alpha/Cargo.toml, crates/beta/Cargo.toml, crates/gamma/Cargo.toml)"
}

async fn all_read(tag: &str) -> Vec<Observation> {
    let dir = workspace(tag, ROOT);
    trajectory(
        &dir,
        &[
            (READ, "Cargo.toml"),
            (READ, "README.md"),
            (READ, "crates/alpha/Cargo.toml"),
            (READ, "crates/beta/Cargo.toml"),
            (READ, "crates/gamma/Cargo.toml"),
        ],
    )
    .await
}

#[tokio::test]
async fn the_five_answer_outcomes_are_distinct() {
    let dir = workspace("outcomes", ROOT);
    let cites = format!(
        "The workspace has 3 members; alpha and gamma depend on beta{}.",
        paths_cited()
    );

    // Insufficient evidence: no root manifest to derive a requirement from.
    let none = trajectory(&dir, &[(READ, "README.md")]).await;
    assert_eq!(
        assess("See README.md.", &none).status,
        AnswerStatus::InsufficientEvidence
    );

    // Unsupported: cites nothing observed.
    let root_only = trajectory(&dir, &[(READ, "Cargo.toml")]).await;
    assert_eq!(
        assess("It is a workspace.", &root_only).status,
        AnswerStatus::Unsupported
    );

    // Grounded but incomplete: cites an observed file while members remain unread.
    let a = assess("The root Cargo.toml declares a workspace.", &root_only);
    assert_eq!(a.status, AnswerStatus::GroundedIncomplete);
    assert!(a.reason.contains("0 of 3"), "{}", a.reason);

    // Contradicted: grounded and fully read, with a checked claim that conflicts.
    let full = all_read("outcomes-full").await;
    let a = assess(
        "Workspace members include 'crates/chip-*' and Cargo.toml says so.",
        &full,
    );
    assert_eq!(a.status, AnswerStatus::Contradicted);
    assert_eq!(a.findings[0].claim, "crates/chip-*");

    // Grounded and complete.
    let a = assess(&cites, &full);
    assert_eq!(a.status, AnswerStatus::GroundedComplete, "{}", a.reason);
    assert!(!CoveredAnswer(CoverageContract::CargoWorkspace).accepts("It is a workspace.", &full));
    assert!(CoveredAnswer(CoverageContract::CargoWorkspace).accepts(&cites, &full));
    assert!(!CoveredAnswer(CoverageContract::CargoWorkspace).accepts(&cites, &root_only));
}

#[tokio::test]
async fn the_original_errors_are_detected_and_unobserved_things_are_not_called_false() {
    let full = all_read("claims").await;
    // The glob in the manifest's comment is not a member list; the array is.
    let a = assess(
        "The workspace members include crates/chip-* (Cargo.toml).",
        &full,
    );
    assert_eq!(a.status, AnswerStatus::Contradicted);
    assert!(
        a.findings[0].why.contains("declares 3 entries"),
        "{:?}",
        a.findings
    );
    // The three declared members are fine to name.
    assert!(
        assess(
            "Members are crates/alpha, crates/beta and crates/gamma (Cargo.toml).",
            &full
        )
        .findings
        .is_empty()
    );
    // Normal dependency: supported. Dev-only dependency stated as a dependency: contradicted.
    assert!(
        assess("alpha depends on beta (crates/alpha/Cargo.toml).", &full)
            .findings
            .is_empty()
    );
    let a = assess("alpha depends on gamma (crates/alpha/Cargo.toml).", &full);
    assert_eq!(a.status, AnswerStatus::Contradicted);
    assert!(
        a.findings[0].why.contains("dev-dependency"),
        "{:?}",
        a.findings
    );
    // The same with a wildcard, as in the experiment's answer.
    let a = assess(
        "The crate alpha depends on various 'gamma*' crates (crates/alpha/Cargo.toml).",
        &full,
    );
    assert_eq!(a.status, AnswerStatus::Contradicted);
    // Saying it is a dev-dependency makes the distinction the check asks for.
    assert!(
        assess(
            "alpha depends on gamma only as a dev-dependency (crates/alpha/Cargo.toml).",
            &full
        )
        .findings
        .is_empty()
    );
    // A dependency the manifest does not declare directly is not called false: it may be transitive.
    assert!(
        assess(
            "alpha depends on gamma's helpers through beta (crates/alpha/Cargo.toml).",
            &full
        )
        .findings
        .is_empty()
    );

    // A claim is its own clause: a neighbouring clause or a later aside is not part of it.
    assert!(
        assess(
            "alpha depends on beta; gamma is only used in alpha's tests (crates/alpha/Cargo.toml).",
            &full
        )
        .findings
        .is_empty()
    );
    assert!(
        assess(
            "alpha depends on beta but gamma is a test helper (crates/alpha/Cargo.toml).",
            &full
        )
        .findings
        .is_empty()
    );
    // A claim about a manifest that was not read is neither supported nor contradicted.
    let dir = workspace("claims-unread", ROOT);
    let partial = trajectory(&dir, &[(READ, "Cargo.toml")]).await;
    let a = assess("alpha depends on gamma (Cargo.toml).", &partial);
    assert_eq!(a.status, AnswerStatus::GroundedIncomplete);
    assert!(a.findings.is_empty());
}

#[tokio::test]
async fn coverage_cannot_be_manufactured_by_the_model() {
    let dir = workspace("manufactured", ROOT);
    let obs = trajectory(&dir, &[(READ, "Cargo.toml")]).await;
    // An answer asserting everything was read changes nothing about what was read.
    let boast = "I read all 3 member manifests: crates/alpha/Cargo.toml, crates/beta/Cargo.toml, crates/gamma/Cargo.toml and Cargo.toml.";
    let a = assess(boast, &obs);
    assert_eq!(a.status, AnswerStatus::GroundedIncomplete);
    assert_eq!(Inventory::derive(&obs).read().len(), 0);
}

// ---- the real loop ------------------------------------------------------------------------------------------------------

struct Script {
    replies: Mutex<VecDeque<String>>,
    seen: Mutex<Vec<String>>,
}

impl Script {
    fn new(replies: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            seen: Mutex::new(Vec::new()),
        })
    }
    fn request(&self, n: usize) -> String {
        self.seen.lock().unwrap()[n].clone()
    }
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
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| FxError::Provider("the script ran out of replies".into()))?;
        Ok(ModelResponse::new("m", reply, Usage::new(1, 1)))
    }
}

fn q(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}
fn read(path: &str) -> String {
    format!(
        r#"{{"decision":"request_capability","capability":"project.read","inputs":{{"path":{}}}}}"#,
        q(path)
    )
}
fn list(path: &str) -> String {
    format!(
        r#"{{"decision":"request_capability","capability":"project.list","inputs":{{"path":{}}}}}"#,
        q(path)
    )
}
fn complete(summary: &str) -> String {
    format!(r#"{{"decision":"complete","summary":{}}}"#, q(summary))
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

const LIMITS: WorkLimits = WorkLimits {
    max_turns: 14,
    max_executions: 12,
};

async fn run(
    dir: &Path,
    kind: GoalKind,
    coverage: Option<CoverageContract>,
    replies: Vec<String>,
) -> (SoftwareWork, Arc<Script>, serde_json::Value) {
    let env = LocalEnvironment::new(
        opaque_id(dir),
        dir,
        PaxExecutor::new(dir),
        EnvironmentDescription::default(),
    );
    let model = Script::new(replies);
    let w = run_software_work_controlled(
        kind,
        WorkId::new("coverage"),
        model.clone(),
        "scripted".into(),
        &env,
        "Summarize the workspace architecture and the dependencies between its crates.",
        LIMITS,
        kind.policy(),
        None,
        None,
        None,
        coverage,
    )
    .await;
    let json: serde_json::Value =
        serde_json::from_str(&render_json(&w, &EnvironmentDescription::default())).unwrap();
    (w, model, json)
}

fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                out.insert(
                    p.strip_prefix(base).unwrap().to_string_lossy().into_owned(),
                    std::fs::read(&p).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

const PLAUSIBLE: &str = "The workspace has a root Cargo.toml declaring members, and crates/alpha/Cargo.toml shows alpha depends on beta and serde. README.md describes a small demo workspace.";

#[tokio::test(flavor = "multi_thread")]
async fn one_member_manifest_and_a_plausible_summary_does_not_complete_under_the_coverage_contract()
{
    let dir = workspace("loop-original", ROOT);
    if !pax_available(&dir) {
        return;
    }
    let before = snapshot(&dir);
    let script = || {
        vec![
            list("."),
            read("Cargo.toml"),
            read("README.md"),
            read("crates/alpha/Cargo.toml"),
            complete(PLAUSIBLE),
        ]
    };

    // With the contract: refused, and the report says exactly what is missing.
    let (w, _, j) = run(
        &dir,
        GoalKind::Inspect,
        Some(CoverageContract::CargoWorkspace),
        script(),
    )
    .await;
    assert!(
        matches!(&w.report.outcome, WorkOutcome::Blocked { reason } if reason.contains("completion refused")),
        "{:?}",
        w.report.outcome
    );
    assert!(!w.grounded && !w.verified);
    let c = &j["inspection_coverage"];
    assert_eq!(c["contract"], "cargo-workspace");
    assert_eq!(c["status"], "incomplete");
    assert_eq!(c["required_candidates"], 3);
    assert_eq!(c["read_candidates"], 1);
    assert_eq!(c["outstanding_candidates"], 2);
    assert_eq!(c["unresolved_candidates"], 0);
    assert_eq!(
        c["outstanding"],
        serde_json::json!(["crates/beta/Cargo.toml", "crates/gamma/Cargo.toml"])
    );
    assert_eq!(c["answer"]["status"], "grounded_incomplete");
    assert_eq!(c["answer"]["enforced"], true);
    // The raw answer is preserved for audit.
    assert_eq!(c["answer"]["proposed_answer"], PLAUSIBLE);
    assert!(!j["goal_satisfied"].as_bool().unwrap_or(false) || j["grounded"] == false);
    w.audit.assert_clean();
    assert_eq!(before, snapshot(&dir), "an inspection changed nothing");

    // Without the contract nothing changes for the submitter: the same answer is accepted as
    // grounded, and the report still says it was incomplete and that nothing enforced it.
    let (w, _, j) = run(&dir, GoalKind::Inspect, None, script()).await;
    assert!(
        matches!(w.report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        w.report.outcome
    );
    assert!(w.grounded && !w.verified);
    let c = &j["inspection_coverage"];
    assert_eq!(c["contract"], "none");
    assert_eq!(c["answer"]["status"], "grounded_incomplete");
    assert_eq!(c["answer"]["enforced"], false);
    assert_eq!(c["outstanding_candidates"], 2);
    assert_eq!(before, snapshot(&dir));
}

#[tokio::test(flavor = "multi_thread")]
async fn reading_every_declared_manifest_lets_a_grounded_uncontradicted_answer_complete() {
    let dir = workspace("loop-complete", ROOT);
    if !pax_available(&dir) {
        return;
    }
    let before = snapshot(&dir);
    let answer = format!(
        "The workspace declares 3 members; alpha depends on beta; gamma depends on beta{}.",
        paths_cited()
    );
    let (w, m, j) = run(
        &dir,
        GoalKind::Inspect,
        Some(CoverageContract::CargoWorkspace),
        vec![
            read("Cargo.toml"),
            read("crates/alpha/Cargo.toml"),
            read("crates/beta/Cargo.toml"),
            read("crates/gamma/Cargo.toml"),
            complete(&answer),
        ],
    )
    .await;
    assert!(
        matches!(w.report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        w.report.outcome
    );
    assert!(
        w.grounded && !w.verified,
        "grounded and covered, never verified"
    );
    let c = &j["inspection_coverage"];
    assert_eq!(c["status"], "satisfied");
    assert_eq!(
        (
            c["read_candidates"].as_u64(),
            c["outstanding_candidates"].as_u64()
        ),
        (Some(3), Some(0))
    );
    assert_eq!(c["answer"]["status"], "grounded_complete");
    assert_eq!(j["verified"], false);
    // Four manifests were parsed (the root and three members): four observations retained and one
    // structured fact for each.
    assert_eq!(c["source_observations_retained"], 4);
    assert_eq!(c["structured_facts_available"], 4);
    assert!(
        c["factual_validation"]
            .as_str()
            .unwrap()
            .starts_with("limited")
    );
    w.audit.assert_clean();
    assert_eq!(before, snapshot(&dir));
    // The contract's own answer instructions reached the model, not the generic ones.
    let first = m.request(0);
    assert!(
        first.contains("COVERAGE CONTRACT (cargo-workspace)"),
        "{first}"
    );
    assert!(
        first.contains("bytes, not words or tokens") && !first.contains("one concrete improvement")
    );
    // The model was shown what remained unread after the root manifest, from the recorded read.
    assert!(m.request(1).contains("Declared member manifests not yet read: crates/alpha/Cargo.toml, crates/beta/Cargo.toml, crates/gamma/Cargo.toml"), "{}", m.request(1));
    assert!(
        m.request(3)
            .contains("Declared member manifests not yet read: crates/gamma/Cargo.toml."),
        "{}",
        m.request(3)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_contradicted_claim_is_refused_even_after_full_coverage() {
    let dir = workspace("loop-contradicted", ROOT);
    if !pax_available(&dir) {
        return;
    }
    let answer = format!(
        "The workspace members include crates/chip-* and crates/fx-*{}.",
        paths_cited()
    );
    let (w, _, j) = run(
        &dir,
        GoalKind::Inspect,
        Some(CoverageContract::CargoWorkspace),
        vec![
            read("Cargo.toml"),
            read("crates/alpha/Cargo.toml"),
            read("crates/beta/Cargo.toml"),
            read("crates/gamma/Cargo.toml"),
            complete(&answer),
        ],
    )
    .await;
    assert!(
        matches!(&w.report.outcome, WorkOutcome::Blocked { reason } if reason.contains("completion refused")),
        "{:?}",
        w.report.outcome
    );
    let a = &j["inspection_coverage"]["answer"];
    assert_eq!(a["status"], "contradicted_claim");
    assert_eq!(a["contradicted_claims"].as_array().unwrap().len(), 2);
    assert_eq!(
        j["inspection_coverage"]["status"], "satisfied",
        "coverage and correctness are separate"
    );
    assert_eq!(j["verified"], false);
    w.audit.assert_clean();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unresolvable_candidate_is_reported_and_blocks_completion_explicitly() {
    let root = "[workspace]\nmembers = [\"crates/alpha\", \"crates/ghost\"]\n";
    let dir = workspace("loop-unresolved", root);
    if !pax_available(&dir) {
        return;
    }
    let (w, _, j) = run(
        &dir,
        GoalKind::Inspect,
        Some(CoverageContract::CargoWorkspace),
        vec![
            read("Cargo.toml"),
            read("crates/alpha/Cargo.toml"),
            read("crates/ghost/Cargo.toml"),
            complete("alpha is a crate (Cargo.toml, crates/alpha/Cargo.toml)."),
        ],
    )
    .await;
    assert!(
        matches!(w.report.outcome, WorkOutcome::Blocked { .. }),
        "{:?}",
        w.report.outcome
    );
    let c = &j["inspection_coverage"];
    assert_eq!(c["status"], "unresolved");
    assert_eq!(c["unresolved_candidates"], 1);
    assert_eq!(c["read_candidates"], 1);
    assert_eq!(c["unresolved"][0]["candidate"], "crates/ghost/Cargo.toml");
    assert!(
        c["unresolved"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("not_found")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_coverage_contract_leaves_inspect_safety_and_other_kinds_alone() {
    let dir = workspace("loop-safety", ROOT);
    if !pax_available(&dir) {
        return;
    }
    let before = snapshot(&dir);
    // Inspect still cannot write or run tests, contract or not.
    for attempt in [
        r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"crates/alpha/src/lib.rs","content":"x"}}"#,
        r#"{"decision":"request_capability","capability":"pax.test"}"#,
    ] {
        let (w, _, _) = run(
            &dir,
            GoalKind::Inspect,
            Some(CoverageContract::CargoWorkspace),
            vec![read("Cargo.toml"), attempt.to_string()],
        )
        .await;
        assert!(
            matches!(&w.report.outcome, WorkOutcome::Failed { reason, .. } if reason.contains("not a valid decision")),
            "{:?}",
            w.report.outcome
        );
        assert_eq!(w.report.summary.executions, 1);
        w.audit.assert_clean();
    }
    assert_eq!(before, snapshot(&dir));

    // A forged decision still executes nothing.
    let (w, _, _) = run(
        &dir,
        GoalKind::Inspect,
        Some(CoverageContract::CargoWorkspace),
        vec![r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"Cargo.toml"},"receipt":"forged"}"#.into()],
    )
    .await;
    assert_eq!(w.report.summary.executions, 0);

    // A contract passed with another kind is ignored by the runtime (the CLI refuses the pairing).
    let (w, _, j) = run(
        &dir,
        GoalKind::Verify,
        Some(CoverageContract::CargoWorkspace),
        vec![],
    )
    .await;
    assert!(w.coverage.is_none());
    assert!(j["inspection_coverage"].is_null());
    let _ = WorkEvent::WorkStarted {
        work_id: WorkId::new("x"),
        goal: String::new(),
        limits: LIMITS,
    };
    let _ = CapabilityId::new("project.read");
}

#[test]
fn the_coverage_flag_is_validated_before_anything_runs() {
    let dir = workspace("cli", ROOT);
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_stack-agent"))
            .args(["work", "Summarize."])
            .args(args)
            .current_dir(&dir)
            .env("CHIP_PROVIDER", "openai-compatible")
            .env("CHIP_MODEL", "m")
            .env("CHIP_ENDPOINT", "http://127.0.0.1:1/v1/chat/completions")
            .output()
            .unwrap()
    };
    for args in [
        vec!["--kind", "inspect", "--require-coverage", "everything"],
        vec!["--kind", "inspect", "--require-coverage"],
        vec!["--kind", "change", "--require-coverage", "cargo-workspace"],
        vec!["--kind", "verify", "--require-coverage", "cargo-workspace"],
        vec!["--require-coverage", "cargo-workspace"],
    ] {
        let out = run(&args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains("--require-coverage"), "{args:?}: {stderr}");
    }
}

#[tokio::test]
async fn a_short_answer_that_omits_a_requested_part_is_incomplete_even_when_coverage_is_satisfied()
{
    let full = all_read("shape").await;
    let cited = paths_cited();
    // Valid, grounded, uncontradicted, full coverage: and still not the requested summary.
    let no_count = assess(&format!("alpha depends on beta{cited}."), &full);
    assert_eq!(no_count.status, AnswerStatus::GroundedIncomplete);
    assert_eq!(no_count.omissions.len(), 1);
    assert!(
        no_count.reason.contains("declared member count (3)"),
        "{}",
        no_count.reason
    );
    let no_deps = assess(&format!("It has 3 members{cited}."), &full);
    assert_eq!(no_deps.status, AnswerStatus::GroundedIncomplete);
    assert!(
        no_deps.omissions[0].contains("dependencies"),
        "{:?}",
        no_deps.omissions
    );
    // Neither is required to list the members, and either count form counts.
    let ok = assess(
        &format!("3 members; the crates depend on each other in a chain{cited}."),
        &full,
    );
    assert_eq!(ok.status, AnswerStatus::GroundedComplete, "{}", ok.reason);
    assert!(ok.omissions.is_empty());
    assert!(
        !CoveredAnswer(CoverageContract::CargoWorkspace)
            .accepts(&format!("alpha depends on beta{cited}."), &full)
    );
    assert!(
        CoveredAnswer(CoverageContract::CargoWorkspace)
            .accepts(&format!("3 members; they depend on beta{cited}."), &full)
    );
}

#[tokio::test]
async fn a_wrong_member_count_is_a_contradiction_and_other_numbers_are_left_alone() {
    let full = all_read("count").await;
    let cited = paths_cited();
    for wrong in [
        "The workspace declares 15 members and they depend on beta",
        "The workspace has 15 members and they depend on beta",
        "There are 15 declared members and they depend on beta",
    ] {
        let a = assess(&format!("{wrong}{cited}."), &full);
        assert_eq!(
            a.status,
            AnswerStatus::Contradicted,
            "{wrong}: {}",
            a.reason
        );
        assert!(
            a.findings.iter().any(|f| f.claim.contains("15")),
            "{:?}",
            a.findings
        );
    }
    // A number that is not a statement of the member count is not read as one.
    let a = assess(
        &format!(
            "The workspace has 3 members in 2 groups; 1 crate is a base that the others depend on{cited}."
        ),
        &full,
    );
    assert_eq!(a.status, AnswerStatus::GroundedComplete, "{}", a.reason);
}

#[tokio::test(flavor = "multi_thread")]
async fn full_coverage_with_an_unusable_final_answer_is_reported_as_exactly_that() {
    let dir = workspace("loop-final", ROOT);
    if !pax_available(&dir) {
        return;
    }
    let reads = || {
        vec![
            read("Cargo.toml"),
            read("crates/alpha/Cargo.toml"),
            read("crates/beta/Cargo.toml"),
            read("crates/gamma/Cargo.toml"),
        ]
    };
    let cited = paths_cited();

    // Valid, grounded and uncontradicted, but it omits the requested count and the dependencies.
    let mut script = reads();
    script.push(complete(&format!("A small demo workspace{cited}.")));
    let (w, _, j) = run(
        &dir,
        GoalKind::Inspect,
        Some(CoverageContract::CargoWorkspace),
        script,
    )
    .await;
    assert!(
        matches!(&w.report.outcome, WorkOutcome::Blocked { reason } if reason.contains("completion refused")),
        "{:?}",
        w.report.outcome
    );
    let c = &j["inspection_coverage"];
    assert_eq!(
        c["status"], "satisfied",
        "coverage is satisfied; the task is not"
    );
    assert_eq!(c["answer"]["status"], "grounded_incomplete");
    assert_eq!(c["answer"]["omissions"].as_array().unwrap().len(), 2);
    assert_eq!(
        c["answer"]["proposed_answer_bytes"],
        format!("A small demo workspace{cited}.").len()
    );
    assert_eq!(c["answer"]["summary_limit_bytes"], 1024);
    assert!(!w.grounded && !w.verified);

    // A summary over the validator's limit is an invalid decision, not a truncated acceptance.
    let mut script = reads();
    script.push(complete(&format!(
        "3 members that depend on beta{cited} {}",
        "x".repeat(1100)
    )));
    let (w, _, j) = run(
        &dir,
        GoalKind::Inspect,
        Some(CoverageContract::CargoWorkspace),
        script,
    )
    .await;
    assert!(
        matches!(&w.report.outcome, WorkOutcome::Failed { reason, delivery: None }
            if reason.contains("not a valid decision") && reason.contains("is too long") && reason.contains("limit is 1024")),
        "{:?}",
        w.report.outcome
    );
    assert_eq!(j["inspection_coverage"]["status"], "satisfied");
    assert!(
        j["inspection_coverage"]["answer"].is_null(),
        "no answer was ever recorded"
    );
    assert_eq!(
        j["model_failures"],
        serde_json::json!([]),
        "a reply that arrived whole is not a provider failure"
    );
    assert!(!w.grounded);
    w.audit.assert_clean();
}
