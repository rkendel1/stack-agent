//! Insufficient evidence must not become false certainty: a panic shows where a bad value was
//! used, not where it was made. And reachability through helper-returned values, `for` loops over
//! trait objects, and operators that call user trait implementations, with every unresolved link
//! reported by its actual cause.

use std::fs;
use std::path::Path;

use chip_symbol_loop::acceptance::{AcceptanceRun, Diagnostic, DiagnosticKind};
use chip_symbol_loop::symbols::{SymbolIndex, sha256};
use chip_symbol_loop::target::{Candidate, ExclusionKind, Ranking, Target, decide, rank};

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn crate_with(lib: &str, test: &str) -> tempfile::TempDir {
    let d = tempfile::Builder::new()
        .prefix("chip-safety-")
        .tempdir()
        .unwrap();
    write(
        d.path(),
        "Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    write(d.path(), "src/lib.rs", lib);
    write(d.path(), "tests/acceptance.rs", test);
    d
}

fn line_of(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|l| l.contains(needle))
        .expect("needle")
        + 1
}

fn diag(kind: DiagnosticKind, test: &str, at: Option<(&str, usize)>) -> Diagnostic {
    Diagnostic {
        kind,
        message: match kind {
            DiagnosticKind::Panic => "range end index 4 out of range for slice of length 3".into(),
            _ => format!("test {test} ..."),
        },
        path: at.map(|(p, _)| p.to_string()),
        line: at.map(|(_, l)| l),
        column: None,
        test: Some(test.into()),
    }
}

fn ranked(d: &tempfile::TempDir, diagnostics: Vec<Diagnostic>) -> Ranking {
    rank(
        &SymbolIndex::build(d.path()).unwrap(),
        &AcceptanceRun {
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
        },
    )
}

fn candidate<'a>(r: &'a Ranking, name: &str) -> &'a Candidate {
    let id = format!("symbol:src/lib.rs:{name}");
    r.candidates
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("{id} is not a candidate: {:#?}", r.summary(50)))
}

fn offered(r: &Ranking) -> Vec<String> {
    match decide(r) {
        Target::Ambiguous(ids) => ids,
        other => panic!("expected an ambiguous decision, got {other:?}"),
    }
}

/// A panicking failure on `test`, located at `needle` in `lib`.
fn panic_in(lib: &str, test: &str, needle: &str) -> Vec<Diagnostic> {
    vec![
        diag(DiagnosticKind::FailedTest, test, None),
        diag(
            DiagnosticKind::Panic,
            test,
            Some(("src/lib.rs", line_of(lib, needle))),
        ),
    ]
}

const PRODUCER: &str = r#"pub struct Page {
    pub items: Vec<u32>,
}

/// The defect: the end is not clamped to the number of items.
fn page_bounds(total: usize, page: usize, per_page: usize) -> (usize, usize) {
    let start = (page * per_page).min(total);
    (start, start + per_page)
}

/// Correct, given correct bounds.
pub fn paginate(items: &[u32], page: usize, per_page: usize) -> Page {
    let (start, end) = page_bounds(items.len(), page, per_page);
    Page { items: items[start..end].to_vec() }
}
"#;

#[test]
fn a_value_produced_wrongly_in_one_function_and_used_in_another_is_not_a_unique_target() {
    let test = "#[test]\nfn last_partial_page() {\n    assert_eq!(app::paginate(&[1, 2, 3], 1, 2).items, vec![3]);\n}\n";
    let d = crate_with(PRODUCER, test);
    let r = ranked(
        &d,
        panic_in(PRODUCER, "last_partial_page", "items[start..end]"),
    );
    let site = candidate(&r, "paginate");
    assert!(site.symptom && !site.direct);
    assert!(
        site.reasons
            .iter()
            .any(|x| x.contains("not necessarily where it was made"))
    );
    let producer = candidate(&r, "page_bounds");
    assert!(
        producer
            .reasons
            .iter()
            .any(|x| x.contains("its result is used by paginate, where the panic occurred"))
    );
    let ids = offered(&r);
    assert!(ids.contains(&producer.id) && ids.contains(&site.id));
}

