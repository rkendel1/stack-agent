//! Implicit Rust reachability (formatting, `.to_string()`, `for` loops and the iterator protocol,
//! iterator adapters, map keys) resolved from operand types, and evidence-based explanations of
//! misses: a miss is explained only by an unresolved edge, a budget cut-off or an uncompiled test
//! that connects to the missed function, never by an unrelated warning.

use std::fs;
use std::path::Path;

use chip_symbol_loop::acceptance::{AcceptanceRun, Diagnostic, DiagnosticKind};
use chip_symbol_loop::symbols::{SymbolIndex, sha256};
use chip_symbol_loop::target::{
    Budget, ExclusionKind, MissExplanation, Ranking, explain_miss, rank, rank_with,
};

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn crate_with(lib: &str, test: &str) -> tempfile::TempDir {
    let d = tempfile::Builder::new()
        .prefix("chip-reach-")
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

fn failing(test: &str) -> Diagnostic {
    Diagnostic {
        kind: DiagnosticKind::FailedTest,
        message: format!("test {test} ... FAILED"),
        path: None,
        line: None,
        column: None,
        test: Some(test.into()),
    }
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

fn ranked(d: &tempfile::TempDir, test: &str) -> Ranking {
    rank(
        &SymbolIndex::build(d.path()).unwrap(),
        &run(vec![failing(test)]),
    )
}

fn id(name: &str) -> String {
    format!("symbol:src/lib.rs:{name}")
}

fn reasons<'a>(r: &'a Ranking, name: &str) -> &'a [String] {
    let id = id(name);
    &r.candidates
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("{id} is not a candidate: {:#?}", r.summary(40)))
        .reasons
}

const STATUS: &str = r#"use std::fmt;

pub enum OrderStatus {
    Open,
    Shipped,
}

impl fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OrderStatus::Open => write!(f, "open"),
            OrderStatus::Shipped => write!(f, "shiped"),
        }
    }
}

impl fmt::Debug for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OrderStatus")
    }
}

pub struct Order {
    shipped: bool,
}

impl Order {
    pub fn new(shipped: bool) -> Self {
        Order { shipped }
    }

    pub fn status(&self) -> OrderStatus {
        if self.shipped { OrderStatus::Shipped } else { OrderStatus::Open }
    }

    pub fn label(&self) -> String {
        let status = self.status();
        format!("order: {status}")
    }

    pub fn debug_label(&self) -> String {
        format!("{:?}", self.status())
    }

    pub fn named(&self) -> String {
        format!("{s}!", s = self.status())
    }
}
"#;

#[test]
fn to_string_on_a_call_result_reaches_its_types_display_impl() {
    // g12's failure mode: `o.status().to_string()` formats `OrderStatus` with its `Display`.
    let test = "#[test]\nfn shipped_label() {\n    let o = app::Order::new(true);\n    assert_eq!(o.status().to_string(), \"shipped\");\n}\n";
    let d = crate_with(STATUS, test);
    let r = ranked(&d, "shipped_label");
    let display = r
        .candidates
        .iter()
        .find(|c| {
            c.reasons
                .iter()
                .any(|x| x.contains("Display (.to_string())"))
        })
        .unwrap_or_else(|| panic!("{:#?}", r.summary(40)));
    assert_eq!(display.id, id("OrderStatus::fmt"));
    // Debug is not what `.to_string()` calls.
    assert!(r.position(&id("OrderStatus::fmt#2")).is_none());
}

#[test]
fn format_placeholders_resolve_inline_captures_positional_debug_and_named_arguments() {
    let cases = [
        ("label", "OrderStatus::fmt", "Display (format! {})"),
        ("debug_label", "OrderStatus::fmt#2", "Debug (format! {:?})"),
        ("named", "OrderStatus::fmt", "Display (format! {})"),
    ];
    for (method, target, via) in cases {
        let test = format!(
            "#[test]\nfn formats() {{\n    let o = app::Order::new(true);\n    assert_eq!(o.{method}(), \"x\");\n}}\n"
        );
        let d = crate_with(STATUS, &test);
        let r = ranked(&d, "formats");
        assert!(
            reasons(&r, target).iter().any(|x| x.contains(via)),
            "{method}: {:?}",
            reasons(&r, target)
        );
    }
}

