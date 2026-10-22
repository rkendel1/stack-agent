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

#[derive(Default)]
struct Calls(Vec<String>);

impl<'ast> Visit<'ast> for Calls {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = &*call.func
            && let Some(seg) = p.path.segments.last()
        {
            self.0.push(seg.ident.to_string());
        }
        syn::visit::visit_expr_call(self, call);
    }
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.0.push(call.method.to_string());
        syn::visit::visit_expr_method_call(self, call);
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
}

impl Collector<'_> {
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
        calls.visit_block(block);
        calls.0.sort();
        calls.0.dedup();
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
                    scope.push(ty);
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
                    scope.pop();
                }
                Item::Trait(t) => {
                    scope.push(t.ident.to_string());
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
                    scope.pop();
                }
                _ => {}
            }
        }
    }
}

/// Every function with a body in one file's text. `path` is the file's repository-relative path.
pub fn index_text(path: &str, text: &str) -> Result<Vec<SymbolRecord>, IndexError> {
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
    };
    c.items(&mut Vec::new(), &file.items, false);
    Ok(c.out)
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

/// The index of a source tree: every `.rs` file outside `target`, `.git` and similar directories,
/// in sorted order. A file that does not parse is recorded as unparsed, not guessed at.
#[derive(Debug, Clone, Default)]
pub struct SymbolIndex {
    pub symbols: BTreeMap<String, SymbolRecord>,
    pub unparsed: Vec<String>,
}

impl SymbolIndex {
    pub fn build(root: &Path) -> Result<Self, IndexError> {
        let mut files = Vec::new();
        collect(root, root, &mut files)?;
        files.sort();
        let mut index = SymbolIndex::default();
        for rel in files {
            let full = root.join(&rel);
            if fs::metadata(&full).map(|m| m.len()).unwrap_or(0) > MAX_FILE_BYTES {
                index.unparsed.push(rel);
                continue;
            }
            let text = fs::read_to_string(&full).map_err(|e| IndexError::Io(e.to_string()))?;
            match index_text(&rel, &text) {
                Ok(records) => {
                    for r in records {
                        index.symbols.insert(r.id.clone(), r);
                    }
                }
                Err(_) => index.unparsed.push(rel),
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
