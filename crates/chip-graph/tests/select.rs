//! The experimental pattern selector: deterministic, explainable, bounded, read-only, and honest
//! about what it cannot say. Uses the P0 fixture (`pattern_fixture`).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use chip_graph::{
    ArchitectureGraph, Classification, PatternReport, PatternTask, SelectLimits, Selection,
    SelectionStatus, Staleness, analyze_with_patterns, render_selection, select_patterns,
};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pattern_fixture")
}

fn copy_fixture(tag: &str) -> PathBuf {
    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for e in fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            let target = to.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy(&e.path(), &target);
            } else {
                fs::copy(e.path(), target).unwrap();
            }
        }
    }
    let dir = std::env::temp_dir().join(format!("chip-select-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    copy(&fixture(), &dir);
    dir
}

struct World {
    root: PathBuf,
    graph: ArchitectureGraph,
    report: PatternReport,
}

fn world(root: PathBuf) -> World {
    let (graph, report, _) = analyze_with_patterns(&root).unwrap();
    World {
        root,
        graph,
        report,
    }
}

impl World {
    fn select(&self, task: &PatternTask) -> Selection {
        let staleness = self.report.check(&self.root);
        select_patterns(
            task,
            &self.report,
            &staleness,
            &self.graph,
            &SelectLimits::default(),
        )
    }
}

fn task(files: &[&str], symbols: &[&str], families: &[&str]) -> PatternTask {
    PatternTask {
        id: "t".to_string(),
        files: files.iter().map(|s| s.to_string()).collect(),
        symbols: symbols.iter().map(|s| s.to_string()).collect(),
        families: families.iter().map(|s| s.to_string()).collect(),
        categories: Vec::new(),
    }
}

#[test]
fn a_file_the_task_names_selects_the_patterns_whose_observations_are_in_it() {
    let w = world(fixture());
    let s = w.select(&task(&["src/handlers.rs"], &[], &[]));
    assert_eq!(s.status, SelectionStatus::Selected, "{}", s.explanation);
    let p = s
        .patterns
        .iter()
        .find(|p| p.family == "Handler" && p.signature == ["handle", "validate"])
        .unwrap();
    assert_eq!(p.classification, Classification::CandidateConvention);
    assert!(
        p.reasons
            .iter()
            .any(|r| r.signal == "file" && r.evidence.contains("src/handlers.rs"))
    );
    // The task's own observations are named.
    let mine: BTreeSet<&str> = p.in_task.iter().map(|e| e.name.as_str()).collect();
    assert!(mine.contains("Alpha as Handler"));
    assert!(!p.enforced);
}

#[test]
fn supporting_examples_and_counterexamples_are_separate_and_the_family_is_shown_whole() {
    let w = world(fixture());
    let s = w.select(&task(&["src/handlers.rs"], &[], &[]));
    let p = s
        .patterns
        .iter()
        .find(|p| p.signature == ["handle", "validate"])
        .unwrap();
    let support: BTreeSet<&String> = p.supporting.iter().map(|e| &e.observation).collect();
    let counter: BTreeSet<&String> = p.counterexamples.iter().map(|e| &e.observation).collect();
    assert!(support.is_disjoint(&counter));
    assert!(
        p.supporting
            .iter()
            .all(|e| e.signature == ["handle", "validate"])
    );
    assert!(
        p.counterexamples
            .iter()
            .all(|e| e.signature != ["handle", "validate"])
    );
    let names: BTreeSet<&str> = p.counterexamples.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains("Delta as Handler") && names.contains("Old as Handler"));
    // The deprecated implementation says so.
    let old = p
        .counterexamples
        .iter()
        .find(|e| e.name == "Old as Handler")
        .unwrap();
    assert!(old.flags.contains(&"legacy".to_string()));
    // Every shape of the family is listed beside this one.
    assert!(p.family_shapes.len() >= 3);
    assert!(
        p.family_shapes
            .iter()
            .any(|sh| sh.signature == ["handle", "migrate", "validate"])
    );
}

#[test]
fn a_symbol_selects_by_subject_and_the_graph_edge_selects_the_trait_family() {
    let w = world(fixture());
    let alpha = "symbol:src/handlers.rs:handlers::Alpha";
    assert!(w.graph.nodes.iter().any(|n| n.id == alpha));
    let s = w.select(&task(&[], &[alpha], &[]));
    assert_eq!(s.status, SelectionStatus::Selected);
    let p = s.patterns.iter().find(|p| p.family == "Handler").unwrap();
    assert!(p.reasons.iter().any(|r| r.signal == "subject"));
    assert!(
        p.reasons
            .iter()
            .any(|r| r.signal == "implements" && r.evidence.contains("handlers::Handler"))
    );
}

#[test]
fn naming_a_family_selects_all_its_shapes_so_conflicting_examples_are_visible() {
    let w = world(fixture());
    let s = w.select(&task(&[], &[], &["Handler"]));
    let sigs: BTreeSet<Vec<String>> = s
        .patterns
        .iter()
        .filter(|p| p.family == "Handler")
        .map(|p| p.signature.clone())
        .collect();
    assert!(sigs.contains(&vec!["handle".to_string(), "validate".to_string()]));
    assert!(sigs.len() >= 3, "{sigs:?}");
    assert!(
        s.patterns
            .iter()
            .all(|p| p.reasons.iter().any(|r| r.signal == "family"))
    );
}

