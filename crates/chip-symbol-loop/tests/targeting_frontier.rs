//! Budgeted call-graph search, trait-object resolution, the search frontier and repair-shape
//! classification. Each fixture is a one-crate workspace written to a temporary directory; the
//! diagnostics are what `cargo test` would report for it.

use std::fs;
use std::path::Path;

use chip_symbol_loop::acceptance::{AcceptanceRun, Diagnostic, DiagnosticKind};
use chip_symbol_loop::symbols::{SymbolIndex, sha256};
use chip_symbol_loop::target::{
    Budget, ExclusionKind, Expressible, MatchKind, Ranking, StopReason, Target, decide, rank,
    rank_with,
};
use chip_symbol_loop::target::{MissExplanation, explain_miss};

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// A crate `app` with the given `src/` files and one integration test file.
fn crate_with(files: &[(&str, &str)], test: &str) -> tempfile::TempDir {
    let d = tempfile::Builder::new()
        .prefix("chip-frontier-")
        .tempdir()
        .unwrap();
    write(
        d.path(),
        "Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    for (rel, text) in files {
        write(d.path(), rel, text);
    }
    write(d.path(), "tests/acceptance.rs", test);
    d
}

fn line_of(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|l| l.contains(needle))
        .expect("needle")
        + 1
}

fn run(diagnostics: Vec<Diagnostic>) -> AcceptanceRun {
    AcceptanceRun {
        exit_status: Some(101),
        timed_out: false,
        spawn_error: None,
        duration_ms: 1,
        passed_tests: 0,
        failed_tests: 1,
        passed: false,
        diagnostics,
        output_tail: String::new(),
        output_digest: sha256(b""),
    }
}

fn failed(test: &str) -> Diagnostic {
    Diagnostic {
        kind: DiagnosticKind::FailedTest,
        message: format!("test {test} ... FAILED"),
        path: None,
        line: None,
        column: None,
        test: Some(test.into()),
    }
}

fn compile_error(path: &str, line: usize) -> Diagnostic {
    Diagnostic {
        kind: DiagnosticKind::CompileError,
        message: "error[E0061]: this function takes 1 argument but 2 arguments were supplied"
            .into(),
        path: Some(path.into()),
        line: Some(line),
        column: None,
        test: None,
    }
}

/// The 1-based character column of `needle` on its line, as rustc prints it.
fn column_of(text: &str, needle: &str) -> usize {
    let line = text.lines().find(|l| l.contains(needle)).expect("needle");
    line[..line.find(needle).unwrap()].chars().count() + 1
}

fn ranked(d: &tempfile::TempDir, diagnostics: Vec<Diagnostic>) -> Ranking {
    rank(&SymbolIndex::build(d.path()).unwrap(), &run(diagnostics))
}

fn candidate<'a>(r: &'a Ranking, id: &str) -> &'a chip_symbol_loop::target::Candidate {
    r.candidates
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("{id} is not a candidate: {:#?}", r.summary(50)))
}

const CHAIN: &str = r#"pub fn entry(x: u32) -> u32 {
    stage_one(x)
}

fn stage_one(x: u32) -> u32 {
    stage_two(x) + 1
}

fn stage_two(x: u32) -> u32 {
    stage_three(x) * 2
}

fn stage_three(x: u32) -> u32 {
    x - 1
}
"#;

const CHAIN_TEST: &str = r#"#[test]
fn entry_doubles() {
    assert_eq!(app::entry(3), 7);
}
"#;

#[test]
fn a_defect_four_calls_deep_enters_the_candidate_set_and_the_search_says_it_was_complete() {
    let d = crate_with(&[("src/lib.rs", CHAIN)], CHAIN_TEST);
    let r = ranked(&d, vec![failed("entry_doubles")]);
    let deep = candidate(&r, "symbol:src/lib.rs:stage_three");
    assert_eq!(deep.depth, 4);
    assert!(
        deep.reasons
            .iter()
            .any(|x| x.contains("at call depth 4 from the failing test"))
    );
    assert_eq!(r.frontier.stop, StopReason::Complete);
    assert!(!r.frontier.budget_exhausted);
    assert_eq!(r.frontier.max_depth_reached, 4);
    // Proximity alone never picks one function.
    assert!(matches!(decide(&r), Target::Ambiguous(_)));
}

