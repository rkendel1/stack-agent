//! The AppPort source end to end, in replay: what a real host running the published `appport`
//! CLI served (discovery, the package contract, results, refusals and its durable records;
//! `fixtures/*.snapshot.json`, recorded by `recorder/record.mjs`) is offered to Chip's ordinary work
//! loop, requested by the (recorded) model, validated and routed by Chip, answered from the
//! recording, and checked against the host's records. The project capabilities' answers are produced
//! here by the real `chip-project` backend. No model, no network, no credential.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use chip_appport_source::{AppPortSource, BINDING_FILE, EvidenceCorrelated, Snapshot, Succeeded};
use chip_core::{
    Effect, ExecutionId, ExecutionRequest, ExecutionStatus, InputValue, LocalWorkPolicy,
    NoLocalPolicy,
};
use chip_experiments::plugin::recorded_inputs;
use chip_experiments::reference::ProjectFixture;
use chip_experiments::{
    CapabilityFixture, CapabilitySource, CriterionResult, DefinitionError, Detection,
    EvidenceField, ExperimentDefinition, ExtensionPoint, Mode, ModelSpec, Provenance,
    RecordedExecution, Recording, Redactor, Registry, RunLimits, RunRecord, Runner, Strategy,
    TaskFixture, Verdict, Versioned, report_json,
};

const AUTHORIZED: &str = include_str!("../fixtures/todos.authorized.snapshot.json");
const READ_ONLY: &str = include_str!("../fixtures/todos.read-only.snapshot.json");
const README: &str = include_str!("../fixtures/project/README.md");
const BINDING: &str = include_str!("../fixtures/project/appport.binding.json");
const INSTALL: &str = include_str!("../fixtures/project/docs/install.md");
const USAGE: &str = include_str!("../fixtures/project/docs/usage.md");

const LIST: &str = "appport.todos.todo.list";
const COMPLETE: &str = "appport.todos.todo.complete";
const GOAL: &str = "README.md links to a documentation page that does not exist. Point that link at the existing page in docs/ that covers the same topic, change nothing else, then mark the matching item complete in the project's Todos application.";

fn source(snapshot: &str) -> AppPortSource {
    AppPortSource::new(Snapshot::from_json(snapshot).unwrap(), "todos").unwrap()
}

fn edited(edit: impl FnOnce(&mut serde_json::Value)) -> Result<AppPortSource, String> {
    let mut v: serde_json::Value = serde_json::from_str(AUTHORIZED).unwrap();
    edit(&mut v);
    AppPortSource::new(Snapshot::from_json(&v.to_string())?, "todos")
}

fn work_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-appport-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn json(text: &str) -> String {
    serde_json::to_string(text).unwrap()
}

fn sha256(text: &str) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "sha256:{}",
        Sha256::digest(text.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

// ---- the task: project answers from the real backend, AppPort answers from the host ------------------

/// Runs `requests` through the real `chip-project` backend on a copy of `files`, recording each
/// answer as replay will serve it.
async fn project_recording(
    files: &[(String, String)],
    requests: &[(&str, &[(&str, &str)])],
) -> Vec<RecordedExecution> {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let root = work_dir(&format!(
        "record-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    for (path, content) in files {
        let at = root.join(path);
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, content).unwrap();
    }
    let backend = ProjectFixture.backend(&root);
    let mut out = Vec::new();
    for (n, (capability, inputs)) in requests.iter().enumerate() {
        let inputs: BTreeMap<String, InputValue> = inputs
            .iter()
            .map(|(k, v)| (k.to_string(), InputValue::Text(v.to_string())))
            .collect();
        let request = ExecutionRequest {
            id: ExecutionId::new(format!("r{n}")),
            intent: capability.to_string(),
            inputs: inputs.clone(),
            idempotency_key: None,
        };
        let result = backend.execute(request).await.unwrap();
        out.push(RecordedExecution {
            capability: capability.to_string(),
            inputs: recorded_inputs(&inputs),
            succeeded: result.status == ExecutionStatus::Success,
            output: result.output,
        });
    }
    let _ = std::fs::remove_dir_all(&root);
    out
}

fn fixed() -> String {
    README.replace("docs/setup.md", "docs/install.md")
}

/// The (recorded) model: fix the README, list the todos, complete one with `complete_inputs`, then
/// claim completion.
fn replies(complete_inputs: &str) -> Vec<String> {
    let done = r#"{"decision":"complete","summary":"README.md links docs/install.md and todo_1 is complete"}"#;
    let mut r = vec![
        r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"README.md"}}"#.to_string(),
        r#"{"decision":"request_capability","capability":"project.list","inputs":{"path":"docs"}}"#.to_string(),
        format!(
            r#"{{"decision":"request_capability","capability":"project.write","inputs":{{"path":"README.md","content":{}}}}}"#,
            json(&fixed())
        ),
        // todo.list declares no inputs, so the request carries no `inputs` member (AGENTS.md
        // invariant 3: an empty object is still an invocation member).
        format!(r#"{{"decision":"request_capability","capability":"{LIST}"}}"#),
        format!(
            r#"{{"decision":"request_capability","capability":"{COMPLETE}","inputs":{complete_inputs}}}"#
        ),
    ];
    r.extend(std::iter::repeat_n(done.to_string(), 4));
    r
}

