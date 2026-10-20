//! Implementation-pattern observations: deterministic, provenance-backed, conservative.
//!
//! The fixture (`pattern_fixture`, a small Rust crate that is data, not part of the workspace) holds
//! known structures and known exceptions: three `Handler` impls of one shape and two that diverge
//! (one deprecated), three `*Error` types of one shape and one that diverges, validate-before-write
//! functions in several orders and conditions, tests, a file that does not parse, a vendored copy
//! and an unreachable file.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use chip_graph::{
    Classification, MIN_SUPPORT, Observation, Pattern, PatternCategory, PatternReport, analyze,
    analyze_with_patterns, read_latest_patterns, write_patterns,
};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/pattern_fixture")
}

/// A private copy of the fixture, so a test can change it.
fn copy_fixture(tag: &str) -> PathBuf {
    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let dir = std::env::temp_dir().join(format!("chip-patterns-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    copy(&fixture(), &dir);
    dir
}

fn report(root: &Path) -> PatternReport {
    analyze_with_patterns(root).unwrap().1
}

fn pattern<'a>(
    r: &'a PatternReport,
    category: PatternCategory,
    family: &str,
    sig: &[&str],
) -> &'a Pattern {
    r.patterns
        .iter()
        .find(|p| p.category == category && p.family == family && p.signature == sig)
        .unwrap_or_else(|| {
            panic!(
                "no pattern {family} {sig:?}; have {:#?}",
                r.patterns
                    .iter()
                    .map(|p| (&p.family, &p.signature))
                    .collect::<Vec<_>>()
            )
        })
}

fn obs<'a>(r: &'a PatternReport, id: &str) -> &'a Observation {
    r.observations.iter().find(|o| o.id == id).unwrap()
}

fn names(r: &PatternReport, ids: &[String]) -> Vec<String> {
    let mut v: Vec<String> = ids.iter().map(|i| obs(r, i).name.clone()).collect();
    v.sort();
    v
}

// ---------------------------------------------------------------------------------------------
// Each structural pattern
// ---------------------------------------------------------------------------------------------

#[test]
fn a_recurring_trait_impl_shape_is_found_with_its_implementations_and_its_exceptions() {
    let r = report(&fixture());
    let p = pattern(
        &r,
        PatternCategory::TraitImpl,
        "Handler",
        &["handle", "validate"],
    );
    // Three impls declare the methods in different orders: the signature is the set, sorted.
    assert_eq!(
        names(&r, &p.support),
        ["Alpha as Handler", "Beta as Handler", "Gamma as Handler"]
    );
    assert_eq!(p.classification, Classification::CandidateConvention);
    // The divergent impls are kept as counterexamples, not dropped.
    assert_eq!(
        names(&r, &p.counterexamples),
        ["Delta as Handler", "Old as Handler"]
    );
    assert_eq!((p.confidence.support, p.confidence.counterexamples), (3, 2));
    assert_eq!(p.confidence.share_permille, 600);
    // The deprecated implementation is flagged legacy.
    let old = r
        .observations
        .iter()
        .find(|o| o.name == "Old as Handler")
        .unwrap();
    assert!(old.flags.contains(&"legacy".to_string()));
    assert_eq!(old.signature, ["handle", "migrate", "validate"]);
}

#[test]
fn the_vendored_copy_is_not_part_of_the_repositorys_own_patterns() {
    let r = report(&fixture());
    assert!(
        r.observations
            .iter()
            .all(|o| !o.file.starts_with("vendor/"))
    );
    // Had it been counted, `Handler [handle]` would have three supporters from vendor/dep.
    assert!(r.patterns.iter().all(|p| !(p.family == "Handler"
        && p.signature == ["handle"]
        && p.confidence.support >= 3)));
    let skipped = r
        .coverage
        .skipped
        .iter()
        .find(|s| s.path == "vendor/dep/src/lib.rs")
        .unwrap();
    assert_eq!(skipped.reason, "vendored tree");
}

#[test]
fn error_types_are_matched_to_their_trait_impls_within_a_crate() {
    let r = report(&fixture());
    let p = pattern(
        &r,
        PatternCategory::ErrorType,
        "error_type",
        &["derive:Debug", "impl:Display", "impl:Error"],
    );
    assert_eq!(
        names(&r, &p.support),
        [
            "errors::AuthError",
            "errors::ParseError",
            "errors::StoreError"
        ]
    );
    assert_eq!(p.classification, Classification::CandidateConvention);
    assert_eq!(names(&r, &p.counterexamples), ["errors::LegacyError"]);
    let legacy = r
        .observations
        .iter()
        .find(|o| o.name == "errors::LegacyError")
        .unwrap();
    assert_eq!(legacy.signature, ["derive:Debug", "impl:Display"]);
}

