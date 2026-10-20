//! Implementation-pattern observations: how a Rust repository implements things, extracted from the
//! syntax the graph has already parsed.
//!
//! This is a document that **references** graph node ids; it is not a second graph. The architecture
//! graph (nodes, edges, snapshot id) is unchanged by it.
//!
//! What is claimed, and what is not:
//!
//! * An observation is a structural fact visible in the syntax tree of one file: an `impl` with these
//!   methods, a type with these derives, a function whose top-level statements call a `validate*`
//!   before an effect. It is never a semantic claim.
//! * A pattern groups observations with the same structure. A pattern with enough observations is a
//!   *candidate convention*. Frequency is evidence of a convention, not proof that it is correct or
//!   required, so nothing here can be promoted: the classification has no policy variant.
//! * Where a relationship cannot be established from the syntax, the narrower fact is recorded and the
//!   stronger one is not asserted. Call classes come from the callee's *name* and say so.
//! * Analysis is partial where it says it is: the coverage lists every Rust file that was skipped or
//!   failed, and why.
//!
//! No model, no process, no network. The same source gives the same bytes and the same report id.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use syn::visit::Visit;
use syn::{Expr, ImplItem, Item, Stmt};

use crate::model::SnapshotId;

pub const PATTERN_SCHEMA: &str = "chip.patterns.v1";
pub const ANALYZER_ID: &str = "chip-graph.rust-patterns";
pub const ANALYZER_VERSION: &str = "1";

/// A signature needs this many supporting observations to be a candidate convention. A count, not a
/// verdict on whether the structure is right.
pub const MIN_SUPPORT: usize = 3;
/// Counterexample ids kept per pattern (the count is always complete).
const MAX_COUNTEREXAMPLE_IDS: usize = 25;

/// Call classes come from the callee's name only, and the report says so (`name_based`).
const VALIDATE_PREFIXES: [&str; 2] = ["validate", "verify"];
const EFFECT_NAMES: [&str; 12] = [
    "execute",
    "write",
    "write_all",
    "create_dir_all",
    "remove_file",
    "remove_dir_all",
    "rename",
    "record_intent",
    "record_outcome",
    "commit",
    "apply",
    "send",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatternCategory {
    TraitImpl,
    ErrorType,
    TestShape,
    CallOrder,
}

impl PatternCategory {
    fn as_str(self) -> &'static str {
        match self {
            PatternCategory::TraitImpl => "trait_impl",
            PatternCategory::ErrorType => "error_type",
            PatternCategory::TestShape => "test_shape",
            PatternCategory::CallOrder => "call_order",
        }
    }
}

