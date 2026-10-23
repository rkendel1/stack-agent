//! Chip's own symbol index: every function with a body in a Rust source tree, with its exact byte
//! spans, its structural signature and a fingerprint of its source. Built with the parser
//! `chip-graph` uses (`syn` 2 with `proc-macro2` source locations); never with text matching.
//!
//! Ids follow `chip-graph`'s form, `symbol:<repository-relative file>:<qualified name>`. The
//! qualified name is the item's path inside its file (`outer::inner::f`, `Type::method`,
//! `Trait::method` for a default method). A second item with the same qualified name in one file
//! gets `#2`, `#3`, ... in source order, so every id names exactly one span.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use proc_macro2::{LineColumn, Span};
use quote::ToTokens;
use sha2::{Digest, Sha256};
use syn::spanned::Spanned;
use syn::visit::Visit;
use syn::{Attribute, Block, ImplItem, Item, Signature, TraitItem, Visibility};

const SKIPPED_DIRS: [&str; 4] = [".git", "target", ".chip", "node_modules"];
/// The largest source file the index reads.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// A half-open byte range `[start, end)` in a file's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ByteSpan {
    pub start: usize,
    pub end: usize,
}

/// The parts of a declaration that a frozen-signature edit must leave exactly as they were, each
/// as its token stream (structure, not layout: whitespace and comments do not count).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StructuralSignature {
    pub attributes: Vec<String>,
    pub visibility: String,
    pub qualifiers: String,
    pub name: String,
    pub generics: String,
    pub where_clause: String,
    pub parameters: Vec<String>,
    pub return_type: String,
}

impl StructuralSignature {
    fn of(attrs: &[Attribute], vis: Option<&Visibility>, sig: &Signature) -> Self {
        let tokens = |t: &dyn ToTokens| t.to_token_stream().to_string();
        Self {
            attributes: attrs.iter().map(|a| tokens(a)).collect(),
            visibility: vis.map(|v| tokens(v)).unwrap_or_default(),
            qualifiers: format!(
                "{} {} {} {}",
                sig.constness.map(|t| tokens(&t)).unwrap_or_default(),
                sig.asyncness.map(|t| tokens(&t)).unwrap_or_default(),
                sig.unsafety.map(|t| tokens(&t)).unwrap_or_default(),
                sig.abi.as_ref().map(|t| tokens(t)).unwrap_or_default()
            ),
            name: sig.ident.to_string(),
            generics: tokens(&sig.generics),
            where_clause: sig
                .generics
                .where_clause
                .as_ref()
                .map(|w| tokens(w))
                .unwrap_or_default(),
            parameters: sig
                .inputs
                .iter()
                .map(|p| tokens(p))
                .chain(sig.variadic.as_ref().map(|v| tokens(v)))
                .collect(),
            return_type: tokens(&sig.output),
        }
    }

    /// The first protected property that differs, by name, or `None` when all are equal.
    pub fn first_difference(&self, other: &StructuralSignature) -> Option<&'static str> {
        if self.attributes != other.attributes {
            Some("attributes")
        } else if self.visibility != other.visibility {
            Some("visibility")
        } else if self.qualifiers != other.qualifiers {
            Some("qualifiers (const, async, unsafe, extern)")
        } else if self.name != other.name {
            Some("name")
        } else if self.generics != other.generics || self.where_clause != other.where_clause {
            Some("generics")
        } else if self.parameters != other.parameters {
            Some("parameters")
        } else if self.return_type != other.return_type {
            Some("return type")
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    TraitDefaultMethod,
}

/// One function with a body, as the index found it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SymbolRecord {
    pub id: String,
    /// Repository-relative, `/`-separated.
    pub path: String,
    pub name: String,
    pub kind: SymbolKind,
    /// From the first attribute (doc comments included) to the body's closing brace.
    pub item: ByteSpan,
    /// The body block, braces included.
    pub body: ByteSpan,
    pub start_line: usize,
    pub end_line: usize,
    pub signature: StructuralSignature,
    /// The declaration's source text up to the body: what a neighbor is shown.
    pub declaration: String,
    /// `sha256:` of the item's exact source bytes.
    pub fingerprint: String,
    /// A `#[test]` function, one inside a `#[cfg(test)]` module, or one under `tests/`.
    pub is_test: bool,
    /// Names this function calls (the last segment of a path call, a method name).
    pub calls: Vec<String>,
    /// The item's path inside its file: `Type::method`, `outer::f`.
    pub qualified: String,
    /// For a method: the type its `impl` is for (or the trait, for a default method).
    pub impl_type: Option<String>,
    /// Every call, with its written path: `["EffectivePolicy", "compose"]`, or a method call.
    pub call_refs: Vec<CallRef>,
    /// Type-like names (capitalized path segments) the function names in its signature and body.
    pub type_refs: Vec<String>,
    /// The Cargo package the file belongs to, as a crate identifier (`compute_policy`). Set by
    /// [`SymbolIndex::build`]; `None` when no package manifest owns the file.
    pub crate_name: Option<String>,
    /// Every identifier in the body, sorted: field names, locals, paths, method names.
    pub body_idents: Vec<String>,
    /// For a method of `impl Trait for Type`: the trait (its last path segment).
    pub impl_trait: Option<String>,
    /// Traits named as bounds in the signature and body: `dyn T`, `impl T`, `X: T`, `where X: T`.
    pub trait_refs: Vec<String>,
    /// The module path of the file inside its crate (`engine::policy`; empty at a crate root).
    pub module: String,
    /// The line the body's opening brace is on: lines before it are the declaration.
    pub body_line: usize,
    /// The 0-based character column of the body's opening brace on `body_line`.
    pub body_column: usize,
    /// The type and trait names of the declared return type.
    pub returns: TypeNames,
    /// The declared return type names `Self` (the impl's type).
    pub returns_self: bool,
    /// Named values in the body and parameters, with what the source says about their type.
    pub bindings: Vec<Binding>,
}

