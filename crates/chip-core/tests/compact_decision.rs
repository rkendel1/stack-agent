//! **Experimental** `CompactDecisionBoundary`: the same strict contract, asked for more briefly.
//!
//! What is pinned:
//! - the default question is byte-for-byte what it was before the compact renderer existed
//!   (`fixtures/work-decision/baseline-question.txt`, generated from the unchanged code);
//! - the compact question is written from `DECISION_SHAPES` and the offered capabilities, so it
//!   names every decision, field, capability and input the validator knows, and nothing it does not;
//! - `interpret` is the full boundary's, unchanged: the same replies are accepted and rejected with
//!   the same errors, a rejected reply executes nothing, and nothing is repaired;
//! - through the loop, a valid decision executes the same under both and an invalid one executes
//!   nothing under either.
//!
//! What this does not show: that the compact wording helps any model. That is measured, against a
//! real model, in `audit/compact-prompt/`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, CompactDecisionBoundary, DECISION_SHAPES, DecisionError,
    ExecutionError, ExecutionObserver, ExecutionRequest, ExecutionResult, Executor, FieldKind,
    MAX_TEXT_BYTES, ModelDecisionBoundary, NoLocalPolicy, WORK_DECISION_SCHEMA, WorkDecision,
    WorkDecisionBoundary, WorkEvent, WorkGoal, WorkId, WorkLimits, WorkOutcome, WorkSpec,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};

fn cap(
    id: &str,
    description: &str,
    inputs: &[(&str, bool)],
    availability: CapabilityAvailability,
) -> Capability {
    let mut d = CapabilityDescriptor::new(CapabilityId::new(id).unwrap(), id, description);
    for (name, required) in inputs {
        d.inputs.push(CapabilityInput {
            name: (*name).into(),
            description: String::new(),
            required: *required,
        });
    }
    Capability {
        descriptor: d,
        availability,
    }
}

/// Required and optional inputs, none, an unavailable capability, a multi-line description and an
/// empty one. The same set the baseline golden was generated from.
fn rich() -> Vec<Capability> {
    use CapabilityAvailability::{Available, Unavailable};
    vec![
        cap(
            "project.list",
            "List the files\n  and directories directly inside a project directory.",
            &[("path", false)],
            Available,
        ),
        cap(
            "project.read",
            "Read a UTF-8 project file by relative path.",
            &[("path", true), ("offset", false), ("length", false)],
            Available,
        ),
        cap(
            "project.git.status",
            "Observe the working-tree state.",
            &[],
            Available,
        ),
        cap(
            "project.write",
            "Creates or replaces a file.",
            &[("path", true), ("content", false)],
            Unavailable("not offered".into()),
        ),
        cap("project.observe", "", &[("scope", true)], Available),
    ]
}

