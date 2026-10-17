//! The experimentation seam end to end: definition validation, the reference comparison in
//! isolated and replay modes, host-side rejection of a false claim, cancellation and deadlines,
//! authorization and isolation enforced by the real execution path, cleanup, and redaction.
//! No model, no network, no credential.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chip_core::{
    CapabilityAvailability, CapabilityBackend, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityId as Id, CapabilityProvider, CapabilityRequest, ExecutionError, ExecutionId,
    ExecutionRequest, ExecutionResult, Executor, InputValue, ScriptedPolicy, WorkDecision,
};
use chip_experiments::reference::{self, DecisionStrategy, ProjectFixture};
use chip_experiments::runner::remove_disposable;
use chip_experiments::{
    CancelHandle, CapabilityFixture, Cleanup, Conclusion, Confinement, DefinitionError,
    ExperimentDefinition, Mode, ModelSpec, Provenance, Redactor, Registry, Runner, Side,
    Termination, Verdict, Versioned, report_json, summary,
};

fn work_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "chip-experiments-test-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// No disposable directory is left behind.
fn leftovers(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn verdicts(
    result: &chip_experiments::ExperimentResult,
) -> Vec<(String, &'static str, &'static str)> {
    result
        .runs
        .iter()
        .map(|r| (r.task.clone(), r.side.name(), r.verdict.class()))
        .collect()
}

fn scripted(id: &'static str, decisions: Vec<Option<WorkDecision>>) -> Arc<DecisionStrategy> {
    Arc::new(DecisionStrategy {
        id,
        version: 1,
        description: "a scripted test strategy",
        policy: Arc::new(ScriptedPolicy::new(decisions)),
    })
}

fn request(capability: &str, inputs: &[(&str, &str)]) -> Option<WorkDecision> {
    let mut r = CapabilityRequest::new(ExecutionId::new("p"), Id::new(capability).unwrap());
    for (k, v) in inputs {
        r.inputs
            .insert(k.to_string(), InputValue::Text(v.to_string()));
    }
    r.inputs_present = !inputs.is_empty();
    Some(WorkDecision::RequestCapability(r))
}

fn complete(summary: &str) -> Option<WorkDecision> {
    Some(WorkDecision::Complete {
        summary: summary.into(),
    })
}

fn with_candidate(base: ExperimentDefinition, id: &str, candidate: &str) -> ExperimentDefinition {
    ExperimentDefinition {
        id: id.into(),
        candidate: Some(Versioned {
            id: candidate.into(),
            version: 1,
        }),
        ..base
    }
}

// ---- definition validation ----------------------------------------------------------------------

fn doc(edit: impl FnOnce(&mut serde_json::Value)) -> String {
    let mut v = reference::isolated_experiment().to_json();
    edit(&mut v);
    v.to_string()
}

#[test]
fn a_definition_round_trips_through_its_document() {
    let d = reference::isolated_experiment();
    let parsed = ExperimentDefinition::from_json(&d.to_json().to_string()).unwrap();
    assert_eq!(parsed, d);
    assert_eq!(parsed.digest(), d.digest());
    assert!(reference::registry().resolve(&parsed).is_ok());
}

#[test]
fn malformed_and_unsupported_definitions_are_refused_by_parsing() {
    let cases: Vec<(&str, String)> = vec![
        ("malformed", "{".into()),
        (
            "malformed",
            doc(|v| v["schema"] = "chip.experiment.v0".into()),
        ),
        ("malformed", doc(|v| v["surprise"] = true.into())),
        ("unsupported_mode", doc(|v| v["mode"] = "shadow".into())),
        ("unsupported_mode", doc(|v| v["mode"] = "canary".into())),
        ("unsupported_mode", doc(|v| v["mode"] = "production".into())),
        (
            "invalid_limits",
            doc(|v| v["limits"]["max_turns"] = 0.into()),
        ),
        (
            "invalid_limits",
            doc(|v| v["limits"]["deadline_ms"] = 600_001.into()),
        ),
        (
            "missing_criteria",
            doc(|v| v["criteria"] = serde_json::json!([])),
        ),
        ("malformed", doc(|v| v["tasks"] = serde_json::json!([]))),
        (
            "malformed",
            doc(|v| v["capabilities"] = serde_json::json!([])),
        ),
        (
            "malformed",
            doc(|v| v["candidate"] = serde_json::Value::Null),
        ),
        ("malformed", doc(|v| v["candidate"] = v["baseline"].clone())),
        ("unsupported_mode", doc(|v| v["mode"] = "observe".into())),
        (
            "unavailable_evidence",
            doc(|v| v["evidence"].as_array_mut().unwrap().push("cost".into())),
        ),
        (
            "malformed",
            doc(|v| v["model"] = serde_json::json!({"kind": "openai"})),
        ),
    ];
    for (code, text) in cases {
        let err = ExperimentDefinition::from_json(&text).unwrap_err();
        assert_eq!(err.code(), code, "{text}: {err}");
    }
}

#[test]
fn unknown_names_and_unenforceable_modes_are_refused_by_the_registry() {
    let r = reference::registry();
    let base = reference::isolated_experiment();
    let refuse = |d: ExperimentDefinition| r.resolve(&d).err().unwrap();
    let mut d = base.clone();
    d.candidate = Some(Versioned {
        id: "nobody".into(),
        version: 1,
    });
    assert!(matches!(refuse(d), DefinitionError::UnknownStrategy(_)));
    let mut d = base.clone();
    d.baseline.version = 2;
    assert!(matches!(refuse(d), DefinitionError::VersionMismatch(_)));
    let mut d = base.clone();
    d.tasks.push(Versioned {
        id: "nothing".into(),
        version: 1,
    });
    assert!(matches!(refuse(d), DefinitionError::UnknownTask(_)));
    let mut d = base.clone();
    d.capabilities.push("pax.test".into());
    assert!(matches!(refuse(d), DefinitionError::UnknownCapability(_)));
    let mut d = base.clone();
    d.criteria.push("trust.the-model".into());
    assert!(matches!(refuse(d), DefinitionError::UnknownCriterion(_)));
    // Git runs a process: nothing confines it, so isolated execution is refused, not pretended.
    let mut d = base.clone();
    d.capabilities.push("project.git.status".into());
    let err = refuse(d);
    assert!(
        matches!(err, DefinitionError::IsolationUnavailable(_)),
        "{err}"
    );
    // A tree criterion cannot be decided in replay, which has no tree.
    let mut d = reference::replay_experiment();
    d.criteria.push("tree.expected-files".into());
    assert!(matches!(refuse(d), DefinitionError::UnsupportedMode(_)));
    // Recorded replies the task does not have.
    let mut d = base.clone();
    d.model = ModelSpec::Recorded;
    assert!(matches!(refuse(d), DefinitionError::FixtureMismatch(_)));
    // A replay recording holding a capability outside the allowlist.
    let mut d = reference::replay_experiment();
    d.capabilities.retain(|c| c != "project.search");
    assert!(matches!(refuse(d), DefinitionError::FixtureMismatch(_)));
    // Observe requires a production baseline; the reference strategies are not production.
    let mut d = base;
    d.mode = Mode::Observe;
    d.candidate = None;
    assert!(matches!(refuse(d), DefinitionError::UnsupportedMode(_)));
}

// ---- the reference comparison -------------------------------------------------------------------

#[tokio::test]
async fn the_reference_experiment_detects_the_difference_in_isolated_mode() {
    let registry = reference::registry();
    let resolved = registry.resolve(&reference::isolated_experiment()).unwrap();
    let dir = work_dir("isolated");
    let result = Runner::new(&dir).run(&resolved).await;
    assert_eq!(
        verdicts(&result),
        vec![
            ("retries.explicit".into(), "baseline", "verified"),
            ("retries.explicit".into(), "candidate", "verified"),
            ("retries.worded".into(), "baseline", "verified"),
            ("retries.worded".into(), "candidate", "incomplete"),
        ]
    );
    let worded_candidate = &result.runs[3];
    assert_eq!(
        worded_candidate.verdict,
        Verdict::Incomplete {
            state: "blocked",
            reason: "searching for `retries` found no stated requirement".into()
        }
    );
    assert_eq!(
        result.comparison.conclusion,
        Conclusion::BaselineVerifiesMore
    );
    assert_eq!(result.comparison.conclusion.winner(), Some(Side::Baseline));
    assert_eq!(
        (
            result.comparison.verified_baseline,
            result.comparison.verified_candidate
        ),
        (2, 1)
    );
    assert!(
        result
            .comparison
            .differences
            .iter()
            .any(|d| d.starts_with("retries.explicit: decisions differ")),
        "{:?}",
        result.comparison.differences
    );
    for run in &result.runs {
        // The same criteria, in the same order, judged every run.
        let ids: Vec<&str> = run.criteria.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "observed.expected-writes",
                "tree.expected-files",
                "tree.no-unexpected-changes"
            ]
        );
        assert!(run.audit_clean, "{:?}", run.audit_details);
        assert_eq!(run.model_calls, 0, "no model was consulted");
        assert_eq!(run.tokens, None);
        assert_eq!(run.cleanup, Cleanup::Removed);
        assert!(
            !run.isolation_root_named,
            "no observation names the host path"
        );
    }
    assert_eq!(
        result.runs[0].decisions,
        [
            "local: request project.read",
            "local: request project.read",
            "local: request project.write",
            "local: complete",
        ]
    );
    assert_eq!(result.runs[1].decisions[0], "local: request project.search");
    assert!(result.cleanup_failures.is_empty());
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));

    let report = report_json(
        &resolved,
        &result,
        &Provenance {
            host: "test".into(),
            source_revision: Some("abc".into()),
        },
        &Redactor::default(),
    );
    assert_eq!(report["schema"], "chip.experiment-report.v1");
    assert_eq!(
        report["experiment"]["definition_digest"],
        resolved.definition.digest()
    );
    assert_eq!(report["comparison"]["winner"], "baseline");
    assert_eq!(report["runs"][0]["tokens"]["available"], false);
    assert_eq!(
        report["runs"][0]["tokens"]["reason"],
        "no model call was made"
    );
    assert_eq!(report["runs"][0]["cost"]["available"], false);
    assert_eq!(report["mode"]["capabilities"][0]["confinement"], "root");
    let text = summary(&resolved, &result, &Redactor::default());
    assert!(
        text.contains("Conclusion: baseline verifies more"),
        "{text}"
    );
}

