//! Read-only diagnostic: run an acceptance command in a checkout and print Chip's target ranking
//! and decision, with every score's reasons. Edits nothing and makes no model call.
//!
//! `cargo run -p chip-symbol-loop --example rank_target -- DIR CMD [ARGS...]`

use std::path::Path;
use std::time::{Duration, Instant};

use chip_symbol_loop::acceptance::{self, RequiredEvidence};
use chip_symbol_loop::symbols::SymbolIndex;
use chip_symbol_loop::target;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((dir, command)) = args.split_first().filter(|(_, c)| !c.is_empty()) else {
        eprintln!("usage: rank_target DIR CMD [ARGS...]");
        std::process::exit(2);
    };
    let root = Path::new(dir);
    let run = acceptance::run(
        root,
        command,
        Duration::from_secs(600),
        &RequiredEvidence::ExitZero,
    );
    let started = Instant::now();
    let index = SymbolIndex::build(root).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(3)
    });
    let indexed_ms = started.elapsed().as_millis() as u64;
    let ranking = target::rank(&index, &run);
    let ranked_ms = started.elapsed().as_millis() as u64 - indexed_ms;
    let out = serde_json::json!({
        "acceptance": {
            "exit_status": run.exit_status,
            "passed": run.passed,
            "timed_out": run.timed_out,
            "diagnostics": run.diagnostics.iter().map(|d| serde_json::json!({
                "kind": d.kind, "path": d.path, "line": d.line, "test": d.test,
            })).collect::<Vec<_>>(),
        },
        "cost": {"index_ms": indexed_ms, "rank_ms": ranked_ms, "symbols": index.symbols.len()},
        "decision": target::decide(&ranking),
        "ranking": ranking.summary(usize::MAX),
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