fn reply(output: &str) -> ModelResponse {
    ModelResponse::new("resp", output, Usage::new(5, 5))
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/compact-decision/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

// ---- the default is unchanged ----------------------------------------------------------------------

#[test]
fn the_default_question_is_byte_identical_to_the_one_before_the_compact_renderer_existed() {
    let golden = std::fs::read_to_string(format!(
        "{}/tests/fixtures/work-decision/baseline-question.txt",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    assert_eq!(ModelDecisionBoundary.question(&rich()), golden);
}

// ---- the compact question is derived from the contract --------------------------------------------

#[test]
fn every_decision_and_field_of_the_contract_is_in_the_compact_question() {
    let q = CompactDecisionBoundary.question(&rich());
    for shape in DECISION_SHAPES {
        assert!(
            q.contains(&format!("\"decision\":\"{}\"", shape.name)),
            "{}",
            shape.name
        );
        assert!(q.contains(shape.says), "{}", shape.name);
        for f in shape.fields {
            assert!(
                q.contains(&format!("\"{}\":", f.name)),
                "{}.{}",
                shape.name,
                f.name
            );
        }
    }
    // The optional field is said to be optional, the required ones are not.
    assert!(q.contains("(optional: inputs)"));
    assert!(q.contains(&format!("{MAX_TEXT_BYTES} bytes of UTF-8")));
    assert!(q.contains(WORK_DECISION_SCHEMA));
    assert!(q.contains("rejected, not repaired"));
    assert!(q.contains("no Markdown code fence"));
    assert!(q.contains("exactly one JSON object"));
    assert!(q.contains("do not treat your own statements as evidence"));
}

#[test]
fn the_field_kinds_decide_how_a_field_is_written() {
    let q = CompactDecisionBoundary.question(&[]);
    for shape in DECISION_SHAPES {
        for f in shape.fields {
            match f.kind {
                FieldKind::Inputs => assert!(q.contains(&format!("\"{}\":{{", f.name))),
                FieldKind::Text => assert!(q.contains(&format!("\"{}\":\"<", f.name))),
            }
        }
    }
}

#[test]
fn real_capability_ids_inputs_and_requirements_are_rendered_and_nothing_else() {
    let q = CompactDecisionBoundary.question(&rich());
    assert!(q.contains("- project.list (inputs: path optional) - List the files and directories directly inside a project directory."));
    assert!(q.contains("- project.read (inputs: path required, offset optional, length optional) - Read a UTF-8 project file by relative path."));
    assert!(q.contains("- project.observe (inputs: scope required)"));
    assert!(
        !q.contains("project.write"),
        "an unavailable capability is not offered"
    );
}

#[test]
fn a_capability_with_no_inputs_is_told_to_carry_no_inputs_field_at_all() {
    let q = CompactDecisionBoundary.question(&rich());
    assert!(q.contains(
        "- project.git.status (takes no inputs: send no \"inputs\" field at all, not even {}) - Observe the working-tree state."
    ));
    // It is the only one told so.
    assert_eq!(q.matches("takes no inputs").count(), 1);
}

#[test]
fn no_capabilities_is_said_not_left_blank() {
    assert!(
        CompactDecisionBoundary
            .question(&[])
            .ends_with("Capabilities:\nnone")
    );
}

#[test]
fn the_same_inputs_give_the_same_question() {
    assert_eq!(
        CompactDecisionBoundary.question(&rich()),
        CompactDecisionBoundary.question(&rich())
    );
}

#[test]
fn a_description_is_one_line_and_bounded_as_in_the_full_question() {
    let long = format!("a   b\n{}", "x".repeat(400));
    let caps = vec![cap("c.one", &long, &[], CapabilityAvailability::Available)];
    let compact = CompactDecisionBoundary.question(&caps);
    let full = ModelDecisionBoundary.question(&caps);
    let described: String = long
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(160)
        .collect();
    assert!(compact.contains(&described) && full.contains(&described));
    // 4 characters of "a b " precede the x's, so the bounded text holds 156 of them and no more.
    assert!(compact.contains(&"x".repeat(156)) && !compact.contains(&"x".repeat(157)));
}

#[test]
fn it_is_shorter_than_the_full_question_for_a_realistic_set() {
    let (full, compact) = (
        ModelDecisionBoundary.question(&rich()).len(),
        CompactDecisionBoundary.question(&rich()).len(),
    );
    assert!(compact < full, "compact {compact} vs full {full}");
}

// ---- the validator is the full boundary's, unchanged -----------------------------------------------

#[test]
fn interpret_is_the_full_boundarys_for_every_kind_of_reply() {
    let replies = [
        r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"Cargo.toml"}}"#,
        r#"{"decision":"request_capability","capability":"project.list"}"#,
        r#"{"decision":"request_capability","capability":"project.git.status"}"#,
        r#"{"decision":"request_capability","capability":"project.git.status","inputs":{}}"#,
        r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"a"},"x":1}"#,
        r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"a"}}"#,
        r#"{"decision":"request_capability","capability":"inspect_cargo_toml"}"#,
        r#"{"decision":"complete","summary":"done"}"#,
        r#"{"decision":"complete"}"#,
        r#"{"decision":"escalate","reason":"help"}"#,
        r#"{"decision":"block","reason":"no"}"#,
        r#"{"decision":"run_shell","command":"rm -rf /"}"#,
        "```json\n{\"decision\":\"complete\",\"summary\":\"x\"}\n```",
        "Sure! {\"decision\":\"complete\",\"summary\":\"x\"}",
        r#"{"decision":"complete","summary":"x"} {"decision":"block","reason":"y"}"#,
        r#"{"decision":"complete","summary":"x"}}"#,
        "",
        "not json",
    ];
    for r in replies {
        assert_eq!(
            CompactDecisionBoundary.interpret(&reply(r), &rich()),
            ModelDecisionBoundary.interpret(&reply(r), &rich()),
            "{r:?}"
        );
    }
}