#[test]
fn test_shapes_record_the_attribute_asyncness_and_placement_and_what_a_test_mentions() {
    let r = report(&fixture());
    let p = pattern(
        &r,
        PatternCategory::TestShape,
        "test_shape",
        &["attr:tokio::test", "async", "returns_unit", "integration"],
    );
    assert_eq!(p.confidence.support, 3);
    assert_eq!(p.classification, Classification::CandidateConvention);
    let plain = pattern(
        &r,
        PatternCategory::TestShape,
        "test_shape",
        &["attr:test", "sync", "returns_unit", "integration"],
    );
    assert_eq!(plain.confidence.support, 1);
    assert_eq!(plain.classification, Classification::Observed);
    // `creates_a_user` names `create_user`, which is a symbol of the graph: a mention, recorded as
    // one, and not as coverage.
    let t = r
        .observations
        .iter()
        .find(|o| o.name == "creates_a_user")
        .unwrap();
    assert_eq!(t.mentions, ["symbol:src/ops.rs:ops::create_user"]);
    assert!(
        r.observations
            .iter()
            .filter(|o| o.category == PatternCategory::TestShape && o.name != "creates_a_user")
            .all(|o| o.mentions.is_empty())
    );
    assert!(p.caveats.iter().any(|c| c.contains("not coverage")));
}

// ---------------------------------------------------------------------------------------------
// Ordering, and the difference between co-occurrence and a proven relationship
// ---------------------------------------------------------------------------------------------

#[test]
fn validation_before_a_write_is_found_where_the_parser_establishes_the_order() {
    let r = report(&fixture());
    let before = pattern(
        &r,
        PatternCategory::CallOrder,
        "call_order",
        &["validate", "effect"],
    );
    assert_eq!(
        names(&r, &before.support),
        [
            "ops::create_item",
            "ops::create_nested",
            "ops::create_order",
            "ops::create_user"
        ],
        "including the call nested in an argument, which Rust evaluates first"
    );
    assert_eq!(before.classification, Classification::CandidateConvention);
    // The reverse order is its own observation and a counterexample, not an error to correct.
    let after = pattern(
        &r,
        PatternCategory::CallOrder,
        "call_order",
        &["effect", "validate"],
    );
    assert_eq!(names(&r, &after.support), ["ops::legacy_import"]);
    assert_eq!(names(&r, &before.counterexamples), ["ops::legacy_import"]);
    assert_eq!(after.classification, Classification::Observed);
}

#[test]
fn a_validation_that_may_not_run_is_not_claimed_to_precede_the_write() {
    let r = report(&fixture());
    for name in ["maybe_validated", "deferred_validation"] {
        // Both calls are in the function, and the order between them is not established.
        assert!(
            r.observations
                .iter()
                .all(|o| !(o.category == PatternCategory::CallOrder
                    && o.name == format!("ops::{name}"))),
            "{name} must not appear as an ordered pair"
        );
        let e = r
            .exceptions
            .iter()
            .find(|e| {
                e.kind == "order_not_established" && e.detail.contains(&format!("ops::{name}"))
            })
            .unwrap_or_else(|| panic!("{name} is recorded as an exception"));
        assert_eq!(e.path, "src/ops.rs");
    }
}

#[test]
fn call_classes_say_they_come_from_names_and_what_names() {
    let r = report(&fixture());
    let o = r
        .observations
        .iter()
        .find(|o| o.name == "ops::create_user")
        .unwrap();
    assert!(o.flags.contains(&"name_based".to_string()));
    assert!(
        o.flags
            .contains(&"ordering_in_top_level_statements".to_string())
    );
    assert_eq!(r.analyzer.validate_prefixes, ["validate", "verify"]);
    assert!(r.analyzer.effect_names.contains(&"write".to_string()));
    let p = pattern(
        &r,
        PatternCategory::CallOrder,
        "call_order",
        &["validate", "effect"],
    );
    assert!(p.caveats.iter().any(|c| c.contains("callee names only")));
}

// ---------------------------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------------------------

#[test]
fn the_same_structure_in_two_places_is_two_observations_of_one_pattern() {
    let r = report(&fixture());
    let p = pattern(
        &r,
        PatternCategory::TraitImpl,
        "Handler",
        &["handle", "validate"],
    );
    let ids: BTreeSet<&String> = p.support.iter().collect();
    assert_eq!(ids.len(), 3, "distinct observation ids");
    assert!(p.id.len() == 16 && p.id.chars().all(|c| c.is_ascii_hexdigit()));
    // Observation ids are unique across the report.
    let all: BTreeSet<&str> = r.observations.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(all.len(), r.observations.len());
}

