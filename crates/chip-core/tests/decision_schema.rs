//! The canonical decision schema (`ModelDecisionBoundary::response_schema`) and the parser it is
//! derived beside. The schema is guidance a provider may enforce in whole, in part or not at all;
//! the parser is authoritative. These tests pin that the two agree, that every malformed shape the
//! contract excludes is refused by the parser whatever a provider did, and that no refused reply
//! executes anything in the real work loop.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chip_core::{
    Agent, Capability, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityInput, CapabilityProvider, DECISION_SHAPES, DecisionError, DecisionSchemaForm,
    ExecutionError, ExecutionObserver, ExecutionRequest, ExecutionResult, Executor, MAX_TEXT_BYTES,
    ModelDecisionBoundary, NoLocalPolicy, TestLocalReasoner, WorkDecision, WorkDecisionBoundary,
    WorkGoal, WorkId, WorkLimits, WorkOutcome, WorkSpec, audit_safety,
};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, Usage};
use serde_json::{Value, json};

// ---- a small evaluator for exactly the schema features the contract uses ------------------------------

fn type_ok(t: &Value, v: &Value) -> bool {
    let one = |name: &str| match name {
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "boolean" => v.is_boolean(),
        "object" => v.is_object(),
        _ => false,
    };
    match t {
        Value::String(s) => one(s),
        Value::Array(a) => a.iter().any(|x| one(x.as_str().unwrap_or(""))),
        _ => true,
    }
}

fn accepts(schema: &Value, v: &Value) -> bool {
    if let Some(t) = schema.get("type") {
        if !type_ok(t, v) {
            return false;
        }
    }
    if let Some(e) = schema.get("enum") {
        if !e.as_array().is_some_and(|a| a.contains(v)) {
            return false;
        }
    }
    if let Some(any) = schema.get("anyOf").and_then(Value::as_array) {
        if !any.iter().any(|b| accepts(b, v)) {
            return false;
        }
    }
    if let (Some(max), Some(s)) = (schema.get("maxLength").and_then(Value::as_u64), v.as_str()) {
        if s.chars().count() as u64 > max {
            return false;
        }
    }
    if let Some(obj) = v.as_object() {
        if let Some(max) = schema.get("maxProperties").and_then(Value::as_u64) {
            if obj.len() as u64 > max {
                return false;
            }
        }
        if let Some(req) = schema.get("required").and_then(Value::as_array) {
            if !req
                .iter()
                .all(|r| obj.contains_key(r.as_str().unwrap_or("")))
            {
                return false;
            }
        }
        let props = schema.get("properties").and_then(Value::as_object);
        for (k, x) in obj {
            match props.and_then(|p| p.get(k)) {
                Some(sub) => {
                    if !accepts(sub, x) {
                        return false;
                    }
                }
                None => match schema.get("additionalProperties") {
                    Some(Value::Bool(false)) => return false,
                    Some(sub @ Value::Object(_)) => {
                        if !accepts(sub, x) {
                            return false;
                        }
                    }
                    _ => {}
                },
            }
        }
    }
    true
}

fn schema(form: DecisionSchemaForm) -> Value {
    serde_json::from_str(&ModelDecisionBoundary::response_schema(form))
        .expect("the generated schema is valid JSON")
}

// ---- capabilities for the parser --------------------------------------------------------------------------

fn id(s: &str) -> CapabilityId {
    CapabilityId::new(s).unwrap()
}

fn caps() -> Vec<Capability> {
    let mut read = CapabilityDescriptor::new(id("project.read"), "read", "read a file");
    read.inputs.push(CapabilityInput {
        name: "path".into(),
        description: "path".into(),
        required: true,
    });
    let list = CapabilityDescriptor::new(id("project.status"), "status", "no inputs");
    [read, list]
        .into_iter()
        .map(|descriptor| Capability {
            descriptor,
            availability: CapabilityAvailability::Available,
        })
        .collect()
}