#[test]
fn the_observed_extra_closing_brace_is_rejected_not_trimmed() {
    let text = fixture("woodcock-extra-closing-brace.reply.txt");
    for boundary in [
        &CompactDecisionBoundary as &dyn WorkDecisionBoundary,
        &ModelDecisionBoundary,
    ] {
        match boundary.interpret(&reply(&text), &rich()) {
            Err(DecisionError::MalformedReply(why)) => {
                assert!(why.contains("text after the JSON object"), "{why}")
            }
            other => panic!("expected a malformed reply, got {other:?}"),
        }
    }
}

#[test]
fn an_invented_capability_is_rejected_even_though_it_is_valid_json() {
    let text = fixture("woodcock-invented-capability.reply.txt");
    assert!(serde_like_object(&text), "the fixture is well-formed JSON");
    for boundary in [
        &CompactDecisionBoundary as &dyn WorkDecisionBoundary,
        &ModelDecisionBoundary,
    ] {
        assert!(
            matches!(
                boundary.interpret(&reply(&text), &rich()),
                Err(DecisionError::Capability(CapabilityError::Unknown(id))) if id == "inspect_cargo_toml"
            ),
            "{:?}",
            boundary.interpret(&reply(&text), &rich())
        );
    }
}

/// A cheap well-formedness check, so the test says the rejection is about the capability and not
/// about the form.
fn serde_like_object(text: &str) -> bool {
    text.starts_with('{')
        && text.ends_with('}')
        && text.matches('{').count() == text.matches('}').count()
}

#[test]
fn invalid_inputs_are_not_renamed_or_coerced() {
    for r in [
        r#"{"decision":"request_capability","capability":"project.read","inputs":{"file":"Cargo.toml"}}"#,
        r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":["Cargo.toml"]}}"#,
        r#"{"decision":"request_capability","capability":"project.read","inputs":"Cargo.toml"}"#,
    ] {
        // The boundary accepts only the shape of the request; whether a named input exists is the
        // capability's own validation, later. What must never happen is a silent rename or coercion.
        let (full, compact) = (
            ModelDecisionBoundary.interpret(&reply(r), &rich()),
            CompactDecisionBoundary.interpret(&reply(r), &rich()),
        );
        assert_eq!(full, compact, "{r}");
        if let Ok(WorkDecision::RequestCapability(req)) = &compact {
            assert!(
                !req.inputs.contains_key("path") || r.contains("\"path\""),
                "{r}"
            );
        }
    }
}

// ---- through the loop ------------------------------------------------------------------------------

struct Replies {
    output: String,
    calls: AtomicUsize,
    seen: Mutex<Vec<ModelRequest>>,
}

impl Replies {
    fn new(output: &str) -> Arc<Self> {
        Arc::new(Self {
            output: output.into(),
            calls: AtomicUsize::new(0),
            seen: Mutex::new(vec![]),
        })
    }
}

#[async_trait::async_trait]
impl ModelProvider for Replies {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, FxError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(request);
        Ok(ModelResponse::new(
            "r",
            self.output.clone(),
            Usage::new(1, 1),
        ))
    }
}

struct Exec(AtomicUsize, Mutex<Vec<(String, Vec<String>)>>);

#[async_trait::async_trait]
impl Executor for Exec {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        self.1
            .lock()
            .unwrap()
            .push((r.intent.clone(), r.inputs.keys().cloned().collect()));
        Ok(ExecutionResult::success(r.id, "ran").with_receipt_id("sha256:real"))
    }
}

struct Caps;

