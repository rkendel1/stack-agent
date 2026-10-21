//! `stack-agent agent-state`: what an operator does with a task's durable agent state.
//!
//! * `list` lists the tasks a database holds, and says which were interrupted.
//! * `show` reads a task's history from FeltDB: its intents, results, and every recorded work
//!   event with its provenance.
//! * `export` maps a task's record to an OTLP/JSON trace (`chip-otel`): printed, or with `--send`
//!   delivered where the `OTEL_*` environment says.
//! * `settle` records an operator's conclusion about an intent whose outcome was never observed,
//!   so a refused (`OutcomeUncertain`) task can resume.
//!
//! Neither executes anything or calls a model. A settlement is a person's conclusion and is
//! recorded as one: it makes up no receipt, and it is never an execution's observation.

use std::collections::BTreeMap;

use chip_agent_state::{AgentStore, History, Settlement, StateError};
use serde_json::{Value, json};

const USAGE: &str = "\
usage: stack-agent agent-state list [--state-db FILE] [--json]
       stack-agent agent-state show [--state-db FILE] --task-id ID [--json]
       stack-agent agent-state export [--state-db FILE] --task-id ID [--provider P] [--model M] [--send] [--json]
       stack-agent agent-state settle --state-db FILE --task-id ID --intent KEY \\
           --executed true|false --decided-by ID --evidence TEXT [--json]

Without --state-db, list and show read the database of the project in the current directory, the
one every `work` and `serve` uses by default.

export prints the task's record as one OTLP/JSON trace request (GenAI semantic conventions; no
prompt, reply, goal, output, reason, path or credential is ever in it). --send delivers it instead,
to the collector and file the OTEL_EXPORTER_OTLP_* and STACK_AGENT_OTLP_FILE variables name
(loopback collectors only). The record does not hold the provider or model the work asked for;
--provider and --model add them.

settle records an operator's conclusion that the intent's effect did (true) or did not (false)
occur, who decided, and the evidence it rests on. It performs nothing and is not a receipt.
Exit codes: 0 done, 1 refused (nothing written), 2 usage, 3 unavailable.";

const EXIT_DONE: i32 = 0;
const EXIT_REFUSED: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_UNAVAILABLE: i32 = 3;

/// `--flag value` pairs and bare `--json`, each at most once, and nothing else.
fn parse(args: &[String], takes: &[&str]) -> Result<(BTreeMap<String, String>, bool), String> {
    let (mut values, mut json) = (BTreeMap::new(), false);
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        if flag == "--json" && !json {
            json = true;
            i += 1;
            continue;
        }
        if !takes.contains(&flag) || values.contains_key(flag) {
            return Err(format!("unexpected argument `{flag}`"));
        }
        let Some(value) = args.get(i + 1) else {
            return Err(format!("{flag} needs a value"));
        };
        values.insert(flag.to_string(), value.clone());
        i += 2;
    }
    Ok((values, json))
}

pub fn agent_state(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("list") => list(&args[1..]),
        Some("show") => show(&args[1..]),
        Some("export") => export(&args[1..]),
        Some("settle") => settle(&args[1..]),
        _ => usage(None),
    }
}

fn usage(why: Option<String>) -> i32 {
    if let Some(why) = why {
        eprintln!("error: {why}");
    }
    eprintln!("{USAGE}");
    EXIT_USAGE
}

/// `--state-db` when given, else the current project's default database (which must exist: reading
/// never creates one).
fn database(given: Option<&String>) -> Result<String, i32> {
    if let Some(path) = given {
        return Ok(path.clone());
    }
    let root = std::env::current_dir().map_err(|e| {
        eprintln!("error: no current directory ({e})");
        EXIT_UNAVAILABLE
    })?;
    let path = crate::work_state::default_database(&root, &|name: &str| std::env::var(name).ok())
        .map_err(|why| {
        eprintln!("error: durable agent state: {why}");
        EXIT_UNAVAILABLE
    })?;
    if !path.exists() {
        eprintln!(
            "error: durable agent state: this project has no database yet (no work has run here); pass --state-db FILE to read another"
        );
        return Err(EXIT_UNAVAILABLE);
    }
    Ok(path.display().to_string())
}