fn read(reply: &str) -> Result<WorkDecision, DecisionError> {
    ModelDecisionBoundary.interpret(&ModelResponse::new("r", reply, Usage::new(1, 1)), &caps())
}

fn invalid(r: &Result<WorkDecision, DecisionError>) -> bool {
    matches!(r, Err(DecisionError::InvalidDecision(_)))
}

// ---- the schema is derived from the contract --------------------------------------------------------------------

#[test]
fn both_projections_are_valid_json_and_name_every_decision_the_contract_has() {
    for form in [DecisionSchemaForm::Full, DecisionSchemaForm::Flat] {
        let s = schema(form);
        assert_eq!(s["type"], "object", "{form:?}");
        let text = s.to_string();
        for shape in DECISION_SHAPES {
            assert!(text.contains(shape.name), "{form:?} lacks {}", shape.name);
        }
        for f in [
            "capability",
            "inputs",
            "summary",
            "reason",
            "schema",
            "chip.work-decision.v1",
        ] {
            assert!(text.contains(f), "{form:?} lacks {f}");
        }
    }
    // Full has one alternative per decision; Flat has none at the root, which is what the
    // providers that reject root-level combinators need.
    assert_eq!(
        schema(DecisionSchemaForm::Full)["anyOf"]
            .as_array()
            .unwrap()
            .len(),
        DECISION_SHAPES.len()
    );
    let flat = schema(DecisionSchemaForm::Flat);
    for banned in ["anyOf", "oneOf", "allOf", "enum", "const", "not"] {
        assert!(flat.get(banned).is_none(), "the flat root carries {banned}");
    }
    assert_eq!(flat["required"], json!(["decision"]));
    assert_eq!(flat["additionalProperties"], json!(false));
}

/// Every reply here is a complete JSON value. `valid` ones are decisions the parser accepts.
fn corpus() -> Vec<(&'static str, Value, bool)> {
    let long = "x".repeat(MAX_TEXT_BYTES);
    vec![
        (
            "request with inputs",
            json!({"decision":"request_capability","capability":"project.read","inputs":{"path":"a.rs"}}),
            true,
        ),
        (
            "request without inputs",
            json!({"decision":"request_capability","capability":"project.status"}),
            true,
        ),
        (
            "with the schema marker",
            json!({"schema":"chip.work-decision.v1","decision":"complete","summary":"done"}),
            true,
        ),
        (
            "integer and boolean inputs",
            json!({"decision":"request_capability","capability":"project.read","inputs":{"path":"a","n":3,"b":true}}),
            true,
        ),
        (
            "complete",
            json!({"decision":"complete","summary":"done"}),
            true,
        ),
        (
            "summary at the limit",
            json!({"decision":"complete","summary":long}),
            true,
        ),
        (
            "escalate",
            json!({"decision":"escalate","reason":"need a person"}),
            true,
        ),
        (
            "block",
            json!({"decision":"block","reason":"cannot proceed"}),
            true,
        ),
        ("empty object", json!({}), false),
        ("no decision", json!({"summary":"done"}), false),
        (
            "unknown decision value",
            json!({"decision":"finish","summary":"done"}),
            false,
        ),
        (
            "decision of the wrong type",
            json!({"decision":7,"summary":"done"}),
            false,
        ),
        (
            "complete without summary",
            json!({"decision":"complete"}),
            false,
        ),
        (
            "escalate without reason",
            json!({"decision":"escalate"}),
            false,
        ),
        (
            "request without capability",
            json!({"decision":"request_capability","inputs":{}}),
            false,
        ),
        (
            "unexpected property",
            json!({"decision":"complete","summary":"s","receipt":"forged"}),
            false,
        ),
        (
            "a field of another decision",
            json!({"decision":"complete","summary":"s","reason":"r"}),
            false,
        ),
        (
            "summary of the wrong type",
            json!({"decision":"complete","summary":42}),
            false,
        ),
        (
            "inputs of the wrong type",
            json!({"decision":"request_capability","capability":"project.read","inputs":"a.rs"}),
            false,
        ),
        (
            "a nested input value",
            json!({"decision":"request_capability","capability":"project.read","inputs":{"path":{"a":1}}}),
            false,
        ),
        (
            "a float input value",
            json!({"decision":"request_capability","capability":"project.read","inputs":{"path":"a","n":1.5}}),
            false,
        ),
        (
            "wrong schema marker",
            json!({"schema":"other.v9","decision":"complete","summary":"s"}),
            false,
        ),
        (
            "summary over the limit",
            json!({"decision":"complete","summary":"x".repeat(MAX_TEXT_BYTES + 1)}),
            false,
        ),
    ]
}