async fn task(
    id: &str,
    binding: bool,
    complete_inputs: &str,
    appport: &AppPortSource,
) -> TaskFixture {
    let mut project = vec![
        ("README.md".to_string(), README.to_string()),
        ("docs/install.md".to_string(), INSTALL.to_string()),
        ("docs/usage.md".to_string(), USAGE.to_string()),
    ];
    if binding {
        project.push((BINDING_FILE.to_string(), BINDING.to_string()));
    }
    let fix = fixed();
    let mut entries = project_recording(
        &project,
        &[
            ("project.read", &[("path", "README.md")]),
            ("project.list", &[("path", "docs")]),
            (
                "project.write",
                &[("path", "README.md"), ("content", fix.as_str())],
            ),
        ],
    )
    .await;
    entries.extend(appport.recorded_executions());
    TaskFixture {
        id: id.into(),
        version: 1,
        goal: GOAL.into(),
        files: project,
        expected_files: vec![("README.md".into(), fix)],
        model_replies: replies(complete_inputs),
        recording: Some(Recording {
            schema: chip_experiments::plugin::RECORDING_SCHEMA.into(),
            entries,
        }),
    }
}

// ---- strategies and the experiment ---------------------------------------------------------------------

/// Chip's default behavior: every step escalates to the model.
struct ModelDriven {
    id: &'static str,
    sources: Vec<Versioned>,
}

impl Strategy for ModelDriven {
    fn id(&self) -> &'static str {
        self.id
    }
    fn version(&self) -> u32 {
        1
    }
    fn description(&self) -> &'static str {
        "no local decision: the model decides every step"
    }
    fn extension_points(&self) -> Vec<ExtensionPoint> {
        Vec::new()
    }
    fn decisions(&self) -> Arc<dyn LocalWorkPolicy> {
        Arc::new(NoLocalPolicy)
    }
    fn sources(&self) -> Vec<Versioned> {
        self.sources.clone()
    }
}