#[test]
fn moving_code_changes_its_line_and_not_its_identity() {
    let root = copy_fixture("move");
    let before = report(&root);
    let ops = root.join("src/ops.rs");
    let text = fs::read_to_string(&ops).unwrap();
    fs::write(&ops, format!("// a comment\n\n\n{text}")).unwrap();
    let after = report(&root);
    let by_id = |r: &PatternReport| -> Vec<(String, usize)> {
        r.observations
            .iter()
            .filter(|o| o.file == "src/ops.rs")
            .map(|o| (o.id.clone(), o.line))
            .collect()
    };
    let (b, a) = (by_id(&before), by_id(&after));
    assert_eq!(
        b.iter().map(|x| &x.0).collect::<Vec<_>>(),
        a.iter().map(|x| &x.0).collect::<Vec<_>>(),
        "same observation ids"
    );
    assert!(
        b.iter().zip(&a).all(|(x, y)| y.1 == x.1 + 3),
        "lines follow the code"
    );
    assert_eq!(
        before.patterns.iter().map(|p| &p.id).collect::<Vec<_>>(),
        after.patterns.iter().map(|p| &p.id).collect::<Vec<_>>()
    );
    // The report itself is different: it describes different bytes.
    assert_ne!(before.report_id, after.report_id);
}

#[test]
fn identical_structure_under_one_name_in_one_file_is_numbered_not_merged() {
    let root = copy_fixture("dupes");
    // Two impls of one trait for one type that differ only by a generic argument.
    fs::write(
        root.join("src/handlers.rs"),
        "pub trait Conv<T> { fn conv(&self); }\npub struct S;\nimpl Conv<u8> for S { fn conv(&self) {} }\nimpl Conv<u16> for S { fn conv(&self) {} }\n",
    )
    .unwrap();
    let r = report(&root);
    let conv: Vec<&Observation> = r
        .observations
        .iter()
        .filter(|o| o.family == "Conv")
        .collect();
    assert_eq!(conv.len(), 2);
    assert_ne!(conv[0].id, conv[1].id);
    assert!(conv.iter().any(|o| o.id.ends_with("#2")));
    // And the pattern counts both.
    assert_eq!(
        pattern(&r, PatternCategory::TraitImpl, "Conv", &["conv"])
            .confidence
            .support,
        2
    );
}

// ---------------------------------------------------------------------------------------------
// Incomplete analysis is said, not hidden
// ---------------------------------------------------------------------------------------------

#[test]
fn skipped_unparsed_and_unreachable_files_are_listed_and_coverage_is_not_claimed_complete() {
    let r = report(&fixture());
    assert!(!r.coverage.complete);
    assert_eq!(r.coverage.parse_failures, ["src/broken.rs"]);
    assert!(
        r.exceptions
            .iter()
            .any(|e| e.kind == "parse_failure" && e.path == "src/broken.rs")
    );
    let reason = |path: &str| {
        r.coverage
            .skipped
            .iter()
            .find(|s| s.path == path)
            .map(|s| s.reason.clone())
    };
    assert_eq!(
        reason("src/orphan.rs").unwrap(),
        "not reachable from a crate root, so the graph did not parse it"
    );
    assert_eq!(reason("vendor/dep/src/lib.rs").unwrap(), "vendored tree");
    // Every discovered Rust file is accounted for: analyzed, failed, or skipped with a reason.
    let accounted =
        r.coverage.files_analyzed + r.coverage.parse_failures.len() + r.coverage.skipped.len();
    assert_eq!(accounted, r.coverage.rust_files_discovered);
    assert!(r.coverage.ignore_rules.contains(".gitignore is not read"));
    // A file that did not parse contributes no observation.
    assert!(r.observations.iter().all(|o| o.file != "src/broken.rs"));
}

#[test]
fn a_repository_with_nothing_missing_says_it_is_complete() {
    let root = copy_fixture("complete");
    fs::remove_dir_all(root.join("vendor")).unwrap();
    fs::remove_file(root.join("src/broken.rs")).unwrap();
    fs::remove_file(root.join("src/orphan.rs")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub mod errors;\npub mod handlers;\npub mod ops;\n",
    )
    .unwrap();
    let r = report(&root);
    assert!(r.coverage.complete, "{:#?}", r.coverage);
    assert!(r.coverage.skipped.is_empty() && r.coverage.parse_failures.is_empty());
}