/// What a pattern is. There is deliberately no variant for a policy, a rule or a requirement:
/// nothing in this crate can promote a pattern to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    /// A structure seen fewer than [`MIN_SUPPORT`] times.
    Observed,
    /// A structure seen at least [`MIN_SUPPORT`] times. Inferred from frequency, never enforced.
    CandidateConvention,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Analyzer {
    pub id: String,
    pub version: String,
    /// The callee names the call classes are read from (so a reader can see what "validate" meant).
    pub validate_prefixes: Vec<String>,
    pub effect_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Observation {
    /// Stable across analyses of the same structure; excludes the line.
    pub id: String,
    pub category: PatternCategory,
    pub family: String,
    /// What was observed, displayed: `Type as Trait`, `Type::method`, a test's path.
    pub name: String,
    pub signature: Vec<String>,
    /// A graph node: the symbol when it has one, else the file.
    pub subject: String,
    pub file: String,
    pub line: usize,
    /// Narrow facts about this observation (`legacy`, `name_based`, `ordering_in_top_level_statements`, ...).
    pub flags: Vec<String>,
    /// Graph symbols a test body mentions (a mention is not coverage).
    pub mentions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Confidence {
    pub support: usize,
    pub counterexamples: usize,
    /// `support / (support + counterexamples)`, in thousandths.
    pub share_permille: usize,
    pub distinct_files: usize,
    pub distinct_crates: usize,
    /// Always `counts`: these are tallies, not a probability.
    pub basis: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pattern {
    pub id: String,
    pub category: PatternCategory,
    pub family: String,
    pub signature: Vec<String>,
    pub classification: Classification,
    /// Always `inferred_from_frequency` for a candidate and `observed` otherwise.
    pub authority: String,
    /// Always `false`: no pattern is a rule.
    pub enforced: bool,
    /// Always `deterministic`: computed from syntax, not proposed by a model.
    pub basis: String,
    pub support: Vec<String>,
    pub counterexamples: Vec<String>,
    pub confidence: Confidence,
    /// What a reader must not read into this pattern.
    pub caveats: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
}

/// Something analysis could not establish, kept rather than guessed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Exception {
    pub path: String,
    pub line: usize,
    pub kind: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Coverage {
    pub rust_files_discovered: usize,
    pub files_analyzed: usize,
    pub skipped: Vec<Skipped>,
    pub parse_failures: Vec<String>,
    /// Macro invocations in item position: any item they expand to is invisible here.
    pub unexpanded_item_macros: usize,
    pub ignore_rules: String,
    /// `true` only when every discovered Rust file was analyzed and nothing is unexpanded.
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternReport {
    pub schema_version: String,
    pub analyzer: Analyzer,
    /// The architecture graph snapshot the node ids refer to.
    pub graph_snapshot: SnapshotId,
    pub coverage: Coverage,
    /// SHA-256 of every analyzed file's bytes: what the report is true of.
    pub sources: BTreeMap<String, String>,
    pub observations: Vec<Observation>,
    pub patterns: Vec<Pattern>,
    pub exceptions: Vec<Exception>,
    /// `sha256:` of the canonical bytes of everything above.
    pub report_id: String,
}

/// What changed in the source since a report was made.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Staleness {
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    pub added: Vec<String>,
}

impl Staleness {
    pub fn is_current(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty() && self.added.is_empty()
    }
}

#[derive(Serialize)]
struct Canonical<'a> {
    schema_version: &'a str,
    analyzer: &'a Analyzer,
    graph_snapshot: &'a SnapshotId,
    coverage: &'a Coverage,
    sources: &'a BTreeMap<String, String>,
    observations: &'a [Observation],
    patterns: &'a [Pattern],
    exceptions: &'a [Exception],
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", hex(&Sha256::digest(bytes)))
}

fn short_id(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    hex(&h.finalize()[..8])
}

impl PatternReport {
    fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&Canonical {
            schema_version: &self.schema_version,
            analyzer: &self.analyzer,
            graph_snapshot: &self.graph_snapshot,
            coverage: &self.coverage,
            sources: &self.sources,
            observations: &self.observations,
            patterns: &self.patterns,
            exceptions: &self.exceptions,
        })
        .expect("a report of strings always serializes")
    }

    pub fn is_consistent(&self) -> bool {
        self.schema_version == PATTERN_SCHEMA
            && digest_of(&self.canonical_bytes()) == self.report_id
    }

    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("a report always serializes");
        text.push('\n');
        text
    }

    pub fn from_json(text: &str) -> Result<PatternReport, String> {
        let report: PatternReport =
            serde_json::from_str(text).map_err(|e| format!("not a valid pattern report: {e}"))?;
        if !report.is_consistent() {
            return Err("report id does not match its content".to_string());
        }
        Ok(report)
    }

    pub fn candidates(&self) -> impl Iterator<Item = &Pattern> {
        self.patterns
            .iter()
            .filter(|p| p.classification == Classification::CandidateConvention)
    }

    /// The observations a change makes untrustworthy, and the patterns that rest on them: the ones
    /// in a changed or removed file. A pattern counts when a supporting observation, or one of the
    /// counterexamples kept (at most 25 are listed), is among them. Observations in unchanged files
    /// stay as true as they were. A new file is not here: it adds observations nobody has made yet.
    pub fn invalidated(&self, staleness: &Staleness) -> (Vec<&Observation>, Vec<&Pattern>) {
        let gone: BTreeSet<&str> = staleness
            .changed
            .iter()
            .chain(&staleness.removed)
            .map(String::as_str)
            .collect();
        let observations: Vec<&Observation> = self
            .observations
            .iter()
            .filter(|o| gone.contains(o.file.as_str()))
            .collect();
        let ids: BTreeSet<&str> = observations.iter().map(|o| o.id.as_str()).collect();
        let patterns = self
            .patterns
            .iter()
            .filter(|p| {
                p.support
                    .iter()
                    .chain(&p.counterexamples)
                    .any(|i| ids.contains(i.as_str()))
            })
            .collect();
        (observations, patterns)
    }

    /// Compares the report with the source on disk: every analyzed file is re-hashed, and the tree
    /// is re-walked for Rust files the report neither analyzed nor skipped. A report is current
    /// only if nothing differs. This never rebuilds anything.
    pub fn check(&self, root: &Path) -> Staleness {
        let mut out = Staleness::default();
        for (path, digest) in &self.sources {
            match fs::read(root.join(path)) {
                Ok(bytes) if digest_of(&bytes) == *digest => {}
                Ok(_) => out.changed.push(path.clone()),
                Err(_) => out.removed.push(path.clone()),
            }
        }
        let mut found = BTreeSet::new();
        if crate::analyze::walk_files(root, &mut found).is_ok() {
            let known: BTreeSet<&str> = self
                .sources
                .keys()
                .map(String::as_str)
                .chain(self.coverage.skipped.iter().map(|s| s.path.as_str()))
                .chain(self.coverage.parse_failures.iter().map(String::as_str))
                .collect();
            out.added = found
                .into_iter()
                .filter(|f| f.ends_with(".rs") && !known.contains(f.as_str()))
                .collect();
        }
        out
    }
}