async fn registry(appport: AppPortSource) -> Registry {
    let appport = Arc::new(appport);
    let tasks = vec![
        task("todos.fix", true, r#"{"id":"todo_1"}"#, &appport).await,
        task(
            "todos.fix.unknown-id",
            true,
            r#"{"id":"todo_99"}"#,
            &appport,
        )
        .await,
        task(
            "todos.fix.undeclared-input",
            true,
            r#"{"todo":"todo_1"}"#,
            &appport,
        )
        .await,
        task("todos.fix.unbound", false, r#"{"id":"todo_1"}"#, &appport).await,
    ];
    let mut r = Registry::new()
        .with_capability_fixture(Arc::new(ProjectFixture))
        .with_capability_source(appport.clone())
        .with_strategy(Arc::new(ModelDriven {
            id: "appport.without-source",
            sources: Vec::new(),
        }))
        .with_strategy(Arc::new(ModelDriven {
            id: "appport.with-source",
            sources: vec![Versioned {
                id: "appport.todos".into(),
                version: 1,
            }],
        }))
        .with_criterion(Arc::new(chip_experiments::criteria::ExpectedWritesObserved))
        .with_criterion(Arc::new(Succeeded::new(COMPLETE)))
        .with_criterion(Arc::new(EvidenceCorrelated::new(appport)));
    for t in tasks {
        r = r.with_task(t);
    }
    r
}

fn definition(mode: Mode, tasks: &[&str]) -> ExperimentDefinition {
    ExperimentDefinition {
        id: "appport.todos-link-fix".into(),
        version: 1,
        description: "the link fix and its todo, with and without the AppPort Todos source".into(),
        mode,
        baseline: Versioned {
            id: "appport.without-source".into(),
            version: 1,
        },
        candidate: Some(Versioned {
            id: "appport.with-source".into(),
            version: 1,
        }),
        tasks: tasks
            .iter()
            .map(|t| Versioned {
                id: (*t).into(),
                version: 1,
            })
            .collect(),
        capabilities: vec![
            "project.read".into(),
            "project.list".into(),
            "project.write".into(),
            LIST.into(),
            COMPLETE.into(),
        ],
        limits: RunLimits {
            max_turns: 10,
            max_executions: 8,
            deadline_ms: 30_000,
        },
        criteria: vec![
            "observed.expected-writes".into(),
            "appport.succeeded".into(),
            "appport.evidence-correlated".into(),
        ],
        model: ModelSpec::Recorded,
        evidence: vec![
            EvidenceField::Outcome,
            EvidenceField::Criteria,
            EvidenceField::Decisions,
            EvidenceField::Executions,
            EvidenceField::ModelCalls,
        ],
    }
}

fn run<'a>(
    result: &'a chip_experiments::ExperimentResult,
    task: &str,
    side: &str,
) -> &'a RunRecord {
    result
        .runs
        .iter()
        .find(|r| r.task == task && r.side.name() == side)
        .unwrap()
}

fn executions(r: &RunRecord, capability: &str) -> usize {
    r.executions_by_capability
        .get(capability)
        .copied()
        .unwrap_or(0)
}

fn criterion<'a>(r: &'a RunRecord, id: &str) -> &'a CriterionResult {
    &r.criteria.iter().find(|(c, _)| c == id).unwrap().1
}

fn incomplete(r: &RunRecord) -> bool {
    !matches!(
        r.verdict,
        Verdict::Verified | Verdict::CompletedUnverified { .. }
    )
}

// ---- the contract, as the host serves it ------------------------------------------------------------

#[test]
fn the_package_contract_becomes_chip_capabilities_within_the_grant() {
    let s = source(AUTHORIZED);
    let mut ids: Vec<&str> = s
        .offered()
        .iter()
        .map(|o| o.descriptor.id.as_str())
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            "appport.todos.todo.complete",
            "appport.todos.todo.create",
            "appport.todos.todo.delete",
            "appport.todos.todo.list"
        ]
    );
    let complete = s
        .offered()
        .iter()
        .find(|o| o.descriptor.id.as_str() == COMPLETE)
        .unwrap();
    let d = &complete.descriptor;
    assert_eq!(d.version.as_deref(), Some("todo.complete@1"));
    assert_eq!(complete.scopes, ["todos.write"]);
    // Inputs and required inputs come from the contract's inputSchema.
    assert_eq!(d.inputs.len(), 1);
    assert_eq!(
        (d.inputs[0].name.as_str(), d.inputs[0].required),
        ("id", true)
    );
    // Never a provider label: the host meters every call, and this one changes state.
    assert_eq!(d.effect, Effect::Consequential);
    assert_eq!(s.effect(COMPLETE), Effect::Consequential);
    assert!(d.description.contains(&s.snapshot().package.identity));
    assert!(
        s.fixture()
            .provides()
            .iter()
            .all(|(_, c)| *c == chip_experiments::Confinement::Unconfined)
    );
}

