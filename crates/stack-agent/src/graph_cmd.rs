//! `chip init` and `chip graph`: build and show the architecture graph snapshot.
//!
//! The graph is a projection of source. These commands only read source (for `init`) and
//! read or write the snapshot cache under `.chip/graph/`; they never run anything.

use std::path::{Path, PathBuf};
use std::time::Instant;

use chip_graph::{
    ArchitectureGraph, CapabilityCatalog, Classification, GraphEdgeKind, GraphNodeKind,
    PatternReport, PatternTask, SelectLimits, SelectionStatus, StoreError, analyze_impact,
    analyze_with_patterns, capability_slice, capability_slice_for_impact, find_repository_root,
    read_latest, read_latest_patterns, render_selection, select_patterns, write_patterns,
    write_snapshot,
};

fn parse_root(args: &[String]) -> Result<PathBuf, String> {
    match args.iter().position(|a| a == "--root") {
        Some(i) => args
            .get(i + 1)
            .map(PathBuf::from)
            .ok_or_else(|| "--root needs a path".to_string()),
        None => {
            let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
            Ok(find_repository_root(&cwd).unwrap_or(cwd))
        }
    }
}

fn count(graph: &ArchitectureGraph, kind: GraphNodeKind) -> usize {
    graph.count_nodes(kind)
}

fn display_name(root: &Path) -> String {
    std::fs::canonicalize(root)
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| ".".to_string())
}

/// Returns the process exit code.
pub fn init(args: &[String]) -> i32 {
    let benchmark = args.iter().any(|a| a == "--benchmark");
    let root = match parse_root(args) {
        Ok(root) => root,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };
    let total = Instant::now();
    let (graph, patterns, stats) = match analyze_with_patterns(&root) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("stack-agent init failed: {e}");
            return 1;
        }
    };
    let serialize = Instant::now();
    let snapshot_size = graph.to_snapshot_json().len();
    let serialize_time = serialize.elapsed();
    let written = match write_snapshot(&root, &graph) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("stack-agent init failed: {e}");
            return 1;
        }
    };
    let patterns_written = match write_patterns(&root, &patterns) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("stack-agent init failed: {e}");
            return 1;
        }
    };
    let total_time = total.elapsed();
    let relative = written.strip_prefix(&root).unwrap_or(&written);

    println!("Chip Init");
    println!("=========");
    println!();
    println!("Repository: {}", display_name(&root));
    println!("Crates: {}", count(&graph, GraphNodeKind::Crate));
    println!("Modules: {}", count(&graph, GraphNodeKind::Module));
    println!("Files: {}", count(&graph, GraphNodeKind::File));
    println!("Symbols: {}", count(&graph, GraphNodeKind::Symbol));
    println!("Tests: {}", count(&graph, GraphNodeKind::TestSuite));
    println!("Binaries: {}", count(&graph, GraphNodeKind::BinaryTarget));
    println!("Config: {}", count(&graph, GraphNodeKind::ConfigSurface));
    println!();
    println!("Nodes: {}", graph.nodes.len());
    println!("Edges: {}", graph.edges.len());
    println!();
    println!("Snapshot: {}", graph.snapshot_id);
    println!("Written: {}", relative.display());
    println!();
    print_pattern_summary(
        &patterns,
        Some(
            patterns_written
                .strip_prefix(&root)
                .unwrap_or(&patterns_written),
        ),
    );
    println!();
    println!("The graph is a projection of source, not authority. Source, tests and evidence win.");
    if stats.unparsed_files > 0 {
        println!(
            "Note: {} Rust file(s) could not be parsed and contribute no symbols.",
            stats.unparsed_files
        );
    }
    if benchmark {
        println!();
        println!("Benchmark");
        println!("---------");
        println!("Files scanned: {}", stats.files_scanned);
        println!("Nodes: {}", graph.nodes.len());
        println!("Edges: {}", graph.edges.len());
        println!(
            "Parse time: {:.3} ms",
            stats.parse_time.as_secs_f64() * 1000.0
        );
        println!(
            "Graph construction time: {:.3} ms",
            stats.graph_time.as_secs_f64() * 1000.0
        );
        println!(
            "Serialization time: {:.3} ms",
            serialize_time.as_secs_f64() * 1000.0
        );
        println!(
            "Pattern collection time (inside the scan): {:.3} ms",
            stats.pattern_time.as_secs_f64() * 1000.0
        );
        println!("Total: {:.3} ms", total_time.as_secs_f64() * 1000.0);
        println!("Snapshot size: {snapshot_size} bytes");
        println!("Pattern report size: {} bytes", patterns.to_json().len());
    }
    0
}