fn list(args: &[String]) -> i32 {
    let (values, json) = match parse(args, &["--state-db"]) {
        Ok(parsed) => parsed,
        Err(why) => return usage(Some(why)),
    };
    let store = match database(values.get("--state-db")).and_then(|db| open(&db)) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let mut tasks = match store.tasks() {
        Ok(tasks) => tasks,
        Err(e) => {
            eprintln!("error: durable agent state: {e}");
            return EXIT_UNAVAILABLE;
        }
    };
    tasks.sort_by_key(|t| t.updated_at_ms);
    // Nothing runs while this command holds the database, so a task still `running` here was
    // interrupted: its process ended before it finished.
    let status = |t: &chip_agent_state::TaskValue| -> String {
        if t.status == "running" {
            "interrupted".into()
        } else {
            t.status.clone()
        }
    };
    if json {
        println!(
            "{}",
            json!({"tasks": tasks.iter().map(|t| json!({
                "task_id": t.task_id, "status": status(t), "fence": t.fence,
                "unresolved_intents": t.unresolved_intents, "updated_at_ms": t.updated_at_ms,
            })).collect::<Vec<_>>()})
        );
    } else {
        for t in &tasks {
            println!(
                "{} {} ({} unresolved intent(s))",
                t.task_id,
                status(t),
                t.unresolved_intents
            );
        }
    }
    EXIT_DONE
}

fn open(path: &str) -> Result<AgentStore, i32> {
    AgentStore::open(path).map_err(|e| {
        eprintln!("error: durable agent state: {e}");
        EXIT_UNAVAILABLE
    })
}

fn settlement_json(s: &Settlement) -> Value {
    json!({
        "intent": s.intent_key, "capability": s.capability, "binding": s.binding,
        "executed": s.executed, "decided_by": s.decided_by, "evidence": s.evidence,
        "kind": "operator_settlement",
    })
}

fn history_json(history: &History, settlements: &[Settlement]) -> Value {
    json!({
        "task": history.to_json(),
        "lease_expires_at_ms": history.task.lease.as_ref().map(|l| l.expires_at),
        "intents": history.intents.iter().map(|i| json!({
            "key": i.value.key, "action": i.value.action, "state": i.value.state,
            "result": i.value.result, "conclusion": i.value.conclusion,
        })).collect::<Vec<_>>(),
        "settlements": settlements.iter().map(settlement_json).collect::<Vec<_>>(),
        "last_checkpoint": history.checkpoints.last().map(|c| json!({
            "version": c.version, "actor": c.actor, "status": c.status, "summary": c.summary,
        })),
        "results": history.results.iter().map(|r| json!({
            "attempt": r.attempt, "work_id": r.work_id, "outcome": r.outcome,
            "requested_status": r.requested_status, "verification": r.verification,
            "persistence_failures": r.persistence_failures,
        })).collect::<Vec<_>>(),
        // The response text stays in the database; what is shown is where it is and what it cost.
        "model_responses": history.model_responses.iter().map(|r| json!({
            "attempt": r.attempt, "turn": r.turn, "model": r.model, "response_id": r.response_id,
            "output_bytes": r.output.len(), "output_sha256": r.output_sha256, "usage": r.usage,
            "observed": r.observed, "intents_before": r.intents_before,
        })).collect::<Vec<_>>(),
        // Each open intent: whether the destination could be asked what happened, and every
        // query already made, with what the destination said and why that was not enough.
        "unresolved": history.unresolved_intents().iter().map(|i| {
            let queries: Vec<&chip_agent_state::ReconciliationRecord> = history
                .reconciliations
                .iter()
                .filter(|r| r.intent_key == i.value.key)
                .collect();
            let askable = chip_agent_state::reconcilability(&i.value);
            json!({
                "intent": i.value.key,
                "reconcilable": askable.is_ok(),
                "not_reconcilable_because": askable.as_ref().err(),
                "queries": queries.len(),
                // A query only reads; it never dispatches the operation. Whether one can be made
                // here is up to the capability having a reconciler, which `show` cannot see.
                "another_query_is_safe": askable.is_ok(),
                "manual_settlement_required": queries.iter().all(|q| q.finding == "indeterminate"),
            })
        }).collect::<Vec<_>>(),
        "reconciliations": history.reconciliations.iter().map(|r| json!({
            "intent": r.intent_key, "query_no": r.query_no, "capability": r.capability,
            "action_key": r.action_key, "binding": r.binding, "finding": r.finding,
            "destination": r.destination, "destination_detail": r.destination_detail,
            "business_effect": r.business_effect, "business_detail": r.business_detail,
            "receipt_id": r.receipt_id, "reason": r.reason, "decided_by": r.decided_by,
            "kind": "destination_evidence",
        })).collect::<Vec<_>>(),
        "model_calls_in_flight": history.model_calls_in_flight().iter().map(|c| json!({
            "attempt": c.attempt, "turn": c.turn,
            "outcome": "unknown: the attempt stopped before the call returned; whether the provider received, answered or charged it is not known, and the resumed attempt asks again",
        })).collect::<Vec<_>>(),
        "events": history.events.iter().map(|e| json!({
            "attempt": e.attempt, "index": e.index, "recorded_at_ms": e.recorded_at_ms,
            "provenance": e.provenance.as_str(), "event": e.event,
        })).collect::<Vec<_>>(),
    })
}