#[test]
fn the_grant_decides_what_is_offered_and_says_why() {
    let s = source(READ_ONLY);
    let ids: Vec<&str> = s
        .offered()
        .iter()
        .map(|o| o.descriptor.id.as_str())
        .collect();
    assert_eq!(ids, [LIST]);
    assert_eq!(s.not_offered().len(), 3);
    assert!(
        s.not_offered().iter().all(|n| n
            .reason
            .contains("[\"todos.write\"], which the grant does not hold")),
        "{:?}",
        s.not_offered()
    );
}

#[test]
fn a_contract_or_document_a_client_must_not_trust_builds_no_source() {
    let e = edited(|v| v["discovery"]["protocol"] = "AppPort/host-discovery/2".into())
        .err()
        .unwrap();
    assert!(e.contains("only `AppPort/host-discovery/1`"), "{e}");
    let e = edited(|v| v["package"]["contract_fingerprint"] = "00".into())
        .err()
        .unwrap();
    assert!(e.contains("not the one the host serves"), "{e}");
    let e = edited(|v| v["package"]["identity"] = "sha256:00".into())
        .err()
        .unwrap();
    assert!(e.contains("not the one the host serves"), "{e}");
    let e = edited(|v| v["package"]["wasm_sha256"] = "00".into())
        .err()
        .unwrap();
    assert!(e.contains("the package holds 00"), "{e}");
    let e = edited(|v| v["discovery"]["applications"][0]["status"] = "stopped".into())
        .err()
        .unwrap();
    assert!(e.contains("did not list `todos` as running"), "{e}");
    let e = AppPortSource::new(Snapshot::from_json(AUTHORIZED).unwrap(), "Todos")
        .err()
        .unwrap();
    assert!(e.contains("cannot name a Chip capability"), "{e}");
}

#[test]
fn a_capability_chip_cannot_request_faithfully_is_not_offered_and_says_why() {
    let s = edited(|v| {
        for p in v["package"]["contract"]["provides"].as_array_mut().unwrap() {
            if p["name"] == "todo.complete" {
                p["inputSchema"]["properties"]["priority"] = serde_json::json!({"type": "number"});
            }
        }
    })
    .unwrap();
    let refused = s
        .not_offered()
        .iter()
        .find(|n| n.capability == "todo.complete@1")
        .unwrap();
    assert!(
        refused
            .reason
            .contains("`priority` has type Some(\"number\")"),
        "{refused:?}"
    );
}

#[tokio::test]
async fn the_source_executes_nothing_itself() {
    let s = source(AUTHORIZED);
    let backend = s.fixture().backend(std::path::Path::new("/nonexistent"));
    let err = backend
        .execute(ExecutionRequest {
            id: ExecutionId::new("x"),
            intent: COMPLETE.into(),
            inputs: BTreeMap::new(),
            idempotency_key: None,
        })
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("only replays what a host served"),
        "{err}"
    );
}

#[test]
fn the_operators_setup_calls_are_never_replayed() {
    let s = source(AUTHORIZED);
    assert!(s.call("seed-link").unwrap().operator);
    let recorded = s.recorded_executions();
    assert!(
        recorded
            .iter()
            .all(|e| !e.capability.ends_with("todo.create"))
    );
    assert_eq!(
        recorded.len(),
        4,
        "list, complete, complete-unknown, undeclared-input"
    );
}

// ---- detection --------------------------------------------------------------------------------------------

#[tokio::test]
async fn detection_needs_the_project_binding_and_the_package_the_host_served() {
    let s = source(AUTHORIZED);
    let bound = task("t", true, "{}", &s).await;
    assert!(
        matches!(s.detect(&bound), Detection::Confirmed { evidence } if evidence[0].contains("running with package sha256:984c4693"))
    );
    let unbound = task("t", false, "{}", &s).await;
    assert!(matches!(s.detect(&unbound), Detection::NotDetected { .. }));
    let mut other = bound.clone();
    other.files.retain(|(p, _)| p != BINDING_FILE);
    other.files.push((
        BINDING_FILE.into(),
        BINDING.replace("sha256:984c4693", "sha256:00000000"),
    ));
    assert!(
        matches!(s.detect(&other), Detection::Inconclusive { reason } if reason.contains("expects package sha256:00000000"))
    );
}