#[test]
fn a_test_that_mentions_a_task_symbol_is_a_verification_signal() {
    let w = world(fixture());
    let s = w.select(&task(&[], &["symbol:src/ops.rs:ops::create_user"], &[]));
    let tests: Vec<_> = s
        .patterns
        .iter()
        .filter(|p| p.category == chip_graph::PatternCategory::TestShape)
        .collect();
    assert!(
        !tests.is_empty(),
        "{:#?}",
        s.patterns
            .iter()
            .map(|p| (&p.family, &p.signature))
            .collect::<Vec<_>>()
    );
    assert!(tests[0].reasons.iter().any(|r| r.signal == "verification"));
    assert!(tests[0].in_task.iter().any(|e| e.name == "creates_a_user"));
}

#[test]
fn a_name_that_merely_looks_similar_selects_nothing() {
    let w = world(fixture());
    // The symbol's name contains `Error` and `Handler`, and `create_user` is nothing like them. No
    // pattern is selected by wording: only the facts about the named symbol count.
    let s = w.select(&task(&[], &["symbol:src/ops.rs:ops::create_user"], &[]));
    assert!(
        s.patterns
            .iter()
            .all(|p| p.family != "error_type" && p.family != "Handler")
    );
    // A task whose *prose* mentions Error and Handler but names no reference has nothing to go on:
    // `PatternTask` has no field for prose.
    let none = w.select(&task(&[], &[], &[]));
    assert_eq!(none.status, SelectionStatus::InsufficientEvidence);
    assert!(none.patterns.is_empty());
    assert!(none.explanation.contains("wording is not interpreted"));
}

#[test]
fn unknown_and_unanalyzed_references_are_reported_not_matched() {
    let w = world(fixture());
    let s = w.select(&task(
        &[
            "src/orphan.rs",
            "src/broken.rs",
            "vendor/dep/src/lib.rs",
            "src/nope.rs",
        ],
        &["symbol:src/nope.rs:Nope"],
        &[],
    ));
    assert_eq!(s.status, SelectionStatus::InsufficientEvidence);
    assert!(
        s.warnings
            .iter()
            .any(|w| w.contains("src/orphan.rs was not analyzed"))
    );
    assert!(
        s.warnings
            .iter()
            .any(|w| w.contains("src/broken.rs did not parse"))
    );
    assert!(
        s.warnings
            .iter()
            .any(|w| w.contains("vendor/dep/src/lib.rs was not analyzed: vendored tree"))
    );
    assert!(
        s.unknown_references
            .contains(&"file src/nope.rs".to_string())
    );
    assert!(
        s.unknown_references
            .contains(&"symbol symbol:src/nope.rs:Nope".to_string())
    );
}

#[test]
fn known_references_with_no_pattern_say_no_applicable_pattern_and_do_not_invent_one() {
    let w = world(fixture());
    // `src/lib.rs` is analyzed and holds only `mod` declarations: no observation.
    let s = w.select(&task(&["src/lib.rs"], &[], &[]));
    assert_eq!(s.status, SelectionStatus::NoApplicablePattern);
    assert!(s.patterns.is_empty());
    let text = render_selection(&s, &SelectLimits::default());
    assert!(text.contains("No applicable observed pattern was found"));
    assert!(text.contains("not an instruction to create one"));
}

#[test]
fn a_stale_report_is_never_offered_as_current() {
    let root = copy_fixture("stale");
    let w = world(root.clone());
    let before = w.select(&task(&["src/handlers.rs"], &[], &[]));
    assert_eq!(before.status, SelectionStatus::Selected);
    fs::write(root.join("src/handlers.rs"), "pub struct Changed;\n").unwrap();
    let after = w.select(&task(&["src/handlers.rs"], &[], &[]));
    assert_eq!(after.status, SelectionStatus::Stale);
    assert!(after.patterns.is_empty());
    assert_eq!(after.stale_files, ["src/handlers.rs"]);
    let text = render_selection(&after, &SelectLimits::default());
    assert!(text.contains("out of date") && text.contains("not offered"));
    // A new file also makes it stale.
    let root2 = copy_fixture("stale-added");
    let w2 = world(root2.clone());
    fs::write(root2.join("src/extra.rs"), "pub fn extra() {}\n").unwrap();
    assert_eq!(
        w2.select(&task(&["src/handlers.rs"], &[], &[])).status,
        SelectionStatus::Stale
    );
}

