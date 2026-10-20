//! The MCP source end to end, in replay: an existing campaign bridge (rkendel1/vikunja
//! `appport-bridge/`, unchanged) served over MCP by its own pinned `@appport/mcp`, over a real
//! Vikunja (`fixtures/mcp/vikunja.*.mcp.json`, recorded by `recorder/record-mcp.mjs`). Chip builds
//! every descriptor from `tools/list`, the model's requests are validated and routed by Chip's
//! ordinary work loop, answered from the recording, and judged against Vikunja's own stored rows.
//!
//! The model replies in `fixtures/mcp/model-run.*.json` are a real model's (recorded by
//! `examples/mcp_work.rs` through Chip's loop over the same recording). Where a path needs a reply
//! the real model did not give, the replies are scripted and the test says so.

use std::sync::Arc;

use chip_appport_source::mcp::{
    AnswerStates, ApplicationAgrees, McpSnapshot, McpSource, Outcome, Rule, classify,
};
use chip_appport_source::{BINDING_FILE, Succeeded};
use chip_core::{Effect, NoLocalPolicy};
use chip_experiments::{
    CapabilitySource, CriterionResult, Detection, EvidenceField, ExperimentDefinition,
    ExtensionPoint, Mode, ModelSpec, Recording, Registry, RunLimits, RunRecord, Runner, Strategy,
    TaskFixture, Verdict, Versioned,
};
use serde_json::Value;

const READ: &str = include_str!("../fixtures/mcp/vikunja.read.mcp.json");
const WRITE: &str = include_str!("../fixtures/mcp/vikunja.write.mcp.json");
const MISMATCH: &str = include_str!("../fixtures/mcp/vikunja.write-token-read-only.mcp.json");
const BINDING: &str = include_str!("../fixtures/mcp/project/appport.binding.json");
const README: &str = include_str!("../fixtures/mcp/project/README.md");
const RUN_READ: &str = include_str!("../fixtures/mcp/model-run.read.json");
const RUN_REFUSED: &str = include_str!("../fixtures/mcp/model-run.read-refused.json");
const RUN_REFUSED_2: &str = include_str!("../fixtures/mcp/model-run.read-refused-2.json");
const RUN_WRITE_UNBACKED: &str = include_str!("../fixtures/mcp/model-run.write-unbacked.json");
const RUN_MISSING: &str = include_str!("../fixtures/mcp/model-run.read-missing.json");
// Qwen3-Coder-30B never requested the write in three attempts (each refused by Chip; see the
// record); gpt-4.1 did, on the first.
const RUN_WRITE: &str = include_str!("../fixtures/mcp/model-run.write.gpt-4.1.json");
const RUN_READ_GPT: &str = include_str!("../fixtures/mcp/model-run.read.gpt-4.1.json");

const GET: &str = "appport.vikunja.vikunja_tasks_get";
const LIST: &str = "appport.vikunja.vikunja_projects_list";
const SETDONE: &str = "appport.vikunja.vikunja_tasks_setdone";

fn source(snapshot: &str) -> McpSource {
    McpSource::new(McpSnapshot::from_json(snapshot).unwrap(), "vikunja").unwrap()
}

fn edited(snapshot: &str, edit: impl FnOnce(&mut Value)) -> McpSource {
    let mut v: Value = serde_json::from_str(snapshot).unwrap();
    edit(&mut v);
    McpSource::new(McpSnapshot::from_json(&v.to_string()).unwrap(), "vikunja").unwrap()
}

/// Applies `edit` to every stored state in the snapshot: each call's and the phase's.
fn tamper(v: &mut Value, edit: impl Fn(&mut Value)) {
    edit(&mut v["oracle"]);
    for call in v["calls"].as_array_mut().unwrap() {
        edit(&mut call["oracle"]);
    }
}