#[test]
fn a_budget_that_cuts_the_search_off_is_recorded_and_flags_possible_concealment() {
    let d = crate_with(&[("src/lib.rs", CHAIN)], CHAIN_TEST);
    let index = SymbolIndex::build(d.path()).unwrap();
    let failure = run(vec![failed("entry_doubles")]);
    let shallow = rank_with(
        &index,
        &failure,
        Budget {
            max_depth: 2,
            ..Budget::default()
        },
    );
    // Depth 3 (`stage_two`) is cut off, so `stage_three` is never seen at all.
    assert!(shallow.position("symbol:src/lib.rs:stage_two").is_none());
    assert!(shallow.position("symbol:src/lib.rs:stage_three").is_none());
    assert_eq!(shallow.frontier.stop, StopReason::DepthBudget);
    assert!(shallow.frontier.budget_exhausted && shallow.frontier.could_conceal_target);
    assert!(
        shallow
            .frontier
            .hidden_examples
            .contains(&"symbol:src/lib.rs:stage_two".to_string())
    );
    let few_visits = rank_with(
        &index,
        &failure,
        Budget {
            max_visits: 2,
            ..Budget::default()
        },
    );
    assert_eq!(few_visits.frontier.stop, StopReason::VisitBudget);
    assert_eq!(few_visits.frontier.explored, 2);
    assert!(few_visits.frontier.cut_off > 0);
    let one_candidate = rank_with(
        &index,
        &failure,
        Budget {
            max_candidates: 1,
            ..Budget::default()
        },
    );
    assert_eq!(one_candidate.frontier.stop, StopReason::CandidateBudget);
    assert_eq!(one_candidate.candidates.len(), 1);
    // One candidate under a cut-off search is not a unique target.
    assert!(matches!(decide(&one_candidate), Target::Ambiguous(c) if c.len() == 1));
}

const DISPATCH: &str = r#"pub trait Gate {
    fn admit(&self, token: &str) -> bool;
    fn name(&self) -> &'static str {
        "gate"
    }
}

pub struct TokenGate {
    pub expected: String,
}

impl Gate for TokenGate {
    fn admit(&self, token: &str) -> bool {
        self.matches(token)
    }
}

impl TokenGate {
    fn matches(&self, token: &str) -> bool {
        token.len() == self.expected.len()
    }
}

pub struct OpenGate;

impl Gate for OpenGate {
    fn admit(&self, _token: &str) -> bool {
        true
    }
}

pub struct Settings {
    pub gate: Box<dyn Gate>,
}

pub struct Server {
    settings: Settings,
}

impl Server {
    pub fn new(gate: Box<dyn Gate>) -> Self {
        Server { settings: Settings { gate } }
    }

    pub fn handle(&self, token: &str) -> u16 {
        if self.settings.gate.admit(token) { 200 } else { 401 }
    }
}

pub fn check_with<G: Gate>(gate: &G, token: &str) -> bool {
    gate.admit(token)
}
"#;

const DISPATCH_TEST: &str = r#"use app::{Gate, Server, TokenGate};

struct Mock;

impl Gate for Mock {
    fn admit(&self, _token: &str) -> bool {
        false
    }
}

#[test]
fn only_the_expected_token_is_admitted() {
    let server = Server::new(Box::new(TokenGate { expected: "abc".into() }));
    assert_eq!(server.handle("abd"), 401);
}
"#;