/// What a parameter or local's declaration says about its type. Only what is written: a type
/// annotation or parameter type, the call it is initialized from (whose declared return type the
/// target search may follow), or another value it aliases (a `for` loop's or closure's element).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Binding {
    pub name: String,
    /// From a type annotation, a parameter type, a struct literal or a cast.
    pub types: Vec<String>,
    pub traits: Vec<String>,
    /// Initialized from this call's result (after `?`, `.await`, `.unwrap()` and similar).
    pub call: Option<CallRef>,
    /// Initialized from, or iterating over, this value: a local or a field chain.
    pub alias: Option<String>,
}

fn idents(tokens: proc_macro2::TokenStream, out: &mut Vec<String>) {
    for t in tokens {
        match t {
            proc_macro2::TokenTree::Ident(i) => out.push(i.to_string()),
            proc_macro2::TokenTree::Group(g) => idents(g.stream(), out),
            _ => {}
        }
    }
}

/// One call as written: a path call keeps its segments; a method call has only its name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct CallRef {
    pub path: Vec<String>,
    pub method: bool,
    /// A method call's receiver when it is `self` or a chain of field accesses from a plain name:
    /// `"self"`, `"self.state.config"`, `"config.authorizer"`. `None` for any other expression.
    #[serde(default)]
    pub receiver: Option<String>,
    /// For a call the source does not write as one: the operator or method that implies it
    /// (`"operator <"`, `".max()"`). Its `path` then names the trait methods it may invoke.
    #[serde(default)]
    pub implicit: Option<String>,
}

/// The receiver form `CallRef::receiver` records: `self`, a plain local, or a field chain from
/// either. `None` for any other expression.
fn receiver_of(e: &syn::Expr) -> Option<String> {
    chain(e)
}

/// `self`, `x`, `self.a.b`, `x.a`, looking through `&`, `&mut` and parentheses.
fn chain(e: &syn::Expr) -> Option<String> {
    {
        match e {
            syn::Expr::Path(p) => p.path.get_ident().map(|i| i.to_string()),
            syn::Expr::Field(f) => Some(format!(
                "{}.{}",
                chain(&f.base)?,
                match &f.member {
                    syn::Member::Named(i) => i.to_string(),
                    syn::Member::Unnamed(i) => i.index.to_string(),
                }
            )),
            syn::Expr::Reference(r) => chain(&r.expr),
            syn::Expr::Paren(p) => chain(&p.expr),
            _ => None,
        }
    }
}

/// Methods that pass a value through without changing what it is, for initializer tracing.
const PASS_THROUGH: [&str; 13] = [
    "unwrap",
    "expect",
    "clone",
    "unwrap_or_default",
    "unwrap_or",
    "unwrap_or_else",
    "to_owned",
    "as_ref",
    "as_mut",
    "borrow",
    "borrow_mut",
    "iter",
    "into_iter",
];

/// Iterator adapters whose closure parameter is an element of the receiver.
const ELEMENT_CLOSURES: [&str; 10] = [
    "map",
    "for_each",
    "filter",
    "filter_map",
    "find",
    "any",
    "all",
    "flat_map",
    "position",
    "inspect",
];

fn strip(mut e: &syn::Expr) -> &syn::Expr {
    loop {
        e = match e {
            syn::Expr::Try(t) => &t.expr,
            syn::Expr::Await(a) => &a.base,
            syn::Expr::Reference(r) => &r.expr,
            syn::Expr::Paren(p) => &p.expr,
            syn::Expr::MethodCall(m) if PASS_THROUGH.contains(&m.method.to_string().as_str()) => {
                &m.receiver
            }
            _ => return e,
        }
    }
}

