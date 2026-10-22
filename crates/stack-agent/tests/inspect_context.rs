//! `inspect-bounded-v1`: what an inspection's model request carries.
//!
//! Policy-level tests build a trajectory from the real project executor's real output and call the
//! policy directly. End-to-end tests run the real work loop with the real project executor and a
//! scripted model: only the model is scripted, and the budget check, condensation check, audit and
//! report are the product's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chip_cli::inspect_context::{
    INSPECT_BOUNDED_ID, INSPECT_COVERED_ID, InspectBoundedContext, InspectCoveredContext,
};
use chip_cli::software_work::{GoalKind, default_context_policy, inspect_context_policy};
use chip_core::{
    Agent, CapabilityId, DecisionRecord, DecisionSource, ExecutionId, ExecutionObserver,
    ExecutionRequest, ExecutionStatus, Executor, InputValue, LimitKind, ModelDecisionBoundary,
    NoLocalPolicy, Observation, ObservationKind, ObservationOrigin, WorkDecision, WorkEvent,
    WorkGoal, WorkId, WorkLimits, WorkOutcome, WorkReport, WorkSpec, WorkState, WorkTrajectory,
    audit_safety, context_report,
};
use chip_core::{EscalationContext, EscalationContextPolicy};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use chip_project::ProjectExecutor;

// ---- a real workspace on disk -----------------------------------------------------------------

