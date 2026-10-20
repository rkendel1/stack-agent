//! `stack-agent agent-state`: what an operator does with a task's durable agent state.
//!
//! * `show` reads a task's history from FeltDB.
//! * `settle` records an operator's conclusion about an intent whose outcome was never observed,
//!   so a refused (`OutcomeUncertain`) task can resume.
//!
//! Neither executes anything or calls a model. A settlement is a person's conclusion and is
//! recorded as one: it makes up no receipt, and it is never an execution's observation.

use std::collections::BTreeMap;

use chip_agent_state::{AgentStore, History, Settlement, StateError};
use serde_json::{Value, json};

const USAGE: &str = "\
usage: stack-agent agent-state show --state-db FILE --task-id ID [--json]
       stack-agent agent-state settle --state-db FILE --task-id ID --intent KEY \\
           --executed true|false --decided-by ID --evidence TEXT [--json]

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
        Some("show") => show(&args[1..]),
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
    })
}

fn show(args: &[String]) -> i32 {
    let (values, json) = match parse(args, &["--state-db", "--task-id"]) {
        Ok(parsed) => parsed,
        Err(why) => return usage(Some(why)),
    };
    let (Some(db), Some(task)) = (values.get("--state-db"), values.get("--task-id")) else {
        return usage(Some("--state-db and --task-id are required".into()));
    };
    let store = match open(db) {
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