fn call_ref(e: &syn::Expr) -> Option<CallRef> {
    match e {
        syn::Expr::Call(c) => match &*c.func {
            syn::Expr::Path(p) => Some(CallRef {
                path: p
                    .path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect(),
                method: false,
                receiver: None,
                implicit: None,
            }),
            _ => None,
        },
        syn::Expr::MethodCall(m) => Some(CallRef {
            path: vec![m.method.to_string()],
            method: true,
            receiver: receiver_of(&m.receiver),
            implicit: None,
        }),
        _ => None,
    }
}

/// The bindings of one function: its typed parameters, `let`s, `for` loops and element closures.
struct Bindings<'b> {
    bounds: &'b BTreeMap<String, Vec<String>>,
    out: Vec<Binding>,
}

impl Bindings<'_> {
    fn record(&mut self, name: String, ty: Option<&syn::Type>, init: Option<&syn::Expr>) {
        let mut b = Binding {
            name,
            ..Binding::default()
        };
        if let Some(ty) = ty {
            let t = TypeNames::of(ty, self.bounds);
            b.types = t.types;
            b.traits = t.traits;
        }
        if let Some(init) = init.map(strip) {
            match init {
                syn::Expr::Struct(st) => {
                    if let Some(seg) = st.path.segments.last() {
                        b.types.push(seg.ident.to_string());
                    }
                }
                syn::Expr::Cast(c) => {
                    let t = TypeNames::of(&c.ty, self.bounds);
                    b.types.extend(t.types);
                }
                other => {
                    b.call = call_ref(other);
                    if b.call.is_none() {
                        b.alias = chain(other);
                    }
                }
            }
        }
        if !b.types.is_empty() || !b.traits.is_empty() || b.call.is_some() || b.alias.is_some() {
            self.out.push(b);
        }
    }
}

fn pat_ident(p: &syn::Pat) -> Option<(String, Option<&syn::Type>)> {
    match p {
        syn::Pat::Ident(i) => Some((i.ident.to_string(), None)),
        syn::Pat::Type(t) => match &*t.pat {
            syn::Pat::Ident(i) => Some((i.ident.to_string(), Some(&*t.ty))),
            _ => None,
        },
        syn::Pat::Reference(r) => pat_ident(&r.pat),
        _ => None,
    }
}

impl<'ast> Visit<'ast> for Bindings<'_> {
    fn visit_local(&mut self, local: &'ast syn::Local) {
        if let Some((name, ty)) = pat_ident(&local.pat) {
            self.record(name, ty, local.init.as_ref().map(|i| &*i.expr));
        }
        syn::visit::visit_local(self, local);
    }
    fn visit_expr_for_loop(&mut self, f: &'ast syn::ExprForLoop) {
        if let Some((name, ty)) = pat_ident(&f.pat) {
            let over = strip(&f.expr);
            let mut b = Binding {
                name,
                alias: chain(over),
                call: if chain(over).is_none() {
                    call_ref(over)
                } else {
                    None
                },
                ..Binding::default()
            };
            if let Some(ty) = ty {
                let t = TypeNames::of(ty, self.bounds);
                b.types = t.types;
                b.traits = t.traits;
            }
            self.out.push(b);
        }
        syn::visit::visit_expr_for_loop(self, f);
    }
    fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
        if ELEMENT_CLOSURES.contains(&m.method.to_string().as_str())
            && let Some(over) = chain(strip(&m.receiver))
        {
            for arg in &m.args {
                if let syn::Expr::Closure(c) = arg
                    && c.inputs.len() == 1
                    && let Some((name, _)) = pat_ident(&c.inputs[0])
                {
                    self.out.push(Binding {
                        name,
                        alias: Some(over.clone()),
                        ..Binding::default()
                    });
                }
            }
        }
        syn::visit::visit_expr_method_call(self, m);
    }
}

/// The trait methods an operator or method implies, and the traits they belong to.
pub fn implicit_targets(label: &str) -> (&'static [&'static str], &'static [&'static str]) {
    match label {
        "operator <" | "operator <=" | "operator >" | "operator >=" | ".max()" | ".min()"
        | ".sort()" | ".sort_unstable()" | ".binary_search()" | ".clamp()" => {
            (&["cmp", "partial_cmp"], &["Ord", "PartialOrd"])
        }
        "operator ==" | "operator !=" | ".contains()" | ".dedup()" => (&["eq"], &["PartialEq"]),
        "operator +" | "operator +=" => (&["add", "add_assign"], &["Add", "AddAssign"]),
        "operator -" | "operator -=" => (&["sub", "sub_assign"], &["Sub", "SubAssign"]),
        "operator *" | "operator *=" => (&["mul", "mul_assign"], &["Mul", "MulAssign"]),
        "operator /" | "operator /=" => (&["div", "div_assign"], &["Div", "DivAssign"]),
        _ => (&[], &[]),
    }
}

fn implicit(label: String) -> CallRef {
    let (methods, _) = implicit_targets(&label);
    CallRef {
        path: methods.iter().map(|m| m.to_string()).collect(),
        method: true,
        receiver: None,
        implicit: Some(label),
    }
}

