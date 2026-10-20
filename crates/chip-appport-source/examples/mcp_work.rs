//! Runs Chip's work loop once with a real model over an MCP recording, and saves the model's
//! replies, so the same run can be replayed deterministically through `chip-experiments`
//! (`tests/mcp_replay.rs`).
//!
//! ```sh
//! CHIP_API_KEY=... cargo run --release -p chip-appport-source --example mcp_work -- \
//!   --snapshot fixtures/mcp/vikunja.read.mcp.json --goal "..." --require <capability id> \
//!   --model alibaba/qwen3-coder-30b-a3b --endpoint https://ai-gateway.vercel.sh/v1/chat/completions \
//!   --out fixtures/mcp/replies.<task>.json
//! ```
//!
//! The model is real; the capabilities are not called. Every capability result comes from the
//! recording, exactly as replay serves it (a request that is not recorded fails and nothing is
//! substituted), so this adds no authority: no MCP server is contacted from inside the run. The loop
//! is composed as `chip-experiments` composes a replay run: the source's descriptors only, no local
//! policy, the model decision boundary, the full escalation context, the required observation in
//! the loop and no evidence reuse.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chip_appport_source::Succeeded;
use chip_appport_source::mcp::{McpSnapshot, McpSource};
use chip_core::{
    Agent, CapabilityAvailability, CapabilityDescriptor, CapabilityError, CapabilityId,
    CapabilityProvider, CapabilitySet, DecisionSchemaForm, ExecutionError, ExecutionObserver,
    ExecutionRequest, ExecutionResult, Executor, FullEscalationContext, ModelDecisionBoundary,
    NoLocalPolicy, WorkGoal, WorkId, WorkLimits, WorkOutcome, WorkSpec,
};
use chip_experiments::plugin::recorded_inputs;
use chip_experiments::{Criterion, RecordedExecution, TaskFixture};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse, ProviderConfig, Secret};

struct Replay {
    descriptors: Vec<CapabilityDescriptor>,
    entries: Vec<RecordedExecution>,
}

#[async_trait::async_trait]
impl CapabilityProvider for Replay {
    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {
        Ok(self.descriptors.clone())
    }
    async fn availability(&self, id: &CapabilityId) -> CapabilityAvailability {
        if self.descriptors.iter().any(|d| &d.id == id) {
            CapabilityAvailability::Available
        } else {
            CapabilityAvailability::Unavailable("not declared in this replay".into())
        }
    }
}

#[async_trait::async_trait]
impl Executor for Replay {
    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let inputs = recorded_inputs(&request.inputs);
        match self
            .entries
            .iter()
            .find(|e| e.capability == request.intent && e.inputs == inputs)
        {
            Some(e) if e.succeeded => Ok(ExecutionResult::success(request.id, e.output.clone())),
            Some(e) => Ok(ExecutionResult::failure(request.id, e.output.clone())),
            None => Err(ExecutionError::ExecutionFailed(format!(
                "{} with these inputs is not in the recording; replay performs no execution",
                request.intent
            ))),
        }
    }
}

/// The real provider, with every reply kept in order. `pace` spaces calls for an endpoint's rate
/// limit (latency only; nothing else changes).
struct Keeping {
    inner: Arc<dyn ModelProvider>,
    pace: Duration,
    last: tokio::sync::Mutex<Option<std::time::Instant>>,
    replies: Mutex<Vec<String>>,
    tokens: Mutex<(u64, u64)>,
}