const CALLER: &str = r#"/// Correct for every `n` in `1..=xs.len()`.
fn mean_of_first(xs: &[u32], n: usize) -> u32 {
    xs[..n].iter().sum::<u32>() / n as u32
}

/// The defect: passes one more than the number of values.
pub fn report(xs: &[u32]) -> u32 {
    mean_of_first(xs, xs.len() + 1)
}
"#;

#[test]
fn a_correct_function_that_receives_invalid_input_is_not_named_alone() {
    let test = "#[test]\nfn mean_of_three() {\n    assert_eq!(app::report(&[1, 2, 3]), 2);\n}\n";
    let d = crate_with(CALLER, test);
    let r = ranked(&d, panic_in(CALLER, "mean_of_three", "xs[..n]"));
    let caller = candidate(&r, "report");
    assert!(
        caller
            .reasons
            .iter()
            .any(|x| x.contains("it calls mean_of_first, where the panic occurred"))
    );
    let ids = offered(&r);
    assert!(ids.contains(&caller.id));
    assert!(ids.contains(&candidate(&r, "mean_of_first").id));
}

#[test]
fn indistinguishable_explanations_stay_ambiguous_however_the_panic_falls() {
    // The same code, with the panic located in either function: never a single target.
    let test = "#[test]\nfn last_partial_page() {\n    assert_eq!(app::paginate(&[1, 2, 3], 1, 2).items, vec![3]);\n}\n";
    let d = crate_with(PRODUCER, test);
    for needle in ["items[start..end]", "(start, start + per_page)"] {
        let r = ranked(&d, panic_in(PRODUCER, "last_partial_page", needle));
        assert!(
            matches!(decide(&r), Target::Ambiguous(ref ids) if ids.len() >= 2),
            "{needle}: {:?}",
            decide(&r)
        );
    }
}

#[test]
fn a_panic_with_no_other_explanation_in_reach_may_still_be_unique() {
    // The test's input is fixed, `head` calls nothing indexed, and nothing is hidden: the only
    // function that can be wrong is the one that panicked.
    let lib = "pub fn head(xs: &[u32]) -> u32 {\n    xs[3]\n}\n";
    let test = "#[test]\nfn first_of_three() {\n    assert_eq!(app::head(&[7, 8, 9]), 7);\n}\n";
    let d = crate_with(lib, test);
    let r = ranked(&d, panic_in(lib, "first_of_three", "xs[3]"));
    assert!(!r.frontier.could_conceal_target, "{:?}", r.frontier);
    assert_eq!(decide(&r), Target::Single("symbol:src/lib.rs:head".into()));
}

const SHOP: &str = r#"pub struct Shop {
    rate: u32,
}

impl Shop {
    pub fn new(rate: u32) -> Self {
        Shop { rate }
    }

    pub fn place(&mut self, qty: u32) -> u32 {
        price(qty, self.rate)
    }
}

fn price(qty: u32, rate: u32) -> u32 {
    qty * rate - discount(qty)
}

fn discount(qty: u32) -> u32 {
    if qty > 10 { 1 } else { 0 }
}
"#;

#[test]
fn a_value_from_a_test_helper_is_typed_by_the_helpers_declared_return_type() {
    let test = "use app::Shop;\n\nfn shop() -> Shop {\n    Shop::new(5)\n}\n\n#[test]\nfn bulk_order() {\n    let mut s = shop();\n    assert_eq!(s.place(20), 90);\n}\n";
    let d = crate_with(SHOP, test);
    let r = ranked(
        &d,
        vec![diag(DiagnosticKind::FailedTest, "bulk_order", None)],
    );
    // `s` is bound to `shop()`, declared `-> Shop`: `s.place` reaches `Shop::place`, and below it
    // `price` and `discount`.
    candidate(&r, "Shop::place");
    assert_eq!(candidate(&r, "discount").depth, 3);
    assert!(r.frontier.excluded.iter().all(|e| e.call != ".place"));
}