fn show(args: &[String]) -> i32 {
    let (values, json) = match parse(args, &["--state-db", "--task-id"]) {
        Ok(parsed) => parsed,
        Err(why) => return usage(Some(why)),
    };
    let Some(task) = values.get("--task-id") else {
        return usage(Some("--task-id is required".into()));
    };
    let db = match database(values.get("--state-db")) {
        Ok(db) => db,
        Err(code) => return code,
    };
    let store = match open(&db) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let read = store
        .history(task)
        .and_then(|h| Ok((h, store.settlements(task)?)));
    match read {
        Ok((Some(history), settlements)) => {
            if json {
                println!("{}", history_json(&history, &settlements));
            } else {
                let t = &history.task;
                println!(
                    "Task {}: {} (owner {}, fence {}, checkpoint {}).",
                    t.task_id, t.status, t.owner, t.fence, t.checkpoint_version
                );
                for i in &history.intents {
                    println!(
                        "  intent {}: {} ({})",
                        i.value.key, i.value.state, i.value.action
                    );
                }
                for e in &history.events {
                    println!(
                        "  event a{}#{} {} [{}]",
                        e.attempt,
                        e.index,
                        e.event["kind"].as_str().unwrap_or("?"),
                        e.provenance.as_str()
                    );
                }
                for r in &history.model_responses {
                    println!(
                        "  model response a{} turn {}: {} bytes from {}",
                        r.attempt,
                        r.turn,
                        r.output.len(),
                        r.model
                    );
                }
                for c in history.model_calls_in_flight() {
                    println!(
                        "  model call a{} turn {}: in flight when the attempt stopped; outcome unknown (it may have been charged), not replayed",
                        c.attempt, c.turn
                    );
                }
                for r in &history.results {
                    if let Some(v) = &r.verification {
                        println!(
                            "  result a{}: {} (verified {}, accepted {}, exit {})",
                            r.attempt, r.outcome, v.verified, v.accepted, v.exit_status
                        );
                    }
                }
                for i in history.unresolved_intents() {
                    if let Err(why) = chip_agent_state::reconcilability(&i.value) {
                        println!("  intent {}: cannot be reconciled: {why}", i.value.key);
                    }
                }
                for r in &history.reconciliations {
                    println!(
                        "  reconciliation {} #{}: {} (destination {}: {}; business effect {}{}){}",
                        r.intent_key,
                        r.query_no,
                        r.finding,
                        r.destination,
                        r.destination_detail,
                        r.business_effect,
                        if r.business_detail.is_empty() {
                            String::new()
                        } else {
                            format!(": {}", r.business_detail)
                        },
                        r.reason
                            .as_deref()
                            .map(|w| format!(" - insufficient: {w}"))
                            .unwrap_or_default()
                    );
                }
                for s in &settlements {
                    println!(
                        "  settled {}: {} by {} (an operator's conclusion): {}",
                        s.intent_key,
                        if s.executed {
                            "executed"
                        } else {
                            "not executed"
                        },
                        s.decided_by.as_deref().unwrap_or("an unrecorded operator"),
                        s.evidence
                    );
                }
            }
            EXIT_DONE
        }
        Ok((None, _)) => {
            eprintln!("error: there is no task {task} in {db}");
            EXIT_REFUSED
        }
        Err(e) => {
            eprintln!("error: durable agent state: {e}");
            EXIT_UNAVAILABLE
        }
    }
}