const STOCK: &str = r#"use std::collections::HashMap;
use std::fmt;

#[derive(Clone, Eq)]
pub struct Sku(pub String);

impl PartialEq for Sku {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl std::hash::Hash for Sku {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.to_lowercase().hash(state)
    }
}

pub struct StockLine {
    pub sku: Sku,
    pub on_hand: u32,
}

impl fmt::Display for StockLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.sku.0, self.on_hand)
    }
}

pub struct Stockroom {
    lines: Vec<StockLine>,
    index: HashMap<Sku, usize>,
}

impl Stockroom {
    pub fn new() -> Self {
        Stockroom { lines: Vec::new(), index: HashMap::new() }
    }

    pub fn report(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            out.push_str(&format!("{line}\n"));
        }
        out
    }

    pub fn has(&self, sku: &Sku) -> bool {
        self.index.contains_key(sku)
    }
}
"#;

#[test]
fn display_through_format_in_a_loop_over_a_field_reaches_the_element_types_impl() {
    // g07's failure mode: `for line in &self.lines { format!("{line}") }` formats `StockLine`.
    let test = "#[test]\nfn report_lists_lines() {\n    let s = app::Stockroom::new();\n    assert_eq!(s.report(), \"x\");\n}\n";
    let d = crate_with(STOCK, test);
    let r = ranked(&d, "report_lists_lines");
    assert!(
        reasons(&r, "StockLine::fmt")
            .iter()
            .any(|x| x.contains("Display (format! {})"))
    );
}

#[test]
fn map_key_operations_reach_the_key_types_hash_and_eq() {
    let test = "#[test]\nfn finds_lowercase() {\n    let s = app::Stockroom::new();\n    assert!(s.has(&app::Sku(\"a\".into())));\n}\n";
    let d = crate_with(STOCK, test);
    let r = ranked(&d, "finds_lowercase");
    for target in ["Sku::eq", "Sku::hash"] {
        assert!(
            reasons(&r, target)
                .iter()
                .any(|x| x.contains("map key .contains_key()")),
            "{target}: {:?}",
            reasons(&r, target)
        );
    }
}

const LOGS: &str = r#"pub struct Record {
    pub text: String,
}

pub struct LineSource {
    lines: Vec<String>,
    at: usize,
}

impl Iterator for LineSource {
    type Item = Record;
    fn next(&mut self) -> Option<Record> {
        let line = self.lines.get(self.at)?.clone();
        self.at += 1;
        Some(Record { text: line })
    }
}

pub struct RotatedSource {
    segments: Vec<Vec<String>>,
    seg: usize,
    at: usize,
}

impl Iterator for RotatedSource {
    type Item = Record;
    fn next(&mut self) -> Option<Record> {
        let segment = self.segments.get(self.seg)?;
        if self.at >= segment.len() {
            return None;
        }
        self.at += 1;
        Some(Record { text: segment[self.at - 1].clone() })
    }
}

pub fn open(rotated: bool) -> Box<dyn Iterator<Item = Record>> {
    if rotated {
        Box::new(RotatedSource { segments: Vec::new(), seg: 0, at: 0 })
    } else {
        Box::new(LineSource { lines: Vec::new(), at: 0 })
    }
}

pub fn count_records(rotated: bool) -> usize {
    let mut n = 0;
    for _record in open(rotated) {
        n += 1;
    }
    n
}

pub fn count_lines(source: LineSource) -> usize {
    source.count()
}

pub struct Archive {
    pub sources: Vec<LineSource>,
}

impl IntoIterator for Archive {
    type Item = Record;
    type IntoIter = LineSource;
    fn into_iter(self) -> LineSource {
        self.sources.into_iter().next().unwrap()
    }
}

pub fn archived(archive: Archive) -> usize {
    let mut n = 0;
    for _r in archive {
        n += 1;
    }
    n
}
"#;