#[test]
fn item_macros_are_counted_because_what_they_define_is_invisible() {
    let root = copy_fixture("macros");
    fs::write(
        root.join("src/lib.rs"),
        "pub mod errors;\npub mod handlers;\npub mod ops;\nmacro_rules! m { () => {}; }\nm!();\n",
    )
    .unwrap();
    let r = report(&root);
    assert_eq!(
        r.coverage.unexpanded_item_macros, 2,
        "the definition and the use"
    );
    assert!(!r.coverage.complete);
}

// ---------------------------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------------------------

#[test]
fn the_support_threshold_is_exact_and_a_candidate_is_never_a_rule() {
    // A structure seen once or twice is observed; three times is a candidate; neither is enforced.
    let root = copy_fixture("threshold");
    fs::write(
        root.join("src/handlers.rs"),
        "pub trait T { fn f(&self); }\npub struct A; pub struct B; pub struct C;\nimpl T for A { fn f(&self) {} }\nimpl T for B { fn f(&self) {} }\n",
    )
    .unwrap();
    let two = report(&root);
    let p = pattern(&two, PatternCategory::TraitImpl, "T", &["f"]);
    assert_eq!(
        (p.confidence.support, p.classification),
        (2, Classification::Observed)
    );
    let text = fs::read_to_string(root.join("src/handlers.rs")).unwrap();
    fs::write(
        root.join("src/handlers.rs"),
        format!("{text}impl T for C {{ fn f(&self) {{}} }}\n"),
    )
    .unwrap();
    let three = report(&root);
    let p = pattern(&three, PatternCategory::TraitImpl, "T", &["f"]);
    assert_eq!(
        (p.confidence.support, p.classification),
        (MIN_SUPPORT, Classification::CandidateConvention)
    );

    for r in [&two, &three, &report(&fixture())] {
        for p in &r.patterns {
            assert!(!p.enforced, "{} {:?}", p.family, p.signature);
            assert_eq!(p.basis, "deterministic");
            assert_eq!(
                p.authority,
                match p.classification {
                    Classification::CandidateConvention => "inferred_from_frequency",
                    Classification::Observed => "observed",
                }
            );
            assert!(
                p.caveats
                    .iter()
                    .any(|c| c.contains("not proof that it is correct or required"))
            );
        }
        // The serialized report has no way to say policy, rule or requirement.
        let json = r.to_json();
        for forbidden in [
            "\"policy\"",
            "\"rule\"",
            "\"required\"",
            "\"mandatory\"",
            "\"promoted\"",
        ] {
            assert!(!json.contains(forbidden), "{forbidden}");
        }
        assert!(json.contains("\"enforced\": false"));
    }
}

#[test]
fn a_common_pattern_is_not_marked_mandatory_however_often_it_occurs() {
    let root = copy_fixture("common");
    let mut src = String::from("pub trait T { fn f(&self); }\n");
    for i in 0..40 {
        src.push_str(&format!(
            "pub struct S{i};\nimpl T for S{i} {{ fn f(&self) {{}} }}\n"
        ));
    }
    fs::write(root.join("src/handlers.rs"), src).unwrap();
    let r = report(&root);
    let p = pattern(&r, PatternCategory::TraitImpl, "T", &["f"]);
    assert_eq!(p.confidence.support, 40);
    assert_eq!(p.confidence.share_permille, 1000);
    assert_eq!(p.classification, Classification::CandidateConvention);
    assert!(!p.enforced);
}

// ---------------------------------------------------------------------------------------------
// Repeatability, source changes, staleness
// ---------------------------------------------------------------------------------------------

#[test]
fn an_unchanged_repository_gives_the_same_report_every_time() {
    let a = report(&fixture());
    let b = report(&fixture());
    assert_eq!(a, b);
    assert_eq!(a.to_json(), b.to_json());
    assert_eq!(a.report_id, b.report_id);
    assert!(a.is_consistent());
    // A copy at another path is the same report: nothing depends on where the repository is.
    let copy = report(&copy_fixture("elsewhere"));
    assert_eq!(a.report_id, copy.report_id);
}

