//! Runs Chip's work loop once against a live MCP endpoint, with every execution bracketed by
//! durable FeltDB agent state, and writes what happened.
//!
//! ```sh
//! MCP_TOKEN=... cargo run -p chip-appport-source --example mcp_live -- \
//!   --endpoint http://127.0.0.1:3922/apps/com.example.invoice-desk/mcp --token-env MCP_TOKEN \
//!   --selector invoicedesk --scopes "invoices.read invoices.write" \
//!   --goal "..." --require appport.invoicedesk.invoices_id_status --replies replies.json \
//!   --state-db state.feltdb --task-id approve-1 --out run.json
//! ```
//!
//! The model is `--replies`: the replies, in order, a scripted model gives (no network, no
//! credential), so a run is deterministic up to what the application answers. The capabilities are
//! real: each request is one `tools/call` to the endpoint (`chip_appport_source::live`). The loop is
//! composed as `examples/mcp_work.rs` composes it (the source's descriptors only, no local policy,
//! the model decision boundary, the full escalation context, the required observation in the loop,
//! no evidence reuse) and as `stack-agent work` composes durability: `chip_agent_state::begin` on
//! the task, re-observation of interrupted observations, the restored and settled actions in the
//! spec, `DurableWork::executor` around the capabilities, and `finish` with the host's verdict.
//!
//! `--fault crash-before-send|crash-after-send --fault-capability <id>` aborts the process at that
//! point of the first execution of that capability: before the request is sent, or after the
//! endpoint answered and before the outcome reaches durable state. It exists only in this driver,
//! to put an interruption at a known point; the transport and durable state are unchanged.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chip_agent_state::{AgentStore, Begun, DurableConfig, StateError};
use chip_appport_source::Succeeded;
use chip_appport_source::live::LiveMcp;
use chip_core::{
    Agent, CapabilitySet, ExecutionError, ExecutionObserver, ExecutionRequest, ExecutionResult,
    Executor, FullEscalationContext, ModelDecisionBoundary, NoLocalPolicy, WorkGoal, WorkId,
    WorkLimits, WorkOutcome, WorkSpec,
};
use chip_experiments::{Criterion, TaskFixture};
use chip_inference::{FxError, ModelProvider, ModelRequest, ModelResponse};

/// Exit statuses: 0 completed and accepted, 1 not completed, 3 an earlier execution's outcome is
/// uncertain (nothing was run), 2 the run could not start.
const EXIT_COMPLETED: i32 = 0;
const EXIT_NOT_COMPLETED: i32 = 1;
const EXIT_UNAVAILABLE: i32 = 2;
const EXIT_UNCERTAIN: i32 = 3;

/// The scripted model: its replies, in order. One more request than it has replies is an error.
struct Scripted {
    replies: Mutex<VecDeque<String>>,
    asked: Mutex<u32>,
}