// ---------------------------------------------------------------------------------------------
// Collection (runs inside the graph's own scan, on the file it already parsed)
// ---------------------------------------------------------------------------------------------

struct ImplFact {
    krate: String,
    file: String,
    line: usize,
    self_name: Option<String>,
    trait_name: String,
    methods: Vec<String>,
    deprecated: bool,
}

struct ErrorFact {
    krate: String,
    file: String,
    line: usize,
    module: Vec<String>,
    name: String,
    derives: Vec<String>,
    deprecated: bool,
}

struct TestFact {
    krate: String,
    file: String,
    line: usize,
    module: Vec<String>,
    name: String,
    attr: String,
    is_async: bool,
    returns_value: bool,
}

struct CallFact {
    krate: String,
    file: String,
    line: usize,
    module: Vec<String>,
    name: String,
    classes: Vec<&'static str>,
    also_conditional: bool,
    deprecated: bool,
}

pub(crate) struct Collector {
    sources: BTreeMap<String, String>,
    parse_failures: BTreeSet<String>,
    impls: Vec<ImplFact>,
    errors: Vec<ErrorFact>,
    tests: Vec<TestFact>,
    calls: Vec<CallFact>,
    exceptions: BTreeSet<Exception>,
    item_macros: usize,
    skipped: BTreeSet<Skipped>,
    /// `(file, module path)` already observed: a file reached twice is observed once.
    seen: BTreeSet<(String, Vec<String>)>,
}

/// A vendored tree, or a generated file, is not the repository's own implementation.
fn vendored(file: &str) -> bool {
    file.split('/').any(|part| part == "vendor")
}

fn generated(text: &str) -> bool {
    text.lines()
        .take(5)
        .any(|l| l.contains("@generated") || l.contains("DO NOT EDIT"))
}

fn is_integration_test(file: &str) -> bool {
    file.starts_with("tests/") || file.contains("/tests/")
}

fn clean(ident: &proc_macro2::Ident) -> String {
    ident.to_string().trim_start_matches("r#").to_string()
}

fn line_of(span: proc_macro2::Span) -> usize {
    span.start().line
}

fn attr_is(attr: &syn::Attribute, name: &str) -> bool {
    attr.path().is_ident(name)
}

fn deprecated(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| attr_is(a, "deprecated"))
}

fn derives(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for attr in attrs.iter().filter(|a| attr_is(a, "derive")) {
        let _ = attr.parse_nested_meta(|meta| {
            if let Some(seg) = meta.path.segments.last() {
                out.push(clean(&seg.ident));
            }
            Ok(())
        });
    }
    out.sort();
    out.dedup();
    out
}

fn path_string(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|s| clean(&s.ident))
        .collect::<Vec<_>>()
        .join("::")
}

fn type_name(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(p) => p.path.segments.last().map(|s| clean(&s.ident)),
        _ => None,
    }
}

fn test_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs
        .iter()
        .find(|a| a.path().segments.last().is_some_and(|s| s.ident == "test"))
        .map(|a| path_string(a.path()))
}

fn class_of(name: &str) -> Option<&'static str> {
    if VALIDATE_PREFIXES.iter().any(|p| name.starts_with(p)) {
        Some("validate")
    } else if EFFECT_NAMES.contains(&name) {
        Some("effect")
    } else {
        None
    }
}

/// The calls in one expression, in evaluation order, and whether any call sits where order is
/// not established (inside a branch, loop, closure, nested block or the right side of `&&`/`||`).
///
/// Order is claimed only for straight-line code: operands before the operation, receiver then
/// arguments then the call. A call under a conditional is counted, never ordered.
struct Ordered {
    calls: Vec<&'static str>,
    conditional: usize,
}