fn print_pattern_summary(report: &PatternReport, written: Option<&Path>) {
    let candidates = report.candidates().count();
    println!(
        "Implementation patterns (Rust, deterministic; inferred from frequency, never enforced)"
    );
    println!("  Observations: {}", report.observations.len());
    println!(
        "  Patterns: {} ({} candidate conventions)",
        report.patterns.len(),
        candidates
    );
    println!(
        "  Coverage: {} of {} Rust files analyzed{}",
        report.coverage.files_analyzed,
        report.coverage.rust_files_discovered,
        if report.coverage.complete {
            "; complete".to_string()
        } else {
            format!(
                "; PARTIAL ({} skipped, {} failed to parse, {} unexpanded item macros)",
                report.coverage.skipped.len(),
                report.coverage.parse_failures.len(),
                report.coverage.unexpanded_item_macros
            )
        }
    );
    println!("  Exceptions: {}", report.exceptions.len());
    if let Some(written) = written {
        println!("  Written: {}", written.display());
    }
}

/// `stack-agent graph patterns select --task FILE [--json] [--root DIR]`: EXPERIMENTAL and read-only.
/// Which stored pattern observations bear on the files, symbols and families a task file names.
/// Prints the block a person or a model can read, or the selection as JSON. Nothing calls this from
/// `work`; it reads the stored graph and report, compares the report with the source, and changes
/// nothing. Exit 2 when the report is stale (and nothing is offered).
fn select(args: &[String]) -> i32 {
    let root = match parse_root(args) {
        Ok(root) => root,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };
    let Some(path) = args
        .iter()
        .position(|a| a == "--task")
        .and_then(|i| args.get(i + 1))
    else {
        eprintln!(
            "usage: stack-agent graph patterns select --task FILE [--json] [--max-bytes N] [--max-examples N] [--root DIR]"
        );
        return 2;
    };
    let task: PatternTask = match std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(task) => task,
        Err(e) => {
            eprintln!("cannot read the task file {path}: {e}");
            return 1;
        }
    };
    let graph = match read_latest(&root) {
        Ok(graph) => graph,
        Err(e) => {
            eprintln!("{e}; run `stack-agent init`");
            return 1;
        }
    };
    let (report, staleness) = match read_latest_patterns(&root) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("{e}; run `stack-agent init`");
            return 1;
        }
    };
    let number = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse::<usize>().ok())
    };
    let limits = SelectLimits {
        max_examples: number("--max-examples").unwrap_or(SelectLimits::default().max_examples),
        max_rendered_bytes: number("--max-bytes")
            .unwrap_or(SelectLimits::default().max_rendered_bytes),
        ..SelectLimits::default()
    };
    let selection = select_patterns(&task, &report, &staleness, &graph, &limits);
    if args.iter().any(|a| a == "--json") {
        println!(
            "{}",
            serde_json::to_string(&selection).expect("a selection serializes")
        );
    } else {
        print!("{}", render_selection(&selection, &limits));
    }
    if selection.status == SelectionStatus::Stale {
        2
    } else {
        0
    }
}