fn is_literal(e: &syn::Expr) -> bool {
    match e {
        syn::Expr::Lit(_) => true,
        syn::Expr::Unary(u) => is_literal(&u.expr),
        syn::Expr::Paren(p) => is_literal(&p.expr),
        _ => false,
    }
}

/// The type and trait names a written type mentions: capitalized path segments, and the traits
/// in its `dyn` and `impl` bounds.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TypeNames {
    pub types: Vec<String>,
    pub traits: Vec<String>,
}

impl TypeNames {
    fn of(ty: &syn::Type, generic_bounds: &BTreeMap<String, Vec<String>>) -> Self {
        let mut c = Calls::default();
        c.visit_type(ty);
        let mut out = TypeNames {
            types: c.2,
            traits: c.3,
        };
        // A field of a generic parameter type carries the parameter's bounds.
        for t in out.types.clone() {
            if let Some(b) = generic_bounds.get(&t) {
                out.traits.extend(b.iter().cloned());
            }
        }
        out.types.sort();
        out.types.dedup();
        out.traits.sort();
        out.traits.dedup();
        out
    }
}

/// What the index knows about a named struct: its fields' type and trait names. Structs with the
/// same name in different files are merged (the index keys types by name, not by path).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TypeFacts {
    pub fields: BTreeMap<String, TypeNames>,
    pub files: Vec<String>,
}

/// A trait definition: every method it declares (with or without a default body).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TraitFacts {
    pub methods: Vec<String>,
    pub files: Vec<String>,
}

/// A non-function item's lines: what a diagnostic outside every function body is inside.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemSpan {
    /// `struct`, `enum`, `trait`, `type`, `const`, `static`, `use`, `impl`, `union`, `macro`.
    pub kind: String,
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// Everything one file contributes to the index.
#[derive(Debug, Clone, Default)]
pub struct FileIndex {
    pub symbols: Vec<SymbolRecord>,
    pub types: BTreeMap<String, TypeFacts>,
    pub traits: BTreeMap<String, TraitFacts>,
    pub items: Vec<ItemSpan>,
    /// `(type, trait)` for every `impl Trait for Type`, with or without methods.
    pub trait_impls: Vec<(String, String)>,
}

fn generic_bounds(generics: &syn::Generics) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let bound_names = |bounds: &mut dyn Iterator<Item = &syn::TypeParamBound>| -> Vec<String> {
        bounds
            .filter_map(|b| match b {
                syn::TypeParamBound::Trait(t) => {
                    t.path.segments.last().map(|s| s.ident.to_string())
                }
                _ => None,
            })
            .collect()
    };
    for p in generics.type_params() {
        out.entry(p.ident.to_string())
            .or_default()
            .extend(bound_names(&mut p.bounds.iter()));
    }
    if let Some(w) = &generics.where_clause {
        for pred in &w.predicates {
            if let syn::WherePredicate::Type(t) = pred
                && let syn::Type::Path(p) = &t.bounded_ty
                && let Some(id) = p.path.get_ident()
            {
                out.entry(id.to_string())
                    .or_default()
                    .extend(bound_names(&mut t.bounds.iter()));
            }
        }
    }
    out
}

/// The module path a file has inside its crate, from its location: `src/engine/policy.rs` and
/// `src/engine/policy/mod.rs` are `engine::policy`; `lib.rs`, `main.rs` and files outside `src/`
/// (tests, examples, build scripts) are crate roots.
pub fn module_of(path: &str) -> String {
    let inner = match path.rfind("/src/") {
        Some(i) => &path[i + 5..],
        None => match path.strip_prefix("src/") {
            Some(rest) => rest,
            None => return String::new(),
        },
    };
    let inner = inner.strip_suffix(".rs").unwrap_or(inner);
    let mut parts: Vec<&str> = inner.split('/').collect();
    if parts.first() == Some(&"bin") {
        return String::new();
    }
    if matches!(parts.last(), Some(&"mod") | Some(&"lib") | Some(&"main")) {
        parts.pop();
    }
    parts.join("::")
}

impl SymbolRecord {
    pub fn contains_line(&self, line: usize) -> bool {
        (self.start_line..=self.end_line).contains(&line)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    Io(String),
    Parse { path: String, message: String },
    NotRelative(String),
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Io(m) => write!(f, "cannot read source: {m}"),
            IndexError::Parse { path, message } => write!(f, "{path} does not parse: {message}"),
            IndexError::NotRelative(p) => write!(f, "{p} is not a plain relative path"),
        }
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

/// Line starts, for converting `proc-macro2`'s line and character column to a byte offset.
struct Lines<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { text, starts }
    }

    fn offset(&self, at: LineColumn) -> usize {
        let start = self
            .starts
            .get(at.line.saturating_sub(1))
            .copied()
            .unwrap_or(self.text.len());
        let line = &self.text[start..];
        start
            + line
                .char_indices()
                .nth(at.column)
                .map(|(i, _)| i)
                .unwrap_or(line.len())
    }
}

