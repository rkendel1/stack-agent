//! sa-state history DB TASK                         -> full History as JSON (stdout)
//! sa-state reconcile DB TASK INTENT_KEY executed|not_executed
//!     Writes the operator's chip.Observation for an uncertain intent, exactly as
//!     crates/stack-agent/src/durability_bench.rs `reconcile_from_world` does (the merged
//!     implementation has no command for this). The next `begin` then reconciles from it.
use chip_agent_state::{AgentStore, OBSERVATION_COLLECTION, OBSERVATION_SCHEMA};
use serde_json::json;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let store = AgentStore::open(&a[2]).unwrap_or_else(|e| {
        eprintln!("open: {e}");
        std::process::exit(3)
    });
    match a[1].as_str() {
        "history" => match store.history(&a[3]) {
            Ok(Some(h)) => {
                let v = json!({
                    "summary": h.to_json(),
                    "checkpoints": h.checkpoints.iter().map(|c| json!({"version": c.version, "status": c.status, "summary": c.summary, "fence": c.fence})).collect::<Vec<_>>(),
                    "intents": h.intents.iter().map(|i| serde_json::to_value(&i.value).unwrap()).collect::<Vec<_>>(),
                    "results": h.results.iter().map(|r| serde_json::to_value(r).unwrap()).collect::<Vec<_>>(),
                });
                println!("{v}");
            }
            Ok(None) => println!("null"),
            Err(e) => {
                println!("{}", json!({"error": e.to_string()}));
                std::process::exit(1)
            }
        },
        "reconcile" => {
            let (task, key, executed) = (&a[3], &a[4], a[5] == "executed");
            let id = format!("{task}~{key}");
            let k = format!("{OBSERVATION_COLLECTION}:{id}");
            let value = json!({
                "schema": OBSERVATION_SCHEMA, "task_id": task, "intent_key": key,
                "execution_id": "operator", "attempt": 0,
                "status": if executed { "success" } else { "error" },
                "executed": executed, "output": "verified by the operator from the ledger", "receipt_id": null,
            });
            let r = store.database().apply_atomic_transaction_guarded(
                &format!("operator-{}-{key}", std::process::id()),
                None, None, &[],
                &[chip_agent_state::feltdb::RecordPrecondition {
                    capability: OBSERVATION_COLLECTION.into(), key: k.clone(), require_absent: true, ..Default::default()
                }],
                &[chip_agent_state::feltdb::AtomicMutation { capability: OBSERVATION_COLLECTION.into(), key: k, value: Some(value) }],
                None,
            );
            match r {
                Ok(_) => println!("{}", json!({"reconciled": key, "executed": executed})),
                Err(e) => { eprintln!("reconcile: {e:?}"); std::process::exit(1) }
            }
        }
        _ => std::process::exit(2),
    }
}
