//! Choosing the one symbol to repair, from observed diagnostics and the symbol index only.
//!
//! * A compile error or a panic located inside a non-test function names that function.
//! * A failed test, or a panic inside a test, names the non-test functions the test calls by name.
//!
//! Exactly one candidate is a target. Several are ambiguous (one bounded classification may pick
//! among them). None is no target. Nothing here asks a model.

use std::collections::BTreeSet;

use crate::acceptance::{AcceptanceRun, DiagnosticKind};
use crate::symbols::SymbolIndex;

/// The most candidates a classification is offered.
pub const MAX_CANDIDATES: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "symbols", rename_all = "snake_case")]
pub enum Target {
    Single(String),
    Ambiguous(Vec<String>),
    None,
}

pub fn resolve(index: &SymbolIndex, run: &AcceptanceRun) -> Target {
    let mut direct = BTreeSet::new();
    let mut tests = BTreeSet::new();
    for d in &run.diagnostics {
        if let (DiagnosticKind::CompileError | DiagnosticKind::Panic, Some(path), Some(line)) =
            (d.kind, &d.path, d.line)
        {
            match index.at(path, line) {
                Some(s) if !s.is_test => {
                    direct.insert(s.id.clone());
                }
                Some(s) => {
                    tests.insert(s.id.clone());
                }
                None => {}
            }
        }
        if let Some(name) = &d.test {
            let last = name.rsplit("::").next().unwrap_or(name);
            for s in index
                .symbols
                .values()
                .filter(|s| s.is_test && s.name == last)
            {
                tests.insert(s.id.clone());
            }
        }
    }
    // Compile errors and panics in code under test are the strongest evidence.
    let candidates: BTreeSet<String> = if !direct.is_empty() {
        direct
    } else {
        let called: BTreeSet<&str> = tests
            .iter()
            .filter_map(|t| index.get(t))
            .flat_map(|t| t.calls.iter().map(String::as_str))
            .collect();
        index
            .symbols
            .values()
            .filter(|s| !s.is_test && called.contains(s.name.as_str()))
            .map(|s| s.id.clone())
            .collect()
    };
    match candidates.len() {
        0 => Target::None,
        1 => Target::Single(candidates.into_iter().next().expect("one")),
        _ => Target::Ambiguous(candidates.into_iter().take(MAX_CANDIDATES).collect()),
    }
}