/// Called names, calls as written, type-like names, and traits named as bounds.
#[derive(Default)]
struct Calls(Vec<String>, Vec<CallRef>, Vec<String>, Vec<String>);

impl<'ast> Visit<'ast> for Calls {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*call.func
            && let Some(seg) = p.path.segments.last()
        {
            self.0.push(seg.ident.to_string());
            self.1.push(CallRef {
                path: p
                    .path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect(),
                method: false,
                receiver: None,
                implicit: None,
            });
        }
        syn::visit::visit_expr_call(self, call);
    }
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        let name = call.method.to_string();
        self.0.push(name.clone());
        self.1.push(CallRef {
            path: vec![name.clone()],
            method: true,
            receiver: receiver_of(&call.receiver),
            implicit: None,
        });
        let label = format!(".{name}()");
        if !implicit_targets(&label).0.is_empty() {
            self.1.push(implicit(label));
        }
        syn::visit::visit_expr_method_call(self, call);
    }
    /// A comparison or arithmetic operator between two non-literal operands may call a trait
    /// method (`PartialOrd::partial_cmp`, `Add::add`) of a user type.
    fn visit_expr_binary(&mut self, b: &'ast syn::ExprBinary) {
        if !is_literal(&b.left) && !is_literal(&b.right) {
            let op = b.op.to_token_stream().to_string();
            let label = format!("operator {op}");
            if !implicit_targets(&label).0.is_empty() {
                self.1.push(implicit(label));
            }
        }
        syn::visit::visit_expr_binary(self, b);
    }
    fn visit_path(&mut self, path: &'ast syn::Path) {
        for seg in &path.segments {
            let name = seg.ident.to_string();
            // `Self` and the prelude's enum variants are not type names.
            if name.starts_with(|c: char| c.is_ascii_uppercase())
                && !matches!(name.as_str(), "Self" | "Some" | "None" | "Ok" | "Err")
            {
                self.2.push(name);
            }
        }
        syn::visit::visit_path(self, path);
    }
    fn visit_trait_bound(&mut self, bound: &'ast syn::TraitBound) {
        if let Some(seg) = bound.path.segments.last() {
            self.3.push(seg.ident.to_string());
        }
        syn::visit::visit_trait_bound(self, bound);
    }
    /// Macro arguments are tokens to `syn`; `assert_eq!(f(), 5)` and the like are read as
    /// comma-separated expressions when they parse as such, and skipped when they do not.
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        use syn::parse::Parser;
        let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
        if let Ok(args) = parser.parse2(mac.tokens.clone()) {
            for arg in &args {
                self.visit_expr(arg);
            }
        }
    }
}

fn has_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.path().is_ident(name))
}

fn cfg_test(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|a| a.path().is_ident("cfg") && a.meta.to_token_stream().to_string().contains("test"))
}

struct Collector<'a> {
    path: &'a str,
    text: &'a str,
    lines: Lines<'a>,
    in_tests_dir: bool,
    out: Vec<SymbolRecord>,
    seen: BTreeMap<String, usize>,
    current_impl: Option<String>,
    current_trait: Option<String>,
    module: String,
    types: BTreeMap<String, TypeFacts>,
    traits: BTreeMap<String, TraitFacts>,
    item_spans: Vec<ItemSpan>,
    trait_impls: Vec<(String, String)>,
}