/// A recorded real-model run: its goal and replies.
fn model_run(text: &str) -> (String, Vec<String>) {
    let v: Value = serde_json::from_str(text).unwrap();
    assert!(
        ["alibaba/qwen3-coder-30b-a3b", "openai/gpt-4.1"].contains(&v["model"].as_str().unwrap()),
        "a real gateway model"
    );
    let replies = v["replies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap().to_string())
        .collect();
    (v["goal"].as_str().unwrap().to_string(), replies)
}

fn work_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("chip-appport-mcp-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// What Vikunja stores, read by the recorder: task `id`'s done flag and its project's title.
fn stored_task(s: &McpSource, id: i64) -> (bool, String) {
    let oracle = &s.snapshot().oracle;
    let task = oracle["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .unwrap();
    let project = oracle["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == task["project_id"])
        .unwrap();
    (
        task["done"].as_bool().unwrap(),
        project["title"].as_str().unwrap().to_string(),
    )
}

/// How each capability relates to Vikunja's tables: the host's statement of what to check, not a
/// tool definition (Chip's descriptors come from `tools/list` alone).
fn rules() -> Vec<(&'static str, Rule)> {
    vec![
        (
            "vikunja.tasks.get",
            Rule::Row {
                collection: "tasks",
                key_input: "id",
                fields: vec![
                    ("/title", "title"),
                    ("/done", "done"),
                    ("/projectId", "project_id"),
                    ("/createdBy", "created_by"),
                ],
                owner: "created_by",
            },
        ),
        (
            "vikunja.tasks.setdone",
            Rule::Row {
                collection: "tasks",
                key_input: "id",
                fields: vec![("/title", "title"), ("/done", "done")],
                owner: "created_by",
            },
        ),
        (
            "vikunja.projects.list",
            Rule::Rows {
                collection: "projects",
                array: "/projects",
                key_field: "id",
                fields: vec![("/title", "title"), ("/owner", "owner")],
                owner: "owner",
            },
        ),
    ]
}

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
    fn decisions(&self) -> Arc<dyn chip_core::LocalWorkPolicy> {
        Arc::new(NoLocalPolicy)
    }
    fn sources(&self) -> Vec<Versioned> {
        self.sources.clone()
    }
}

struct Case {
    id: &'static str,
    goal: String,
    replies: Vec<String>,
    require: &'static str,
    answer: Vec<(&'static str, String)>,
}

async fn experiment(source: McpSource, case: Case) -> (RunRecord, RunRecord) {
    let source = Arc::new(source);
    let task = TaskFixture {
        id: case.id.into(),
        version: 1,
        goal: case.goal,
        files: vec![
            ("README.md".into(), README.into()),
            (BINDING_FILE.into(), BINDING.into()),
        ],
        expected_files: Vec::new(),
        model_replies: case.replies,
        recording: Some(Recording {
            schema: chip_experiments::plugin::RECORDING_SCHEMA.into(),
            entries: source.recorded_executions(),
        }),
    };
    let registry = Registry::new()
        .with_capability_source(source.clone())
        .with_strategy(Arc::new(ModelDriven {
            id: "mcp.without-source",
            sources: Vec::new(),
        }))
        .with_strategy(Arc::new(ModelDriven {
            id: "mcp.with-source",
            sources: vec![Versioned {
                id: "appport.vikunja.mcp".into(),
                version: 1,
            }],
        }))
        .with_criterion(Arc::new(Succeeded::new(case.require)))
        .with_criterion(Arc::new(ApplicationAgrees::new(
            source.clone(),
            "bob",
            rules(),
        )))
        .with_criterion(Arc::new(AnswerStates::new(case.answer)))
        .with_task(task);
    let definition = ExperimentDefinition {
        id: "appport.vikunja-mcp".into(),
        version: 1,
        description: "a Vikunja question or change, with and without the campaign bridge over MCP"
            .into(),
        mode: Mode::Replay,
        baseline: Versioned {
            id: "mcp.without-source".into(),
            version: 1,
        },
        candidate: Some(Versioned {
            id: "mcp.with-source".into(),
            version: 1,
        }),
        tasks: vec![Versioned {
            id: case.id.into(),
            version: 1,
        }],
        // The operator's ceiling is what the source offers under the grant: nothing is named here.
        capabilities: source.ids(),
        limits: RunLimits {
            max_turns: 8,
            max_executions: 6,
            deadline_ms: 30_000,
        },
        criteria: vec![
            "appport.succeeded".into(),
            "appport.mcp.application-agrees".into(),
            "appport.mcp.answer-states".into(),
        ],
        model: ModelSpec::Recorded,
        evidence: vec![
            EvidenceField::Outcome,
            EvidenceField::Criteria,
            EvidenceField::Decisions,
            EvidenceField::Executions,
            EvidenceField::ModelCalls,
        ],
    };
    let resolved = registry.resolve(&definition).unwrap();
    let result = Runner::new(work_dir(case.id)).run(&resolved).await;
    let side = |name: &str| {
        result
            .runs
            .iter()
            .find(|r| r.side.name() == name)
            .unwrap()
            .clone()
    };
    (side("baseline"), side("candidate"))
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

fn not_met(r: &RunRecord, id: &str) -> bool {
    matches!(criterion(r, id), CriterionResult::NotMet(_))
}

fn incomplete(r: &RunRecord) -> bool {
    !matches!(
        r.verdict,
        Verdict::Verified | Verdict::CompletedUnverified { .. }
    )
}

// ---- discovery: descriptors from tools/list, within the grant -------------------------------------------------

#[test]
fn every_descriptor_comes_from_tools_list_within_the_grant() {
    let s = source(READ);
    assert_eq!(s.snapshot().tools.len(), 3, "MCP lists all three tools");
    let ids: Vec<&str> = s
        .offered()
        .iter()
        .map(|o| o.descriptor.id.as_str())
        .collect();
    assert_eq!(ids, [LIST, GET]);
    // The write tool is listed by MCP but outside the read grant: not offered, and why.
    assert_eq!(s.not_offered().len(), 1);
    assert_eq!(s.not_offered()[0].capability, "vikunja_tasks_setdone");
    assert!(
        s.not_offered()[0]
            .reason
            .contains("[\"vikunja.tasks.write\"], which the grant does not hold"),
        "{:?}",
        s.not_offered()
    );

    let get = s
        .offered()
        .iter()
        .find(|o| o.descriptor.id.as_str() == GET)
        .unwrap();
    assert_eq!(get.capability, "vikunja.tasks.get@1");
    assert_eq!(get.scopes, ["vikunja.read"]);
    assert_eq!(get.descriptor.inputs.len(), 1);
    let id = &get.descriptor.inputs[0];
    assert_eq!((id.name.as_str(), id.required), ("id", true));
    assert!(id.description.contains("JSON integer"), "{id:?}");
    // The provider says "observation"; Chip reports it and still treats the tool as consequential
    // and unconfined, so it is offered in replay only.
    assert_eq!(get.declared_effect.as_deref(), Some("observation"));
    assert_eq!(get.descriptor.effect, Effect::Consequential);
    assert!(
        get.descriptor
            .description
            .contains("declared effect: observation")
    );
    assert!(
        s.fixture()
            .provides()
            .iter()
            .all(|(_, c)| *c == chip_experiments::Confinement::Unconfined)
    );

    let w = source(WRITE);
    let ids: Vec<&str> = w
        .offered()
        .iter()
        .map(|o| o.descriptor.id.as_str())
        .collect();
    assert_eq!(ids, [LIST, GET, SETDONE]);
    let setdone = &w.offered()[2];
    assert_eq!(setdone.integration["verify"][0]["id"], "task-done-stored");
}

#[test]
fn a_tool_whose_authority_cannot_be_read_is_not_offered() {
    let s = edited(READ, |v| {
        v["tools"][1]["_meta"] = Value::Null;
        v["tools"][0]["_meta"]["dev.appport/capability"]["authorization"] = Value::Null;
    });
    assert!(s.offered().is_empty());
    let reasons: Vec<&str> = s.not_offered().iter().map(|n| n.reason.as_str()).collect();
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("declares no authorization")),
        "{reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("carries no dev.appport/capability")),
        "{reasons:?}"
    );
}

#[test]
fn detection_needs_the_binding_and_the_server_that_answered() {
    let s = source(READ);
    let task = |files: Vec<(String, String)>| TaskFixture {
        id: "t".into(),
        version: 1,
        goal: String::new(),
        files,
        expected_files: Vec::new(),
        model_replies: Vec::new(),
        recording: None,
    };
    assert!(matches!(
        s.detect(&task(Vec::new())),
        Detection::NotDetected { .. }
    ));
    assert!(matches!(
        s.detect(&task(vec![(BINDING_FILE.into(), BINDING.into())])),
        Detection::Confirmed { .. }
    ));
    let other = BINDING.replace("appport-bridge-1", "appport-bridge-2");
    assert!(matches!(
        s.detect(&task(vec![(BINDING_FILE.into(), other)])),
        Detection::Inconclusive { .. }
    ));
}

// ---- outcomes: success, the application's refusals, its errors --------------------------------------------------

#[test]
fn mcp_answers_are_classified_and_the_operators_session_is_never_chosen_by_chip() {
    let s = source(READ);
    let outcome = |label: &str| {
        let (o, e) = classify(&s.call(label).unwrap().response);
        (o, e["code"].as_str().unwrap_or_default().to_string())
    };
    assert_eq!(outcome("get-own-open"), (Outcome::Success, String::new()));
    // Vikunja refused bob's token for carol's task: an authorization refusal, not a failure.
    assert_eq!(
        outcome("get-other-users-task"),
        (Outcome::Refused, "FORBIDDEN".into())
    );
    assert_eq!(
        outcome("get-missing"),
        (Outcome::Failed, "NOT_FOUND".into())
    );
    // The bridge's own contract validated the input; the text id never reached Vikunja.
    assert_eq!(
        outcome("get-id-as-text"),
        (Outcome::Failed, "INVALID_INPUT".into())
    );
    assert_eq!(
        outcome("get-no-id"),
        (Outcome::Failed, "INVALID_INPUT".into())
    );
    // AppPort refused the write outside the grant; Chip never offers it, so never replays it.
    assert_eq!(
        outcome("setdone-outside-grant"),
        (Outcome::Refused, "FORBIDDEN".into())
    );
    assert!(
        s.call("setdone-outside-grant").unwrap().response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Missing permission: vikunja.tasks.write")
    );
    // A wrong MCP bearer was refused by the server, and is not something Chip can request.
    assert_eq!(
        outcome("get-wrong-mcp-bearer"),
        (Outcome::Refused, "UNAUTHORIZED".into())
    );
    let replayed = s.recorded_executions();
    assert_eq!(
        replayed.len(),
        10,
        "every call of an offered tool, with the operator's session"
    );
    assert!(replayed.iter().all(|e| !e.capability.ends_with("setdone")));

    // The grant and the token disagree: AppPort grants the write, Vikunja's token cannot update.
    let m = source(MISMATCH);
    assert!(
        m.offered()
            .iter()
            .any(|o| o.descriptor.id.as_str() == SETDONE)
    );
    let (o, e) = classify(&m.call("setdone-token-cannot-update").unwrap().response);
    assert_eq!(
        (o, e["code"].as_str()),
        (Outcome::Refused, Some("UNAUTHORIZED"))
    );
}

#[test]
fn every_recorded_answer_agrees_with_what_vikunja_stores() {
    for snapshot in [READ, WRITE, MISMATCH] {
        let s = Arc::new(source(snapshot));
        let check = ApplicationAgrees::new(s.clone(), "bob", rules());
        for e in s.recorded_executions() {
            let (head, result) = e.output.split_once("\n--- result ---\n").unwrap();
            let head: Value = serde_json::from_str(head).unwrap();
            if head["outcome"] == "refused" && head["error"]["code"] == "UNAUTHORIZED" {
                continue; // the token's scope, not a row: nothing stored to agree with
            }
            check
                .check(&head, result)
                .unwrap_or_else(|why| panic!("{}: {why}", s.snapshot().label));
        }
    }
}

#[test]
fn an_answer_or_refusal_the_application_does_not_back_is_caught() {
    // A result that differs from the stored row.
    let s = Arc::new(edited(READ, |v| {
        tamper(v, |o| o["tasks"][0]["title"] = "Mow the lawn".into())
    }));
    let e = &s.recorded_executions()[0];
    let (head, result) = e.output.split_once("\n--- result ---\n").unwrap();
    let why = ApplicationAgrees::new(s.clone(), "bob", rules())
        .check(&serde_json::from_str(head).unwrap(), result)
        .unwrap_err();
    assert!(
        why.contains("the application stored \"Mow the lawn\""),
        "{why}"
    );

    // A refusal for a row the caller owns is not an authorization refusal it can be.
    let s = Arc::new(edited(READ, |v| {
        tamper(v, |o| o["tasks"][3]["created_by"] = "bob".into())
    }));
    let refused = s
        .recorded_executions()
        .into_iter()
        .find(|e| !e.succeeded && e.output.contains("FORBIDDEN"))
        .unwrap();
    let (head, result) = refused.output.split_once("\n--- result ---\n").unwrap();
    let why = ApplicationAgrees::new(s.clone(), "bob", rules())
        .check(&serde_json::from_str(head).unwrap(), result)
        .unwrap_err();
    assert!(why.contains("which bob owns"), "{why}");

    // The contract's declared verification evaluated on stored state: done must be stored.
    let s = Arc::new(edited(WRITE, |v| {
        tamper(v, |o| o["tasks"][0]["done"] = false.into())
    }));
    let done = s
        .recorded_executions()
        .into_iter()
        .find(|e| e.capability == SETDONE && e.succeeded)
        .unwrap();
    let (head, result) = done.output.split_once("\n--- result ---\n").unwrap();
    let why = ApplicationAgrees::new(s.clone(), "bob", rules())
        .check(&serde_json::from_str(head).unwrap(), result)
        .unwrap_err();
    assert!(
        why.contains("result /done is true; the application stored false"),
        "{why}"
    );
}

// ---- the agent: a real model's run, replayed through chip-experiments -----------------------------------------

#[tokio::test]
async fn a_real_model_answers_from_vikunja_through_mcp_and_vikunja_backs_every_claim() {
    let s = source(READ);
    let (done, project) = stored_task(&s, 1);
    assert_eq!((done, project.as_str()), (false, "Garden"));
    let (goal, replies) = model_run(RUN_READ);
    let (baseline, candidate) = experiment(
        s,
        Case {
            id: "vikunja.read",
            goal,
            replies,
            require: GET,
            answer: vec![("done", done.to_string()), ("project", project)],
        },
    )
    .await;

    assert_eq!(candidate.verdict, Verdict::Verified, "{candidate:#?}");
    assert_eq!(
        (executions(&candidate, LIST), executions(&candidate, GET)),
        (1, 1)
    );
    for c in [
        "appport.succeeded",
        "appport.mcp.application-agrees",
        "appport.mcp.answer-states",
    ] {
        assert_eq!(*criterion(&candidate, c), CriterionResult::Met, "{c}");
    }
    assert!(candidate.sources[0].active, "{:?}", candidate.sources);
    assert!(candidate.audit_clean, "{:?}", candidate.audit_details);

    // The same replies without the source: nothing was declared, nothing ran, nothing completed.
    assert_eq!(
        (executions(&baseline, LIST), executions(&baseline, GET)),
        (0, 0)
    );
    assert!(incomplete(&baseline), "{baseline:#?}");
}

#[tokio::test]
async fn a_real_model_recovers_from_the_contracts_refusal() {
    // gpt-4.1 first sent the id as text; the bridge's contract refused it (INVALID_INPUT, Vikunja
    // never called), Chip recorded the failure, and the model asked again with an integer.
    let s = source(READ);
    let (done, project) = stored_task(&s, 1);
    let (goal, replies) = model_run(RUN_READ_GPT);
    let (_, candidate) = experiment(
        s,
        Case {
            id: "vikunja.read.gpt",
            goal,
            replies,
            require: GET,
            answer: vec![("done", done.to_string()), ("project", project)],
        },
    )
    .await;
    assert_eq!(candidate.verdict, Verdict::Verified, "{candidate:#?}");
    assert_eq!(
        (executions(&candidate, GET), executions(&candidate, LIST)),
        (2, 1)
    );
    assert_eq!(candidate.failed_observations, 1, "{candidate:#?}");
    for c in [
        "appport.succeeded",
        "appport.mcp.application-agrees",
        "appport.mcp.answer-states",
    ] {
        assert_eq!(*criterion(&candidate, c), CriterionResult::Met, "{c}");
    }
}

#[tokio::test]
async fn a_real_models_unbacked_answer_is_refused() {
    // Asked about carol's task, the real model answered without calling anything (and invented a
    // project). Chip refused the completion: no observation supports it.
    let s = source(READ);
    let (goal, replies) = model_run(RUN_REFUSED);
    assert_eq!(replies.len(), 1);
    let (_, candidate) = experiment(
        s,
        Case {
            id: "vikunja.read.unbacked",
            goal,
            replies,
            require: GET,
            answer: vec![("done", "false".into()), ("project", "Studio".into())],
        },
    )
    .await;
    assert_eq!(executions(&candidate, GET), 0);
    assert!(incomplete(&candidate), "{candidate:#?}");
    assert!(not_met(&candidate, "appport.succeeded"));
    assert!(not_met(&candidate, "appport.mcp.answer-states"));
}

#[tokio::test]
async fn a_real_model_meets_the_applications_refusal_and_does_not_claim_an_answer() {
    // Asked again, differently worded, the real model requested carol's task. Vikunja refused bob's
    // token (FORBIDDEN, category forbidden), and the model escalated instead of answering.
    let (goal, replies) = model_run(RUN_REFUSED_2);
    let (_, candidate) = experiment(
        source(READ),
        Case {
            id: "vikunja.read.refused",
            goal,
            replies,
            require: GET,
            answer: vec![("done", "false".into()), ("project", "Studio".into())],
        },
    )
    .await;
    assert_eq!(executions(&candidate, GET), 1);
    assert!(candidate.failed_observations >= 1, "{candidate:#?}");
    assert!(incomplete(&candidate), "{candidate:#?}");
    assert!(not_met(&candidate, "appport.succeeded"));
    assert!(not_met(&candidate, "appport.mcp.answer-states"));
    // The refusal agrees with Vikunja: task 4 is stored and belongs to carol.
    assert_eq!(
        *criterion(&candidate, "appport.mcp.application-agrees"),
        CriterionResult::Met
    );
}

#[tokio::test]
async fn a_real_model_meets_an_application_error_and_blocks() {
    // Vikunja has no task 999999: NOT_FOUND, a failed observation; the real model blocked.
    let (goal, replies) = model_run(RUN_MISSING);
    let (_, candidate) = experiment(
        source(READ),
        Case {
            id: "vikunja.read.missing",
            goal,
            replies,
            require: GET,
            answer: vec![("done", "false".into())],
        },
    )
    .await;
    assert_eq!(executions(&candidate, GET), 1);
    assert!(candidate.failed_observations >= 1, "{candidate:#?}");
    assert!(incomplete(&candidate), "{candidate:#?}");
    assert!(not_met(&candidate, "appport.succeeded"));
    // NOT_FOUND agrees with Vikunja: no task 999999 is stored.
    assert_eq!(
        *criterion(&candidate, "appport.mcp.application-agrees"),
        CriterionResult::Met
    );
}

#[tokio::test]
async fn input_the_contract_refuses_is_a_failed_observation_and_no_completion() {
    // Scripted replies (no real model sent the id as text): the bridge's own contract refuses it
    // (INVALID_INPUT) before Vikunja is called, and the completion claimed after it is refused.
    let replies = vec![
        format!(
            r#"{{"decision":"request_capability","capability":"{GET}","inputs":{{"id":"1"}}}}"#
        ),
        r#"{"decision":"complete","summary":"done=false; project=Garden"}"#.to_string(),
    ];
    let (_, candidate) = experiment(
        source(READ),
        Case {
            id: "vikunja.read.text-id",
            goal: "Is task 1 done? End with done=<true or false>; project=<title>".into(),
            replies,
            require: GET,
            answer: vec![("done", "false".into()), ("project", "Garden".into())],
        },
    )
    .await;
    assert_eq!(executions(&candidate, GET), 1);
    assert!(incomplete(&candidate), "{candidate:#?}");
    assert!(not_met(&candidate, "appport.succeeded"));
    assert_eq!(
        *criterion(&candidate, "appport.mcp.application-agrees"),
        CriterionResult::Met
    );
}

#[tokio::test]
async fn a_real_model_makes_the_scoped_write_and_vikunja_stores_it() {
    let s = source(WRITE);
    let (done, _) = stored_task(&s, 1);
    assert!(done, "the recorder's oracle, read after the write");
    let (goal, replies) = model_run(RUN_WRITE);
    let (baseline, candidate) = experiment(
        s,
        Case {
            id: "vikunja.write",
            goal,
            replies,
            require: SETDONE,
            answer: vec![("done", "true".into())],
        },
    )
    .await;
    assert_eq!(candidate.verdict, Verdict::Verified, "{candidate:#?}");
    // The model read the task (stored then as not done), then changed it.
    assert_eq!(
        (executions(&candidate, GET), executions(&candidate, SETDONE)),
        (1, 1)
    );
    for c in [
        "appport.succeeded",
        "appport.mcp.application-agrees",
        "appport.mcp.answer-states",
    ] {
        assert_eq!(*criterion(&candidate, c), CriterionResult::Met, "{c}");
    }
    assert_eq!(executions(&baseline, SETDONE), 0);
    assert!(incomplete(&baseline), "{baseline:#?}");
}

#[tokio::test]
async fn a_real_models_unbacked_write_claim_is_refused_and_nothing_is_changed() {
    // One real-model attempt at the write answered "done=false" without calling anything.
    let (goal, replies) = model_run(RUN_WRITE_UNBACKED);
    let (_, candidate) = experiment(
        source(WRITE),
        Case {
            id: "vikunja.write.unbacked",
            goal,
            replies,
            require: SETDONE,
            answer: vec![("done", "true".into())],
        },
    )
    .await;
    assert_eq!(executions(&candidate, SETDONE), 0);
    assert!(incomplete(&candidate), "{candidate:#?}");
    assert!(not_met(&candidate, "appport.succeeded"));
}

#[test]
fn answer_pairs_are_read_strictly() {
    let p = AnswerStates::pairs("Task 1 is open. done=false; project=\"Garden\"");
    // gpt-4.1's summary: the pair in prose, then the required ending.
    let g = AnswerStates::pairs(
        "Vikunja task 1 is not done (done=false). The project it belongs to is titled 'Garden'. done=false; project=Garden",
    );
    assert_eq!(g.get("done").map(String::as_str), Some("false"));
    assert_eq!(g.get("project").map(String::as_str), Some("Garden"));
    let c = AnswerStates::pairs("done=false; project=Carol's studio");
    assert_eq!(c.get("project").map(String::as_str), Some("Carol's studio"));
    assert_eq!(p.get("done").map(String::as_str), Some("false"));
    assert_eq!(p.get("project").map(String::as_str), Some("Garden"));
    assert!(AnswerStates::pairs("the task is done").is_empty());
}