#[test]
fn a_trait_object_field_resolves_to_the_in_scope_implementations_and_not_to_test_mocks() {
    let d = crate_with(&[("src/lib.rs", DISPATCH)], DISPATCH_TEST);
    let r = ranked(&d, vec![failed("only_the_expected_token_is_admitted")]);
    // `self.settings.gate.admit(..)`: Server.settings is Settings, Settings.gate is `dyn Gate`.
    for id in [
        "symbol:src/lib.rs:TokenGate::admit",
        "symbol:src/lib.rs:OpenGate::admit",
    ] {
        let c = candidate(&r, id);
        assert!(
            c.reasons
                .iter()
                .any(|x| x.contains("one of 2 in-scope implementations of Gate::admit")),
            "{id}: {:?}",
            c.reasons
        );
    }
    // Below the dispatch, the implementation's own calls are followed.
    assert!(r.position("symbol:src/lib.rs:TokenGate::matches").is_some());
    // The test's mock is test code, never a candidate.
    assert!(r.candidates.iter().all(|c| !c.id.contains("tests/")));
    assert_eq!(r.frontier.stop, StopReason::Complete);
}

#[test]
fn a_generic_bound_dispatches_like_a_trait_object() {
    let test =
        "#[test]\nfn generic_gate() {\n    assert!(!app::check_with(&app::OpenGate, \"x\"));\n}\n";
    let d = crate_with(&[("src/lib.rs", DISPATCH)], test);
    let r = ranked(&d, vec![failed("generic_gate")]);
    let c = candidate(&r, "symbol:src/lib.rs:OpenGate::admit");
    assert_eq!(c.depth, 2);
    assert!(c.reasons.iter().any(|x| x.contains("Gate::admit")));
}

#[test]
fn a_value_returned_by_a_factory_dispatches_through_its_declared_return_type() {
    let lib = format!(
        "{DISPATCH}\npub fn gate_for(open: bool) -> Box<dyn Gate> {{\n    if open {{ Box::new(OpenGate) }} else {{ Box::new(TokenGate {{ expected: String::new() }}) }}\n}}\n\npub fn decide(open: bool, token: &str) -> bool {{\n    let gate = gate_for(open);\n    gate.admit(token)\n}}\n"
    );
    let test = "#[test]\nfn closed_gate() {\n    assert!(!app::decide(false, \"x\"));\n}\n";
    let d = crate_with(&[("src/lib.rs", &lib)], test);
    let r = ranked(&d, vec![failed("closed_gate")]);
    // `gate` is bound to `gate_for(..)`, declared to return `Box<dyn Gate>`.
    for id in [
        "symbol:src/lib.rs:TokenGate::admit",
        "symbol:src/lib.rs:OpenGate::admit",
    ] {
        assert!(
            candidate(&r, id)
                .reasons
                .iter()
                .any(|x| x.contains("implementations of Gate::admit"))
        );
    }
    assert!(!r.frontier.unresolved_dispatch);
}

const GATES: &str = "pub fn all_gates() -> Vec<Box<dyn Gate>> {\n    vec![Box::new(OpenGate), Box::new(TokenGate { expected: String::new() })]\n}\n";