/// `stack-agent graph patterns [--json]`: the stored pattern report, compared with the source on
/// disk. Never rebuilds anything, and never presents a report as current without having compared it.
fn patterns(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("select") {
        return select(&args[1..]);
    }
    let root = match parse_root(args) {
        Ok(root) => root,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };
    let (report, staleness) = match read_latest_patterns(&root) {
        Ok(found) => found,
        Err(StoreError::NotFound) => {
            println!("No pattern report found.");
            println!("Run `stack-agent init`.");
            return 1;
        }
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    if args.iter().any(|a| a == "--json") {
        let value = serde_json::json!({
            "current": staleness.is_current(),
            "staleness": staleness,
            "report": report,
        });
        println!("{value}");
        return if staleness.is_current() { 0 } else { 2 };
    }
    println!("Implementation Patterns");
    println!("=======================");
    println!();
    println!("Report: {}", report.report_id);
    println!("Graph snapshot: {}", report.graph_snapshot);
    println!(
        "Analyzer: {} v{} (call classes are read from callee names)",
        report.analyzer.id, report.analyzer.version
    );
    println!();
    if staleness.is_current() {
        println!("Status: current (every analyzed file matches, and no new Rust file exists)");
    } else {
        println!(
            "Status: STALE: {} changed, {} removed, {} new. Run `stack-agent init`. Nothing below is current.",
            staleness.changed.len(),
            staleness.removed.len(),
            staleness.added.len()
        );
        for f in staleness
            .changed
            .iter()
            .chain(&staleness.removed)
            .chain(&staleness.added)
        {
            println!("  {f}");
        }
    }
    println!();
    print_pattern_summary(&report, None);
    println!();
    println!(
        "Candidate conventions (a count of at least {}, not a rule)",
        chip_graph::MIN_SUPPORT
    );
    for p in report.candidates() {
        println!(
            "  [{}] {} {:?}: {} supporting, {} counterexamples, {} file(s), {} crate(s)",
            format!("{:?}", p.category).to_lowercase(),
            p.family,
            p.signature,
            p.confidence.support,
            p.confidence.counterexamples,
            p.confidence.distinct_files,
            p.confidence.distinct_crates
        );
    }
    let observed = report
        .patterns
        .iter()
        .filter(|p| p.classification == Classification::Observed)
        .count();
    println!(
        "  (and {observed} observed structures seen fewer than {} times)",
        chip_graph::MIN_SUPPORT
    );
    if staleness.is_current() { 0 } else { 2 }
}

/// Returns the process exit code. Reads the stored snapshot; never rebuilds the graph.
pub fn graph(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("patterns") {
        return patterns(&args[1..]);
    }
    let root = match parse_root(args) {
        Ok(root) => root,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };
    let graph = match read_latest(&root) {
        Ok(graph) => graph,
        Err(StoreError::NotFound) => {
            println!("No architecture snapshot found.");
            println!("Run `stack-agent init`.");
            return 1;
        }
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    println!("Architecture Graph");
    println!("==================");
    println!();
    println!("Snapshot: {}", graph.snapshot_id);
    println!();
    println!("Nodes");
    println!("  Crates: {}", count(&graph, GraphNodeKind::Crate));
    println!("  Modules: {}", count(&graph, GraphNodeKind::Module));
    println!("  Files: {}", count(&graph, GraphNodeKind::File));
    println!("  Symbols: {}", count(&graph, GraphNodeKind::Symbol));
    println!("  Tests: {}", count(&graph, GraphNodeKind::TestSuite));
    println!("  Binaries: {}", count(&graph, GraphNodeKind::BinaryTarget));
    println!("  Config: {}", count(&graph, GraphNodeKind::ConfigSurface));
    println!();
    println!("Edges");
    let edge = |kind: GraphEdgeKind| graph.count_edges(kind);
    println!("  Contains: {}", edge(GraphEdgeKind::Contains));
    println!("  Imports: {}", edge(GraphEdgeKind::Imports));
    println!("  Defines: {}", edge(GraphEdgeKind::Defines));
    println!("  Implements: {}", edge(GraphEdgeKind::Implements));
    println!("  Tests: {}", edge(GraphEdgeKind::Tests));
    println!("  Targets: {}", edge(GraphEdgeKind::Targets));
    0
}

/// Options shared by the commands that read a stored snapshot plus a capability catalog.
struct CatalogArgs {
    catalog: Option<String>,
    root_args: Vec<String>,
    changed: Vec<String>,
    positional: Vec<String>,
}

fn parse_catalog_args(args: &[String], allow_changed: bool) -> CatalogArgs {
    let mut parsed = CatalogArgs {
        catalog: None,
        root_args: Vec::new(),
        changed: Vec::new(),
        positional: Vec::new(),
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--capabilities" => parsed.catalog = iter.next().cloned(),
            "--root" => {
                parsed.root_args.push(arg.clone());
                parsed.root_args.extend(iter.next().cloned());
            }
            "--changed" if allow_changed => parsed.changed.extend(iter.next().cloned()),
            _ => parsed.positional.push(arg.clone()),
        }
    }
    parsed
}

/// Reads the stored snapshot and the catalog, validated against it. Never runs `init`, never
/// infers or invents a catalog. On failure the message is printed and the exit code returned.
fn load_inputs(parsed: &CatalogArgs) -> Result<(ArchitectureGraph, CapabilityCatalog), i32> {
    let catalog_path = parsed.catalog.as_deref().unwrap_or_default();
    let root = parse_root(&parsed.root_args).map_err(|message| {
        eprintln!("{message}");
        1
    })?;
    let graph = match read_latest(&root) {
        Ok(graph) => graph,
        Err(StoreError::NotFound) => {
            println!("No architecture snapshot found.");
            println!("Run `stack-agent init`.");
            return Err(1);
        }
        Err(e) => {
            eprintln!("{e}");
            return Err(1);
        }
    };
    let text = match std::fs::read_to_string(catalog_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("Capability catalog not found:");
            println!("  {catalog_path}");
            return Err(1);
        }
        Err(e) => {
            eprintln!("cannot read {catalog_path}: {e}");
            return Err(1);
        }
    };
    match CapabilityCatalog::from_json(&text, &graph) {
        Ok(catalog) => Ok((graph, catalog)),
        Err(e) => {
            eprintln!("{catalog_path}: {e}");
            Err(1)
        }
    }
}