#[test]
fn a_for_loop_over_a_factory_made_trait_object_reaches_every_iterator_impl() {
    // g10's failure mode: `for r in open(..)` where `open` returns `Box<dyn Iterator>`.
    let test = "#[test]\nfn counts_rotated() {\n    assert_eq!(app::count_records(true), 2);\n}\n";
    let d = crate_with(LOGS, test);
    let r = ranked(&d, "counts_rotated");
    for target in ["RotatedSource::next", "LineSource::next"] {
        assert!(
            reasons(&r, target)
                .iter()
                .any(|x| x.contains("Iterator/IntoIterator (for loop)")),
            "{target}: {:?}",
            reasons(&r, target)
        );
    }
}

#[test]
fn an_iterator_adapter_on_a_typed_receiver_drives_its_next() {
    let test = "#[test]\nfn counts_lines() {\n    let s: app::LineSource = todo!();\n    assert_eq!(app::count_lines(s), 2);\n}\n";
    let d = crate_with(LOGS, test);
    let r = ranked(&d, "counts_lines");
    assert!(
        reasons(&r, "LineSource::next")
            .iter()
            .any(|x| x.contains("Iterator (iterator .count())"))
    );
    assert!(r.position(&id("RotatedSource::next")).is_none());
}

#[test]
fn a_for_loop_over_an_into_iterator_type_reaches_into_iter_and_the_yielded_iterators_next() {
    let test = "#[test]\nfn archived_count() {\n    let a = app::Archive { sources: Vec::new() };\n    assert_eq!(app::archived(a), 1);\n}\n";
    let d = crate_with(LOGS, test);
    let r = ranked(&d, "archived_count");
    assert!(
        reasons(&r, "Archive::into_iter")
            .iter()
            .any(|x| x.contains("Iterator/IntoIterator (for loop)"))
    );
    reasons(&r, "LineSource::next");
}

#[test]
fn library_typed_operands_reach_no_user_impl_and_record_nothing() {
    // Display impls exist, but these operands are a `u32` and a `Vec` iterator: no edge, no
    // unresolved edge.
    let lib = format!(
        "{STATUS}\npub fn total(xs: Vec<u32>) -> String {{\n    let mut n: u32 = 0;\n    for x in xs.iter() {{\n        n += x;\n    }}\n    format!(\"{{}}\", n)\n}}\n"
    );
    let test = "#[test]\nfn totals() {\n    assert_eq!(app::total(vec![1, 2]), \"4\");\n}\n";
    let d = crate_with(&lib, test);
    let r = ranked(&d, "totals");
    assert!(r.position(&id("OrderStatus::fmt")).is_none());
    assert!(r.frontier.material.is_empty(), "{:#?}", r.frontier.material);
    assert!(!r.frontier.could_conceal_target);
}