impl Collector<'_> {
    fn span_item(&mut self, kind: &str, name: String, tokens: &dyn ToTokens) {
        let mut stream = tokens.to_token_stream().into_iter();
        let Some(first) = stream.next() else { return };
        let end = stream.last().map(|t| t.span()).unwrap_or(first.span());
        self.item_spans.push(ItemSpan {
            kind: kind.to_string(),
            name,
            start_line: first.span().start().line,
            end_line: end.end().line,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        scope: &[String],
        name: String,
        kind: SymbolKind,
        attrs: &[Attribute],
        vis: Option<&Visibility>,
        sig: &Signature,
        block: &Block,
        first: Span,
        in_test_scope: bool,
    ) {
        let start = self.lines.offset(first.start());
        let body = ByteSpan {
            start: self.lines.offset(block.brace_token.span.open().start()),
            end: self.lines.offset(block.brace_token.span.close().end()),
        };
        let item = ByteSpan {
            start,
            end: body.end,
        };
        let qualified = if scope.is_empty() {
            name.clone()
        } else {
            format!("{}::{name}", scope.join("::"))
        };
        let n = self.seen.entry(qualified.clone()).or_insert(0);
        *n += 1;
        let qualified = if *n == 1 {
            qualified
        } else {
            format!("{qualified}#{n}")
        };
        let mut calls = Calls::default();
        calls.visit_signature(sig);
        calls.visit_block(block);
        calls.0.sort();
        calls.0.dedup();
        calls.1.sort();
        calls.1.dedup();
        calls.2.sort();
        calls.2.dedup();
        calls.3.sort();
        calls.3.dedup();
        let bounds = generic_bounds(&sig.generics);
        let mut bindings = Bindings {
            bounds: &bounds,
            out: Vec::new(),
        };
        for input in &sig.inputs {
            if let syn::FnArg::Typed(t) = input
                && let Some((name, _)) = pat_ident(&t.pat)
            {
                bindings.record(name, Some(&t.ty), None);
            }
        }
        bindings.visit_block(block);
        let (returns, returns_self) = match &sig.output {
            syn::ReturnType::Default => (TypeNames::default(), false),
            syn::ReturnType::Type(_, ty) => (TypeNames::of(ty, &bounds), {
                let mut names = Vec::new();
                idents(ty.to_token_stream(), &mut names);
                names.iter().any(|n| n == "Self")
            }),
        };
        self.out.push(SymbolRecord {
            id: format!("symbol:{}:{qualified}", self.path),
            path: self.path.to_string(),
            name: sig.ident.to_string(),
            kind,
            item,
            body,
            start_line: first.start().line,
            end_line: block.brace_token.span.close().end().line,
            signature: StructuralSignature::of(attrs, vis, sig),
            declaration: self.text[item.start..body.start].trim_end().to_string(),
            fingerprint: sha256(&self.text.as_bytes()[item.start..item.end]),
            is_test: self.in_tests_dir || in_test_scope || has_attr(attrs, "test"),
            calls: calls.0,
            qualified: qualified.clone(),
            impl_type: self.current_impl.clone(),
            call_refs: calls.1,
            type_refs: calls.2,
            crate_name: None,
            body_idents: {
                let mut v = Vec::new();
                idents(block.to_token_stream(), &mut v);
                v.sort();
                v.dedup();
                v
            },
            impl_trait: self.current_trait.clone(),
            trait_refs: calls.3,
            module: self.module.clone(),
            body_line: block.brace_token.span.open().start().line,
            body_column: block.brace_token.span.open().start().column,
            returns,
            returns_self,
            bindings: bindings.out,
        });
    }

    fn items(&mut self, scope: &mut Vec<String>, items: &[Item], in_test_scope: bool) {
        for item in items {
            match item {
                Item::Fn(f) => {
                    let first = f.to_token_stream().into_iter().next().map(|t| t.span());
                    self.add(
                        scope,
                        f.sig.ident.to_string(),
                        SymbolKind::Function,
                        &f.attrs,
                        Some(&f.vis),
                        &f.sig,
                        &f.block,
                        first.unwrap_or_else(|| f.span()),
                        in_test_scope,
                    );
                }
                Item::Mod(m) => {
                    if let Some((_, content)) = &m.content {
                        scope.push(m.ident.to_string());
                        self.items(scope, content, in_test_scope || cfg_test(&m.attrs));
                        scope.pop();
                    }
                }
                Item::Struct(st) => {
                    self.span_item("struct", st.ident.to_string(), st);
                    let bounds = generic_bounds(&st.generics);
                    let facts = self.types.entry(st.ident.to_string()).or_default();
                    for (n, field) in st.fields.iter().enumerate() {
                        let name = field
                            .ident
                            .as_ref()
                            .map(|i| i.to_string())
                            .unwrap_or_else(|| n.to_string());
                        facts.fields.insert(name, TypeNames::of(&field.ty, &bounds));
                    }
                    facts.files = vec![self.path.to_string()];
                }
                Item::Enum(e) => self.span_item("enum", e.ident.to_string(), e),
                Item::Union(u) => self.span_item("union", u.ident.to_string(), u),
                Item::Type(t) => self.span_item("type", t.ident.to_string(), t),
                Item::Const(c) => self.span_item("const", c.ident.to_string(), c),
                Item::Static(st) => self.span_item("static", st.ident.to_string(), st),
                Item::Use(u) => self.span_item("use", String::new(), u),
                Item::Macro(m) => self.span_item(
                    "macro",
                    m.ident.as_ref().map(|i| i.to_string()).unwrap_or_default(),
                    m,
                ),
                Item::Impl(i) => {
                    let ty = match &*i.self_ty {
                        syn::Type::Path(p) => p
                            .path
                            .segments
                            .last()
                            .map(|s| s.ident.to_string())
                            .unwrap_or_else(|| "impl".into()),
                        other => other.to_token_stream().to_string().replace(' ', ""),
                    };
                    // The impl's header lines (and anything between its methods) are declarations.
                    self.span_item("impl", ty.clone(), i);
                    if let Some((_, p, _)) = &i.trait_
                        && let Some(t) = p.segments.last()
                        && !(in_test_scope || cfg_test(&i.attrs) || self.in_tests_dir)
                    {
                        self.trait_impls.push((ty.clone(), t.ident.to_string()));
                    }
                    scope.push(ty.clone());
                    let outer = self.current_impl.replace(ty);
                    let outer_trait = std::mem::replace(
                        &mut self.current_trait,
                        i.trait_
                            .as_ref()
                            .and_then(|(_, p, _)| p.segments.last())
                            .map(|s| s.ident.to_string()),
                    );
                    for member in &i.items {
                        if let ImplItem::Fn(f) = member {
                            let first = f.to_token_stream().into_iter().next().map(|t| t.span());
                            self.add(
                                scope,
                                f.sig.ident.to_string(),
                                SymbolKind::Method,
                                &f.attrs,
                                Some(&f.vis),
                                &f.sig,
                                &f.block,
                                first.unwrap_or_else(|| f.span()),
                                in_test_scope || cfg_test(&i.attrs),
                            );
                        }
                    }
                    self.current_impl = outer;
                    self.current_trait = outer_trait;
                    scope.pop();
                }
                Item::Trait(t) => {
                    self.span_item("trait", t.ident.to_string(), t);
                    let facts = self.traits.entry(t.ident.to_string()).or_default();
                    facts.methods.extend(t.items.iter().filter_map(|m| match m {
                        TraitItem::Fn(f) => Some(f.sig.ident.to_string()),
                        _ => None,
                    }));
                    facts.methods.sort();
                    facts.methods.dedup();
                    facts.files = vec![self.path.to_string()];
                    scope.push(t.ident.to_string());
                    let outer = self.current_impl.replace(t.ident.to_string());
                    let outer_trait = self.current_trait.replace(t.ident.to_string());
                    for member in &t.items {
                        if let TraitItem::Fn(f) = member
                            && let Some(block) = &f.default
                        {
                            let first = f.to_token_stream().into_iter().next().map(|t| t.span());
                            self.add(
                                scope,
                                f.sig.ident.to_string(),
                                SymbolKind::TraitDefaultMethod,
                                &f.attrs,
                                None,
                                &f.sig,
                                block,
                                first.unwrap_or_else(|| f.span()),
                                in_test_scope,
                            );
                        }
                    }
                    self.current_impl = outer;
                    self.current_trait = outer_trait;
                    scope.pop();
                }
                _ => {}
            }
        }
    }
}

/// Every function with a body in one file's text. `path` is the file's repository-relative path.
pub fn index_text(path: &str, text: &str) -> Result<Vec<SymbolRecord>, IndexError> {
    index_file(path, text).map(|f| f.symbols)
}

/// One file's functions, struct field types, trait definitions and other item spans.
pub fn index_file(path: &str, text: &str) -> Result<FileIndex, IndexError> {
    let file = syn::parse_file(text).map_err(|e| IndexError::Parse {
        path: path.to_string(),
        message: e.to_string(),
    })?;
    let mut c = Collector {
        path,
        text,
        lines: Lines::new(text),
        in_tests_dir: path.starts_with("tests/") || path.contains("/tests/"),
        out: Vec::new(),
        seen: BTreeMap::new(),
        current_impl: None,
        current_trait: None,
        module: module_of(path),
        types: BTreeMap::new(),
        traits: BTreeMap::new(),
        item_spans: Vec::new(),
        trait_impls: Vec::new(),
    };
    c.items(&mut Vec::new(), &file.items, false);
    Ok(FileIndex {
        symbols: c.out,
        types: c.types,
        traits: c.traits,
        items: c.item_spans,
        trait_impls: c.trait_impls,
    })
}

/// A checked repository-relative path: `/`-separated, no empty, `.` or `..` component, not
/// absolute.
pub fn check_relative(path: &str) -> Result<(), IndexError> {
    let ok = !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != "..");
    if ok {
        Ok(())
    } else {
        Err(IndexError::NotRelative(path.to_string()))
    }
}