impl Ordered {
    fn call(&mut self, name: &str) {
        if let Some(class) = class_of(name) {
            self.calls.push(class);
        }
    }
}

struct AnyCall(usize);

impl<'ast> Visit<'ast> for AnyCall {
    fn visit_expr_call(&mut self, e: &'ast syn::ExprCall) {
        if let Expr::Path(p) = &*e.func {
            if let Some(seg) = p.path.segments.last() {
                self.0 += usize::from(class_of(&clean(&seg.ident)).is_some());
            }
        }
        syn::visit::visit_expr_call(self, e);
    }

    fn visit_expr_method_call(&mut self, e: &'ast syn::ExprMethodCall) {
        self.0 += usize::from(class_of(&clean(&e.method)).is_some());
        syn::visit::visit_expr_method_call(self, e);
    }
}

impl Ordered {
    fn conditional(&mut self, e: &Expr) {
        let mut any = AnyCall(0);
        any.visit_expr(e);
        self.conditional += any.0;
    }

    fn walk(&mut self, e: &Expr) {
        match e {
            Expr::Call(c) => {
                self.walk(&c.func);
                for a in &c.args {
                    self.walk(a);
                }
                if let Expr::Path(p) = &*c.func {
                    if let Some(seg) = p.path.segments.last() {
                        self.call(&clean(&seg.ident));
                    }
                }
            }
            Expr::MethodCall(m) => {
                self.walk(&m.receiver);
                for a in &m.args {
                    self.walk(a);
                }
                self.call(&clean(&m.method));
            }
            Expr::Await(a) => self.walk(&a.base),
            Expr::Try(t) => self.walk(&t.expr),
            Expr::Paren(p) => self.walk(&p.expr),
            Expr::Group(g) => self.walk(&g.expr),
            Expr::Reference(r) => self.walk(&r.expr),
            Expr::Unary(u) => self.walk(&u.expr),
            Expr::Cast(c) => self.walk(&c.expr),
            Expr::Field(f) => self.walk(&f.base),
            Expr::Index(i) => {
                self.walk(&i.expr);
                self.walk(&i.index);
            }
            Expr::Assign(a) => {
                self.walk(&a.right);
                self.walk(&a.left);
            }
            Expr::Return(r) => {
                if let Some(v) = &r.expr {
                    self.walk(v);
                }
            }
            Expr::Binary(b) => {
                self.walk(&b.left);
                if matches!(b.op, syn::BinOp::And(_) | syn::BinOp::Or(_)) {
                    self.conditional(&b.right);
                } else {
                    self.walk(&b.right);
                }
            }
            Expr::Tuple(t) => t.elems.iter().for_each(|x| self.walk(x)),
            Expr::Array(t) => t.elems.iter().for_each(|x| self.walk(x)),
            // Anything that branches, repeats, defers or nests: counted, not ordered.
            Expr::If(_)
            | Expr::Match(_)
            | Expr::While(_)
            | Expr::Loop(_)
            | Expr::ForLoop(_)
            | Expr::Closure(_)
            | Expr::Block(_)
            | Expr::Unsafe(_)
            | Expr::Async(_)
            | Expr::TryBlock(_) => self.conditional(e),
            _ => {}
        }
    }
}

impl Collector {
    pub(crate) fn new() -> Self {
        Collector {
            sources: BTreeMap::new(),
            parse_failures: BTreeSet::new(),
            impls: Vec::new(),
            errors: Vec::new(),
            tests: Vec::new(),
            calls: Vec::new(),
            exceptions: BTreeSet::new(),
            item_macros: 0,
            skipped: BTreeSet::new(),
            seen: BTreeSet::new(),
        }
    }

    /// A file the graph is about to parse. `false`: its text is not the repository's own
    /// implementation, and it is not observed (it is still in the graph).
    pub(crate) fn admit(&mut self, file: &str, text: &str) -> bool {
        if vendored(file) {
            self.skipped.insert(Skipped {
                path: file.to_string(),
                reason: "vendored tree".to_string(),
            });
            return false;
        }
        if generated(text) {
            self.skipped.insert(Skipped {
                path: file.to_string(),
                reason: "generated file".to_string(),
            });
            return false;
        }
        self.sources
            .insert(file.to_string(), digest_of(text.as_bytes()));
        true
    }

    pub(crate) fn parse_failed(&mut self, file: &str) {
        self.sources.remove(file);
        self.parse_failures.insert(file.to_string());
    }

