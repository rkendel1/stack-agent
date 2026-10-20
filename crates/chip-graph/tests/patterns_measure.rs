//! Measurements for the pattern foundation, on a real repository. Not part of the ordinary run:
//!
//! ```text
//! PATTERN_REPO=/path/to/repo cargo test --release -p chip-graph --test patterns_measure \
//!     -- --ignored --nocapture
//! ```
//!
//! `PATTERN_MODE=with|without` runs only the timing loop in one mode, so the process can be
//! measured with `/usr/bin/time -l` for peak memory.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chip_graph::{
    Classification, GraphNodeKind, PatternCategory, analyze_with_patterns, analyze_with_stats,
};

fn repo() -> PathBuf {
    PathBuf::from(std::env::var("PATTERN_REPO").expect("PATTERN_REPO"))
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().into_owned();
        if [".git", "target", ".chip", "node_modules"].contains(&name.as_str()) {
            continue;
        }
        let target = to.join(&name);
        let ty = e.file_type().unwrap();
        if ty.is_symlink() {
            continue;
        }
        if ty.is_dir() {
            copy_tree(&e.path(), &target);
        } else {
            fs::copy(e.path(), target).unwrap();
        }
    }
}

#[test]
#[ignore = "measures a real repository"]
fn measure() {
    let root = repo();
    let n: usize = std::env::var("PATTERN_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    match std::env::var("PATTERN_MODE").as_deref() {
        Ok("with") => {
            for _ in 0..n {
                std::hint::black_box(analyze_with_patterns(&root).unwrap());
            }
            return;
        }
        Ok("without") => {
            for _ in 0..n {
                std::hint::black_box(analyze_with_stats(&root).unwrap());
            }
            return;
        }
        _ => {}
    }

    // What was found.
    let (graph, report, stats) = analyze_with_patterns(&root).unwrap();
    println!("== graph");
    for kind in GraphNodeKind::ALL {
        println!("  {kind:?}: {}", graph.count_nodes(kind));
    }
    println!("  nodes {} edges {}", graph.nodes.len(), graph.edges.len());
    println!(
        "  imports edges {}",
        graph.count_edges(chip_graph::GraphEdgeKind::Imports)
    );
    println!("== coverage");
    let c = &report.coverage;
    println!(
        "  rust files discovered {} analyzed {} skipped {} parse failures {} unexpanded item macros {} complete {}",
        c.rust_files_discovered,
        c.files_analyzed,
        c.skipped.len(),
        c.parse_failures.len(),
        c.unexpanded_item_macros,
        c.complete
    );
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    for s in &c.skipped {
        *reasons.entry(&s.reason).or_default() += 1;
    }
    for (r, k) in reasons {
        println!("    skipped: {k} x {r}");
    }
    println!(
        "  graph parse failures (unparsed_files) {}",
        stats.unparsed_files
    );
    println!("== observations and patterns");
    for cat in [
        PatternCategory::TraitImpl,
        PatternCategory::ErrorType,
        PatternCategory::TestShape,
        PatternCategory::CallOrder,
    ] {
        let obs = report
            .observations
            .iter()
            .filter(|o| o.category == cat)
            .count();
        let pats: Vec<_> = report
            .patterns
            .iter()
            .filter(|p| p.category == cat)
            .collect();
        let cands = pats
            .iter()
            .filter(|p| p.classification == Classification::CandidateConvention)
            .count();
        println!(
            "  {cat:?}: {obs} observations, {} patterns, {cands} candidates",
            pats.len()
        );
    }
    let mut exceptions: BTreeMap<&str, usize> = BTreeMap::new();
    for e in &report.exceptions {
        *exceptions.entry(&e.kind).or_default() += 1;
    }
    println!("  exceptions: {exceptions:?}");
    let flagged = |f: &str| {
        report
            .observations
            .iter()
            .filter(|o| o.flags.iter().any(|x| x == f))
            .count()
    };
    println!("  legacy-flagged observations {}", flagged("legacy"));
    let with_mentions = report
        .observations
        .iter()
        .filter(|o| o.category == PatternCategory::TestShape && !o.mentions.is_empty())
        .count();
    let tests = report
        .observations
        .iter()
        .filter(|o| o.category == PatternCategory::TestShape)
        .count();
    println!("  tests with at least one mentioned symbol: {with_mentions} of {tests}");
    println!("== candidates with counterexamples (support / counterexamples / files / crates)");
    let mut divergent: Vec<_> = report
        .candidates()
        .filter(|p| p.confidence.counterexamples > 0 && p.category != PatternCategory::TestShape)
        .collect();
    divergent.sort_by_key(|p| std::cmp::Reverse(p.confidence.support));
    for p in divergent.iter().take(12) {
        println!(
            "  {:?} {} {:?}: {} / {} / {} / {}",
            p.category,
            p.family,
            p.signature,
            p.confidence.support,
            p.confidence.counterexamples,
            p.confidence.distinct_files,
            p.confidence.distinct_crates
        );
    }
    println!("== sizes");
    println!("  graph snapshot {} bytes", graph.to_snapshot_json().len());
    println!(
        "  pattern report {} bytes ({} bytes of it source digests)",
        report.to_json().len(),
        report
            .sources
            .iter()
            .map(|(k, v)| k.len() + v.len())
            .sum::<usize>()
    );

    // Determinism.
    let ids: Vec<String> = (0..3)
        .map(|_| analyze_with_patterns(&root).unwrap().1.report_id)
        .collect();
    println!(
        "== determinism: 3 runs, ids equal = {}",
        ids.iter().all(|i| *i == ids[0])
    );

    // Time.
    let (mut without, mut with, mut collect) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..n {
        let t = Instant::now();
        analyze_with_stats(&root).unwrap();
        without.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let (_, _, s) = analyze_with_patterns(&root).unwrap();
        with.push(t.elapsed().as_secs_f64() * 1000.0);
        collect.push(s.pattern_time.as_secs_f64() * 1000.0);
    }
    println!("== time ({n} alternating runs, release)");
    println!("  graph only: median {:.1} ms", median(without.clone()));
    println!("  graph + patterns: median {:.1} ms", median(with.clone()));
    println!(
        "  of which pattern collection and report: median {:.1} ms",
        median(collect)
    );

    // A source change, on a copy.
    let copy = std::env::temp_dir().join(format!("chip-patterns-measure-{}", std::process::id()));
    let _ = fs::remove_dir_all(&copy);
    copy_tree(&root, &copy);
    let (_, before, _) = analyze_with_patterns(&copy).unwrap();
    assert!(before.check(&copy).is_current());
    // Add a fourth three-method CapabilityProvider impl and drop one file's content.
    let target = copy.join("crates/chip-core/src/capability_set.rs");
    let text = fs::read_to_string(&target).unwrap();
    fs::write(&target, format!("{text}\npub struct Probe;\n#[async_trait::async_trait]\nimpl CapabilityProvider for Probe {{\n    async fn capabilities(&self) -> Result<Vec<CapabilityDescriptor>, CapabilityError> {{ Ok(vec![]) }}\n    async fn availability(&self, _id: &CapabilityId) -> CapabilityAvailability {{ CapabilityAvailability::Available }}\n    async fn validate_inputs(&self, _id: &CapabilityId, _i: &std::collections::BTreeMap<String, InputValue>) -> Result<(), CapabilityError> {{ Ok(()) }}\n}}\n")).unwrap();
    let st = before.check(&copy);
    println!("== after editing one file");
    println!(
        "  stale: changed {:?} removed {:?} added {:?}",
        st.changed, st.removed, st.added
    );
    let (_, after, _) = analyze_with_patterns(&copy).unwrap();
    let find = |r: &chip_graph::PatternReport, sig: &[&str]| {
        r.patterns
            .iter()
            .find(|p| p.family == "CapabilityProvider" && p.signature == sig)
            .map(|p| p.confidence.support)
    };
    let three = ["availability", "capabilities", "validate_inputs"];
    println!(
        "  CapabilityProvider {three:?} support: {:?} -> {:?}",
        find(&before, &three),
        find(&after, &three)
    );
    println!(
        "  observations {} -> {}; report ids differ: {}",
        before.observations.len(),
        after.observations.len(),
        before.report_id != after.report_id
    );
    let ids_before: std::collections::BTreeSet<_> =
        before.observations.iter().map(|o| &o.id).collect();
    let ids_after: std::collections::BTreeSet<_> =
        after.observations.iter().map(|o| &o.id).collect();
    println!(
        "  observation ids kept {} added {} removed {}",
        ids_before.intersection(&ids_after).count(),
        ids_after.difference(&ids_before).count(),
        ids_before.difference(&ids_after).count()
    );
    assert!(after.check(&copy).is_current());
    let _ = fs::remove_dir_all(&copy);
}