#[test]
fn a_changed_source_file_is_detected_and_the_new_report_differs_where_the_code_does() {
    let root = copy_fixture("change");
    let (graph, before, _) = analyze_with_patterns(&root).unwrap();
    write_patterns(&root, &before).unwrap();
    let (stored, staleness) = read_latest_patterns(&root).unwrap();
    assert_eq!(stored, before);
    assert!(staleness.is_current());
    assert_eq!(stored.graph_snapshot, graph.snapshot_id);

    // Remove the validation from one function.
    let ops = root.join("src/ops.rs");
    let text = fs::read_to_string(&ops).unwrap();
    fs::write(
        &ops,
        text.replacen(
            "    validate_input(x)?;\n    write(x);\n    Ok(())\n",
            "    write(x);\n    Ok(())\n",
            1,
        ),
    )
    .unwrap();

    // The stored report is read back with the verdict that it is stale, and which file.
    let (stale, staleness) = read_latest_patterns(&root).unwrap();
    assert!(!staleness.is_current());
    assert_eq!(staleness.changed, ["src/ops.rs"]);
    assert_eq!(
        stale.report_id, before.report_id,
        "the stored bytes are what they were"
    );
    // Which observations and patterns the change touches: those in the changed file, and the
    // patterns resting on them. Everything else is as true as it was.
    let (gone, patterns) = stale.invalidated(&staleness);
    assert!(!gone.is_empty() && gone.iter().all(|o| o.file == "src/ops.rs"));
    assert!(patterns.iter().any(|p| p.family == "call_order"));
    assert!(
        patterns
            .iter()
            .all(|p| p.family != "Handler" && p.family != "error_type")
    );
    assert!(
        stale.observations.len() > gone.len(),
        "observations in other files are not invalidated"
    );

    // Analyzing again reflects the change: one fewer validate-then-effect, one more effect-only
    // function (which has no ordered pair at all).
    let after = report(&root);
    let p = |r: &PatternReport| {
        pattern(
            r,
            PatternCategory::CallOrder,
            "call_order",
            &["validate", "effect"],
        )
        .confidence
        .support
    };
    assert_eq!((p(&before), p(&after)), (4, 3));
    assert!(
        after
            .observations
            .iter()
            .all(|o| o.name != "ops::create_user" || o.category != PatternCategory::CallOrder)
    );
    assert_ne!(before.report_id, after.report_id);
    assert!(after.check(&root).is_current());
}

#[test]
fn added_and_removed_files_make_a_stored_report_stale() {
    let root = copy_fixture("addremove");
    let r = report(&root);
    write_patterns(&root, &r).unwrap();
    fs::write(root.join("src/extra.rs"), "pub fn extra() {}\n").unwrap();
    let (_, s) = read_latest_patterns(&root).unwrap();
    assert_eq!(s.added, ["src/extra.rs"]);
    fs::remove_file(root.join("src/extra.rs")).unwrap();
    fs::remove_file(root.join("src/errors.rs")).unwrap();
    let (_, s) = read_latest_patterns(&root).unwrap();
    assert_eq!(s.removed, ["src/errors.rs"]);
    assert!(!s.is_current());
}

#[test]
fn a_damaged_stored_report_is_refused() {
    let root = copy_fixture("damaged");
    let path = write_patterns(&root, &report(&root)).unwrap();
    let text = fs::read_to_string(&path)
        .unwrap()
        .replacen("\"support\": 3", "\"support\": 9", 1);
    fs::write(&path, text).unwrap();
    assert!(read_latest_patterns(&root).is_err());
}

// ---------------------------------------------------------------------------------------------
// Integration with the graph
// ---------------------------------------------------------------------------------------------

#[test]
fn collecting_patterns_does_not_change_the_graph() {
    for root in [
        fixture(),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture"),
    ] {
        let plain = analyze(&root).unwrap();
        let (with, r, _) = analyze_with_patterns(&root).unwrap();
        assert_eq!(plain, with, "same nodes, edges and snapshot id");
        assert_eq!(r.graph_snapshot, plain.snapshot_id);
    }
}

#[test]
fn every_reference_in_a_report_is_a_node_of_the_graph_it_names() {
    let (graph, r, _) = analyze_with_patterns(&fixture()).unwrap();
    let nodes: BTreeSet<&str> = graph.nodes.iter().map(|n| n.id.as_str()).collect();
    for o in &r.observations {
        // Test subjects and symbols are nodes; a file subject is a node too.
        assert!(
            nodes.contains(o.subject.as_str()),
            "{} -> {}",
            o.name,
            o.subject
        );
        for m in &o.mentions {
            assert!(nodes.contains(m.as_str()), "{m}");
        }
    }
}

#[test]
fn the_report_holds_references_and_digests_and_never_source_text() {
    let r = report(&fixture());
    let json = r.to_json();
    for source_text in [
        "write(checked)",
        "validate_input(x)?",
        "fn handle(&self) {}",
    ] {
        assert!(!json.contains(source_text), "{source_text}");
    }
    assert!(r.sources.values().all(|d| d.starts_with("sha256:")));
}