#[test]
fn a_type_lost_in_an_unindexed_call_is_reported_as_missing_return_type_propagation() {
    let lib = format!(
        "{DISPATCH}\n{GATES}\npub fn decide(open: bool, token: &str) -> bool {{\n    let gates = all_gates();\n    let gate = gates.into_iter().nth(usize::from(open)).unwrap();\n    gate.admit(token)\n}}\n"
    );
    let test = "#[test]\nfn closed_gate() {\n    assert!(!app::decide(false, \"x\"));\n}\n";
    let d = crate_with(&[("src/lib.rs", &lib)], test);
    let r = ranked(&d, vec![failed("closed_gate")]);
    assert!(r.position("symbol:src/lib.rs:TokenGate::admit").is_none());
    let site = r
        .frontier
        .excluded
        .iter()
        .find(|e| e.call == ".admit")
        .expect("the call is recorded");
    assert_eq!(site.kind, ExclusionKind::ReturnTypeUnknown);
    assert!(
        site.cause.contains("gate is bound to .nth()"),
        "{}",
        site.cause
    );
    assert_eq!(site.plausible_count, 2);
    // The material report names this edge, where it is, and that it might reach the target.
    let m = r
        .frontier
        .material
        .iter()
        .find(|e| e.call == ".admit")
        .expect("a material edge");
    assert_eq!((m.path.as_str(), m.from.as_str()), ("src/lib.rs", "decide"));
    assert!(
        m.line > 0 && m.mechanism.contains("unresolved type"),
        "{m:?}"
    );
    match explain_miss(&r, "symbol:src/lib.rs:TokenGate::admit") {
        MissExplanation::Explained(why) => assert!(why.contains(".admit at src/lib.rs:"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_trait_call_on_a_receiver_of_unknown_type_is_reported_as_unresolved_dispatch() {
    let lib = format!(
        "{DISPATCH}\n{GATES}\npub fn any_admits(token: &str) -> bool {{\n    all_gates().iter().any(|g| g.admit(token))\n}}\n"
    );
    let test = "#[test]\nfn none_admit() {\n    assert!(!app::any_admits(\"x\"));\n}\n";
    let d = crate_with(&[("src/lib.rs", &lib)], test);
    let r = ranked(&d, vec![failed("none_admit")]);
    assert!(r.position("symbol:src/lib.rs:TokenGate::admit").is_none());
    let site = r
        .frontier
        .excluded
        .iter()
        .find(|e| e.call == ".admit")
        .expect("the dispatch site is recorded");
    assert_eq!(site.kind, ExclusionKind::UnresolvedDispatch);
    assert!(
        site.cause.contains("trait object of Gate"),
        "{}",
        site.cause
    );
    assert!(r.frontier.unresolved_dispatch && r.frontier.could_conceal_target);
    assert!(
        r.frontier
            .concealment
            .iter()
            .any(|c| c.contains(".admit [trait-object or generic dispatch] is unresolved"))
    );
}

#[test]
fn a_trait_with_no_production_implementation_is_recorded_not_invented() {
    let lib = "pub trait Clock {\n    fn now(&self) -> u64;\n}\n\npub fn elapsed(clock: &dyn Clock, since: u64) -> u64 {\n    clock.now() - since\n}\n";
    let test = "use app::Clock;\nstruct Fixed;\nimpl Clock for Fixed {\n    fn now(&self) -> u64 {\n        5\n    }\n}\n#[test]\nfn elapsed_since_two() {\n    assert_eq!(app::elapsed(&Fixed, 2), 3);\n}\n";
    let d = crate_with(&[("src/lib.rs", lib)], test);
    let r = ranked(&d, vec![failed("elapsed_since_two")]);
    assert!(
        r.frontier
            .excluded
            .iter()
            .any(|e| e.call == ".now" && e.kind == ExclusionKind::NoImplementation)
    );
    assert_eq!(r.candidates.len(), 1);
    // A test-only implementation hides no production target.
    assert!(!r.frontier.unresolved_dispatch);
}

#[test]
fn module_paths_resolve_through_the_file_layout() {
    let lib = "pub mod engine;\n\npub fn run(x: u32) -> u32 {\n    engine::policy::limit(x)\n}\n";
    let engine = "pub mod policy;\n";
    let policy = "pub fn limit(x: u32) -> u32 {\n    x.min(3)\n}\n";
    let other = "pub fn limit(x: u32) -> u32 {\n    x\n}\n";
    let test = "#[test]\nfn limit_is_four() {\n    assert_eq!(app::run(9), 4);\n}\n";
    let d = crate_with(
        &[
            ("src/lib.rs", lib),
            ("src/engine/mod.rs", engine),
            ("src/engine/policy.rs", policy),
            ("src/other.rs", other),
        ],
        test,
    );
    let r = ranked(&d, vec![failed("limit_is_four")]);
    let c = candidate(&r, "symbol:src/engine/policy.rs:limit");
    assert!(c.reasons.iter().any(|x| x.contains("qualified path")));
    assert!(r.position("symbol:src/other.rs:limit").is_none());
}

#[test]
fn external_paths_conceal_nothing_and_name_only_matches_are_listed() {
    let lib = "pub struct Store;\n\nimpl Store {\n    pub fn get(&self) -> u32 {\n        1\n    }\n}\n\npub fn total(values: &[u32]) -> u32 {\n    let first = values.get(0).copied().unwrap_or(0);\n    let s = String::new();\n    first + s.len() as u32\n}\n";
    let test = "#[test]\nfn total_of_one() {\n    assert_eq!(app::total(&[2]), 3);\n}\n";
    let d = crate_with(&[("src/lib.rs", lib)], test);
    let r = ranked(&d, vec![failed("total_of_one")]);
    assert_eq!(r.frontier.excluded_counts.get("external_path"), Some(&1));
    let get = r
        .frontier
        .excluded
        .iter()
        .find(|e| e.call == ".get")
        .unwrap();
    assert_eq!(get.kind, ExclusionKind::UntypedMethod);
    assert_eq!(get.plausible, ["symbol:src/lib.rs:Store::get"]);
    // `Store::get` might be what `.get` reaches; the frontier says so instead of guessing.
    assert!(r.frontier.could_conceal_target);
    assert!(matches!(decide(&r), Target::Ambiguous(c) if c.len() == 1));
}

#[test]
fn one_candidate_with_nothing_hidden_is_a_unique_target() {
    let lib = "pub fn double(x: u32) -> u32 {\n    x + x + 1\n}\n";
    let test = "#[test]\nfn doubles() {\n    assert_eq!(app::double(2), 4);\n}\n";
    let d = crate_with(&[("src/lib.rs", lib)], test);
    let r = ranked(&d, vec![failed("doubles")]);
    assert!(!r.frontier.could_conceal_target, "{:?}", r.frontier);
    assert_eq!(
        decide(&r),
        Target::Single("symbol:src/lib.rs:double".into())
    );
    assert_eq!(r.shape.expressible, Expressible::Possible);
}

#[test]
fn a_compile_error_in_the_test_is_not_expressible_as_a_body_replacement() {
    let lib = "pub fn scale(x: u32) -> u32 {\n    x\n}\n";
    let test = "#[test]\nfn scales_by_factor() {\n    assert_eq!(app::scale(2, 3), 6);\n}\n";
    let d = crate_with(&[("src/lib.rs", lib)], test);
    let r = ranked(
        &d,
        vec![compile_error(
            "tests/acceptance.rs",
            line_of(test, "app::scale"),
        )],
    );
    assert_eq!(r.shape.expressible, Expressible::NotExpressible);
    assert!(r.shape.reasons[0].contains("in test code"), "{:?}", r.shape);
    assert!(matches!(decide(&r), Target::NotExpressible(_)));
}

#[test]
fn tests_that_never_ran_because_their_file_does_not_compile_are_reported_unobserved() {
    let lib =
        "pub fn scale(x: u32) -> u32 {\n    x\n}\n\npub fn shift(x: u32) -> u32 {\n    x + 1\n}\n";
    let test = "#[test]\nfn scales_by_factor() {\n    assert_eq!(app::scale(2, 3), 6);\n}\n\n#[test]\nfn shifts() {\n    assert_eq!(app::shift(2), 4);\n}\n";
    let d = crate_with(&[("src/lib.rs", lib)], test);
    let r = ranked(
        &d,
        vec![compile_error(
            "tests/acceptance.rs",
            line_of(test, "app::scale"),
        )],
    );
    // `shifts` never ran; `shift` is not a candidate, and the frontier says why.
    assert!(r.position("symbol:src/lib.rs:shift").is_none());
    assert!(r.frontier.could_conceal_target);
    assert!(
        r.frontier.concealment[0]
            .contains("tests/acceptance.rs does not compile, so none of its tests ran"),
        "{:?}",
        r.frontier.concealment
    );
}

#[test]
fn a_compile_error_in_a_type_or_signature_is_not_expressible() {
    let lib = "pub enum Source {\n    File(Missing),\n}\n\npub fn read(path: &Unknown) -> u32 {\n    1\n}\n";
    let test = "#[test]\nfn reads() {\n    let _ = app::Source::File;\n}\n";
    let d = crate_with(&[("src/lib.rs", lib)], test);
    let r = ranked(
        &d,
        vec![
            compile_error("src/lib.rs", line_of(lib, "File(Missing)")),
            Diagnostic {
                column: Some(column_of(lib, "Unknown")),
                ..compile_error("src/lib.rs", line_of(lib, "&Unknown"))
            },
        ],
    );
    assert_eq!(r.shape.expressible, Expressible::NotExpressible);
    assert!(
        r.shape
            .reasons
            .iter()
            .any(|x| x.contains("inside enum Source"))
    );
    assert!(
        r.shape
            .reasons
            .iter()
            .any(|x| x.contains("declaration of read"))
    );
    assert!(matches!(decide(&r), Target::NotExpressible(_)));
    // On the same line, a location after the body's opening brace is in the body.
    let one_line = "pub fn read(path: &str) -> u32 { path.len() as u32 + missing }\n";
    let d = crate_with(&[("src/lib.rs", one_line)], test);
    let r = ranked(
        &d,
        vec![Diagnostic {
            column: Some(column_of(one_line, "missing")),
            ..compile_error("src/lib.rs", 1)
        }],
    );
    assert_eq!(r.shape.expressible, Expressible::Possible);
    assert_eq!(decide(&r), Target::Single("symbol:src/lib.rs:read".into()));
}

#[test]
fn failing_tests_that_share_no_function_need_more_than_one_change() {
    let lib = "pub fn area(w: u32, h: u32) -> u32 {\n    w + h\n}\n\npub fn perimeter(w: u32, h: u32) -> u32 {\n    w * h\n}\n";
    let test = "#[test]\nfn area_is_product() {\n    assert_eq!(app::area(2, 3), 6);\n}\n\n#[test]\nfn perimeter_is_sum() {\n    assert_eq!(app::perimeter(2, 3), 10);\n}\n";
    let d = crate_with(&[("src/lib.rs", lib)], test);
    let r = ranked(
        &d,
        vec![failed("area_is_product"), failed("perimeter_is_sum")],
    );
    assert_eq!(r.per_test.len(), 2);
    assert!(r.per_test.iter().all(|t| t.complete && t.reached == 1));
    assert_eq!(r.shape.expressible, Expressible::NotExpressible);
    assert!(r.shape.reasons[0].contains("no function is reached by all 2 failing tests"));
    assert!(matches!(decide(&r), Target::NotExpressible(_)));
}

#[test]
fn a_function_every_failing_test_reaches_is_evidence_not_a_verdict() {
    let lib = "fn base(x: u32) -> u32 {\n    x\n}\n\npub fn area(w: u32, h: u32) -> u32 {\n    base(w) * h\n}\n\npub fn perimeter(w: u32, h: u32) -> u32 {\n    2 * (base(w) + h)\n}\n";
    let test = "#[test]\nfn area_is_product() {\n    assert_eq!(app::area(2, 3), 7);\n}\n\n#[test]\nfn perimeter_is_sum() {\n    assert_eq!(app::perimeter(2, 3), 11);\n}\n";
    let d = crate_with(&[("src/lib.rs", lib)], test);
    let r = ranked(
        &d,
        vec![failed("area_is_product"), failed("perimeter_is_sum")],
    );
    let base = candidate(&r, "symbol:src/lib.rs:base");
    assert!(
        base.reasons
            .iter()
            .any(|x| x.contains("reached by every one of the 2 failing tests"))
    );
    assert_eq!(r.shape.expressible, Expressible::Possible);
    assert!(matches!(decide(&r), Target::Ambiguous(_)));
}

#[test]
fn dispatch_is_its_own_match_kind_ranked_below_named_calls() {
    assert!(MatchKind::Qualified < MatchKind::Unique);
    assert!(MatchKind::Unique < MatchKind::Dispatch);
    assert!(MatchKind::Dispatch < MatchKind::Shared);
}