#[test]
fn the_parser_accepts_nothing_the_schema_excludes_and_the_schema_excludes_what_it_can() {
    for (what, value, valid) in corpus() {
        let reply = value.to_string();
        let parsed = read(&reply);
        assert_eq!(parsed.is_ok(), valid, "{what}: {parsed:?}");
        let (full, flat) = (
            accepts(&schema(DecisionSchemaForm::Full), &value),
            accepts(&schema(DecisionSchemaForm::Flat), &value),
        );
        if valid {
            // The schema never excludes a decision the runtime accepts.
            assert!(full && flat, "{what}: full {full}, flat {flat}");
        } else {
            // The full projection excludes every structural fault in the corpus; the flat one
            // cannot say which decision needs which field, so it may pass a missing required
            // field. Either way the parser is what refuses it.
            let flat_blind = matches!(
                what,
                "complete without summary"
                    | "escalate without reason"
                    | "request without capability"
                    | "a field of another decision"
                    | "wrong schema marker"
            );
            assert!(
                !full || what == "wrong schema marker" && false,
                "the full schema admits {what}"
            );
            assert!(!flat || flat_blind, "the flat schema admits {what}");
        }
    }
}

#[test]
fn a_reply_the_schema_cannot_even_describe_is_refused_by_the_parser() {
    // Not complete JSON values at all: no schema applies, only the parser.
    let long = "x".repeat(MAX_TEXT_BYTES);
    let cases: Vec<(&str, String)> = vec![
        ("empty", String::new()),
        ("truncated JSON", r#"{"decision":"complete","summary":"par"#.into()),
        ("truncated after a key", r#"{"decision":"#.into()),
        ("two objects", r#"{"decision":"complete","summary":"a"}{"decision":"complete","summary":"b"}"#.into()),
        ("two objects on lines", "{\"decision\":\"complete\",\"summary\":\"a\"}\n{\"decision\":\"complete\",\"summary\":\"b\"}".into()),
        ("valid JSON then prose", r#"{"decision":"complete","summary":"a"} I hope this helps."#.into()),
        ("prose then valid JSON", r#"Here you go: {"decision":"complete","summary":"a"}"#.into()),
        ("a JSON array", r#"[{"decision":"complete","summary":"a"}]"#.into()),
        ("a bare string", r#""complete""#.into()),
        ("a duplicated key's second object", r#"{"decision":"complete","summary":"a"},"#.into()),
        ("an oversized reply", format!(r#"{{"decision":"complete","summary":"a"}}{}"#, " ".repeat(5000))),
        ("an over-long summary inside a valid object", format!(r#"{{"decision":"complete","summary":"{long}y"}}"#)),
    ];
    for (what, reply) in cases {
        assert!(invalid(&read(&reply)), "{what}: {:?}", read(&reply));
    }
    // What the protocol does tolerate: whitespace around one object, and one code fence around it.
    for ok in [
        "  {\"decision\":\"complete\",\"summary\":\"a\"}  \n\n",
        "{\"decision\":\"complete\",\"summary\":\"a\"}\n",
        "```json\n{\"decision\":\"complete\",\"summary\":\"a\"}\n```",
    ] {
        assert!(read(ok).is_ok(), "{ok:?}");
    }
}

#[test]
fn a_decision_for_a_capability_that_was_not_declared_is_refused_whatever_the_schema_says() {
    // Structurally perfect, and in the full schema's language: only the runtime knows the list.
    for cap in ["project.write", "shell.run", "Project.Read", "pax.test"] {
        let v = json!({"decision":"request_capability","capability":cap});
        assert!(accepts(&schema(DecisionSchemaForm::Full), &v));
        assert!(
            matches!(read(&v.to_string()), Err(DecisionError::Capability(_))),
            "{cap}"
        );
    }
    // A capability that declares no inputs refuses an empty `inputs` object too.
    let v = json!({"decision":"request_capability","capability":"project.status","inputs":{}});
    let parsed = read(&v.to_string());
    assert!(
        parsed.is_ok(),
        "the request parses; the loop refuses the invented inputs"
    );
}

// ---- nothing malformed executes ------------------------------------------------------------------------------------

struct World {
    executions: AtomicUsize,
}

#[async_trait::async_trait]
impl CapabilityProvider for World {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(caps().into_iter().map(|c| c.descriptor).collect())
    }
    async fn availability(&self, _: &CapabilityId) -> CapabilityAvailability {
        CapabilityAvailability::Available
    }
}

#[async_trait::async_trait]
impl Executor for World {
    async fn execute(&self, r: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ExecutionResult::success(r.id, "ran"))
    }
}

struct Script(Mutex<Vec<String>>);

#[async_trait::async_trait]
impl ModelProvider for Script {
    async fn complete(&self, _: ModelRequest) -> Result<ModelResponse, FxError> {
        let mut r = self.0.lock().unwrap();
        if r.is_empty() {
            return Err(FxError::Provider("out of replies".into()));
        }
        Ok(ModelResponse::new("r", r.remove(0), Usage::new(1, 1)))
    }
}

#[tokio::test]
async fn no_malformed_or_unauthorized_reply_reaches_an_executor() {
    let bad: Vec<String> = vec![
        "{}".into(),
        r#"{"summary":"no decision"}"#.into(),
        r#"{"decision":"finish"}"#.into(),
        r#"{"decision":"request_capability"}"#.into(),
        r#"{"decision":"request_capability","capability":"project.status","extra":1}"#.into(),
        r#"{"decision":"request_capability","capability":"project.read","inputs":"a"}"#.into(),
        r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"a"}"#.into(),
        r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"a"}} then I ran it."#.into(),
        r#"{"decision":"request_capability","capability":"project.read","inputs":{"path":"a"}}{"decision":"request_capability","capability":"project.read","inputs":{"path":"b"}}"#.into(),
        r#"{"decision":"request_capability","capability":"project.write","inputs":{"path":"a","content":"x"}}"#.into(),
        r#"{"decision":"request_capability","capability":"project.status","inputs":{}}"#.into(),
    ];
    for reply in bad {
        let world = Arc::new(World {
            executions: AtomicUsize::new(0),
        });
        let agent = Agent::new(Arc::new(Script(Mutex::new(vec![
            reply.clone(),
            r#"{"decision":"block","reason":"x"}"#.into(),
        ]))))
        .with_capabilities(world.clone())
        .with_executor(world.clone())
        .with_observer(Arc::new(ExecutionObserver))
        .with_local_reasoner(Arc::new(TestLocalReasoner::default()));
        let spec = WorkSpec::new(WorkId::new("w"), WorkGoal::new("g")).with_limits(WorkLimits {
            max_turns: 4,
            max_executions: 4,
        });
        let report = agent
            .run_work(&spec, &NoLocalPolicy, &ModelDecisionBoundary)
            .await;
        assert_eq!(
            world.executions.load(Ordering::SeqCst),
            0,
            "{reply}: {:?}",
            report.outcome
        );
        assert!(
            matches!(
                report.outcome,
                WorkOutcome::Failed { .. } | WorkOutcome::Blocked { .. }
            ),
            "{reply}: {:?}",
            report.outcome
        );
        assert!(report.observations.is_empty(), "{reply}");
        assert!(
            audit_safety(&report, &spec, &[id("project.read"), id("project.status")]).is_clean(),
            "{reply}"
        );
    }
}