    /// One module level's items. Called by the graph's own walk for each item list, so inline
    /// `mod` bodies arrive as their own call.
    pub(crate) fn items(&mut self, krate: &str, file: &str, module: &[String], items: &[Item]) {
        if !self.sources.contains_key(file)
            || !self.seen.insert((file.to_string(), module.to_vec()))
        {
            return;
        }
        for item in items {
            match item {
                Item::Impl(i) => self.impl_item(krate, file, module, i),
                Item::Struct(s) => self.maybe_error(krate, file, module, &s.ident, &s.attrs),
                Item::Enum(e) => self.maybe_error(krate, file, module, &e.ident, &e.attrs),
                Item::Fn(f) => self.function(krate, file, module, None, &f.attrs, &f.sig, &f.block),
                Item::Macro(m) => {
                    // An item macro may define anything; none of it can be seen.
                    self.item_macros += 1;
                    let _ = m;
                }
                _ => {}
            }
        }
    }

    fn maybe_error(
        &mut self,
        krate: &str,
        file: &str,
        module: &[String],
        ident: &proc_macro2::Ident,
        attrs: &[syn::Attribute],
    ) {
        let name = clean(ident);
        if name.ends_with("Error") {
            self.errors.push(ErrorFact {
                krate: krate.to_string(),
                file: file.to_string(),
                line: line_of(ident.span()),
                module: module.to_vec(),
                name,
                derives: derives(attrs),
                deprecated: deprecated(attrs),
            });
        }
    }