#[async_trait::async_trait]
impl ModelProvider for Scripted {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, FxError> {
        *self.asked.lock().unwrap() += 1;
        let reply = self.replies.lock().unwrap().pop_front();
        match reply {
            Some(output) => Ok(ModelResponse::new(
                "scripted",
                output,
                chip_inference::Usage::default(),
            )),
            None => Err(FxError::Provider(
                "the scripted model has no more replies".into(),
            )),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    CrashBeforeSend,
    CrashAfterSend,
}

/// Aborts the process at the configured point of the first execution of one capability.
struct Faulty {
    inner: Arc<dyn Executor>,
    fault: Option<(Fault, String)>,
}

#[async_trait::async_trait]
impl Executor for Faulty {
    async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult, ExecutionError> {
        let armed = self
            .fault
            .as_ref()
            .filter(|(_, capability)| *capability == request.intent)
            .map(|(fault, _)| *fault);
        if armed == Some(Fault::CrashBeforeSend) {
            eprintln!("fault: aborting before {} is sent", request.intent);
            std::process::abort();
        }
        let result = self.inner.execute(request.clone()).await;
        if armed == Some(Fault::CrashAfterSend) {
            eprintln!(
                "fault: aborting after the endpoint answered {} and before its outcome is recorded",
                request.intent
            );
            std::process::abort();
        }
        result
    }
}

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn write(out: &str, record: &serde_json::Value) {
    let text = serde_json::to_string_pretty(record).unwrap() + "\n";
    std::fs::write(out, &text).unwrap();
    println!("{text}");
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let need = |name: &str| arg(&args, name).unwrap_or_else(|| panic!("{name} is required"));
    let out = need("--out");
    let goal = need("--goal");
    let task_id = need("--task-id");
    let require = arg(&args, "--require");
    let scopes: Vec<String> = need("--scopes")
        .split_whitespace()
        .map(String::from)
        .collect();
    let bearer = std::env::var(need("--token-env")).expect("the --token-env variable is not set");
    let replies: Vec<String> = serde_json::from_str::<Vec<serde_json::Value>>(
        &std::fs::read_to_string(need("--replies")).expect("--replies"),
    )
    .expect("--replies is a JSON array")
    .into_iter()
    .map(|r| match r {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    })
    .collect();
    let fault = match arg(&args, "--fault").as_deref() {
        None => None,
        Some("crash-before-send") => Some((Fault::CrashBeforeSend, need("--fault-capability"))),
        Some("crash-after-send") => Some((Fault::CrashAfterSend, need("--fault-capability"))),
        Some(other) => panic!("--fault {other}: crash-before-send or crash-after-send"),
    };
    let lease_ms = arg(&args, "--lease-ms").map_or(2_000, |v| v.parse().expect("--lease-ms"));

    let live = match LiveMcp::connect(
        &need("--endpoint"),
        &bearer,
        &need("--selector"),
        &scopes,
        Duration::from_secs(
            arg(&args, "--timeout-secs").map_or(10, |v| v.parse().expect("--timeout-secs")),
        ),
    ) {
        Ok(live) => Arc::new(live),
        Err(why) => {
            write(
                &out,
                &serde_json::json!({"schema": "chip.appport-mcp-live-run.v1", "started": false, "error": why.to_string()}),
            );
            std::process::exit(EXIT_UNAVAILABLE);
        }
    };
    let descriptors: Vec<_> = live
        .offered()
        .iter()
        .map(|o| o.descriptor.clone())
        .collect();
    let set = Arc::new(CapabilitySet::new().with(live.clone()));

    // Durable state, as `stack-agent work` opens it: every offered capability's effect and
    // idempotency as its descriptor states them (consequential, no key).
    let store = AgentStore::open(need("--state-db")).expect("--state-db");
    let config = DurableConfig {
        lease_ms,
        idempotency: descriptors
            .iter()
            .map(|d| (d.id.to_string(), d.idempotency))
            .collect(),
        effects: descriptors
            .iter()
            .map(|d| (d.id.to_string(), d.effect))
            .collect(),
        ..DurableConfig::default()
    };
    let base = serde_json::json!({
        "schema": "chip.appport-mcp-live-run.v1",
        "endpoint": need("--endpoint"),
        "server": live.server(),
        "grant": scopes,
        "listed": live.tools().iter().map(|t| t["name"].clone()).collect::<Vec<_>>(),
        "offered": descriptors.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
        "not_offered": live.not_offered().iter().map(|n| serde_json::json!({"tool": n.capability, "reason": n.reason})).collect::<Vec<_>>(),
        "task_id": task_id,
        "goal": goal,
        "require": require,
    });
    let durable = match chip_agent_state::begin(&store, &task_id, &goal, &config) {
        Ok(Begun::Ready(work)) => work,
        Ok(Begun::Finished(history)) => {
            let mut record = base;
            record["started"] = false.into();
            record["finished_earlier"] = serde_json::json!(format!("{:?}", history.task.status));
            write(&out, &record);
            std::process::exit(EXIT_COMPLETED);
        }
        Err(why) => {
            let uncertain = matches!(
                why,
                StateError::OutcomeUncertain { .. } | StateError::UncertainLeaseHeld { .. }
            );
            let mut record = base;
            record["started"] = false.into();
            record["durable_refusal"] =
                serde_json::json!({"uncertain": uncertain, "error": why.to_string()});
            write(&out, &record);
            std::process::exit(if uncertain {
                EXIT_UNCERTAIN
            } else {
                EXIT_UNAVAILABLE
            });
        }
    };
    if let Err(why) = durable.reobserve(set.clone(), None).await {
        let mut record = base;
        record["started"] = false.into();
        record["durable_refusal"] =
            serde_json::json!({"uncertain": true, "error": why.to_string()});
        write(&out, &record);
        std::process::exit(EXIT_UNCERTAIN);
    }

    let model = Arc::new(Scripted {
        replies: Mutex::new(replies.into()),
        asked: Mutex::new(0),
    });
    let mut spec = WorkSpec::new(WorkId::new(task_id.clone()), WorkGoal::new(goal.clone()))
        .with_limits(WorkLimits {
            max_turns: 8,
            max_executions: 6,
        })
        .with_restored(durable.restored())
        .with_settled(durable.settled_actions());
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
    let executor = durable.executor(Arc::new(Faulty {
        inner: set.clone(),
        fault,
    }));
    let agent = Agent::with_model(model.clone(), "scripted".to_string())
        .with_capabilities(set)
        .with_executor(executor)
        .with_observer(Arc::new(ExecutionObserver));
    let report = agent
        .run_work_with_context_policy(
            &spec,
            &NoLocalPolicy,
            &ModelDecisionBoundary,
            &FullEscalationContext,
        )
        .await;
    let completed = matches!(report.outcome, WorkOutcome::Completed { .. });
    let finished = durable.finish(&report, completed);

    let mut record = base;
    record["started"] = true.into();
    record["attempt"] = durable.attempt().into();
    record["resumed_from"] = serde_json::json!(durable.resumed_from());
    record["outcome"] = match &report.outcome {
        WorkOutcome::Completed { summary } => serde_json::json!({"completed": summary}),
        other => serde_json::json!({"not_completed": format!("{other:?}")}),
    };
    record["observations"] = report
        .observations
        .iter()
        .map(|o| {
            serde_json::json!({
                "execution_id": o.execution_id.0,
                "receipt_id": o.receipt_id,
                "status": format!("{:?}", o.status),
                "head": o.output.as_deref().and_then(|t| t.lines().next()).and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok()),
                "output": o.output,
            })
        })
        .collect();
    record["model_requests"] = (*model.asked.lock().unwrap()).into();
    record["durable"] = serde_json::json!({
        "finished": finished.as_ref().map(|f| format!("{f:?}")).map_err(|e| e.to_string()),
        "failures": durable.failures().iter().map(|f| f.to_string()).collect::<Vec<_>>(),
    });
    write(&out, &record);
    std::process::exit(if completed {
        EXIT_COMPLETED
    } else {
        EXIT_NOT_COMPLETED
    });
}