const FANOUT: &str = r#"pub trait Channel {
    fn render(&self, text: &str) -> String;
}

pub struct Sms;

impl Channel for Sms {
    fn render(&self, text: &str) -> String {
        format!("{}...", &text[..text.len().min(10)])
    }
}

pub struct Email;

impl Channel for Email {
    fn render(&self, text: &str) -> String {
        text.to_string()
    }
}

pub struct Dispatcher {
    channels: Vec<Box<dyn Channel>>,
}

impl Dispatcher {
    pub fn standard() -> Dispatcher {
        Dispatcher { channels: vec![Box::new(Sms), Box::new(Email)] }
    }

    pub fn send(&self, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for channel in &self.channels {
            out.push(channel.render(text));
        }
        out
    }
}
"#;

#[test]
fn trait_dispatch_through_a_helper_created_object_and_a_loop_over_trait_objects() {
    let test = "fn dispatcher() -> app::Dispatcher {\n    app::Dispatcher::standard()\n}\n\n#[test]\nfn short_sms() {\n    let d = dispatcher();\n    assert_eq!(d.send(\"a long message here\")[0].len(), 10);\n}\n";
    let d = crate_with(FANOUT, test);
    let r = ranked(
        &d,
        vec![diag(DiagnosticKind::FailedTest, "short_sms", None)],
    );
    let sms = candidate(&r, "Sms::render");
    assert!(
        sms.reasons
            .iter()
            .any(|x| x.contains("one of 2 in-scope implementations of Channel::render")),
        "{:?}",
        sms.reasons
    );
    candidate(&r, "Email::render");
}

const VERSION: &str = r#"#[derive(PartialEq, Eq)]
pub struct Version {
    pub major: u32,
    pub pre: Option<String>,
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.major.cmp(&other.major).then(self.pre.cmp(&other.pre))
    }
}

pub fn newest(versions: &[Version]) -> Option<&Version> {
    versions.iter().max()
}

pub fn is_newer(a: &Version, b: &Version) -> bool {
    a > b
}

pub struct Release {
    pub version: Version,
}

pub fn latest(releases: &[Release]) -> u32 {
    releases.iter().map(|r| r.version.major).max().unwrap_or(0)
}
"#;

#[test]
fn comparison_operators_and_max_reach_user_ord_implementations() {
    let test = "use app::Version;\n\n#[test]\nfn release_beats_pre_release() {\n    let a = Version { major: 1, pre: None };\n    let b = Version { major: 1, pre: Some(\"rc\".into()) };\n    assert!(app::is_newer(&a, &b));\n    assert_eq!(app::newest(&[b]).unwrap().major, 1);\n}\n";
    let d = crate_with(VERSION, test);
    let r = ranked(
        &d,
        vec![diag(
            DiagnosticKind::FailedTest,
            "release_beats_pre_release",
            None,
        )],
    );
    // `a > b` and `.max()` on `Version` reach its `PartialOrd`/`Ord` implementations; `cmp` is
    // also reached by name from `partial_cmp`.
    let partial = candidate(&r, "Version::partial_cmp");
    assert!(
        partial
            .reasons
            .iter()
            .any(|x| x.contains("Ord/PartialOrd (operator >)")
                || x.contains("Ord/PartialOrd (.max())")),
        "{:?}",
        partial.reasons
    );
    candidate(&r, "Version::cmp");
}

#[test]
fn an_operator_whose_operand_types_are_not_visible_is_reported_as_implicit_dispatch() {
    // `latest` compares `u32`s, but names `Release`, not `Version`; the user `Ord` impl exists,
    // so the frontier says the `.max()` might reach it instead of inventing an edge.
    let test = "use app::{Release, Version};\n\n#[test]\nfn latest_major() {\n    let r = Release { version: Version { major: 2, pre: None } };\n    assert_eq!(app::latest(&[r]), 3);\n}\n";
    let d = crate_with(VERSION, test);
    let r = ranked(
        &d,
        vec![diag(DiagnosticKind::FailedTest, "latest_major", None)],
    );
    assert!(r.position("symbol:src/lib.rs:Version::cmp").is_none());
    let site = r
        .frontier
        .excluded
        .iter()
        .find(|e| e.kind == ExclusionKind::ImplicitOperator)
        .expect("the implicit call is recorded");
    assert_eq!(site.call, ".max()");
    // The operand is traced to the call that produces it, and the cause says why it stops there.
    assert!(
        site.cause
            .contains("the operand comes from .map(), which resolves to no indexed function"),
        "{}",
        site.cause
    );
    assert!(
        r.frontier
            .concealment
            .iter()
            .any(|c| c.contains(".max() [implicit Ord/PartialOrd"))
    );
}