#[tokio::test]
async fn replay_reaches_the_same_verdicts_from_the_recording_and_executes_nothing() {
    let registry = reference::registry();
    let isolated = Runner::new(work_dir("replay-a"))
        .run(&registry.resolve(&reference::isolated_experiment()).unwrap())
        .await;
    let dir = work_dir("replay-b");
    let resolved = registry.resolve(&reference::replay_experiment()).unwrap();
    let replayed = Runner::new(&dir).run(&resolved).await;
    assert_eq!(verdicts(&replayed), verdicts(&isolated));
    assert_eq!(
        replayed.comparison.conclusion,
        Conclusion::BaselineVerifiesMore
    );
    assert!(!dir.exists(), "replay creates no directory");
    for (r, i) in replayed.runs.iter().zip(&isolated.runs) {
        assert_eq!(r.cleanup, Cleanup::NotNeeded);
        // The recording is exactly what execution observed: the same evidence, digest for digest.
        let digests = |run: &chip_experiments::RunRecord| {
            run.evidence
                .iter()
                .map(|e| e.output_sha256.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(digests(r), digests(i), "{}", r.work_id);
    }
}

#[tokio::test]
async fn a_request_missing_from_the_recording_fails_and_nothing_is_substituted() {
    let registry = reference::registry().with_strategy(scripted(
        "test.reads-readme",
        vec![
            request("project.read", &[("path", "README.md")]),
            complete("done"),
        ],
    ));
    let d = with_candidate(
        reference::replay_experiment(),
        "test.unrecorded",
        "test.reads-readme",
    );
    let result = Runner::new(work_dir("unrecorded"))
        .run(&registry.resolve(&d).unwrap())
        .await;
    let candidate = &result.runs[1];
    match &candidate.verdict {
        Verdict::RuntimeFailure { reason, .. } => {
            assert!(reason.contains("not in the recording"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
    assert!(candidate.evidence.is_empty(), "no observation was invented");
}

// ---- the evaluator rejects claims ----------------------------------------------------------------

#[tokio::test]
async fn a_claimed_success_that_fails_the_host_criterion_is_rejected() {
    // No in-loop requirement: Chip accepts the completion, and only the host's reading of the
    // project can show it is false.
    let registry = reference::registry().with_strategy(scripted(
        "test.claims-done",
        vec![complete("retries is set to 3")],
    ));
    let mut d = with_candidate(
        reference::isolated_experiment(),
        "test.claims",
        "test.claims-done",
    );
    d.criteria = vec!["tree.expected-files".into()];
    d.tasks.truncate(1);
    let result = Runner::new(work_dir("claims"))
        .run(&registry.resolve(&d).unwrap())
        .await;
    let candidate = &result.runs[1];
    assert_eq!(
        candidate.terminal_state,
        Some("completed"),
        "Chip accepted the completion"
    );
    assert_eq!(
        candidate.verdict,
        Verdict::CompletedUnverified {
            failed: vec!["tree.expected-files".into()]
        }
    );
    assert_eq!(result.runs[0].verdict, Verdict::Verified);
    assert_eq!(result.comparison.conclusion.winner(), Some(Side::Baseline));
}

#[tokio::test]
async fn with_an_in_loop_criterion_chip_itself_refuses_the_claim() {
    let registry = reference::registry().with_strategy(scripted(
        "test.claims-done",
        vec![complete("retries is set to 3")],
    ));
    let mut d = with_candidate(
        reference::isolated_experiment(),
        "test.claims-loop",
        "test.claims-done",
    );
    d.tasks.truncate(1);
    let result = Runner::new(work_dir("claims-loop"))
        .run(&registry.resolve(&d).unwrap())
        .await;
    let candidate = &result.runs[1];
    // The refused local completion escalates; there is no model, so the work fails, typed.
    match &candidate.verdict {
        Verdict::RuntimeFailure { reason, .. } => assert!(reason.contains("no model"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(candidate.executions, 0);
    assert_eq!(candidate.model_calls, 1);
}

// ---- cancellation and deadlines -----------------------------------------------------------------

/// A pure test capability that, when executed, cancels the experiment (or just waits).
struct Trigger {
    cancel: Option<CancelHandle>,
    wait: Duration,
}

#[async_trait::async_trait]
impl CapabilityProvider for Trigger {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        // It has an effect, so its result may never answer a later request; Chip runs a locally
        // proposed request only for such a capability (otherwise it asks the model).
        let mut d = CapabilityDescriptor::new(
            CapabilityId::new("test.trigger").unwrap(),
            "trigger",
            "test trigger",
        );
        d.reuse_evidence = false;
        Ok(vec![d])
    }

    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

#[async_trait::async_trait]
impl Executor for Trigger {
    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        tokio::time::sleep(self.wait).await;
        if let Some(c) = &self.cancel {
            c.cancel();
        }
        Ok(ExecutionResult::success(request.id, "triggered"))
    }
}

struct TriggerFixture(Option<CancelHandle>, Duration);

impl CapabilityFixture for TriggerFixture {
    fn id(&self) -> &'static str {
        "test.trigger"
    }
    fn version(&self) -> u32 {
        1
    }
    fn provides(&self) -> Vec<(&'static str, Confinement)> {
        vec![("test.trigger", Confinement::Pure)]
    }
    fn backend(&self, _root: &std::path::Path) -> Arc<dyn CapabilityBackend> {
        Arc::new(Trigger {
            cancel: self.0.clone(),
            wait: self.1,
        })
    }
}

#[tokio::test]
async fn a_cancelled_candidate_is_reported_and_no_winner_is_declared() {
    let dir = work_dir("cancel");
    let runner = Runner::new(&dir);
    let registry = reference::registry()
        .with_capability_fixture(Arc::new(TriggerFixture(
            Some(runner.cancel_handle()),
            Duration::ZERO,
        )))
        .with_strategy(scripted(
            "test.cancels",
            vec![
                request("test.trigger", &[]),
                request("project.read", &[("path", "README.md")]),
                complete("done"),
            ],
        ));
    let mut d = with_candidate(
        reference::isolated_experiment(),
        "test.cancel",
        "test.cancels",
    );
    d.capabilities.push("test.trigger".into());
    let result = runner.run(&registry.resolve(&d).unwrap()).await;
    assert_eq!(result.runs[0].verdict, Verdict::Verified);
    let candidate = &result.runs[1];
    match &candidate.verdict {
        Verdict::Cancelled {
            cause, termination, ..
        } => {
            assert_eq!(*cause, "cancelled");
            assert_eq!(*termination, Termination::Known, "nothing was in flight");
        }
        other => panic!("{other:?} {candidate:#?}"),
    }
    assert_eq!(
        candidate.executions, 1,
        "the execution in flight finished; nothing started after it"
    );
    assert!(matches!(result.runs[2].verdict, Verdict::NotRun { .. }));
    assert!(matches!(result.runs[3].verdict, Verdict::NotRun { .. }));
    assert!(matches!(
        result.comparison.conclusion,
        Conclusion::Incomplete(_)
    ));
    assert_eq!(result.comparison.conclusion.winner(), None);
    assert!(
        leftovers(&dir).is_empty(),
        "the cancelled run's directory was removed too"
    );
}

#[tokio::test]
async fn the_run_deadline_stops_the_work_at_its_next_boundary() {
    let registry = reference::registry()
        .with_capability_fixture(Arc::new(TriggerFixture(None, Duration::from_millis(50))))
        .with_strategy(scripted(
            "test.slow",
            vec![
                request("test.trigger", &[]),
                request("test.trigger", &[]),
                complete("done"),
            ],
        ));
    let mut d = with_candidate(
        reference::isolated_experiment(),
        "test.deadline",
        "test.slow",
    );
    d.capabilities.push("test.trigger".into());
    d.tasks.truncate(1);
    d.limits.deadline_ms = 10;
    let result = Runner::new(work_dir("deadline"))
        .run(&registry.resolve(&d).unwrap())
        .await;
    match &result.runs[1].verdict {
        Verdict::Cancelled { cause, .. } => assert_eq!(*cause, "deadline"),
        other => panic!("{other:?}"),
    }
    assert!(result.runs[1].executions <= 1);
    assert!(matches!(
        result.comparison.conclusion,
        Conclusion::Incomplete(_)
    ));
}

// ---- authorization and isolation ------------------------------------------------------------------

#[tokio::test]
async fn a_capability_off_the_allowlist_is_never_executed() {
    let registry = reference::registry().with_strategy(scripted(
        "test.writes",
        vec![
            request(
                "project.write",
                &[
                    ("path", "config/settings.toml"),
                    ("content", "retries = 3\n"),
                ],
            ),
            complete("done"),
        ],
    ));
    let mut d = with_candidate(
        reference::isolated_experiment(),
        "test.unauthorized",
        "test.writes",
    );
    d.capabilities = vec!["project.read".into(), "project.search".into()];
    d.criteria = vec![
        "tree.no-unexpected-changes".into(),
        "tree.expected-files".into(),
    ];
    d.tasks.truncate(1);
    let result = Runner::new(work_dir("unauthorized"))
        .run(&registry.resolve(&d).unwrap())
        .await;
    let candidate = &result.runs[1];
    assert_eq!(candidate.executions, 0, "{candidate:?}");
    assert_ne!(candidate.verdict, Verdict::Verified);
    assert!(candidate.audit_clean, "{:?}", candidate.audit_details);
    // The host read the project afterwards: nothing changed.
    assert_eq!(
        candidate.criteria[0].1,
        chip_experiments::CriterionResult::Met
    );
}

#[tokio::test]
async fn paths_outside_the_disposable_directory_are_refused_by_the_executor() {
    let dir = work_dir("escape");
    let outside = dir.join("outside.txt");
    let absolute = outside.to_string_lossy().into_owned();
    let registry = reference::registry().with_strategy(scripted(
        "test.escapes",
        vec![
            request(
                "project.write",
                &[("path", "../outside.txt"), ("content", "x")],
            ),
            request("project.write", &[("path", &absolute), ("content", "x")]),
            request("project.read", &[("path", "../../etc/hostname")]),
            complete("done"),
        ],
    ));
    let mut d = with_candidate(
        reference::isolated_experiment(),
        "test.escape",
        "test.escapes",
    );
    d.tasks.truncate(1);
    let result = Runner::new(&dir).run(&registry.resolve(&d).unwrap()).await;
    let candidate = &result.runs[1];
    assert!(
        !outside.exists(),
        "nothing was written outside the disposable directory"
    );
    assert_ne!(candidate.verdict, Verdict::Verified);
    assert!(candidate.audit_clean, "{:?}", candidate.audit_details);
    assert!(!candidate.isolation_root_named);
    assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
}

// ---- cleanup and redaction ----------------------------------------------------------------------

#[test]
fn a_cleanup_failure_is_reported_never_ignored() {
    let gone = work_dir("cleanup-gone").join("never-created");
    assert!(
        matches!(remove_disposable(&gone), Cleanup::Failed(why) if why.contains("already gone"))
    );
    let dir = work_dir("cleanup-ok");
    std::fs::create_dir_all(dir.join("a/b")).unwrap();
    assert_eq!(remove_disposable(&dir), Cleanup::Removed);
}

#[tokio::test]
async fn secrets_never_reach_the_report_or_the_summary() {
    const SENTINEL: &str = "sk-SENTINEL-experiment-0123456789";
    let registry = reference::registry().with_strategy(scripted(
        "test.leaks",
        vec![Some(WorkDecision::Block {
            reason: format!("the key is {SENTINEL}"),
        })],
    ));
    let mut d = with_candidate(
        reference::isolated_experiment(),
        "test.redaction",
        "test.leaks",
    );
    d.tasks.truncate(1);
    let resolved = registry.resolve(&d).unwrap();
    let result = Runner::new(work_dir("redaction")).run(&resolved).await;
    assert!(
        result.runs[1]
            .outcome_reason
            .as_deref()
            .unwrap()
            .contains(SENTINEL),
        "the leak is real"
    );
    let redactor = Redactor::new([SENTINEL.to_string(), "abc".into()]);
    let report = report_json(&resolved, &result, &Provenance::default(), &redactor).to_string();
    let text = summary(&resolved, &result, &redactor);
    assert!(!report.contains(SENTINEL) && !text.contains(SENTINEL));
    assert!(report.contains("[redacted]"));
    assert_eq!(
        redactor.scrub("abc"),
        "abc",
        "values under 4 bytes are not treated as secrets"
    );
}

// ---- registration -------------------------------------------------------------------------------

#[test]
#[should_panic(expected = "registered twice")]
fn a_strategy_cannot_be_registered_twice() {
    let _ = Registry::new()
        .with_strategy(reference::read_then_write())
        .with_strategy(reference::read_then_write());
}

#[test]
#[should_panic(expected = "provided by both")]
fn two_fixtures_cannot_provide_the_same_capability() {
    let _ = Registry::new()
        .with_capability_fixture(Arc::new(ProjectFixture))
        .with_capability_fixture(Arc::new(ProjectFixture));
}