#[test]
fn a_report_from_another_graph_is_refused() {
    let w = world(fixture());
    let other = analyze_with_patterns(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture"))
        .unwrap()
        .0;
    let s = select_patterns(
        &task(&["src/handlers.rs"], &[], &[]),
        &w.report,
        &Staleness::default(),
        &other,
        &SelectLimits::default(),
    );
    assert_eq!(s.status, SelectionStatus::Stale);
    assert!(s.explanation.contains("different graph snapshot"));
}

#[test]
fn selection_is_deterministic_and_independent_of_the_order_the_task_lists_things_in() {
    let w = world(fixture());
    let a = w.select(&task(
        &["src/handlers.rs", "src/errors.rs", "src/ops.rs"],
        &[
            "symbol:src/ops.rs:ops::create_user",
            "symbol:src/handlers.rs:handlers::Alpha",
        ],
        &["Handler"],
    ));
    let b = w.select(&task(
        &["src/ops.rs", "src/handlers.rs", "src/errors.rs"],
        &[
            "symbol:src/handlers.rs:handlers::Alpha",
            "symbol:src/ops.rs:ops::create_user",
        ],
        &["Handler"],
    ));
    assert_eq!(a, b);
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&w.select(&task(
            &["src/handlers.rs", "src/errors.rs", "src/ops.rs"],
            &[
                "symbol:src/ops.rs:ops::create_user",
                "symbol:src/handlers.rs:handlers::Alpha"
            ],
            &["Handler"]
        )))
        .unwrap()
    );
}

#[test]
fn ranking_follows_the_signal_not_the_frequency() {
    let w = world(fixture());
    let s = w.select(&task(&["src/errors.rs"], &[], &["Handler"]));
    let scores: Vec<u32> = s.patterns.iter().map(|p| p.score).collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "{scores:?}");
    // The file signal (100) outranks the explicit family signal (60), whatever the support counts.
    assert_eq!(s.patterns[0].reasons[0].signal, "file");
    assert!(
        s.patterns
            .iter()
            .any(|p| p.family == "Handler" && p.score == 60)
    );
}

#[test]
fn the_result_is_bounded_in_patterns_examples_and_size() {
    let w = world(fixture());
    let limits = SelectLimits {
        max_patterns: 2,
        max_examples: 1,
        max_rendered_bytes: 700,
    };
    let staleness = w.report.check(&w.root);
    let t = task(
        &["src/handlers.rs", "src/errors.rs", "src/ops.rs"],
        &[],
        &["Handler"],
    );
    let s = select_patterns(&t, &w.report, &staleness, &w.graph, &limits);
    assert!(s.patterns.len() <= 2 && s.omitted_patterns > 0);
    assert!(
        s.patterns.iter().all(|p| p.supporting.len() <= 1
            && p.counterexamples.len() <= 1
            && p.in_task.len() <= 1)
    );
    let text = render_selection(&s, &limits);
    assert!(text.len() <= 700 + 200, "{}", text.len());
    assert!(text.contains("omitted to stay within the size limit") || text.len() <= 700);
}

#[test]
fn the_rendered_block_says_it_is_not_a_requirement_and_carries_no_source() {
    let w = world(fixture());
    let s = w.select(&task(&["src/handlers.rs", "src/ops.rs"], &[], &[]));
    let text = render_selection(&s, &SelectLimits::default());
    assert!(text.contains("They are NOT requirements"));
    assert!(text.contains("frequency is not correctness"));
    assert!(text.contains("different shape:"));
    for forbidden in [
        "fn handle(&self)",
        "validate_input(x)?",
        "must ",
        "required to",
        "mandatory",
    ] {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn every_reference_in_a_selection_resolves_in_the_report_and_the_graph() {
    let w = world(fixture());
    let s = w.select(&task(
        &["src/handlers.rs", "src/errors.rs", "src/ops.rs"],
        &["symbol:src/handlers.rs:handlers::Alpha"],
        &["Handler"],
    ));
    let obs: BTreeSet<&str> = w
        .report
        .observations
        .iter()
        .map(|o| o.id.as_str())
        .collect();
    let pats: BTreeSet<&str> = w.report.patterns.iter().map(|p| p.id.as_str()).collect();
    let files: BTreeSet<&str> = w.report.sources.keys().map(String::as_str).collect();
    assert_eq!(s.report_id, w.report.report_id);
    assert_eq!(s.graph_snapshot, w.graph.snapshot_id.to_string());
    for p in &s.patterns {
        assert!(pats.contains(p.pattern_id.as_str()));
        for e in p
            .in_task
            .iter()
            .chain(&p.supporting)
            .chain(&p.counterexamples)
        {
            assert!(obs.contains(e.observation.as_str()));
            assert!(files.contains(e.file.as_str()));
        }
        for sh in &p.family_shapes {
            assert!(pats.contains(sh.pattern_id.as_str()));
        }
    }
}

#[test]
fn the_selector_changes_nothing() {
    let w = world(fixture());
    let before = (w.report.clone(), w.graph.clone());
    let sources: Vec<(String, Vec<u8>)> = w
        .report
        .sources
        .keys()
        .map(|p| (p.clone(), fs::read(w.root.join(p)).unwrap()))
        .collect();
    let _ = w.select(&task(&["src/handlers.rs"], &[], &["Handler"]));
    assert_eq!(before, (w.report.clone(), w.graph.clone()));
    for (p, bytes) in sources {
        assert_eq!(fs::read(w.root.join(&p)).unwrap(), bytes);
    }
}