    fn impl_item(&mut self, krate: &str, file: &str, module: &[String], i: &syn::ItemImpl) {
        let self_name = type_name(&i.self_ty);
        if let Some((_, trait_path, _)) = &i.trait_ {
            let Some(last) = trait_path.segments.last() else {
                return;
            };
            let mut methods: Vec<String> = i
                .items
                .iter()
                .filter_map(|m| match m {
                    ImplItem::Fn(f) => Some(clean(&f.sig.ident)),
                    _ => None,
                })
                .collect();
            methods.sort();
            if self_name.is_none() {
                self.exceptions.insert(Exception {
                    path: file.to_string(),
                    line: line_of(last.ident.span()),
                    kind: "impl_for_non_path_type".to_string(),
                    detail: format!(
                        "impl {} for a type that is not a plain path; its subject is the file",
                        clean(&last.ident)
                    ),
                });
            }
            self.impls.push(ImplFact {
                krate: krate.to_string(),
                file: file.to_string(),
                line: line_of(last.ident.span()),
                self_name: self_name.clone(),
                trait_name: clean(&last.ident),
                methods,
                deprecated: deprecated(&i.attrs),
            });
        }
        let Some(owner) = self_name else { return };
        for member in &i.items {
            if let ImplItem::Fn(f) = member {
                self.function(
                    krate,
                    file,
                    module,
                    Some(&owner),
                    &f.attrs,
                    &f.sig,
                    &f.block,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn function(
        &mut self,
        krate: &str,
        file: &str,
        module: &[String],
        owner: Option<&str>,
        attrs: &[syn::Attribute],
        sig: &syn::Signature,
        block: &syn::Block,
    ) {
        let name = clean(&sig.ident);
        let line = line_of(sig.ident.span());
        let qualified = match owner {
            Some(o) => format!("{o}::{name}"),
            None => name.clone(),
        };
        if let Some(attr) = test_attr(attrs) {
            self.tests.push(TestFact {
                krate: krate.to_string(),
                file: file.to_string(),
                line,
                module: module.to_vec(),
                name: qualified,
                attr,
                is_async: sig.asyncness.is_some(),
                returns_value: !matches!(sig.output, syn::ReturnType::Default),
            });
            return;
        }
        let mut ordered = Ordered {
            calls: Vec::new(),
            conditional: 0,
        };
        for stmt in &block.stmts {
            match stmt {
                Stmt::Local(l) => {
                    if let Some(init) = &l.init {
                        ordered.walk(&init.expr);
                        if let Some((_, diverge)) = &init.diverge {
                            ordered.conditional(diverge);
                        }
                    }
                }
                Stmt::Expr(e, _) => ordered.walk(e),
                _ => {}
            }
        }
        let has = |c: &str| ordered.calls.contains(&c.to_string().as_str());
        let (validates, effects) = (has("validate"), has("effect"));
        if validates && effects {
            let mut classes: Vec<&'static str> = Vec::new();
            for c in &ordered.calls {
                if classes.last() != Some(c) {
                    classes.push(c);
                }
            }
            self.calls.push(CallFact {
                krate: krate.to_string(),
                file: file.to_string(),
                line,
                module: module.to_vec(),
                name: qualified,
                classes,
                also_conditional: ordered.conditional > 0,
                deprecated: deprecated(attrs),
            });
        } else if ordered.conditional > 0 && (validates || effects) {
            // One half is in straight-line code and the other only under a condition: no order
            // is claimed, and the function is recorded as the exception it is.
            self.exceptions.insert(Exception {
                path: file.to_string(),
                line,
                kind: "order_not_established".to_string(),
                detail: format!(
                    "{} calls {} in straight-line code and another class only under a condition, a loop or a closure",
                    qualified_name(module, &qualified),
                    if validates { "a validate*" } else { "an effect" }
                ),
            });
        }
    }
}

// ---------------------------------------------------------------------------------------------
// From facts to a report
// ---------------------------------------------------------------------------------------------

pub(crate) struct GraphView<'a> {
    pub snapshot: SnapshotId,
    /// `(crate dir, name, kind, id)` of every symbol node.
    pub symbols: Vec<(&'a str, &'a str, &'a str, &'a str)>,
    pub symbol_ids: BTreeSet<&'a str>,
    /// `Tests` edges: test node id to the symbols it mentions.
    pub test_mentions: BTreeMap<&'a str, Vec<&'a str>>,
    pub rust_files: Vec<String>,
    pub analyzed_by_graph: BTreeSet<String>,
}

fn qualified_name(module: &[String], tail: &str) -> String {
    qualified(module, tail)
}

fn qualified(module: &[String], tail: &str) -> String {
    if module.is_empty() {
        tail.to_string()
    } else {
        format!("{}::{tail}", module.join("::"))
    }
}

struct Draft {
    category: PatternCategory,
    family: String,
    name: String,
    signature: Vec<String>,
    subject: String,
    file: String,
    krate: String,
    line: usize,
    flags: Vec<String>,
    mentions: Vec<String>,
}

impl Collector {
    pub(crate) fn finish(self, view: &GraphView<'_>) -> PatternReport {
        let mut drafts: Vec<Draft> = Vec::new();
        let mut extra_exceptions: Vec<Exception> = Vec::new();
        let file_node = |f: &str| format!("file:{f}");
        let unique_symbol = |krate: &str, name: &str, kinds: &[&str]| -> Option<String> {
            let ids: BTreeSet<&str> = view
                .symbols
                .iter()
                .filter(|(k, n, kind, _)| *k == krate && *n == name && kinds.contains(kind))
                .map(|(_, _, _, id)| *id)
                .collect();
            (ids.len() == 1).then(|| ids.iter().next().unwrap().to_string())
        };

        // Trait impls.
        for i in &self.impls {
            let subject = i
                .self_name
                .as_deref()
                .and_then(|n| unique_symbol(&i.krate, n, &["struct", "enum", "type"]))
                .unwrap_or_else(|| file_node(&i.file));
            let mut flags = Vec::new();
            if i.deprecated {
                flags.push("legacy".to_string());
            }
            drafts.push(Draft {
                category: PatternCategory::TraitImpl,
                family: i.trait_name.clone(),
                name: format!(
                    "{} as {}",
                    i.self_name.as_deref().unwrap_or("<non-path type>"),
                    i.trait_name
                ),
                signature: i.methods.clone(),
                subject,
                file: i.file.clone(),
                krate: i.krate.clone(),
                line: i.line,
                flags,
                mentions: Vec::new(),
            });
        }

        // Error types: the traits implemented for them in the same crate, when the name is unique.
        let mut by_crate_name: BTreeMap<(&str, &str), usize> = BTreeMap::new();
        for e in &self.errors {
            *by_crate_name.entry((&e.krate, &e.name)).or_default() += 1;
        }
        for e in &self.errors {
            let ambiguous = by_crate_name[&(e.krate.as_str(), e.name.as_str())] > 1;
            let mut signature: Vec<String> =
                e.derives.iter().map(|d| format!("derive:{d}")).collect();
            if ambiguous {
                extra_exceptions.push(Exception {
                    path: e.file.clone(),
                    line: e.line,
                    kind: "ambiguous_type_name".to_string(),
                    detail: format!(
                        "{} is defined more than once in its crate, so no trait is matched to it",
                        e.name
                    ),
                });
            } else {
                for i in &self.impls {
                    if i.krate == e.krate && i.self_name.as_deref() == Some(e.name.as_str()) {
                        signature.push(format!("impl:{}", i.trait_name));
                    }
                }
            }
            signature.sort();
            signature.dedup();
            let mut flags = Vec::new();
            if e.deprecated {
                flags.push("legacy".to_string());
            }
            if ambiguous {
                flags.push("ambiguous_type_name".to_string());
            }
            drafts.push(Draft {
                category: PatternCategory::ErrorType,
                family: "error_type".to_string(),
                name: qualified(&e.module, &e.name),
                signature,
                subject: unique_symbol(&e.krate, &e.name, &["struct", "enum"])
                    .unwrap_or_else(|| file_node(&e.file)),
                file: e.file.clone(),
                krate: e.krate.clone(),
                line: e.line,
                flags,
                mentions: Vec::new(),
            });
        }

        // Tests.
        for t in &self.tests {
            let id = format!("test:{}:{}", t.file, qualified(&t.module, &t.name));
            let mut mentions: Vec<String> = view
                .test_mentions
                .get(id.as_str())
                .map(|v| v.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default();
            mentions.sort();
            drafts.push(Draft {
                category: PatternCategory::TestShape,
                family: "test_shape".to_string(),
                name: qualified(&t.module, &t.name),
                signature: vec![
                    format!("attr:{}", t.attr),
                    (if t.is_async { "async" } else { "sync" }).to_string(),
                    (if t.returns_value {
                        "returns_value"
                    } else {
                        "returns_unit"
                    })
                    .to_string(),
                    (if is_integration_test(&t.file) {
                        "integration"
                    } else {
                        "unit"
                    })
                    .to_string(),
                ],
                subject: id,
                file: t.file.clone(),
                krate: t.krate.clone(),
                line: t.line,
                flags: Vec::new(),
                mentions,
            });
        }

        // Call order.
        for c in &self.calls {
            let id = format!("symbol:{}:{}", c.file, qualified(&c.module, &c.name));
            let subject = if view.symbol_ids.contains(id.as_str()) {
                id
            } else {
                file_node(&c.file)
            };
            let mut flags = vec![
                "name_based".to_string(),
                "ordering_in_top_level_statements".to_string(),
            ];
            if c.also_conditional {
                flags.push("also_calls_under_conditions".to_string());
            }
            if c.deprecated {
                flags.push("legacy".to_string());
            }
            drafts.push(Draft {
                category: PatternCategory::CallOrder,
                family: "call_order".to_string(),
                name: qualified(&c.module, &c.name),
                signature: c.classes.iter().map(|s| s.to_string()).collect(),
                subject,
                file: c.file.clone(),
                krate: c.krate.clone(),
                line: c.line,
                flags,
                mentions: Vec::new(),
            });
        }

        // Stable ids. The line is not part of identity. Observations that would share an id
        // (the same structure twice in one file under one name) are numbered in source order.
        drafts.sort_by(|a, b| {
            (&a.file, a.line, &a.name, a.category).cmp(&(&b.file, b.line, &b.name, b.category))
        });
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        let mut observations: Vec<Observation> = Vec::new();
        let mut crates_of: BTreeMap<String, String> = BTreeMap::new();
        for d in drafts {
            let sig = d.signature.join(",");
            let base = short_id(&[d.category.as_str(), &d.file, &d.name, &sig]);
            let n = seen.entry(base.clone()).or_default();
            *n += 1;
            let id = if *n == 1 { base } else { format!("{base}#{n}") };
            crates_of.insert(id.clone(), d.krate);
            observations.push(Observation {
                id,
                category: d.category,
                family: d.family,
                name: d.name,
                signature: d.signature,
                subject: d.subject,
                file: d.file,
                line: d.line,
                flags: d.flags,
                mentions: d.mentions,
            });
        }
        observations.sort_by(|a, b| a.id.cmp(&b.id));

        let patterns = group(&observations, &crates_of);

        // Coverage.
        let mut skipped = self.skipped.clone();
        for f in &view.rust_files {
            if self.sources.contains_key(f)
                || self.parse_failures.contains(f)
                || skipped.iter().any(|s| &s.path == f)
            {
                continue;
            }
            let reason = if view.analyzed_by_graph.contains(f) {
                "not observed"
            } else {
                "not reachable from a crate root, so the graph did not parse it"
            };
            skipped.insert(Skipped {
                path: f.clone(),
                reason: reason.to_string(),
            });
        }
        let parse_failures: Vec<String> = self.parse_failures.iter().cloned().collect();
        let coverage = Coverage {
            rust_files_discovered: view.rust_files.len(),
            files_analyzed: self.sources.len(),
            complete: skipped.is_empty() && parse_failures.is_empty() && self.item_macros == 0,
            skipped: skipped.into_iter().collect(),
            parse_failures,
            unexpanded_item_macros: self.item_macros,
            ignore_rules: "built-in skip list (.git, target, .chip, node_modules) and vendor/ for observations; .gitignore is not read".to_string(),
        };
        let mut exceptions: Vec<Exception> = self.exceptions.into_iter().collect();
        exceptions.extend(extra_exceptions);
        for f in &coverage.parse_failures {
            exceptions.push(Exception {
                path: f.clone(),
                line: 0,
                kind: "parse_failure".to_string(),
                detail: "the file does not parse; nothing in it was observed".to_string(),
            });
        }
        exceptions.sort();

        let analyzer = Analyzer {
            id: ANALYZER_ID.to_string(),
            version: ANALYZER_VERSION.to_string(),
            validate_prefixes: VALIDATE_PREFIXES.iter().map(|s| s.to_string()).collect(),
            effect_names: EFFECT_NAMES.iter().map(|s| s.to_string()).collect(),
        };
        let mut report = PatternReport {
            schema_version: PATTERN_SCHEMA.to_string(),
            analyzer,
            graph_snapshot: view.snapshot.clone(),
            coverage,
            sources: self.sources,
            observations,
            patterns,
            exceptions,
            report_id: String::new(),
        };
        report.report_id = digest_of(&report.canonical_bytes());
        report
    }
}

fn caveats(category: PatternCategory, family: &str) -> Vec<String> {
    let mut v = vec![
        "frequency is evidence of a convention, not proof that it is correct or required"
            .to_string(),
    ];
    match category {
        PatternCategory::TraitImpl => v.push(format!(
            "the method set may be what `{family}` requires of every implementor, which a repetition would not show"
        )),
        PatternCategory::CallOrder => v.push(
            "call classes are read from callee names only, and order is established only for top-level straight-line statements"
                .to_string(),
        ),
        PatternCategory::TestShape => v.push(
            "a test's mentions are identifiers its body names, not coverage".to_string(),
        ),
        PatternCategory::ErrorType => v.push(
            "traits are matched to a type by name within one crate".to_string(),
        ),
    }
    v
}

fn group(observations: &[Observation], crates_of: &BTreeMap<String, String>) -> Vec<Pattern> {
    type Key<'a> = (PatternCategory, &'a str);
    let mut families: BTreeMap<Key<'_>, BTreeMap<&[String], Vec<&Observation>>> = BTreeMap::new();
    for o in observations {
        families
            .entry((o.category, o.family.as_str()))
            .or_default()
            .entry(o.signature.as_slice())
            .or_default()
            .push(o);
    }
    let mut patterns = Vec::new();
    for ((category, family), signatures) in &families {
        for (signature, members) in signatures {
            let counter: Vec<&Observation> = signatures
                .iter()
                .filter(|(s, _)| *s != signature)
                .flat_map(|(_, v)| v.iter().copied())
                .collect();
            let support = members.len();
            let classification = if support >= MIN_SUPPORT {
                Classification::CandidateConvention
            } else {
                Classification::Observed
            };
            let files: BTreeSet<&str> = members.iter().map(|o| o.file.as_str()).collect();
            let crates: BTreeSet<&str> = members
                .iter()
                .filter_map(|o| crates_of.get(&o.id).map(String::as_str))
                .collect();
            patterns.push(Pattern {
                id: short_id(&[category.as_str(), family, &signature.join(",")]),
                category: *category,
                family: family.to_string(),
                signature: signature.to_vec(),
                classification,
                authority: match classification {
                    Classification::CandidateConvention => "inferred_from_frequency",
                    Classification::Observed => "observed",
                }
                .to_string(),
                enforced: false,
                basis: "deterministic".to_string(),
                support: members.iter().map(|o| o.id.clone()).collect(),
                counterexamples: counter
                    .iter()
                    .take(MAX_COUNTEREXAMPLE_IDS)
                    .map(|o| o.id.clone())
                    .collect(),
                confidence: Confidence {
                    support,
                    counterexamples: counter.len(),
                    share_permille: support * 1000 / (support + counter.len()),
                    distinct_files: files.len(),
                    distinct_crates: crates.len(),
                    basis: "counts".to_string(),
                },
                caveats: caveats(*category, family),
            });
        }
    }
    patterns.sort_by(|a, b| a.id.cmp(&b.id));
    patterns
}
