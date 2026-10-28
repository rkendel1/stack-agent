//! **Experimental.** Entry point for Chip's single-symbol repair loop (`chip-symbol-loop`,
//! `docs/product/symbol-loop.md`). Not part of the product: an example, built only on request.
//!
//! ```text
//! cargo run -p stack-agent --example symbol_loop -- \
//!     --task task.json --worktree DIR --store FILE --run-id ID \
//!     --endpoint http://127.0.0.1:PORT/v1/chat/completions --model ID
//! ```
//!
//! Inference spending is structurally off: the endpoint must be a loopback address (an already
//! running local server), no API key is read or sent, and no other provider is ever tried.

use std::path::PathBuf;
use std::sync::Arc;

use chip_cli::provider_selection::{self, Selection};
use chip_symbol_loop::fx::ModelFx;
use chip_symbol_loop::machine::{Budgets, LoopConfig, TaskDefinition};
use chip_symbol_loop::store::LoopStore;

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn loopback(endpoint: &str) -> bool {
    let Some(rest) = endpoint.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or("");
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = |why: &str| -> ! {
        eprintln!("error: {why}");
        eprintln!(
            "usage: symbol_loop --task FILE --worktree DIR --store FILE --run-id ID --endpoint http://127.0.0.1:PORT/... --model ID [--namespace NS] [--experiment-id ID] [--max-decisions N] [--max-elapsed-secs N]"
        );
        std::process::exit(2)
    };
    let need =
        |name: &str| flag(&args, name).unwrap_or_else(|| usage(&format!("{name} is required")));
    let task: TaskDefinition = serde_json::from_str(
        &std::fs::read_to_string(need("--task"))
            .unwrap_or_else(|e| usage(&format!("cannot read the task: {e}"))),
    )
    .unwrap_or_else(|e| usage(&format!("the task does not parse: {e}")));
    let worktree = PathBuf::from(need("--worktree"));
    let endpoint = need("--endpoint");
    if !loopback(&endpoint) {
        usage(
            "the endpoint must be a local server at a loopback address; no hosted inference is used",
        );
    }
    let model = need("--model");
    let mut budgets = Budgets::default();
    if let Some(n) = flag(&args, "--max-decisions").and_then(|v| v.parse().ok()) {
        budgets.max_decisions = n;
    }
    if let Some(n) = flag(&args, "--max-elapsed-secs").and_then(|v| v.parse::<u64>().ok()) {
        budgets.max_elapsed_ms = n * 1000;
    }
    let cfg = LoopConfig {
        experiment_id: flag(&args, "--experiment-id").unwrap_or_else(|| "symbol-loop".into()),
        run_id: need("--run-id"),
        budgets,
        halt_after_transitions: None,
    };
    let store = LoopStore::open(
        need("--store"),
        &flag(&args, "--namespace").unwrap_or_else(|| "symbol-loop".into()),
    )
    .unwrap_or_else(|e| usage(&e.to_string()));
    // No key is read: the selection names the local server and nothing else.
    let selection = Selection {
        provider: Some("openai-compatible".into()),
        model: Some(model.clone()),
        endpoint: Some(endpoint),
        executor: None,
        ..Selection::default()
    };
    let no_env = |_: &str| None::<String>;
    let config = provider_selection::resolve(&selection, no_env)
        .unwrap_or_else(|e| usage(&format!("no local model: {e}")));
    let provider =
        provider_selection::build_model(config, provider_selection::ExecutorKind::Fx, &no_env)
            .unwrap_or_else(|e| usage(&format!("the local provider is unusable: {e}")));
    let fx = ModelFx {
        provider: Arc::clone(&provider),
        model,
        max_tokens: 2048,
    };
    match chip_symbol_loop::run(&task, &worktree, &store, &fx, &cfg).await {
        Ok(result) => {
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
            let pass = result
                .termination
                .as_ref()
                .is_some_and(|t| t.class == "pass");
            std::process::exit(if pass { 0 } else { 1 });
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(4);
        }
    }
}