/// `chip impact --capabilities FILE [--root PATH] <path>...`
///
/// Reads the stored snapshot and an external capability catalog; never runs `init`, never
/// infers or invents a catalog. Returns the process exit code.
pub fn impact(args: &[String]) -> i32 {
    let usage = "usage: stack-agent impact --capabilities FILE [--root PATH] <changed path>...";
    let parsed = parse_catalog_args(args, false);
    if parsed.catalog.is_none() || parsed.positional.is_empty() {
        eprintln!("{usage}");
        return 2;
    }
    let (graph, catalog) = match load_inputs(&parsed) {
        Ok(inputs) => inputs,
        Err(code) => return code,
    };
    print!(
        "{}",
        analyze_impact(&graph, &catalog, &parsed.positional).render()
    );
    0
}

/// `chip slice --capabilities FILE [--root PATH] [--changed PATH]... <capability id>`
///
/// Prints the capability's relevant graph slice and its StateToken, from the stored snapshot.
/// Returns the process exit code.
pub fn slice(args: &[String]) -> i32 {
    let usage = "usage: stack-agent slice --capabilities FILE [--root PATH] [--changed PATH]... <capability id>";
    let parsed = parse_catalog_args(args, true);
    if parsed.catalog.is_none() || parsed.positional.len() != 1 {
        eprintln!("{usage}");
        return 2;
    }
    let (graph, catalog) = match load_inputs(&parsed) {
        Ok(inputs) => inputs,
        Err(code) => return code,
    };
    let id = &parsed.positional[0];
    let (slice, impact) = if parsed.changed.is_empty() {
        match capability_slice(&graph, &catalog, id) {
            Ok(slice) => (slice, None),
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        }
    } else {
        match capability_slice_for_impact(&graph, &catalog, id, &parsed.changed) {
            Ok(selection) => {
                let label = if selection.is_impacted() {
                    "impacted"
                } else {
                    "unchanged"
                };
                (selection.slice().clone(), Some(label))
            }
            Err(e) => {
                eprintln!("{e}");
                return 1;
            }
        }
    };
    print!("{}", slice.render(impact));
    if impact.is_some() {
        // "Chip could not map this change" is different from "this change has no impact".
        let unresolved = analyze_impact(&graph, &catalog, &parsed.changed).unresolved_paths;
        if !unresolved.is_empty() {
            println!("\nUnresolved:");
            for path in unresolved {
                println!("  {path}");
            }
        }
    }
    0
}