fn export(args: &[String]) -> i32 {
    let mut args = args.to_vec();
    let send = match args.iter().position(|a| a == "--send") {
        Some(at) => {
            args.remove(at);
            true
        }
        None => false,
    };
    let (values, json) = match parse(&args, &["--state-db", "--task-id", "--provider", "--model"]) {
        Ok(parsed) => parsed,
        Err(why) => return usage(Some(why)),
    };
    let Some(task) = values.get("--task-id") else {
        return usage(Some("--task-id is required".into()));
    };
    let db = match database(values.get("--state-db")) {
        Ok(db) => db,
        Err(code) => return code,
    };
    let store = match open(&db) {
        Ok(store) => store,
        Err(code) => return code,
    };
    let history = match store.history(task) {
        Ok(Some(history)) => history,
        Ok(None) => {
            eprintln!("error: there is no task {task} in {db}");
            return EXIT_REFUSED;
        }
        Err(e) => {
            eprintln!("error: durable agent state: {e}");
            return EXIT_UNAVAILABLE;
        }
    };
    let env = |name: &str| std::env::var(name).ok();
    let config = if send {
        match crate::telemetry::configured(&env) {
            Ok(Some(config)) => config,
            Ok(None) => {
                eprintln!(
                    "error: --send needs an export destination: OTEL_EXPORTER_OTLP_ENDPOINT (a loopback collector) or STACK_AGENT_OTLP_FILE"
                );
                return EXIT_USAGE;
            }
            Err(why) => {
                eprintln!("error: telemetry export: {why}");
                return EXIT_REFUSED;
            }
        }
    } else {
        // Printing reads the service name the same way sending does, and nothing else.
        chip_otel::export::Config {
            sinks: Vec::new(),
            service_name: env("OTEL_SERVICE_NAME"),
        }
    };
    let mut context = crate::telemetry::context(&config, "", "");
    context.provider = values.get("--provider").cloned();
    context.request_model = values.get("--model").cloned();
    if !send {
        println!("{}", chip_otel::trace(&history, &context));
        return EXIT_DONE;
    }
    let report = crate::telemetry::export_history(&config, &history, &context);
    if json {
        println!("{report}");
    } else {
        for sink in report["sinks"].as_array().into_iter().flatten() {
            match sink["error"].as_str() {
                None => println!(
                    "exported trace {} to {}",
                    report["trace_id"].as_str().unwrap_or_default(),
                    sink["sink"].as_str().unwrap_or_default()
                ),
                Some(why) => eprintln!(
                    "error: export to {} failed: {why}",
                    sink["sink"].as_str().unwrap_or_default()
                ),
            }
        }
    }
    if report["exported"] == true {
        EXIT_DONE
    } else {
        EXIT_UNAVAILABLE
    }
}

fn settle(args: &[String]) -> i32 {
    let (values, json) = match parse(
        args,
        &[
            "--state-db",
            "--task-id",
            "--intent",
            "--executed",
            "--decided-by",
            "--evidence",
        ],
    ) {
        Ok(parsed) => parsed,
        Err(why) => return usage(Some(why)),
    };
    let get = |k: &str| values.get(k).map(String::as_str);
    let (Some(db), Some(task), Some(intent), Some(executed), Some(decided_by), Some(evidence)) = (
        get("--state-db"),
        get("--task-id"),
        get("--intent"),
        get("--executed"),
        get("--decided-by"),
        get("--evidence"),
    ) else {
        return usage(Some(
            "--state-db, --task-id, --intent, --executed, --decided-by and --evidence are all required: a settlement names who decided and on what evidence".into(),
        ));
    };
    let executed = match executed {
        "true" => true,
        "false" => false,
        other => {
            return usage(Some(format!(
                "--executed takes true or false, not {other:?}"
            )));
        }
    };
    let store = match open(db) {
        Ok(store) => store,
        Err(code) => return code,
    };
    match chip_agent_state::settle(&store, task, intent, executed, decided_by, evidence) {
        Ok(settled) => {
            if json {
                println!(
                    "{}",
                    json!({
                        "command": "agent-state settle",
                        "settled": settlement_json(&settled.settlement),
                        "record": settled.record_id,
                        "state": history_json(&settled.history, std::slice::from_ref(&settled.settlement)),
                    })
                );
            } else {
                println!(
                    "Settled intent {intent} of task {task}: {} by {decided_by}. This is your conclusion, not an observation; the task can be resumed now.",
                    if executed {
                        "the effect occurred, so it will not be performed again"
                    } else {
                        "the effect did not occur, so the work may perform it if it still needs to"
                    },
                );
            }
            EXIT_DONE
        }
        Err(e @ StateError::SettlementRefused { .. }) => {
            eprintln!("error: {e}");
            EXIT_REFUSED
        }
        Err(e) => {
            eprintln!("error: durable agent state: {e}");
            EXIT_UNAVAILABLE
        }
    }
}