fn workspace(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("chip-inspect-ctx-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for d in ["crates/alpha/src", "crates/beta/src", "docs"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let w = |p: &str, t: &str| std::fs::write(root.join(p), t).unwrap();
    w(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/alpha\", \"crates/beta\"]\n",
    );
    let mut readme = String::from("# Demo\n\nA demonstration workspace.\n\n");
    for i in 0..60 {
        readme.push_str(&format!(
            "## Section {i}\n\nSome prose about section {i} that makes the document large.\n\n"
        ));
    }
    w("README.md", &readme);
    w(
        "crates/alpha/Cargo.toml",
        "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\n\n[dependencies]\nbeta = { path = \"../beta\" }\n",
    );
    w(
        "crates/beta/Cargo.toml",
        "[package]\nname = \"beta\"\nversion = \"0.1.0\"\n",
    );
    w("crates/alpha/src/lib.rs", "pub fn alpha() {}\n");
    w("crates/beta/src/lib.rs", &"// filler line\n".repeat(1500));
    root
}

async fn observe(
    project: &ProjectExecutor,
    n: usize,
    capability: &str,
    given: &[(&str, &str)],
) -> (Observation, ObservationOrigin) {
    let inputs: BTreeMap<String, InputValue> = given
        .iter()
        .map(|(k, v)| (k.to_string(), InputValue::Text(v.to_string())))
        .collect();
    let id = ExecutionId::new(format!("w-exec-{n}"));
    let result = project
        .execute(ExecutionRequest::new(id.clone(), capability).with_inputs(inputs))
        .await
        .unwrap();
    let observation = Observation {
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
    };
    let origin = ObservationOrigin {
        capability: CapabilityId::new(capability).unwrap(),
        invocation: format!("{capability}{given:?}"),
        reusable: true,
        provider_response_id: None,
    };
    (observation, origin)
}

/// The trajectory a manifest-first inspection of the workspace has produced so far.
async fn trajectory_parts(
    project: &ProjectExecutor,
    steps: &[(&str, &[(&str, &str)])],
) -> (Vec<Observation>, Vec<ObservationOrigin>) {
    let (mut observations, mut origins) = (Vec::new(), Vec::new());
    for (i, (capability, given)) in steps.iter().enumerate() {
        let (o, origin) = observe(project, i + 1, capability, given).await;
        observations.push(o);
        origins.push(origin);
    }
    (observations, origins)
}

fn build(
    policy: &dyn EscalationContextPolicy,
    observations: &[Observation],
    origins: &[ObservationOrigin],
    decisions: &[DecisionRecord],
    budget: Option<usize>,
) -> EscalationContext {
    policy.build(
        &WorkState {
            goal: "Summarize the workspace architecture.",
            turn: observations.len(),
            max_turns: 8,
            executions: observations.len(),
            max_executions: 12,
            context_budget_bytes: budget,
        },
        &WorkTrajectory {
            observations,
            origins,
            decisions,
            ruled_out: &[],
            evidence: &[],
            question: "the question",
        },
    )
}

const LIST_ROOT: (&str, &[(&str, &str)]) = ("project.list", &[("path", ".")]);
const READ_ROOT: (&str, &[(&str, &str)]) = ("project.read", &[("path", "Cargo.toml")]);
const READ_README: (&str, &[(&str, &str)]) = ("project.read", &[("path", "README.md")]);
const LIST_CRATES: (&str, &[(&str, &str)]) = ("project.list", &[("path", "crates")]);
const READ_ALPHA: (&str, &[(&str, &str)]) =
    ("project.read", &[("path", "crates/alpha/Cargo.toml")]);

// ---- selection ----------------------------------------------------------------------------------

#[test]
fn inspect_defaults_to_bounded_and_the_other_kinds_and_the_old_policy_are_unchanged() {
    assert_eq!(
        default_context_policy(GoalKind::Inspect).id(),
        "inspect-bounded-v1"
    );
    assert_eq!(INSPECT_BOUNDED_ID, "inspect-bounded-v1");
    assert_eq!(default_context_policy(GoalKind::Change).id(), "dedup-v1");
    assert_eq!(default_context_policy(GoalKind::Verify).id(), "dedup-v1");
    // The earlier policy is still selectable, by name, for compatibility and comparison.
    assert_eq!(inspect_context_policy().id(), "inspect-progress-v1");
}

#[tokio::test]
async fn with_room_to_spare_selection_sends_everything_and_is_deterministic() {
    let root = workspace("room");
    let project = ProjectExecutor::new(&root);
    let (obs, origins) = trajectory_parts(&project, &[LIST_ROOT, READ_ROOT, READ_README]).await;
    let first = build(&InspectBoundedContext, &obs, &origins, &[], Some(1_000_000));
    let second = build(&InspectBoundedContext, &obs, &origins, &[], Some(1_000_000));
    assert_eq!(first, second, "same state and trajectory, same context");
    assert_eq!(first.render(), second.render());
    assert!(first.condensed.is_empty(), "nothing is condensed that fits");
    assert_eq!(first.relevant_observations, obs);
    // Without a budget there is nothing to fit, and the result is the same.
    assert_eq!(
        build(&InspectBoundedContext, &obs, &origins, &[], None),
        first
    );
}

#[tokio::test]
async fn over_budget_it_condenses_the_largest_earlier_observation_and_keeps_the_rest() {
    let root = workspace("condense");
    let project = ProjectExecutor::new(&root);
    let (obs, origins) = trajectory_parts(
        &project,
        &[LIST_ROOT, READ_ROOT, READ_README, LIST_CRATES, READ_ALPHA],
    )
    .await;
    let full = build(&InspectBoundedContext, &obs, &origins, &[], None);
    let readme_bytes = obs[2].render().len();
    assert!(readme_bytes > 3000, "the README is the large one");
    let budget = full.metrics().bytes - readme_bytes / 2;

    let context = build(&InspectBoundedContext, &obs, &origins, &[], Some(budget));
    assert!(context.metrics().bytes <= budget);
    // Only the README was condensed, and its place in the record is named.
    assert_eq!(context.condensed.len(), 1);
    let condensed = &context.condensed[0];
    assert_eq!(condensed.execution_id, obs[2].execution_id);
    assert_eq!(condensed.withheld_bytes, readme_bytes);
    assert!(
        condensed.facts.iter().all(|f| f.contains("observation 3")),
        "{condensed:?}"
    );
    assert!(
        condensed.facts.iter().any(|f| f.starts_with("observed"))
            && condensed.facts.iter().any(|f| f.starts_with("derived")),
        "directly observed facts and derivations are told apart: {condensed:?}"
    );
    let kept: Vec<_> = context
        .relevant_observations
        .iter()
        .map(|o| &o.execution_id)
        .collect();
    assert_eq!(
        kept,
        [
            &obs[0].execution_id,
            &obs[1].execution_id,
            &obs[3].execution_id,
            &obs[4].execution_id
        ]
    );
    // The recorded observations themselves are untouched.
    assert!(obs[2].output.as_deref().unwrap().contains("## Section 59"));
    // Everything the model is told is in the rendered request, and the withheld text is not.
    let text = context.render();
    assert!(text.contains("Condensed (recorded in full; not retransmitted in this request)"));
    assert!(text.contains("61 headings"), "{text}");
    assert!(!text.contains("makes the document large"), "{text}");
}

#[tokio::test]
async fn the_latest_observation_and_every_failure_stay_in_full_even_when_that_cannot_fit() {
    let root = workspace("keep");
    let project = ProjectExecutor::new(&root);
    let (obs, origins) = trajectory_parts(
        &project,
        &[
            READ_README,
            ("project.read", &[("path", "crates/missing/Cargo.toml")]),
            ("project.read", &[("path", "crates/beta/src/lib.rs")]),
        ],
    )
    .await;
    assert_eq!(obs[1].status, ExecutionStatus::Failure);
    let context = build(&InspectBoundedContext, &obs, &origins, &[], Some(2_000));
    // The README is condensed; the failure and the latest (large) observation are not, and the
    // result is still over budget: the loop's check, not this policy, refuses to send it.
    assert_eq!(context.condensed.len(), 1);
    assert_eq!(context.condensed[0].execution_id, obs[0].execution_id);
    assert_eq!(context.relevant_observations.len(), 2);
    assert!(context.metrics().bytes > 2_000);
}

#[tokio::test]
async fn model_words_are_never_an_input_to_the_facts() {
    let root = workspace("words");
    let project = ProjectExecutor::new(&root);
    let (obs, origins) = trajectory_parts(&project, &[READ_README, READ_ROOT]).await;
    let forged = DecisionRecord {
        turn: 0,
        source: DecisionSource::Model,
        decision: WorkDecision::Complete {
            summary: "FORGED: crates/ghost depends on crates/phantom".into(),
        },
    };
    let context = build(
        &InspectBoundedContext,
        &obs,
        &origins,
        &[forged],
        Some(1_500),
    );
    let text = context.render();
    assert!(
        !text.contains("FORGED") && !text.contains("phantom"),
        "{text}"
    );
    assert!(
        context
            .condensed
            .iter()
            .flat_map(|c| &c.facts)
            .all(|f| !f.contains("ghost")),
        "{context:?}"
    );
}

// ---- manifest-first -----------------------------------------------------------------------------

fn status_of(context: &EscalationContext) -> String {
    context
        .relevant_evidence
        .iter()
        .find(|e| e.starts_with("Manifest-first status"))
        .cloned()
        .expect("the policy always states the manifest-first status")
}

#[tokio::test]
async fn the_next_unread_manifest_is_named_from_observations_and_a_read_one_is_not_offered_again() {
    let root = workspace("manifest");
    let project = ProjectExecutor::new(&root);

    let (obs, origins) = trajectory_parts(&project, &[LIST_ROOT]).await;
    let status = status_of(&build(&InspectBoundedContext, &obs, &origins, &[], None));
    assert!(
        status.contains("shown by a listing and not yet read: Cargo.toml, README.md"),
        "{status}"
    );
    assert!(status.contains("Next: project.read"), "{status}");

    let (obs, origins) =
        trajectory_parts(&project, &[LIST_ROOT, READ_ROOT, READ_README, LIST_CRATES]).await;
    let status = status_of(&build(&InspectBoundedContext, &obs, &origins, &[], None));
    assert!(
        status.contains("Cargo.toml (observation 2)")
            && status.contains("README.md (observation 3)"),
        "{status}"
    );
    assert!(
        status.contains("crates/alpha/Cargo.toml, crates/beta/Cargo.toml"),
        "{status}"
    );
    assert!(
        status.contains("Directories already listed") && status.contains("., crates"),
        "{status}"
    );
    assert!(
        !status.contains("docs/Cargo.toml"),
        "only directories under a listed subdirectory: {status}"
    );

    // Alpha read: no longer offered; beta remains. A model saying it read beta changes nothing.
    let (obs, origins) = trajectory_parts(
        &project,
        &[LIST_ROOT, READ_ROOT, READ_README, LIST_CRATES, READ_ALPHA],
    )
    .await;
    let status = status_of(&build(&InspectBoundedContext, &obs, &origins, &[], None));
    assert!(
        status.contains("crates/alpha/Cargo.toml (observation 5)"),
        "{status}"
    );
    assert!(
        status.contains(
            "not yet read (whether each exists is not yet observed): crates/beta/Cargo.toml."
        ),
        "{status}"
    );
    assert!(
        !status.contains("Workspace members declared"),
        "an inspection without a coverage contract is not pushed toward exhaustive reading: {status}"
    );
    // Under a declared coverage contract the declared members themselves are listed.
    let covered = status_of(&build(&InspectCoveredContext, &obs, &origins, &[], None));
    assert!(
        covered.contains(
            "Workspace members declared by Cargo.toml (observation 2): 2; their manifests read: 1."
        ),
        "{covered}"
    );
    assert!(
        covered.contains("Declared member manifests not yet read: crates/beta/Cargo.toml."),
        "{covered}"
    );
    assert_eq!(InspectCoveredContext.id(), INSPECT_COVERED_ID);
    assert_ne!(INSPECT_COVERED_ID, INSPECT_BOUNDED_ID);

    // A read that failed is not offered again either.
    let (obs, origins) = trajectory_parts(
        &project,
        &[
            LIST_ROOT,
            READ_ROOT,
            LIST_CRATES,
            ("project.read", &[("path", "crates/alpha/Cargo.toml")]),
            ("project.read", &[("path", "crates/beta/Cargo.toml")]),
        ],
    )
    .await;
    let status = status_of(&build(&InspectBoundedContext, &obs, &origins, &[], None));
    assert!(
        status.contains("No unread manifest is known") || status.contains("README.md"),
        "{status}"
    );
    assert!(!status.contains("crates/alpha/Cargo.toml."), "{status}");
}

#[tokio::test]
async fn nothing_assumes_the_project_is_a_cargo_workspace() {
    let root = std::env::temp_dir().join(format!("chip-inspect-ctx-{}-node", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("packages/ui")).unwrap();
    std::fs::write(root.join("package.json"), "{\"name\":\"root\"}\n").unwrap();
    std::fs::write(root.join("README.md"), "# Root\n").unwrap();
    let project = ProjectExecutor::new(&root);
    let (obs, origins) = trajectory_parts(
        &project,
        &[
            LIST_ROOT,
            ("project.read", &[("path", "package.json")]),
            ("project.list", &[("path", "packages")]),
        ],
    )
    .await;
    let status = status_of(&build(&InspectBoundedContext, &obs, &origins, &[], None));
    assert!(status.contains("packages/ui/package.json"), "{status}");
    assert!(!status.contains("Cargo.toml"), "{status}");

    // Before anything is observed it asks for no particular ecosystem.
    let status = status_of(&build(&InspectBoundedContext, &[], &[], &[], None));
    assert!(
        status.contains("no project metadata has been observed yet"),
        "{status}"
    );
    assert!(!status.contains("Cargo"), "{status}");
}

#[tokio::test]
async fn dependency_facts_come_from_the_manifest_that_declares_them() {
    let root = workspace("deps");
    let project = ProjectExecutor::new(&root);
    let beta = ("project.read", &[("path", "crates/beta/Cargo.toml")][..]);
    let (obs, origins) =
        trajectory_parts(&project, &[READ_ALPHA, beta, READ_README, READ_ROOT]).await;
    let context = build(&InspectBoundedContext, &obs, &origins, &[], Some(1));
    let facts: Vec<String> = context
        .condensed
        .iter()
        .flat_map(|c| c.facts.clone())
        .collect();
    let alpha = facts
        .iter()
        .find(|f| f.contains("[dependencies]"))
        .expect("alpha declares a dependency");
    assert!(
        alpha.contains("observation 1") && alpha.contains("beta (path ../beta)"),
        "{alpha}"
    );
    // Beta declares none, so no dependency fact exists for observation 2.
    assert!(
        !facts
            .iter()
            .any(|f| f.contains("observation 2") && f.contains("depend")),
        "{facts:?}"
    );
    // The workspace manifest's members are what its [workspace] table lists.
    let members = obs[3].output.as_deref().unwrap();
    assert!(members.contains("crates/alpha") && members.contains("crates/beta"));
}

// ---- the real loop --------------------------------------------------------------------------------

struct Script {
    replies: Mutex<Vec<String>>,
    seen: Mutex<Vec<ModelRequest>>,
}

#[async_trait::async_trait]
impl ModelProvider for Script {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, FxError> {
        self.seen.lock().unwrap().push(request);
        let mut replies = self.replies.lock().unwrap();
        if replies.is_empty() {
            return Err(FxError::Provider("the provider timed out".into()));
        }
        Ok(ModelResponse::new(
            "r",
            replies.remove(0),
            Usage::new(11, 3),
        ))
    }
}

fn request(capability: &str, path: &str) -> String {
    format!(
        r#"{{"decision":"request_capability","capability":"{capability}","inputs":{{"path":"{path}"}}}}"#
    )
}

fn answer() -> String {
    r#"{"decision":"complete","summary":"The workspace has alpha and beta; alpha depends on beta (crates/alpha/Cargo.toml)."}"#.to_string()
}

struct Run {
    report: WorkReport,
    spec: WorkSpec,
    seen: Vec<ModelRequest>,
}

async fn run_loop(root: &Path, replies: Vec<String>, budget: Option<usize>) -> Run {
    let project = Arc::new(ProjectExecutor::new(root));
    let model = Arc::new(Script {
        replies: Mutex::new(replies),
        seen: Mutex::new(Vec::new()),
    });
    let agent = Agent::new(model.clone())
        .with_capabilities(project.clone())
        .with_executor(project)
        .with_observer(Arc::new(ExecutionObserver));
    let mut spec = WorkSpec::new(
        WorkId::new("w"),
        WorkGoal::new("Summarize the workspace architecture."),
    )
    .with_limits(WorkLimits {
        max_turns: 12,
        max_executions: 12,
    });
    if let Some(bytes) = budget {
        spec = spec.with_context_budget_bytes(bytes);
    }
    let report = agent
        .run_work_with_context_policy(
            &spec,
            &NoLocalPolicy,
            &ModelDecisionBoundary,
            &InspectBoundedContext,
        )
        .await;
    let seen = model.seen.lock().unwrap().clone();
    Run { report, spec, seen }
}

fn script() -> Vec<String> {
    vec![
        request("project.list", "."),
        request("project.read", "Cargo.toml"),
        request("project.read", "README.md"),
        request("project.list", "crates"),
        request("project.read", "crates/alpha/Cargo.toml"),
        request("project.read", "crates/beta/Cargo.toml"),
        answer(),
    ]
}

fn declared() -> Vec<CapabilityId> {
    ["project.list", "project.read", "project.search"]
        .iter()
        .map(|c| CapabilityId::new(*c).unwrap())
        .collect()
}

fn all_text(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn a_budget_the_full_history_exceeds_is_met_by_condensing_and_the_record_stays_whole() {
    let root = workspace("loop");
    let unbounded = run_loop(&root, script(), None).await;
    let unbounded_report = context_report(&unbounded.report, &unbounded.spec);
    assert_eq!(unbounded_report.condensed_observations(), 0);
    let largest = unbounded_report.max_request_bytes();

    // Room for every request up to and including the one that carries the just-read README in
    // full (the latest observation is never condensed), but not for the later ones unless the
    // README is condensed.
    let budget = unbounded_report.calls[3].request_bytes + 200;
    assert!(
        largest > budget,
        "the later requests would not fit uncondensed"
    );
    let run = run_loop(&root, script(), Some(budget)).await;
    assert!(
        matches!(run.report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        run.report.outcome
    );
    let c = context_report(&run.report, &run.spec);
    assert!(
        c.max_request_bytes() <= budget,
        "{} > {budget}",
        c.max_request_bytes()
    );
    assert!(c.condensed_observations() >= 1);
    assert_eq!(c.context_limit_rejections, 0);
    assert!(c.rejections.is_empty());
    assert_eq!((c.requests_sent(), c.provider_failures()), (7, 0));

    // All six executions and their observations are recorded in full, identically to the run
    // that condensed nothing.
    assert_eq!(run.report.observations.len(), 6);
    assert_eq!(
        run.report
            .observations
            .iter()
            .map(|o| o.output.clone())
            .collect::<Vec<_>>(),
        unbounded
            .report
            .observations
            .iter()
            .map(|o| o.output.clone())
            .collect::<Vec<_>>()
    );
    assert!(audit_safety(&run.report, &run.spec, &declared()).is_clean());
    // The last request names what it left out, and says where the full text is.
    let last = all_text(run.seen.last().unwrap());
    assert!(last.contains("Condensed (recorded in full"), "{last}");
    assert!(last.contains("Manifest-first status"), "{last}");
    assert_eq!(
        run.report
            .events
            .iter()
            .filter_map(|e| match e {
                WorkEvent::ModelEscalation { context_policy, .. } => Some(context_policy.as_str()),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>(),
        ["inspect-bounded-v1"].into()
    );
}

#[tokio::test]
async fn when_the_latest_observation_alone_cannot_fit_the_request_is_rejected_not_sent() {
    let root = workspace("reject");
    // The last read is a ~22 KB file: it must be sent in full with the next request.
    let replies = vec![
        request("project.read", "Cargo.toml"),
        request("project.read", "crates/beta/src/lib.rs"),
        answer(),
    ];
    let run = run_loop(&root, replies, Some(9_000)).await;
    assert_eq!(
        run.report.outcome,
        WorkOutcome::LimitReached {
            limit: LimitKind::Context
        }
    );
    // Two requests were sent, the third was refused before transmission and never retried smaller.
    assert_eq!(run.seen.len(), 2);
    let c = context_report(&run.report, &run.spec);
    assert_eq!(c.context_limit_rejections, 1);
    assert_eq!(c.rejections.len(), c.context_limit_rejections);
    let r = &c.rejections[0];
    assert_eq!((r.turn, r.budget_bytes), (2, 9_000));
    assert!(r.request_bytes > 9_000);
    assert_eq!((c.requests_sent(), c.provider_failures()), (2, 0));
    assert!(audit_safety(&run.report, &run.spec, &declared()).is_clean());
    // The work did not complete, and nothing was claimed to be established by the refused request.
    assert!(!matches!(run.report.outcome, WorkOutcome::Completed { .. }));
}

#[tokio::test]
async fn a_timeout_after_condensation_is_a_provider_failure_and_not_a_context_rejection() {
    let root = workspace("timeout");
    let mut replies = script();
    replies.truncate(5); // the sixth model call gets no reply, as a timeout would
    let unbounded = run_loop(&root, script(), None).await;
    let budget = context_report(&unbounded.report, &unbounded.spec).calls[3].request_bytes + 200;
    let run = run_loop(&root, replies, Some(budget)).await;
    assert!(
        matches!(run.report.outcome, WorkOutcome::Failed { .. }),
        "{:?}",
        run.report.outcome
    );
    let c = context_report(&run.report, &run.spec);
    assert_eq!(c.context_limit_rejections, 0);
    assert_eq!(c.provider_failures(), 1);
    assert!(audit_safety(&run.report, &run.spec, &declared()).is_clean());
}

#[tokio::test]
async fn a_repeated_large_observation_is_condensed_first_and_searches_still_run() {
    let root = workspace("repeat");
    let replies = vec![
        request("project.read", "README.md"),
        request("project.read", "README.md"),
        r#"{"decision":"request_capability","capability":"project.search","inputs":{"query":"alpha"}}"#.into(),
        answer(),
    ];
    // project.read does not allow an earlier observation to stand in for a later one (a file can
    // change), so the existing omission rule keeps both copies; with no room for two, the policy
    // condenses the redundant earlier copy and keeps the latest.
    let unbounded = run_loop(&root, replies.clone(), None).await;
    let before = context_report(&unbounded.report, &unbounded.spec);
    assert_eq!(before.omitted_observations(), 0);
    let budget = before.calls.last().unwrap().request_bytes - 2_000;
    let run = run_loop(&root, replies, Some(budget)).await;
    assert!(
        matches!(run.report.outcome, WorkOutcome::Completed { .. }),
        "{:?}",
        run.report.outcome
    );
    // An implementation search is still available, and it executed for real.
    assert!(run.report.observations.iter().any(|o| {
        o.output
            .as_deref()
            .is_some_and(|t| t.contains("\"capability\":\"project.search\""))
            && o.status == ExecutionStatus::Success
    }));
    let c = context_report(&run.report, &run.spec);
    assert!(c.condensed_observations() >= 1);
    assert_eq!(
        all_text(run.seen.last().unwrap())
            .matches("Some prose about section 59")
            .count(),
        1,
        "the README text travels once"
    );
    // Both copies stay recorded.
    assert_eq!(run.report.observations.len(), 3);
    assert!(audit_safety(&run.report, &run.spec, &declared()).is_clean());
}

#[tokio::test]
async fn without_a_coverage_contract_declared_members_change_nothing_in_the_context() {
    let root = workspace("members-off");
    let project = ProjectExecutor::new(&root);
    // The root manifest and README are read; no directory was listed, so a listing-based guess has
    // nothing to offer. The declared members must not turn that into "keep reading".
    let (obs, origins) = trajectory_parts(&project, &[READ_ROOT, READ_README]).await;
    let default = status_of(&build(&InspectBoundedContext, &obs, &origins, &[], None));
    assert!(default.contains("No unread manifest is known"), "{default}");
    assert!(
        !default.contains("Workspace members declared") && !default.contains("Declared member"),
        "{default}"
    );
    let covered = status_of(&build(&InspectCoveredContext, &obs, &origins, &[], None));
    assert!(
        covered.contains("Declared member manifests not yet read"),
        "{covered}"
    );
    assert!(covered.contains("Next: project.read"), "{covered}");
    // Everything but those lines is the same text.
    let strip = |s: &str| {
        s.lines()
            .filter(|l| {
                !l.contains("Workspace members declared")
                    && !l.contains("Declared member")
                    && !l.contains("Coverage is required")
                    && !l.contains("Next:")
                    && !l.contains("No unread manifest")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(strip(&default), strip(&covered));
}

#[tokio::test]
async fn the_loops_own_evidence_is_kept_ahead_of_the_inspection_progress() {
    // The loop may hand an escalation evidence of its own (why a decision was rejected, that the
    // goal's requirements are met). Under a coverage contract the progress summary is compacted in
    // place; what the loop said must stay exactly as it was, and the progress must appear once.
    let root = workspace("loop-evidence");
    let project = ProjectExecutor::new(&root);
    let (obs, origins) = trajectory_parts(&project, &[READ_ROOT, READ_ALPHA]).await;
    let said = vec!["turn 3: your reply was rejected and nothing in it was done: x".to_string()];
    let context = InspectCoveredContext.build(
        &WorkState {
            goal: "Summarize the workspace architecture.",
            turn: obs.len(),
            max_turns: 8,
            executions: obs.len(),
            max_executions: 12,
            context_budget_bytes: None,
        },
        &WorkTrajectory {
            observations: &obs,
            origins: &origins,
            decisions: &[],
            ruled_out: &[],
            evidence: &said,
            question: "the question",
        },
    );
    assert_eq!(context.relevant_evidence[0], said[0]);
    let text = context.render();
    assert_eq!(text.matches("Inspection progress").count(), 1, "{text}");
}