#[async_trait::async_trait]
impl ModelProvider for Keeping {
    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, FxError> {
        let mut last = self.last.lock().await;
        if let Some(at) = *last {
            let wait = self.pace.saturating_sub(at.elapsed());
            tokio::time::sleep(wait).await;
        }
        *last = Some(std::time::Instant::now());
        drop(last);
        let response = self.inner.complete(request).await?;
        self.replies.lock().unwrap().push(response.output.clone());
        let mut t = self.tokens.lock().unwrap();
        t.0 += response.usage.prompt_tokens.unwrap_or(0);
        t.1 += response.usage.completion_tokens.unwrap_or(0);
        Ok(response)
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let need = |name: &str| arg(&args, name).unwrap_or_else(|| panic!("{name} is required"));
    let snapshot = std::fs::read_to_string(need("--snapshot")).expect("snapshot");
    let source = McpSource::new(McpSnapshot::from_json(&snapshot).unwrap(), "vikunja").unwrap();
    let goal = need("--goal");
    let require = arg(&args, "--require");
    let out = need("--out");

    let mut config = ProviderConfig::new("openai-compatible", need("--model"), need("--endpoint"));
    config.api_key = Some(Secret::new(
        std::env::var("CHIP_API_KEY").expect("CHIP_API_KEY is required"),
    ));
    config.temperature = Some(0.0);
    config.timeout = Duration::from_secs(120);
    // `stack-agent work`'s defaults for this provider: the full decision schema as the response
    // format, its output-token cap, temperature 0.
    config.json_object_output = true;
    // `--schema flat` is the form `stack-agent`'s OpenAI profile sends (no root `anyOf`).
    let form = match arg(&args, "--schema").as_deref() {
        None | Some("full") => DecisionSchemaForm::Full,
        Some("flat") => DecisionSchemaForm::Flat,
        Some(other) => panic!("--schema {other}: full or flat"),
    };
    config.response_schema_json = Some(ModelDecisionBoundary::response_schema(form));
    config.max_tokens = Some(chip_cli::software_work::WORK_MAX_OUTPUT_TOKENS);
    let real = chip_cli::provider_selection::build_model_from_process(config).unwrap();
    let model = Arc::new(Keeping {
        inner: real,
        pace: Duration::from_millis(
            arg(&args, "--pace-ms").map_or(0, |v| v.parse().expect("--pace-ms")),
        ),
        last: tokio::sync::Mutex::new(None),
        replies: Mutex::new(Vec::new()),
        tokens: Mutex::new((0, 0)),
    });

    let descriptors: Vec<CapabilityDescriptor> = source
        .offered()
        .iter()
        .map(|o| o.descriptor.clone())
        .collect();
    let set = Arc::new(CapabilitySet::new().with(Arc::new(Replay {
        descriptors: descriptors.clone(),
        entries: source.recorded_executions(),
    })));
    let mut spec = WorkSpec::new(WorkId::new("mcp-live-model"), WorkGoal::new(goal.clone()))
        .with_limits(WorkLimits {
            max_turns: 8,
            max_executions: 6,
        });
    if let Some(capability) = &require {
        let task = TaskFixture {
            id: "driver".into(),
            version: 1,
            goal: goal.clone(),
            files: Vec::new(),
            expected_files: Vec::new(),
            model_replies: Vec::new(),
            recording: None,
        };
        if let Some(p) = Succeeded::new(capability).in_loop(&task) {
            spec = spec.with_required_observation(p);
        }
    }
    for d in &descriptors {
        spec = spec.with_evidence_reuse_prohibited(d.id.clone());
    }
    let agent = Agent::with_model(model.clone(), need("--model"))
        .with_capabilities(set.clone())
        .with_executor(set)
        .with_observer(Arc::new(ExecutionObserver));
    let report = agent
        .run_work_with_context_policy(
            &spec,
            &NoLocalPolicy,
            &ModelDecisionBoundary,
            &FullEscalationContext,
        )
        .await;

    let replies = model.replies.lock().unwrap().clone();
    let (input_tokens, output_tokens) = *model.tokens.lock().unwrap();
    let outcome = match &report.outcome {
        WorkOutcome::Completed { summary } => serde_json::json!({"completed": summary}),
        other => serde_json::json!({"other": format!("{other:?}")}),
    };
    let observations: Vec<serde_json::Value> = report
        .observations
        .iter()
        .map(|o| {
            serde_json::json!({
                "status": format!("{:?}", o.status),
                "head": o.output.as_deref().and_then(|t| t.lines().next()),
            })
        })
        .collect();
    let record = serde_json::json!({
        "schema": "chip.appport-mcp-model-run.v1",
        "model": need("--model"),
        "endpoint": need("--endpoint"),
        "decision_schema": arg(&args, "--schema").unwrap_or_else(|| "full".into()),
        "snapshot": source.snapshot().label,
        "goal": goal,
        "require": require,
        "offered": descriptors.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
        "replies": replies,
        "outcome": outcome,
        "observations": observations,
        "model_calls": replies.len(),
        "tokens": {"input": input_tokens, "output": output_tokens},
    });
    std::fs::write(&out, serde_json::to_string_pretty(&record).unwrap() + "\n").unwrap();
    println!("{}", serde_json::to_string_pretty(&record).unwrap());
}