#[test]
fn an_operand_of_unknown_type_is_a_material_unresolved_edge_that_explains_the_miss() {
    let lib = format!(
        "{STATUS}\npub fn all_statuses() -> Vec<OrderStatus> {{\n    vec![OrderStatus::Open]\n}}\n\npub fn describe() -> String {{\n    let it = all_statuses().into_iter().next().unwrap();\n    format!(\"{{}}\", it)\n}}\n"
    );
    let test = "#[test]\nfn describes() {\n    assert_eq!(app::describe(), \"open\");\n}\n";
    let d = crate_with(&lib, test);
    let r = ranked(&d, "describes");
    let e = r
        .frontier
        .material
        .iter()
        .find(|e| e.kind == ExclusionKind::ImplicitOperator)
        .unwrap_or_else(|| panic!("{:#?}", r.frontier));
    assert_eq!(
        (e.call.as_str(), e.from.as_str()),
        ("format! {}", "describe")
    );
    assert!(e.line > 0 && e.mechanism.contains("Display"), "{e:?}");
    match explain_miss(&r, &id("OrderStatus::fmt")) {
        MissExplanation::Explained(why) => {
            assert!(why.contains("format! {} at src/lib.rs:"), "{why}");
            assert!(why.contains("not established"), "{why}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_unrelated_unresolved_call_does_not_explain_a_miss() {
    // `apply` reaches `double` only through a function-pointer field, which the index does not
    // model and does not record. `.describe()` on an untyped value is unresolved and material, but
    // it cannot reach `double`: the miss is unexplained, and is reported as such.
    let lib = r#"pub struct Item;

impl Item {
    pub fn describe(&self) -> String {
        String::from("item")
    }
}

pub fn double(x: u32) -> u32 {
    x * 3
}

pub struct Runner {
    pub hook: fn(u32) -> u32,
}

impl Runner {
    pub fn apply(&self, x: u32) -> u32 {
        let first = Default::default();
        let _ = first.describe();
        (self.hook)(x)
    }
}
"#;
    let test = "#[test]\nfn doubles() {\n    let r = app::Runner { hook: app::double };\n    assert_eq!(r.apply(2), 4);\n}\n";
    let d = crate_with(lib, test);
    let r = ranked(&d, "doubles");
    assert!(r.position(&id("double")).is_none());
    assert!(
        r.frontier.could_conceal_target,
        "the unrelated edge is still reported"
    );
    assert!(r.frontier.material.iter().any(|e| e.call == ".describe"));
    assert_eq!(
        explain_miss(&r, &id("double")),
        MissExplanation::Unexplained
    );
    // ... while the function the unresolved edge might reach is explained by it.
    assert!(matches!(
        explain_miss(&r, &id("Item::describe")),
        MissExplanation::Found | MissExplanation::Explained(_)
    ));
}

#[test]
fn an_unresolved_edge_whose_possible_callees_are_all_candidates_is_benign() {
    // `other` names no type, so `x.size()` is unresolved; its only possible callee, `A::size`, is
    // a candidate anyway (through `total`): nothing is hidden.
    let lib = "pub struct A;\n\nimpl A {\n    pub fn size(&self) -> u32 {\n        1\n    }\n}\n\npub fn total(a: &A) -> u32 {\n    a.size() + other()\n}\n\nfn other() -> u32 {\n    let x = Default::default();\n    x.size()\n}\n";
    let test = "#[test]\nfn totals() {\n    assert_eq!(app::total(&app::A), 3);\n}\n";
    let d = crate_with(lib, test);
    let r = ranked(&d, "totals");
    assert!(r.position(&id("A::size")).is_some());
    assert!(r.frontier.material.is_empty());
    assert!(r.frontier.benign >= 1);
}

#[test]
fn a_budget_cut_off_explains_misses_beyond_it_and_only_those() {
    let lib = "pub fn a(x: u32) -> u32 {\n    b(x)\n}\n\nfn b(x: u32) -> u32 {\n    c(x)\n}\n\nfn c(x: u32) -> u32 {\n    d(x)\n}\n\nfn d(x: u32) -> u32 {\n    x\n}\n\npub fn unrelated() -> u32 {\n    1\n}\n";
    let test = "#[test]\nfn chain() {\n    assert_eq!(app::a(1), 2);\n}\n";
    let d = crate_with(lib, test);
    let r = rank_with(
        &SymbolIndex::build(d.path()).unwrap(),
        &run(vec![failing("chain")]),
        Budget {
            max_depth: 2,
            ..Budget::default()
        },
    );
    assert!(matches!(
        explain_miss(&r, &id("d")),
        MissExplanation::Explained(ref why) if why.contains("cut off")
    ));
    assert_eq!(
        explain_miss(&r, &id("unrelated")),
        MissExplanation::Unexplained
    );
}

#[test]
fn tests_that_never_ran_explain_only_what_they_reach() {
    let lib = "pub fn scale(x: u32) -> u32 {\n    x\n}\n\npub fn shift(x: u32) -> u32 {\n    x + 1\n}\n\npub fn unrelated() -> u32 {\n    1\n}\n";
    let test = "#[test]\nfn scales() {\n    assert_eq!(app::scale(2, 3), 6);\n}\n\n#[test]\nfn shifts() {\n    assert_eq!(app::shift(2), 4);\n}\n";
    let d = crate_with(lib, test);
    let r = rank(
        &SymbolIndex::build(d.path()).unwrap(),
        &run(vec![Diagnostic {
            kind: DiagnosticKind::CompileError,
            message: "error[E0061]: this function takes 1 argument but 2 arguments were supplied"
                .into(),
            path: Some("tests/acceptance.rs".into()),
            line: Some(3),
            column: None,
            test: None,
        }]),
    );
    assert!(matches!(
        explain_miss(&r, &id("shift")),
        MissExplanation::Explained(ref why) if why.contains("never ran")
    ));
    assert_eq!(
        explain_miss(&r, &id("unrelated")),
        MissExplanation::Unexplained
    );
}

#[test]
fn self_referential_operands_terminate() {
    let lib = "use std::fmt;\n\npub struct Node {\n    pub next: Option<Box<Node>>,\n}\n\nimpl fmt::Display for Node {\n    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {\n        write!(f, \"node\")\n    }\n}\n\npub fn show(n: Node) -> String {\n    let n = n;\n    let a = b;\n    let b = a;\n    format!(\"{n}{a}{b}\")\n}\n";
    let test =
        "#[test]\nfn shows() {\n    assert_eq!(app::show(app::Node { next: None }), \"x\");\n}\n";
    let d = crate_with(lib, test);
    let r = ranked(&d, "shows");
    reasons(&r, "show");
}

#[test]
fn proximity_is_a_weak_prior_and_logic_is_credited_only_where_it_exists() {
    // A constructor the test calls is not favored by depth alone over the code two calls down
    // that makes the decisions: their depth weights differ by at most 4 points. (Whether the
    // constructor still ranks higher depends on the other signals; this test does not assert it.)
    let lib = "pub struct Cart {\n    qty: u32,\n}\n\nimpl Cart {\n    pub fn new(qty: u32) -> Self {\n        Cart { qty }\n    }\n\n    pub fn total(&self) -> u32 {\n        price(self.qty)\n    }\n}\n\nfn price(qty: u32) -> u32 {\n    qty * 10 - discount(qty)\n}\n\nfn discount(qty: u32) -> u32 {\n    if qty >= 10 { qty } else if qty >= 5 { qty / 2 } else { 0 }\n}\n";
    let test = "#[test]\nfn bulk_total() {\n    let c = app::Cart::new(10);\n    assert_eq!(c.total(), 90);\n}\n";
    let d = crate_with(lib, test);
    let r = ranked(&d, "bulk_total");
    let depth_points = |n: &str| -> i64 {
        reasons(&r, n)
            .iter()
            .filter(|x| {
                x.contains("called directly")
                    || x.contains("called by a function")
                    || x.contains("at call depth")
            })
            .map(|x| x[1..].split(' ').next().unwrap().parse::<i64>().unwrap())
            .sum()
    };
    assert_eq!(depth_points("Cart::new"), 10);
    assert_eq!(depth_points("discount"), 6);
    assert!(
        reasons(&r, "discount")
            .iter()
            .any(|x| x.contains("+5 its body has 5 decision or computation points"))
    );
    assert!(
        !reasons(&r, "Cart::new")
            .iter()
            .any(|x| x.contains("decision or computation points"))
    );
}

#[test]
fn every_applicable_reason_is_reported_for_a_miss() {
    // `shift` is reachable both from tests that never ran and, from the compiled code, through an
    // unresolved call: both reasons are given.
    let lib = "pub struct T;\n\nimpl T {\n    pub fn go(&self) -> u32 {\n        shift(1)\n    }\n}\n\npub fn scale(x: u32) -> u32 {\n    let t = Default::default();\n    x + t.go()\n}\n\nfn shift(x: u32) -> u32 {\n    x + 1\n}\n";
    let test = "#[test]\nfn scales() {\n    assert_eq!(app::scale(2, 3), 6);\n}\n\n#[test]\nfn shifts() {\n    assert_eq!(app::scale(2), 4);\n}\n";
    let d = crate_with(lib, test);
    let r = rank(
        &SymbolIndex::build(d.path()).unwrap(),
        &run(vec![Diagnostic {
            kind: DiagnosticKind::CompileError,
            message: "error[E0061]: this function takes 1 argument but 2 arguments were supplied"
                .into(),
            path: Some("tests/acceptance.rs".into()),
            line: Some(3),
            column: None,
            test: None,
        }]),
    );
    match explain_miss(&r, &id("shift")) {
        MissExplanation::Explained(why) => {
            assert!(why.contains("unresolved .go"), "{why}");
            assert!(why.contains("never ran"), "{why}");
        }
        other => panic!("{other:?}"),
    }
}