#[async_trait::async_trait]
impl CapabilityProvider for Caps {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(rich()
            .into_iter()
            .filter(|c| c.availability == CapabilityAvailability::Available)
            .map(|c| c.descriptor)
            .collect())
    }
    async fn availability(&self, _: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

fn agent(model: &Arc<Replies>, exec: &Arc<Exec>) -> Agent {
    Agent::new(model.clone())
        .with_capabilities(Arc::new(Caps))
        .with_executor(exec.clone())
        .with_observer(Arc::new(ExecutionObserver))
}

fn spec(limit_executions: usize) -> WorkSpec {
    WorkSpec::new(WorkId::new("w"), WorkGoal::new("Read Cargo.toml"))
        .with_limits(WorkLimits {
            max_turns: 3,
            max_executions: limit_executions,
        })
        // One rejected reply ends the work, so nothing but the boundary decides what happens.
        .with_malformed_reply_limit(0)
}

async fn run_with(
    boundary: &dyn WorkDecisionBoundary,
    output: &str,
    executions: usize,
) -> (chip_core::WorkReport, Arc<Replies>, Arc<Exec>) {
    let (model, exec) = (
        Replies::new(output),
        Arc::new(Exec(AtomicUsize::new(0), Mutex::new(vec![]))),
    );
    let report = agent(&model, &exec)
        .run_work(&spec(executions), &NoLocalPolicy, boundary)
        .await;
    (report, model, exec)
}

#[tokio::test]
async fn the_asked_question_differs_and_nothing_else_in_the_request_does() {
    let valid = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"Cargo.toml"}}"#;
    let (_, full_model, _) = run_with(&ModelDecisionBoundary, valid, 1).await;
    let (_, compact_model, _) = run_with(&CompactDecisionBoundary, valid, 1).await;
    let (f, c) = (
        full_model.seen.lock().unwrap()[0].clone(),
        compact_model.seen.lock().unwrap()[0].clone(),
    );
    let text = |r: &ModelRequest| format!("{r:?}");
    assert!(text(&f).contains("Available capabilities:"));
    assert!(text(&c).contains("Capabilities:\\n- project.list"));
    assert!(!text(&c).contains("Available capabilities:"));
    assert_ne!(text(&f), text(&c));
}

#[tokio::test]
async fn a_valid_decision_executes_the_same_under_both_boundaries() {
    let valid = r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"Cargo.toml"}}"#;
    let (full, _, full_exec) = run_with(&ModelDecisionBoundary, valid, 1).await;
    let (compact, _, compact_exec) = run_with(&CompactDecisionBoundary, valid, 1).await;
    assert_eq!(full_exec.0.load(Ordering::SeqCst), 1);
    assert_eq!(compact_exec.0.load(Ordering::SeqCst), 1);
    assert_eq!(
        *full_exec.1.lock().unwrap(),
        *compact_exec.1.lock().unwrap(),
        "the same execution request"
    );
    assert_eq!(
        format!("{:?}", full.outcome),
        format!("{:?}", compact.outcome)
    );
}

#[tokio::test]
async fn nothing_executes_after_an_invalid_decision_under_either_boundary() {
    for output in [
        r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"Cargo.toml"}}}"#,
        r#"{"decision":"request_capability","capability":"inspect_cargo_toml","inputs":{"file":"Cargo.toml"}}"#,
        r#"{"decision":"request_capability","capability":"project.git.status","inputs":{}}"#,
        r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"x"}}"#,
        r#"{"decision":"complete"}"#,
        "Here you go: {\"decision\":\"complete\",\"summary\":\"x\"}",
    ] {
        for boundary in [
            &ModelDecisionBoundary as &dyn WorkDecisionBoundary,
            &CompactDecisionBoundary,
        ] {
            let (report, model, exec) = run_with(boundary, output, 5).await;
            assert_eq!(exec.0.load(Ordering::SeqCst), 0, "{output}");
            assert_eq!(
                model.calls.load(Ordering::SeqCst),
                1,
                "no retry, no repair: {output}"
            );
            assert!(report.observations.is_empty(), "{output}");
            // Refused by the decision contract (a rejected decision, the work fails) or one layer
            // later by the capability's own input validation (the work is blocked): either way the
            // reply never becomes an execution, an observation or evidence.
            assert!(
                matches!(
                    report.outcome,
                    WorkOutcome::Failed { .. } | WorkOutcome::Blocked { .. }
                ),
                "{output}: {:?}",
                report.outcome
            );
            assert!(
                !report.events.iter().any(|e| matches!(
                    e,
                    WorkEvent::Execution(_) | WorkEvent::EvidenceRecorded { .. }
                )),
                "{output}"
            );
        }
    }
}