// ---- the run ----------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_agent_uses_the_appport_capabilities_and_the_results_trace_to_the_host() {
    let resolved = registry(source(AUTHORIZED))
        .await
        .resolve(&definition(Mode::Replay, &["todos.fix"]))
        .unwrap();
    let result = Runner::new(work_dir("fix")).run(&resolved).await;
    let candidate = run(&result, "todos.fix", "candidate");
    let baseline = run(&result, "todos.fix", "baseline");

    assert_eq!(candidate.verdict, Verdict::Verified, "{candidate:#?}");
    assert_eq!(
        (executions(candidate, LIST), executions(candidate, COMPLETE)),
        (1, 1),
        "{candidate:#?}"
    );
    assert!(candidate.sources[0].active, "{:?}", candidate.sources);
    assert_eq!(candidate.sources[0].source, "appport.todos@1");
    for c in [
        "observed.expected-writes",
        "appport.succeeded",
        "appport.evidence-correlated",
    ] {
        assert_eq!(*criterion(candidate, c), CriterionResult::Met, "{c}");
    }
    assert!(candidate.audit_clean, "{:?}", candidate.audit_details);

    // The baseline wrote the same fix, but the Todos capabilities were never declared to it: its
    // requests ran nothing, the todo is not complete, and the completion was refused.
    assert_eq!(
        (executions(baseline, LIST), executions(baseline, COMPLETE)),
        (0, 0),
        "{baseline:#?}"
    );
    assert!(matches!(
        criterion(baseline, "appport.succeeded"),
        CriterionResult::NotMet(_)
    ));
    assert!(incomplete(baseline), "{baseline:#?}");
}

#[tokio::test]
async fn a_failed_invocation_is_a_failed_observation_and_no_completion() {
    let s = source(AUTHORIZED);
    assert_eq!(s.call("complete-unknown").unwrap().http_status, 404);
    let resolved = registry(s)
        .await
        .resolve(&definition(Mode::Replay, &["todos.fix.unknown-id"]))
        .unwrap();
    let result = Runner::new(work_dir("unknown-id")).run(&resolved).await;
    let r = run(&result, "todos.fix.unknown-id", "candidate");

    // The host refused `todo_99` (404). Chip recorded a failed observation, refused the model's
    // completion claims that followed, and the work ended incomplete.
    assert_eq!(executions(r, COMPLETE), 1, "{r:#?}");
    assert!(r.failed_observations >= 1, "{r:#?}");
    assert!(incomplete(r), "{r:#?}");
    assert!(matches!(
        criterion(r, "appport.succeeded"),
        CriterionResult::NotMet(_)
    ));
    // The failure agrees with the host: it completed no such call.
    assert_eq!(
        *criterion(r, "appport.evidence-correlated"),
        CriterionResult::Met
    );
}

#[tokio::test]
async fn chip_enforces_the_contract_the_published_host_does_not() {
    let s = source(AUTHORIZED);
    // The published host passed this input to the module unvalidated; the module answered 404.
    let call = s.call("undeclared-input").unwrap();
    assert_eq!(
        (call.http_status, call.body["error"].as_str()),
        (404, Some("todo undefined not found"))
    );
    let resolved = registry(s)
        .await
        .resolve(&definition(Mode::Replay, &["todos.fix.undeclared-input"]))
        .unwrap();
    let result = Runner::new(work_dir("undeclared")).run(&resolved).await;
    let r = run(&result, "todos.fix.undeclared-input", "candidate");
    // Chip refused it from the contract's inputSchema before anything was replayed.
    assert_eq!(executions(r, COMPLETE), 0, "{r:#?}");
    assert!(incomplete(r), "{r:#?}");
}

#[tokio::test]
async fn without_the_project_binding_the_source_stays_inactive() {
    let resolved = registry(source(AUTHORIZED))
        .await
        .resolve(&definition(Mode::Replay, &["todos.fix.unbound"]))
        .unwrap();
    let result = Runner::new(work_dir("unbound")).run(&resolved).await;
    let r = run(&result, "todos.fix.unbound", "candidate");
    assert!(matches!(
        r.sources[0].detection,
        Detection::NotDetected { .. }
    ));
    assert!(!r.sources[0].active);
    assert_eq!(executions(r, LIST) + executions(r, COMPLETE), 0, "{r:#?}");
}