/// What the index knows about one file, beyond its symbols.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileFacts {
    /// The owning Cargo package, as a crate identifier; `None` when no manifest names one.
    pub crate_name: Option<String>,
    /// The first segment of every `use` and `extern crate` (`compute_policy`, `std`, `crate`).
    pub imports: Vec<String>,
    pub parsed: bool,
    /// Under a `tests/` directory: an integration-test crate.
    pub test_file: bool,
    /// Non-function items and their lines.
    pub items: Vec<ItemSpan>,
}

/// The index of a source tree: every `.rs` file outside `target`, `.git` and similar directories,
/// in sorted order. A file that does not parse is recorded as unparsed, not guessed at.
#[derive(Debug, Clone, Default)]
pub struct SymbolIndex {
    pub symbols: BTreeMap<String, SymbolRecord>,
    pub unparsed: Vec<String>,
    pub files: BTreeMap<String, FileFacts>,
    /// Structs by name, with their fields' type and trait names.
    pub types: BTreeMap<String, TypeFacts>,
    /// Trait definitions by name.
    pub traits: BTreeMap<String, TraitFacts>,
    /// Production `impl Trait for Type` relations: type name to trait names.
    pub trait_impls: BTreeMap<String, std::collections::BTreeSet<String>>,
}

fn use_roots(tree: &syn::UseTree, out: &mut Vec<String>) {
    match tree {
        syn::UseTree::Path(p) => out.push(p.ident.to_string()),
        syn::UseTree::Name(n) => out.push(n.ident.to_string()),
        syn::UseTree::Rename(r) => out.push(r.ident.to_string()),
        syn::UseTree::Group(g) => g.items.iter().for_each(|t| use_roots(t, out)),
        syn::UseTree::Glob(_) => {}
    }
}