const ORDER: &str = r#"pub fn order_total(qty: u32) -> u32 {
    line_total(qty) + tax(qty)
}

fn line_total(qty: u32) -> u32 {
    qty * 10 - discount_for(qty)
}

fn discount_for(qty: u32) -> u32 {
    qty / 3
}

pub fn tax(qty: u32) -> u32 {
    qty
}
"#;

#[test]
fn functions_no_passing_test_reaches_are_ranked_up() {
    let test = "#[test]\nfn total_of_six() {\n    assert_eq!(app::order_total(6), 64);\n}\n\n#[test]\nfn tax_of_six() {\n    assert_eq!(app::tax(6), 6);\n}\n";
    let d = crate_with(ORDER, test);
    let r = ranked(
        &d,
        vec![
            diag(DiagnosticKind::FailedTest, "total_of_six", None),
            diag(DiagnosticKind::PassedTest, "tax_of_six", None),
        ],
    );
    let marker = "reached by none of the 1 passing tests checked";
    assert!(
        candidate(&r, "discount_for")
            .reasons
            .iter()
            .any(|x| x.contains(marker))
    );
    assert!(
        !candidate(&r, "tax")
            .reasons
            .iter()
            .any(|x| x.contains(marker))
    );
    // A passing test is never a seed.
    assert!(r.seeds.iter().all(|s| !s.ends_with("tax_of_six")));
}

#[test]
fn passing_tests_are_observed_but_never_shown_as_failures() {
    let output = "running 2 tests\ntest tax_of_six ... ok\ntest total_of_six ... FAILED\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n";
    let found = chip_symbol_loop::acceptance::diagnostics(Path::new("."), output);
    let kinds: Vec<_> = found.iter().map(|d| (d.kind, d.test.clone())).collect();
    assert_eq!(
        kinds,
        [
            (DiagnosticKind::PassedTest, Some("tax_of_six".into())),
            (DiagnosticKind::FailedTest, Some("total_of_six".into())),
        ]
    );
    let run = AcceptanceRun {
        exit_status: Some(101),
        timed_out: false,
        spawn_error: None,
        duration_ms: 1,
        passed_tests: 1,
        failed_tests: 1,
        passed: false,
        diagnostics: found,
        output_tail: String::new(),
        output_digest: sha256(b""),
    };
    let excerpt = chip_symbol_loop::acceptance::excerpt(&run, 4096);
    assert!(excerpt.contains("total_of_six") && !excerpt.contains("tax_of_six"));
}

#[test]
fn self_referential_bindings_terminate_and_are_reported() {
    // `node = node.next` and `a = b; b = a`: binding resolution follows at most three links.
    let lib = "pub struct Node {\n    pub next: Option<Box<Node>>,\n    pub value: u32,\n}\n\nimpl Node {\n    pub fn get(&self) -> u32 {\n        self.value\n    }\n}\n\npub fn last(start: Node) -> u32 {\n    let mut node = start;\n    let node = node.next;\n    let a = b;\n    let b = a;\n    node.get() + a.get() + b.get()\n}\n";
    let test = "#[test]\nfn walks() {\n    let n = app::Node { next: None, value: 1 };\n    assert_eq!(app::last(n), 2);\n}\n";
    let d = crate_with(lib, test);
    let r = ranked(&d, vec![diag(DiagnosticKind::FailedTest, "walks", None)]);
    candidate(&r, "last");
}