// ---- authority --------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_grant_without_the_scope_offers_less_and_the_host_refuses_the_rest() {
    let s = source(READ_ONLY);
    // The host refused the call itself, before execution, and recorded nothing for it.
    let call = s.call("complete").unwrap();
    assert_eq!(call.http_status, 403);
    assert_eq!(call.body["missingScopes"][0], "todos.write");
    assert!(
        s.snapshot()
            .host_state
            .records
            .iter()
            .all(|r| r["capability"]["name"] != "todo.complete")
    );
    // Chip is never offered it, so a definition that needs it cannot be built.
    assert!(s.recorded_executions().iter().all(|e| e.capability == LIST));
    let err = registry(s)
        .await
        .resolve(&definition(Mode::Replay, &["todos.fix"]))
        .err()
        .unwrap();
    assert!(
        matches!(err, DefinitionError::UnknownCapability(ref m) if m.contains(COMPLETE)),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_live_mode_cannot_offer_the_host() {
    // Isolated execution offers only what its executor confines; the host is another process.
    let mut d = definition(Mode::Isolated, &["todos.fix"]);
    d.model = ModelSpec::None;
    d.criteria = vec!["observed.expected-writes".into()];
    let err = registry(source(AUTHORIZED))
        .await
        .resolve(&d)
        .err()
        .unwrap();
    assert!(
        matches!(err, DefinitionError::IsolationUnavailable(ref m) if m.contains("unconfined")),
        "{err:?}"
    );
}

#[test]
fn evidence_that_does_not_match_the_host_records_is_not_met() {
    let s = Arc::new(source(AUTHORIZED));
    let check = EvidenceCorrelated::new(s.clone());
    let head = |label: &str| -> serde_json::Value {
        let e = s
            .recorded_executions()
            .into_iter()
            .find(|e| e.capability == COMPLETE && e.inputs == s.call(label).unwrap().input)
            .unwrap();
        serde_json::from_str(e.output.lines().next().unwrap()).unwrap()
    };
    let success = head("complete");
    assert!(check.check(&success, true).is_ok());
    assert!(
        check
            .check(&success, false)
            .unwrap_err()
            .contains("the host completed this call")
    );
    let mut forged = success.clone();
    forged["execution_unit"] = "exec_0000000000000000".into();
    assert!(
        check
            .check(&forged, true)
            .unwrap_err()
            .contains("no invocation record")
    );
    let mut other = success.clone();
    other["artifact_hash"] = "00".into();
    assert!(
        check
            .check(&other, true)
            .unwrap_err()
            .contains("artifact_hash")
    );
    let mut swapped = success.clone();
    swapped["request"] = serde_json::json!({"id": "todo_2"});
    assert!(
        check
            .check(&swapped, true)
            .unwrap_err()
            .contains("not this completed call")
    );
    // The published host names no unit on a failure: checked by absence, and refused as a success.
    let failure = head("complete-unknown");
    assert!(failure["execution_unit"].is_null());
    assert!(check.check(&failure, false).is_ok());
    assert!(
        check
            .check(&failure, true)
            .unwrap_err()
            .contains("names no execution unit")
    );
}

// ---- the evidence trail ----------------------------------------------------------------------------------

/// Every link from the host's discovery document to its durable records, for the verified run.
/// With `CHIP_APPPORT_TRAIL=<file>` the trail and the experiment report are also written there
/// (that is how the committed trail was produced).
#[tokio::test]
async fn the_evidence_trail_links_the_run_to_the_host_records() {
    let s = source(AUTHORIZED);
    let snapshot = s.snapshot().clone();
    let offered = s.offered().to_vec();
    let recorded = s.recorded_executions();
    let resolved = registry(s)
        .await
        .resolve(&definition(Mode::Replay, &["todos.fix"]))
        .unwrap();
    let result = Runner::new(work_dir("trail")).run(&resolved).await;
    let candidate = run(&result, "todos.fix", "candidate");
    assert_eq!(candidate.verdict, Verdict::Verified);

    let application = snapshot.discovery["applications"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["selector"] == "todos")
        .unwrap()
        .clone();
    let mut links = Vec::new();
    for capability in [LIST, COMPLETE] {
        let o = offered
            .iter()
            .find(|o| o.descriptor.id.as_str() == capability)
            .unwrap();
        // 1. discovery advertises the capability and its scopes; the contract is the one it serves.
        assert_eq!(
            application["authorization"][&o.capability],
            serde_json::json!(o.scopes)
        );
        assert_eq!(
            application["contractFingerprint"].as_str(),
            Some(snapshot.package.contract_fingerprint.as_str())
        );
        // 2. the model's decision named it and Chip executed it once.
        assert!(
            candidate
                .decisions
                .iter()
                .any(|d| d == &format!("model: request {capability}"))
        );
        let executed = candidate
            .evidence
            .iter()
            .find(|e| e.capability == capability)
            .unwrap();
        assert_eq!(executed.status, "success");
        // 3. the observation is, byte for byte, the host's recorded answer.
        let answer = recorded
            .iter()
            .find(|e| e.succeeded && executed.output_sha256 == sha256(&e.output))
            .expect("the observation is the recorded host answer");
        let head: serde_json::Value =
            serde_json::from_str(answer.output.lines().next().unwrap()).unwrap();
        // 4. its execution unit is the host's completed invocation and success usage record.
        let unit = head["execution_unit"].as_str().unwrap();
        let record = snapshot
            .host_state
            .records
            .iter()
            .find(|r| r["executionUnitId"] == unit)
            .unwrap()
            .clone();
        let usage = snapshot
            .host_state
            .usage
            .iter()
            .find(|u| u["executionId"] == unit)
            .unwrap()
            .clone();
        assert_eq!(
            (record["status"].as_str(), usage["status"].as_str()),
            (Some("completed"), Some("success"))
        );
        assert_eq!(record["artifactHash"], application["artifactHash"]);
        // 5. the host's stored output is the result the observation carried.
        let carried: serde_json::Value =
            serde_json::from_str(answer.output.split("\n--- result ---\n").nth(1).unwrap())
                .unwrap();
        assert_eq!(record["output"], carried);
        links.push(serde_json::json!({
            "capability": capability,
            "descriptor": {
                "version": o.descriptor.version,
                "inputs": o.descriptor.inputs.iter().map(|i| serde_json::json!({"name": i.name, "required": i.required})).collect::<Vec<_>>(),
                "effect": o.descriptor.effect.name(),
                "confinement": "unconfined",
                "scopes": o.scopes,
            },
            "execution": { "execution_id": executed.execution_id, "status": executed.status, "output_sha256": executed.output_sha256 },
            "observation_head": head,
            "host_invocation_record": record,
            "host_usage_record": usage,
        }));
    }
    // The id the model completed is the one the host's list returned.
    assert_eq!(
        links[0]["host_invocation_record"]["output"]["todos"][0]["id"],
        links[1]["observation_head"]["request"]["id"]
    );

    if let Ok(path) = std::env::var("CHIP_APPPORT_TRAIL") {
        let trail = serde_json::json!({
            "schema": "chip.appport-evidence-trail.v1",
            "note": "replay of a recorded AppPort host; the model replies are recorded, not live",
            "appport_cli": snapshot.appport_cli,
            "discovery_record": application,
            "decisions": candidate.decisions,
            "links": links,
            "verdict": format!("{:?}", candidate.verdict),
            "criteria": candidate.criteria.iter().map(|(c, r)| (c.clone(), r.name())).collect::<BTreeMap<_, _>>(),
            "report": report_json(&resolved, &result, &Provenance { host: "chip-appport-source tests".into(), source_revision: None }, &Redactor::new(Vec::new())),
        });
        std::fs::write(path, serde_json::to_string_pretty(&trail).unwrap() + "\n").unwrap();
    }
}