fn imports_of(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(file) = syn::parse_file(text) {
        for item in &file.items {
            match item {
                Item::Use(u) => use_roots(&u.tree, &mut out),
                Item::ExternCrate(e) => out.push(e.ident.to_string()),
                _ => {}
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The crate identifier of the nearest `Cargo.toml` with a `[package]` above `rel`, inside `root`.
fn owning_crate(
    root: &Path,
    rel: &str,
    cache: &mut BTreeMap<PathBuf, Option<String>>,
) -> Option<String> {
    let mut dir = Path::new(rel).parent();
    while let Some(d) = dir {
        let at = root.join(d);
        let found = cache
            .entry(at.clone())
            .or_insert_with(|| {
                let text = fs::read_to_string(at.join("Cargo.toml")).ok()?;
                let value: toml::Value = toml::from_str(&text).ok()?;
                value
                    .get("package")?
                    .get("name")?
                    .as_str()
                    .map(|n| n.replace('-', "_"))
            })
            .clone();
        if found.is_some() {
            return found;
        }
        if d.as_os_str().is_empty() {
            break;
        }
        dir = d.parent();
    }
    None
}

impl SymbolIndex {
    pub fn build(root: &Path) -> Result<Self, IndexError> {
        let mut files = Vec::new();
        collect(root, root, &mut files)?;
        files.sort();
        let mut index = SymbolIndex::default();
        let mut manifests = BTreeMap::new();
        for rel in files {
            let full = root.join(&rel);
            let crate_name = owning_crate(root, &rel, &mut manifests);
            if fs::metadata(&full).map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
                index.files.insert(
                    rel.clone(),
                    FileFacts {
                        crate_name,
                        ..FileFacts::default()
                    },
                );
                index.unparsed.push(rel);
                continue;
            }
            let text = fs::read_to_string(&full).map_err(|e| IndexError::Io(e.to_string()))?;
            let test_file = rel.starts_with("tests/") || rel.contains("/tests/");
            match index_file(&rel, &text) {
                Ok(file) => {
                    for mut r in file.symbols {
                        r.crate_name = crate_name.clone();
                        index.symbols.insert(r.id.clone(), r);
                    }
                    for (name, facts) in file.types {
                        let merged = index.types.entry(name).or_default();
                        merged.fields.extend(facts.fields);
                        merged.files.extend(facts.files);
                    }
                    for (ty, tr) in file.trait_impls {
                        index.trait_impls.entry(ty).or_default().insert(tr);
                    }
                    for (name, facts) in file.traits {
                        let merged = index.traits.entry(name).or_default();
                        merged.methods.extend(facts.methods);
                        merged.methods.sort();
                        merged.methods.dedup();
                        merged.files.extend(facts.files);
                    }
                    index.files.insert(
                        rel,
                        FileFacts {
                            crate_name,
                            imports: imports_of(&text),
                            parsed: true,
                            test_file,
                            items: file.items,
                        },
                    );
                }
                Err(_) => {
                    index.files.insert(
                        rel.clone(),
                        FileFacts {
                            crate_name,
                            ..FileFacts::default()
                        },
                    );
                    index.unparsed.push(rel);
                }
            }
        }
        Ok(index)
    }

    pub fn get(&self, id: &str) -> Option<&SymbolRecord> {
        self.symbols.get(id)
    }

    /// The innermost non-test symbol of `path` whose span holds `line`.
    pub fn at(&self, path: &str, line: usize) -> Option<&SymbolRecord> {
        self.symbols
            .values()
            .filter(|s| s.path == path && s.contains_line(line))
            .min_by_key(|s| s.end_line - s.start_line)
    }
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), IndexError> {
    let entries = fs::read_dir(dir).map_err(|e| IndexError::Io(e.to_string()))?;
    for entry in entries.flatten() {
        let path: PathBuf = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if meta.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_str()) {
                collect(root, &path, out)?;
            }
        } else if name.ends_with(".rs")
            && let Ok(rel) = path.strip_prefix(root)
            && let Some(rel) = rel.to_str()
        {
            out.push(rel.replace(std::path::MAIN_SEPARATOR, "/"));
        }
    }
    Ok(())
}
