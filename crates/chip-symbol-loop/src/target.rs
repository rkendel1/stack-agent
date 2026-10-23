//! Choosing the symbol to repair, from observed diagnostics and the symbol index only. Nothing here
//! asks a model.
//!
//! 1. **Direct evidence.** A compile error or a panic located inside a non-test function body
//!    names it.
//! 2. **Seeds.** The failing tests, and any test helper a panic is located in.
//! 3. **Scope.** The crate each seed belongs to, the workspace crates its file imports, and,
//!    transitively, the workspace crates those crates' production code imports or calls by path.
//!    A seed whose crate and imports are unknown widens the scope to every crate, and says so.
//! 4. **Call chain.** From the seeds, calls are followed breadth-first, with no fixed depth: test
//!    helpers are transparent (a test's own helper functions are part of the test), and each
//!    production call is one level deeper. The walk is bounded by one [`Budget`] (functions
//!    expanded, distinct candidates, depth); whatever a bound cuts off is recorded, never
//!    dropped silently.
//!    An edge needs evidence: a same-file helper; a path whose segments end the callee's module
//!    path and qualified name; a method of a type the caller names (or of the type of the `self`
//!    field it is called on); or, for a method a trait declares, the in-scope implementations of
//!    that trait when the caller holds it as a bound (`dyn T`, `impl T`, `X: T`, a field of such a
//!    type). A call that matches functions by name alone gives no edge: it is recorded in the
//!    [`Frontier`] with the functions it might have reached.
//! 5. **Ranking, before any truncation.** Each candidate gets an integer score and the reasons for
//!    it: direct evidence, depth, how the call named it, crate relationship, type compatibility
//!    with the failing test, failure-output terms its body uses, how many calls on the failing
//!    path reach it, and whether every failing test reaches it. Ties are broken by id, and that is
//!    recorded too.
//! 6. **Repair shape.** Some failures cannot be fixed by replacing one function body: a compile
//!    error in test code or in a declaration (rustc checks a call against the callee's declaration,
//!    not its body), or failing tests that share no reachable function. These are reported as not
//!    expressible instead of being given a plausible target.
//! 7. **Decision.** One target only on direct evidence pointing to exactly one function, or when
//!    there is exactly one candidate and nothing in the frontier could conceal another.
//!    Call-graph proximity alone never picks a single function.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::acceptance::{AcceptanceRun, DiagnosticKind};
use crate::symbols::{CallRef, SymbolIndex, SymbolRecord, implicit_targets};

/// The most candidates a classification is offered, taken from the ranking's top.
pub const MAX_CANDIDATES: usize = 8;
/// The most excluded calls a frontier lists (all are counted).
pub const MAX_FRONTIER_EXAMPLES: usize = 25;
/// The most functions an excluded call lists as what it might have reached (all are counted).
const MAX_PLAUSIBLE_LISTED: usize = 5;

/// Standard-library types too common to say anything about which function a test exercises.
const COMMON_TYPES: [&str; 12] = [
    "Option", "Result", "String", "Vec", "Box", "Arc", "Rc", "HashMap", "BTreeMap", "HashSet",
    "BTreeSet", "Default",
];

/// The one bound on a call-graph walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    /// Functions expanded (their calls resolved).
    pub max_visits: usize,
    /// Distinct production candidates admitted.
    pub max_candidates: usize,
    /// Production call depth from the failing test.
    pub max_depth: u8,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_visits: 5_000,
            max_candidates: 400,
            max_depth: 32,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "symbols", rename_all = "snake_case")]
pub enum Target {
    Single(String),
    Ambiguous(Vec<String>),
    /// The observed failure cannot be repaired by replacing one function body; the reasons say why.
    NotExpressible(Vec<String>),
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    /// Named by a path that ends in its module path and qualified name (`EffectivePolicy::compose`).
    Qualified,
    /// The only function in scope the call can name.
    Unique,
    /// One of the in-scope implementations of a trait method called through a trait bound.
    Dispatch,
    /// One of several functions in scope the call can name.
    Shared,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub score: i64,
    /// 0 for direct evidence, otherwise the production call depth from the failing test.
    pub depth: u8,
    /// A compile error inside its body: static evidence that this body must change.
    pub direct: bool,
    /// A panic inside its body: where a bad value was used, not necessarily where it was made.
    #[serde(default)]
    pub symptom: bool,
    pub reasons: Vec<String>,
}

/// Why the walk ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Every function reachable through evidenced edges was expanded.
    Complete,
    VisitBudget,
    CandidateBudget,
    DepthBudget,
}

/// Why a call gave no edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionKind {
    /// A path naming a type or function the index has nothing for (a standard-library or external
    /// item, a derived impl). It cannot reach an indexed function.
    ExternalPath,
    /// A path whose segments match no indexed function's module path, though functions with its
    /// final name exist in scope (a re-export or an alias may lead to one of them).
    UnmatchedPath,
    /// A method call whose receiver has no visible type (not `self`, a typed binding or a field
    /// chain); in-scope methods share its name.
    UntypedMethod,
    /// As `UntypedMethod`, where some of those methods implement a trait that declares it: a
    /// trait object or generic whose type is not visible may dispatch to one of them.
    UnresolvedDispatch,
    /// A trait the caller holds as a bound declares the method, but no in-scope production code
    /// implements it with a body (implementations may be in tests, generated, or outside scope).
    NoImplementation,
    /// A method call on a value bound to a call whose declared return type could not be followed
    /// (the callee is unresolved, or its return type names no indexed type).
    ReturnTypeUnknown,
    /// An operator or method (`<`, `==`, `+`, `.max()`, `.sort()`) that may call a user type's
    /// trait implementation (`Ord::cmp`, `PartialEq::eq`, `Add::add`) whose operand types are not
    /// visible.
    ImplicitOperator,
}

impl ExclusionKind {
    /// Whether the functions it lists might include the true target.
    pub fn may_conceal(self) -> bool {
        matches!(
            self,
            ExclusionKind::UnmatchedPath
                | ExclusionKind::UntypedMethod
                | ExclusionKind::UnresolvedDispatch
                | ExclusionKind::ReturnTypeUnknown
                | ExclusionKind::ImplicitOperator
        )
    }

    fn label(self) -> &'static str {
        match self {
            ExclusionKind::ExternalPath => "external_path",
            ExclusionKind::UnmatchedPath => "unmatched_path",
            ExclusionKind::UntypedMethod => "untyped_method",
            ExclusionKind::UnresolvedDispatch => "unresolved_dispatch",
            ExclusionKind::NoImplementation => "no_implementation",
            ExclusionKind::ReturnTypeUnknown => "return_type_unknown",
            ExclusionKind::ImplicitOperator => "implicit_operator",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exclusion {
    /// The qualified name of the calling function.
    pub from: String,
    /// The call as written: `a::b` or `.method`.
    pub call: String,
    pub kind: ExclusionKind,
    /// Functions the call might have reached, not followed (up to 5).
    pub plausible: Vec<String>,
    pub plausible_count: usize,
    /// The specific missing link: which value's type was unknown and why, which operator.
    #[serde(default)]
    pub cause: String,
}

/// What the walk explored, what it did not, and whether that could hide the target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frontier {
    pub budget: Budget,
    pub stop: StopReason,
    /// Functions expanded.
    pub explored: usize,
    /// Evidenced edges followed.
    pub edges: usize,
    pub max_depth_reached: u8,
    /// Functions a budget kept out (not admitted or not expanded).
    pub cut_off: usize,
    /// Excluded calls by kind.
    pub excluded_counts: BTreeMap<String, usize>,
    /// Up to [`MAX_FRONTIER_EXAMPLES`] excluded calls, those that may conceal the target first.
    pub excluded: Vec<Exclusion>,
    /// Production functions an excluded call might have reached, or a budget kept out, that are
    /// not candidates.
    pub hidden: usize,
    pub hidden_examples: Vec<String>,
    /// Some trait-method call could not be resolved to its implementations, and some of them are
    /// not candidates.
    pub unresolved_dispatch: bool,
    /// A budget stopped the walk before it was complete.
    pub budget_exhausted: bool,
    pub could_conceal_target: bool,
    /// Why the target could lie outside the candidates, if it could.
    pub concealment: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Expressible {
    /// Nothing observed rules out a single function-body replacement (which does not show that one
    /// suffices).
    Possible,
    /// Something observed may need more than one body replacement or a declaration change, but the
    /// evidence is not conclusive.
    Unknown,
    /// The observed failure cannot be fixed by replacing one function body.
    NotExpressible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairShape {
    pub expressible: Expressible,
    pub reasons: Vec<String>,
}

/// How far one failing test's own walk reached, for the shared-function test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestReach {
    pub test: String,
    pub reached: usize,
    /// Its walk finished within budget and no unresolved call could hide another function.
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ranking {
    /// Every candidate, best first. Nothing is dropped before ranking.
    pub candidates: Vec<Candidate>,
    pub seeds: Vec<String>,
    /// The crates candidates were drawn from; empty means every crate (see `notes`).
    pub scope: Vec<String>,
    /// What was missing or uncertain: unparsed files, tests not found, an unknown scope.
    pub notes: Vec<String>,
    pub frontier: Frontier,
    pub shape: RepairShape,
    /// Per failing test, when more than one fails.
    pub per_test: Vec<TestReach>,
}

impl Ranking {
    /// The top `n` candidates with their scores and reasons, the frontier and the repair shape:
    /// the explanation a checkpoint records.
    pub fn summary(&self, n: usize) -> Value {
        json!({
            "seeds": self.seeds,
            "scope": self.scope,
            "notes": self.notes,
            "total_candidates": self.candidates.len(),
            "top": self.candidates.iter().take(n).collect::<Vec<_>>(),
            "frontier": self.frontier,
            "shape": self.shape,
            "per_test": self.per_test,
        })
    }

    pub fn position(&self, id: &str) -> Option<usize> {
        self.candidates.iter().position(|c| c.id == id)
    }
}

#[derive(Default)]
struct Evidence {
    direct: Vec<String>,
    symptom: Vec<String>,
    /// Functions with a panic inside that use this function's result.
    feeds: BTreeSet<String>,
    /// Functions with a panic inside that this function calls.
    calls_symptom: BTreeSet<String>,
    depth: Option<u8>,
    best_match: Option<MatchKind>,
    alternatives: usize,
    via: Option<String>,
    callers: BTreeSet<String>,
}

fn last(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

fn unnumbered(q: &str) -> &str {
    q.split('#').next().unwrap_or(q)
}

/// The function's path inside its crate: module path and qualified name, without a `#n` suffix.
fn crate_path(s: &SymbolRecord) -> String {
    if s.module.is_empty() {
        unnumbered(&s.qualified).to_string()
    } else {
        format!("{}::{}", s.module, unnumbered(&s.qualified))
    }
}

struct Edge<'a> {
    to: &'a SymbolRecord,
    kind: Option<MatchKind>,
    alternatives: usize,
    via: Option<String>,
}

impl<'a> Edge<'a> {
    fn all(found: Vec<&'a SymbolRecord>, kind: MatchKind, via: Option<&str>) -> Vec<Edge<'a>> {
        let n = found.len();
        found
            .into_iter()
            .map(|s| Edge {
                to: s,
                kind: Some(kind),
                alternatives: n,
                via: via.map(str::to_string),
            })
            .collect()
    }
}

#[derive(Default)]
struct Resolution<'a> {
    edges: Vec<Edge<'a>>,
    exclusion: Option<(ExclusionKind, Vec<&'a SymbolRecord>, String)>,
}

/// What the source says about a receiver's type.
enum Receiver {
    Known(BTreeSet<String>, BTreeSet<String>),
    /// It is a binding, but its type could not be followed; the reason names the missing link.
    Unknown(String),
    /// Nothing is written about it (not `self`, a binding or a resolvable field chain).
    NoInfo,
}

impl<'a> Resolution<'a> {
    fn edges(edges: Vec<Edge<'a>>) -> Self {
        Self {
            edges,
            exclusion: None,
        }
    }

    fn excluded(kind: ExclusionKind, plausible: Vec<&'a SymbolRecord>, cause: String) -> Self {
        Self {
            edges: Vec::new(),
            exclusion: Some((kind, plausible, cause)),
        }
    }
}

struct Ctx<'a> {
    index: &'a SymbolIndex,
    by_name: BTreeMap<&'a str, Vec<&'a SymbolRecord>>,
    scope: BTreeSet<String>,
    crates: BTreeSet<String>,
    impl_types: BTreeSet<&'a str>,
    budget: Budget,
}

impl<'a> Ctx<'a> {
    fn in_scope(&self, s: &SymbolRecord) -> bool {
        self.scope.is_empty()
            || s.crate_name
                .as_ref()
                .is_some_and(|c| self.scope.contains(c))
    }

    fn named(&self, name: &str) -> &[&'a SymbolRecord] {
        self.by_name.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// In-scope production functions named `name`, other than `from`.
    fn production(&self, name: &str, from: &SymbolRecord) -> Vec<&'a SymbolRecord> {
        self.named(name)
            .iter()
            .copied()
            .filter(|s| self.in_scope(s) && !s.is_test && s.id != from.id)
            .collect()
    }

    /// Whether the index has anything for a capitalized path segment: a struct, a trait, or an
    /// `impl` of a type by that name.
    fn known_type(&self, name: &str) -> bool {
        self.index.types.contains_key(name)
            || self.index.traits.contains_key(name)
            || self.index.trait_impls.contains_key(name)
            || self.impl_types.contains(name)
    }

    /// The in-scope production implementations of `trait_name::method` (its default included).
    fn implementations(
        &self,
        trait_name: &str,
        method: &str,
        from: &SymbolRecord,
    ) -> Vec<&'a SymbolRecord> {
        self.production(method, from)
            .into_iter()
            .filter(|s| s.impl_trait.as_deref() == Some(trait_name))
            .collect()
    }

    fn declares(&self, trait_name: &str, method: &str) -> bool {
        self.index
            .traits
            .get(trait_name)
            .is_some_and(|t| t.methods.iter().any(|m| m == method))
    }

    /// The type and trait names a field chain ends in (`self.state.config.authorizer`), following
    /// struct field declarations from `self`'s type, or from the types the caller names for any
    /// other base. `None` when some field is on no struct the chain can be at. `depth` is the
    /// number of binding links already followed (see [`Ctx::binding_types`]).
    fn field_chain(
        &self,
        receiver: &str,
        from: &'a SymbolRecord,
        depth: u8,
    ) -> Option<(BTreeSet<String>, BTreeSet<String>)> {
        let mut parts = receiver.split('.');
        let base = parts.next()?;
        let mut types: BTreeSet<String> = if base == "self" {
            from.impl_type.iter().cloned().collect()
        } else if let Receiver::Known(t, _) = self.binding_types(base, from, depth + 1) {
            t
        } else {
            from.type_refs.iter().cloned().collect()
        };
        let mut traits = BTreeSet::new();
        let mut any = false;
        for field in parts {
            any = true;
            let (mut next_types, mut next_traits) = (BTreeSet::new(), BTreeSet::new());
            for t in &types {
                if let Some(f) = self.index.types.get(t).and_then(|s| s.fields.get(field)) {
                    next_types.extend(f.types.iter().cloned());
                    next_traits.extend(f.traits.iter().cloned());
                }
            }
            if next_types.is_empty() && next_traits.is_empty() {
                return None;
            }
            types = next_types;
            traits = next_traits;
        }
        any.then_some((types, traits))
    }

    /// What the bindings named `name` in `from` say about its type: a written type, the declared
    /// return type of the call it is initialized from, or the value it aliases. Followed at most
    /// three links deep.
    fn binding_types(&self, name: &str, from: &'a SymbolRecord, depth: u8) -> Receiver {
        let bindings: Vec<_> = from.bindings.iter().filter(|b| b.name == name).collect();
        if bindings.is_empty() {
            return Receiver::NoInfo;
        }
        if depth > 3 {
            return Receiver::Unknown(format!("{name}'s type is more than three links away"));
        }
        let (mut types, mut traits) = (BTreeSet::new(), BTreeSet::new());
        let mut why = Vec::new();
        for b in bindings {
            if !b.types.is_empty() || !b.traits.is_empty() {
                types.extend(b.types.iter().cloned());
                traits.extend(b.traits.iter().cloned());
            } else if let Some(call) = &b.call {
                let written = if call.method {
                    format!(".{}()", call.path.join("::"))
                } else {
                    format!("{}()", call.path.join("::"))
                };
                let r = self.resolve_at(call, from, depth + 1);
                if r.edges.is_empty() {
                    why.push(format!(
                        "{name} is bound to {written}, which resolves to no indexed function"
                    ));
                    continue;
                }
                let before = types.len() + traits.len();
                for e in &r.edges {
                    types.extend(e.to.returns.types.iter().cloned());
                    traits.extend(e.to.returns.traits.iter().cloned());
                    if e.to.returns_self {
                        types.extend(e.to.impl_type.iter().cloned());
                    }
                }
                if types.len() + traits.len() == before {
                    why.push(format!(
                        "{name} is bound to {written}, whose declared return type names no type"
                    ));
                }
            } else if let Some(alias) = &b.alias {
                let r = if alias.contains('.') {
                    match self.field_chain(alias, from, depth + 1) {
                        Some((t, tr)) => Receiver::Known(t, tr),
                        None => Receiver::Unknown(format!(
                            "{name} comes from {alias}, a field chain the index cannot follow"
                        )),
                    }
                } else if alias == "self" {
                    Receiver::Known(from.impl_type.iter().cloned().collect(), BTreeSet::new())
                } else if alias == name {
                    Receiver::Unknown(format!("{name} is rebound from itself"))
                } else {
                    self.binding_types(alias, from, depth + 1)
                };
                match r {
                    Receiver::Known(t, tr) => {
                        types.extend(t);
                        traits.extend(tr);
                    }
                    Receiver::Unknown(w) => why.push(w),
                    Receiver::NoInfo => why.push(format!(
                        "{name} comes from {alias}, which has no visible type"
                    )),
                }
            }
        }
        if types.is_empty() && traits.is_empty() {
            Receiver::Unknown(why.join("; "))
        } else {
            Receiver::Known(types, traits)
        }
    }

    /// The trait implementations an operator or iterator method may call, for operand types the
    /// caller names (in its signature, body or typed bindings).
    fn resolve_implicit(&self, label: &str, from: &'a SymbolRecord) -> Resolution<'a> {
        let (methods, traits) = implicit_targets(label);
        let impls: Vec<&'a SymbolRecord> = methods
            .iter()
            .flat_map(|m| self.production(m, from))
            .filter(|s| s.impl_trait.as_deref().is_some_and(|t| traits.contains(&t)))
            .collect();
        if impls.is_empty() {
            return Resolution::default();
        }
        let mut context: BTreeSet<&str> = from.type_refs.iter().map(String::as_str).collect();
        context.extend(from.impl_type.as_deref());
        for b in &from.bindings {
            context.extend(b.types.iter().map(String::as_str));
        }
        let typed: Vec<&'a SymbolRecord> = impls
            .iter()
            .copied()
            .filter(|s| s.impl_type.as_deref().is_some_and(|t| context.contains(t)))
            .collect();
        if typed.is_empty() {
            let n = impls.len();
            return Resolution::excluded(
                ExclusionKind::ImplicitOperator,
                impls,
                format!(
                    "{label} in {} may call one of {n} user implementations of {}; its operand types are not visible",
                    from.qualified,
                    traits.join("/")
                ),
            );
        }
        let via = format!("{} ({label})", traits.join("/"));
        Resolution::edges(Edge::all(typed, MatchKind::Dispatch, Some(&via)))
    }

    fn resolve(&self, call: &CallRef, from: &'a SymbolRecord) -> Resolution<'a> {
        self.resolve_at(call, from, 0)
    }

    fn resolve_at(&self, call: &CallRef, from: &'a SymbolRecord, depth: u8) -> Resolution<'a> {
        if let Some(label) = &call.implicit {
            return self.resolve_implicit(label, from);
        }
        let name = call.path.last().map(String::as_str).unwrap_or("");
        // A plain call to a function in the same file (a test's own helper) names that function.
        if !call.method && call.path.len() == 1 {
            let local: Vec<_> = self
                .named(name)
                .iter()
                .filter(|s| s.path == from.path && s.id != from.id)
                .map(|s| Edge {
                    to: s,
                    kind: None,
                    alternatives: 1,
                    via: None,
                })
                .collect();
            if !local.is_empty() {
                return Resolution::edges(local);
            }
            // A bare name that is not a same-file helper (imported with `use`).
            let found = self.production(name, from);
            let kind = if found.len() == 1 {
                MatchKind::Unique
            } else {
                MatchKind::Shared
            };
            return Resolution::edges(Edge::all(found, kind, None));
        }
        if call.method {
            self.resolve_method(call, from, depth)
        } else {
            self.resolve_path(call, from)
        }
    }

    fn resolve_path(&self, call: &CallRef, from: &'a SymbolRecord) -> Resolution<'a> {
        let name = call.path.last().map(String::as_str).unwrap_or("");
        let mut segs: Vec<String> = call
            .path
            .iter()
            .map(|s| match (s.as_str(), &from.impl_type) {
                ("Self", Some(t)) => t.clone(),
                _ => s.clone(),
            })
            .collect();
        let mut rooted: Option<String> = None;
        while segs.len() > 1 {
            let first = segs[0].as_str();
            if matches!(first, "crate" | "self" | "super") {
                if first == "crate" {
                    rooted = from.crate_name.clone();
                }
                segs.remove(0);
            } else if self.crates.contains(first) {
                rooted = Some(first.to_string());
                segs.remove(0);
            } else {
                break;
            }
        }
        let candidates: Vec<&'a SymbolRecord> = self
            .named(name)
            .iter()
            .copied()
            // A test may name its own helpers by path (`Fixture::start`); production code cannot
            // reach a test function, so only a test caller sees them.
            .filter(|s| s.id != from.id && (from.is_test || !s.is_test))
            .filter(|s| match &rooted {
                Some(c) => s.crate_name.as_deref() == Some(c.as_str()),
                None => self.in_scope(s),
            })
            .collect();
        // The written path is a suffix of the callee's crate path.
        let joined = segs.join("::");
        let suffix: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|s| {
                let full = crate_path(s);
                full == joined || full.ends_with(&format!("::{joined}"))
            })
            .collect();
        if !suffix.is_empty() {
            return Resolution::edges(Edge::all(suffix, MatchKind::Qualified, None));
        }
        let n = segs.len();
        let owner = (n >= 2).then(|| segs[n - 2].clone());
        if let Some(owner) = &owner {
            // The last two segments name a function's qualified name (`Type::method`).
            let tail = format!("{owner}::{name}");
            let found: Vec<_> = candidates
                .iter()
                .copied()
                .filter(|s| {
                    let q = unnumbered(&s.qualified);
                    q == tail || q.ends_with(&format!("::{tail}"))
                })
                .collect();
            if !found.is_empty() {
                return Resolution::edges(Edge::all(found, MatchKind::Qualified, None));
            }
            // `Trait::method(x)`: the trait's implementations.
            if self.declares(owner, name) {
                let impls = self.implementations(owner, name, from);
                if impls.is_empty() {
                    return Resolution::excluded(
                        ExclusionKind::NoImplementation,
                        Vec::new(),
                        format!(
                            "{owner} declares {name}, but no in-scope production code implements it"
                        ),
                    );
                }
                return Resolution::edges(Edge::all(impls, MatchKind::Dispatch, Some(owner)));
            }
        }
        // Nothing matched. A type segment the index knows nothing about is external; otherwise the
        // functions with the final name are what the call might reach.
        let typed_owner = owner
            .as_deref()
            .filter(|o| o.starts_with(|c: char| c.is_ascii_uppercase()));
        let plausible: Vec<&'a SymbolRecord> = match typed_owner {
            Some(o) if !self.known_type(o) => Vec::new(),
            Some(o) => candidates
                .iter()
                .copied()
                .filter(|s| s.impl_type.as_deref() == Some(o))
                .collect(),
            // A module path: free functions by that name.
            None => candidates
                .iter()
                .copied()
                .filter(|s| s.impl_type.is_none())
                .collect(),
        };
        let plausible: Vec<_> = plausible.into_iter().filter(|s| !s.is_test).collect();
        if plausible.is_empty() {
            Resolution::excluded(
                ExclusionKind::ExternalPath,
                Vec::new(),
                format!("{} names nothing the index has", call.path.join("::")),
            )
        } else {
            Resolution::excluded(
                ExclusionKind::UnmatchedPath,
                plausible,
                format!(
                    "{} matches no indexed module path; functions named {name} exist",
                    call.path.join("::")
                ),
            )
        }
    }

    fn resolve_method(&self, call: &CallRef, from: &'a SymbolRecord, depth: u8) -> Resolution<'a> {
        let name = call.path.last().map(String::as_str).unwrap_or("");
        let all = self.production(name, from);
        // A test's own helper methods, on types it names (`fixture.call(...)`).
        let helpers: Vec<&'a SymbolRecord> = if from.is_test {
            self.named(name)
                .iter()
                .copied()
                .filter(|s| s.is_test && s.id != from.id && s.crate_name == from.crate_name)
                .filter(|s| {
                    s.impl_type.as_ref().is_some_and(|t| {
                        from.type_refs.contains(t) || from.impl_type.as_ref() == Some(t)
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        // The types and traits the receiver may have.
        let mut types: BTreeSet<String> = BTreeSet::new();
        let mut traits: BTreeSet<String> = BTreeSet::new();
        let receiver = match call.receiver.as_deref() {
            Some("self") => Receiver::Known(BTreeSet::new(), BTreeSet::new()),
            Some(r) if r.contains('.') => match self.field_chain(r, from, depth) {
                Some((t, tr)) => Receiver::Known(t, tr),
                None => Receiver::NoInfo,
            },
            Some(r) => self.binding_types(r, from, depth),
            None => Receiver::NoInfo,
        };
        let mut unknown: Option<String> = None;
        match (call.receiver.as_deref(), receiver) {
            (Some("self"), _) => {
                types.extend(from.impl_type.iter().cloned());
                traits.extend(from.impl_trait.iter().cloned());
                if let Some(t) = &from.impl_type {
                    traits.extend(self.index.trait_impls.get(t).into_iter().flatten().cloned());
                }
            }
            (_, Receiver::Known(t, tr)) => {
                types.extend(t);
                traits.extend(tr);
            }
            (_, other) => {
                if let Receiver::Unknown(why) = other {
                    unknown = Some(why);
                }
                // Nothing precise: the types and bounds the caller names anywhere.
                types.extend(from.type_refs.iter().cloned());
                types.extend(from.impl_type.iter().cloned());
                traits.extend(from.trait_refs.iter().cloned());
            }
        }
        // A named type's own methods.
        let typed: Vec<&'a SymbolRecord> = all
            .iter()
            .copied()
            .filter(|s| s.impl_type.as_ref().is_some_and(|t| types.contains(t)))
            .collect();
        let kind = if typed.len() == 1 {
            MatchKind::Unique
        } else {
            MatchKind::Shared
        };
        let mut edges = Edge::all(typed, kind, None);
        // A named type's traits supply default methods it does not override.
        if edges.is_empty() {
            for t in &types {
                traits.extend(self.index.trait_impls.get(t).into_iter().flatten().cloned());
            }
        }
        // A trait the receiver is bounded by, or implements, that declares the method: its
        // in-scope implementations.
        let mut no_impl = false;
        for tr in traits.iter().filter(|t| self.declares(t, name)) {
            let impls = self.implementations(tr, name, from);
            if impls.is_empty() {
                no_impl = true;
            }
            for e in Edge::all(impls, MatchKind::Dispatch, Some(tr)) {
                if !edges.iter().any(|x| x.to.id == e.to.id) {
                    edges.push(e);
                }
            }
        }
        edges.extend(helpers.into_iter().map(|s| Edge {
            to: s,
            kind: None,
            alternatives: 1,
            via: None,
        }));
        if !edges.is_empty() {
            return Resolution::edges(edges);
        }
        if all.is_empty() {
            return if no_impl {
                Resolution::excluded(
                    ExclusionKind::NoImplementation,
                    Vec::new(),
                    format!(
                        "a trait bound on the receiver declares {name}, but no in-scope production code implements it"
                    ),
                )
            } else {
                Resolution::default()
            };
        }
        let dispatchers: BTreeSet<&str> = all
            .iter()
            .filter_map(|s| s.impl_trait.as_deref())
            .filter(|t| self.declares(t, name))
            .collect();
        let receiver = call.receiver.as_deref().unwrap_or("an expression");
        let (kind, cause) = match (unknown, dispatchers.is_empty()) {
            (Some(why), _) => (ExclusionKind::ReturnTypeUnknown, why),
            (None, false) => (
                ExclusionKind::UnresolvedDispatch,
                format!(
                    "the receiver {receiver} of .{name} has no visible type; it may be a trait object of {}",
                    dispatchers.into_iter().collect::<Vec<_>>().join(", ")
                ),
            ),
            (None, true) => (
                ExclusionKind::UntypedMethod,
                format!("the receiver {receiver} of .{name} has no visible type"),
            ),
        };
        Resolution::excluded(kind, all, cause)
    }
}

/// What one walk found.
#[derive(Default)]
struct Walk {
    evidence: BTreeMap<String, Evidence>,
    test_types: BTreeSet<String>,
    explored: usize,
    edges: usize,
    max_depth: u8,
    visit_cut: bool,
    candidate_cut: bool,
    depth_cut: bool,
    cut_off: BTreeSet<String>,
    exclusions: Vec<Exclusion>,
    plausible: BTreeSet<String>,
}

impl Walk {
    fn stop(&self) -> StopReason {
        if self.visit_cut {
            StopReason::VisitBudget
        } else if self.candidate_cut {
            StopReason::CandidateBudget
        } else if self.depth_cut {
            StopReason::DepthBudget
        } else {
            StopReason::Complete
        }
    }
}

fn strength(kind: Option<MatchKind>) -> u8 {
    match kind {
        None | Some(MatchKind::Qualified) => 0,
        Some(MatchKind::Unique) => 1,
        Some(MatchKind::Dispatch) => 2,
        Some(MatchKind::Shared) => 3,
    }
}

/// Breadth-first from `seeds` (ordered by depth, then edge strength, then id), within the budget.
fn walk(ctx: &Ctx, seeds: &BTreeSet<String>) -> Walk {
    let mut w = Walk::default();
    let mut queue: BTreeSet<(u8, u8, String)> = seeds.iter().map(|s| (0, 0, s.clone())).collect();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while let Some((depth, _, id)) = queue.pop_first() {
        if seen.contains(&id) {
            continue;
        }
        if w.explored >= ctx.budget.max_visits {
            w.visit_cut = true;
            w.cut_off.insert(id);
            w.cut_off.extend(
                queue
                    .into_iter()
                    .map(|(_, _, i)| i)
                    .filter(|i| !seen.contains(i)),
            );
            break;
        }
        seen.insert(id.clone());
        w.explored += 1;
        w.max_depth = w.max_depth.max(depth);
        let Some(node) = ctx.index.get(&id) else {
            continue;
        };
        if node.is_test {
            w.test_types.extend(
                node.type_refs
                    .iter()
                    .filter(|t| !COMMON_TYPES.contains(&t.as_str()))
                    .cloned(),
            );
        }
        for call in &node.call_refs {
            let r = ctx.resolve(call, node);
            if let Some((kind, plausible, cause)) = r.exclusion {
                if kind.may_conceal() {
                    w.plausible.extend(plausible.iter().map(|s| s.id.clone()));
                }
                w.exclusions.push(Exclusion {
                    from: node.qualified.clone(),
                    call: match (&call.implicit, call.method) {
                        (Some(label), _) => label.clone(),
                        (None, true) => format!(".{}", call.path.join("::")),
                        (None, false) => call.path.join("::"),
                    },
                    cause,
                    kind,
                    plausible: plausible
                        .iter()
                        .take(MAX_PLAUSIBLE_LISTED)
                        .map(|s| s.id.clone())
                        .collect(),
                    plausible_count: plausible.len(),
                });
            }
            for edge in r.edges {
                let target = edge.to;
                w.edges += 1;
                if target.is_test {
                    // A test helper: part of the test, at the test's depth.
                    if depth == 0 && !seen.contains(&target.id) {
                        queue.insert((0, 0, target.id.clone()));
                    }
                    continue;
                }
                let next = depth.saturating_add(1);
                if next > ctx.budget.max_depth {
                    w.depth_cut = true;
                    if !w.evidence.contains_key(&target.id) {
                        w.cut_off.insert(target.id.clone());
                    }
                    continue;
                }
                if !w.evidence.contains_key(&target.id)
                    && w.evidence.len() >= ctx.budget.max_candidates
                {
                    w.candidate_cut = true;
                    w.cut_off.insert(target.id.clone());
                    continue;
                }
                let e = w.evidence.entry(target.id.clone()).or_default();
                e.depth = Some(e.depth.map_or(next, |d| d.min(next)));
                if let Some(k) = edge.kind
                    && e.best_match.is_none_or(|b| k < b)
                {
                    e.best_match = Some(k);
                    e.alternatives = edge.alternatives;
                    e.via = edge.via.clone();
                }
                e.callers.insert(node.id.clone());
                if !seen.contains(&target.id) {
                    queue.insert((next, strength(edge.kind), target.id.clone()));
                }
            }
        }
    }
    w
}

/// Identifier-like terms in the compile-error and panic messages: words with an underscore, at
/// least 8 bytes, not starting with a digit (`allowed_runtime_versions`). Test names are not read.
pub fn failure_terms(run: &AcceptanceRun) -> BTreeSet<String> {
    run.diagnostics
        .iter()
        .filter(|d| matches!(d.kind, DiagnosticKind::CompileError | DiagnosticKind::Panic))
        .flat_map(|d| {
            d.message
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .filter(|w| {
                    w.len() >= 8 && w.contains('_') && !w.starts_with(|c: char| c.is_ascii_digit())
                })
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Every candidate for the failure `run` reports, ranked and explained, within the default budget.
pub fn rank(index: &SymbolIndex, run: &AcceptanceRun) -> Ranking {
    rank_with(index, run, Budget::default())
}

/// What the diagnostics say before any walk: direct evidence, seeds, and repair-shape findings.
#[derive(Default)]
struct Observed {
    /// Compile errors inside a production function body.
    direct: BTreeMap<String, Vec<String>>,
    /// Panics inside a production function body.
    symptom: BTreeMap<String, Vec<String>>,
    /// Direct or symptom evidence attributed to a failing test (by its name), for the
    /// shared-function test.
    direct_by_test: BTreeMap<String, BTreeSet<String>>,
    seeds: BTreeSet<String>,
    tests: BTreeSet<String>,
    /// Tests the run reports as passing.
    passing: BTreeSet<String>,
    /// Test files with compile errors: their tests did not run, so only the functions holding the
    /// errors are known to be involved.
    uncompiled: BTreeSet<String>,
    notes: BTreeSet<String>,
    not_expressible: Vec<String>,
    unknown: Vec<String>,
}

fn observe(index: &SymbolIndex, run: &AcceptanceRun) -> Observed {
    let mut o = Observed::default();
    for d in &run.diagnostics {
        let compile = d.kind == DiagnosticKind::CompileError;
        if compile && d.path.is_none() {
            o.unknown.push(format!(
                "a compile error has no source location: {}",
                crate::contract::head(&d.message, 120)
            ));
        }
        if let (DiagnosticKind::CompileError | DiagnosticKind::Panic, Some(path), Some(line)) =
            (d.kind, &d.path, d.line)
        {
            let facts = index.files.get(path);
            let test_file = facts.is_some_and(|f| f.test_file);
            match index.at(path, line) {
                Some(s) if !s.is_test => {
                    let what = if compile {
                        "a compile error"
                    } else {
                        "a panic"
                    };
                    // rustc prints 1-based columns; a location before the body's brace is in
                    // the declaration (on the brace's own line, only the column can tell).
                    let before_body = line < s.body_line
                        || (line == s.body_line
                            && d.column
                                .is_some_and(|c| c.saturating_sub(1) < s.body_column));
                    if compile && before_body {
                        o.not_expressible.push(format!(
                            "the compile error at {path}:{line} is in the declaration of {}, not its body",
                            s.qualified
                        ));
                    }
                    let evidence = if compile {
                        &mut o.direct
                    } else {
                        &mut o.symptom
                    };
                    evidence
                        .entry(s.id.clone())
                        .or_default()
                        .push(format!("{what} at {path}:{line} is inside it"));
                    if let Some(t) = &d.test {
                        o.direct_by_test
                            .entry(last(t).to_string())
                            .or_default()
                            .insert(s.id.clone());
                    }
                }
                Some(s) => {
                    if compile {
                        o.uncompiled.insert(path.clone());
                        o.not_expressible.push(format!(
                            "the compile error at {path}:{line} is in test code ({}): rustc checks a call against the callee's declaration, not its body",
                            s.qualified
                        ));
                    }
                    o.seeds.insert(s.id.clone());
                }
                None => {
                    let item = facts.and_then(|f| {
                        f.items
                            .iter()
                            .filter(|i| (i.start_line..=i.end_line).contains(&line))
                            .min_by_key(|i| i.end_line - i.start_line)
                    });
                    match (compile, item, test_file) {
                        (true, _, true) => {
                            o.uncompiled.insert(path.clone());
                            o.not_expressible.push(format!(
                                "the compile error at {path}:{line} is in test code, outside every test function"
                            ))
                        }
                        (true, Some(i), false) => o.not_expressible.push(format!(
                            "the compile error at {path}:{line} is inside {} {}: a declaration, not a function body",
                            i.kind, i.name
                        )),
                        (true, None, false) => o.unknown.push(format!(
                            "the compile error at {path}:{line} is outside every indexed item"
                        )),
                        _ => {}
                    }
                    o.notes.insert(if facts.is_some_and(|f| f.parsed) {
                        format!("{path}:{line} is outside every indexed function")
                    } else {
                        format!(
                            "{path} is not indexed (missing or unparsed); its diagnostics cannot be located"
                        )
                    });
                }
            }
        }
        if let Some(test) = &d.test {
            let passed = d.kind == DiagnosticKind::PassedTest;
            let name = last(test);
            let module = test.rsplit_once("::").map(|(m, _)| m);
            let matches: Vec<&SymbolRecord> = index
                .symbols
                .values()
                .filter(|s| s.is_test && s.name == name)
                .collect();
            let narrowed: Vec<&SymbolRecord> = match module {
                Some(m) => {
                    let exact: Vec<_> = matches
                        .iter()
                        .copied()
                        .filter(|s| s.qualified.ends_with(&format!("{m}::{name}")))
                        .collect();
                    if exact.is_empty() { matches } else { exact }
                }
                None => matches,
            };
            if passed {
                o.passing.extend(narrowed.into_iter().map(|s| s.id.clone()));
                continue;
            }
            if narrowed.is_empty() {
                o.notes
                    .insert(format!("failing test {test} is not in the index"));
            }
            for s in narrowed {
                o.seeds.insert(s.id.clone());
                o.tests.insert(s.id.clone());
            }
        }
    }
    o
}

/// The crates candidates may come from: the seeds' crates (`own`), what the seeds' files import or
/// name by path, and what those crates' production code imports or names by path, transitively.
/// Empty when a seed's crate is unknown and it imports nothing.
fn scope_of(
    index: &SymbolIndex,
    seeds: &BTreeSet<String>,
    notes: &mut BTreeSet<String>,
) -> (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>) {
    let known: BTreeSet<String> = index
        .files
        .values()
        .filter_map(|f| f.crate_name.clone())
        .collect();
    let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for f in index.files.values().filter(|f| !f.test_file) {
        if let Some(c) = &f.crate_name {
            deps.entry(c.clone())
                .or_default()
                .extend(f.imports.iter().filter(|i| known.contains(*i)).cloned());
        }
    }
    for s in index.symbols.values().filter(|s| !s.is_test) {
        if let Some(c) = &s.crate_name {
            for call in s.call_refs.iter().filter(|r| r.path.len() > 1) {
                if known.contains(&call.path[0]) {
                    deps.entry(c.clone())
                        .or_default()
                        .insert(call.path[0].clone());
                }
            }
        }
    }
    let mut own = BTreeSet::new();
    let mut direct = BTreeSet::new();
    let mut unknown = false;
    for seed in seeds.iter().filter_map(|s| index.get(s)) {
        let facts = index.files.get(&seed.path).cloned().unwrap_or_default();
        match &facts.crate_name {
            Some(c) => {
                own.insert(c.clone());
            }
            None => unknown = true,
        }
        direct.extend(facts.imports.iter().filter(|i| known.contains(*i)).cloned());
        for call in seed.call_refs.iter().filter(|r| r.path.len() > 1) {
            if known.contains(&call.path[0]) {
                direct.insert(call.path[0].clone());
            }
        }
    }
    if unknown && direct.is_empty() {
        notes.insert("a failing test belongs to no known crate and imports none: candidates are drawn from every crate".into());
        return (BTreeSet::new(), own, known);
    }
    let mut scope: BTreeSet<String> = own.union(&direct).cloned().collect();
    let mut pending: Vec<String> = scope.iter().cloned().collect();
    while let Some(c) = pending.pop() {
        for d in deps.get(&c).into_iter().flatten() {
            if scope.insert(d.clone()) {
                pending.push(d.clone());
                notes.insert(format!(
                    "{d} is in scope because {c} production code uses it"
                ));
            }
        }
    }
    (scope, own, known)
}

/// As [`rank`], within an explicit budget.
pub fn rank_with(index: &SymbolIndex, run: &AcceptanceRun, budget: Budget) -> Ranking {
    let terms = failure_terms(run);
    let Observed {
        direct,
        symptom,
        direct_by_test,
        seeds,
        tests,
        passing,
        uncompiled,
        mut notes,
        not_expressible: mut shape_not,
        unknown: mut shape_unknown,
    } = observe(index, run);
    let (scope, own, known) = scope_of(index, &seeds, &mut notes);
    let imported: BTreeSet<String> = scope.difference(&own).cloned().collect();
    let unparsed_in_scope: Vec<&String> = index
        .unparsed
        .iter()
        .filter(|p| {
            scope.is_empty()
                || index
                    .files
                    .get(*p)
                    .and_then(|f| f.crate_name.as_ref())
                    .is_none_or(|c| scope.contains(c))
        })
        .collect();
    let mut by_name: BTreeMap<&str, Vec<&SymbolRecord>> = BTreeMap::new();
    for s in index.symbols.values() {
        by_name.entry(s.name.as_str()).or_default().push(s);
    }
    let ctx = Ctx {
        index,
        by_name,
        scope: scope.clone(),
        crates: known,
        impl_types: index
            .symbols
            .values()
            .filter_map(|s| s.impl_type.as_deref())
            .collect(),
        budget,
    };
    let mut w = walk(&ctx, &seeds);
    for (id, reasons) in direct {
        w.evidence.entry(id).or_default().direct = reasons;
    }
    // A panic shows where a bad value was used. The functions whose results the panicking function
    // uses (producers) and those on the failing path that call it (which may pass it a bad value)
    // are candidates alongside it, so the panic site is never the only explanation offered.
    for (id, reasons) in symptom {
        let Some(site) = index.get(&id) else { continue };
        let site_depth = w.evidence.get(&id).and_then(|e| e.depth);
        let callers: Vec<String> = w
            .evidence
            .get(&id)
            .map(|e| e.callers.iter().cloned().collect())
            .unwrap_or_default();
        w.evidence.entry(id.clone()).or_default().symptom = reasons;
        for call in &site.call_refs {
            for edge in ctx.resolve(call, site).edges {
                if edge.to.is_test || edge.to.id == id {
                    continue;
                }
                let e = w.evidence.entry(edge.to.id.clone()).or_default();
                if e.depth.is_none() {
                    e.depth = site_depth.map(|d| d.saturating_add(1));
                }
                e.feeds.insert(site.qualified.clone());
            }
        }
        for caller in callers {
            if index.get(&caller).is_some_and(|c| !c.is_test)
                && let Some(e) = w.evidence.get_mut(&caller)
            {
                e.calls_symptom.insert(site.qualified.clone());
            }
        }
    }
    // Tests that passed: a function no passing test reaches is more suspect than one they all do.
    const MAX_PASSING_WALKS: usize = 32;
    let mut passed_reach: BTreeSet<String> = BTreeSet::new();
    let checked: Vec<&String> = passing
        .iter()
        .filter(|t| !tests.contains(*t))
        .take(MAX_PASSING_WALKS)
        .collect();
    if passing.len() > MAX_PASSING_WALKS {
        notes.insert(format!(
            "{} passing tests; the first {MAX_PASSING_WALKS} by id were walked for the passing-test signal",
            passing.len()
        ));
    }
    for t in &checked {
        passed_reach.extend(
            walk(&ctx, &BTreeSet::from([(*t).clone()]))
                .evidence
                .into_keys(),
        );
    }
    // Each failing test's own reach, when more than one fails: a function every failing test
    // reaches is evidence, and none at all means no single body replacement can fix them all.
    let mut per_test = Vec::new();
    let mut shared: Option<BTreeSet<String>> = None;
    let mut conclusive = true;
    if tests.len() > 1 {
        for t in &tests {
            let own_walk = walk(&ctx, &BTreeSet::from([t.clone()]));
            let mut reached: BTreeSet<String> = own_walk.evidence.keys().cloned().collect();
            let name = index.get(t).map(|s| s.name.clone()).unwrap_or_default();
            reached.extend(direct_by_test.get(&name).into_iter().flatten().cloned());
            let hidden = own_walk.plausible.iter().any(|p| !reached.contains(p));
            let complete = own_walk.stop() == StopReason::Complete && !hidden;
            conclusive &= complete;
            per_test.push(TestReach {
                test: t.clone(),
                reached: reached.len(),
                complete,
            });
            shared = Some(match shared {
                None => reached,
                Some(s) => s.intersection(&reached).cloned().collect(),
            });
        }
    }
    let shared = shared.unwrap_or_default();
    if tests.len() > 1 && shared.is_empty() {
        let msg = format!(
            "no function is reached by all {} failing tests: a repair needs changes to more than one function",
            tests.len()
        );
        if conclusive {
            shape_not.push(msg);
        } else {
            shape_unknown.push(format!(
                "{msg} (inconclusive: some test's search was cut off or has unresolved calls)"
            ));
        }
    }
    let direct_count = w.evidence.values().filter(|e| !e.direct.is_empty()).count();
    if direct_count > 1 {
        shape_unknown.push(format!(
            "compile errors are inside {direct_count} different function bodies; the fix may need more than one"
        ));
    }
    let mut candidates: Vec<Candidate> = w
        .evidence
        .iter()
        .filter_map(|(id, e)| {
            let s = index.get(id)?;
            let mut score = 0i64;
            let mut reasons = Vec::new();
            for d in &e.direct {
                score += 100;
                reasons.push(format!("+100 {d}"));
            }
            for d in &e.symptom {
                score += 60;
                reasons.push(format!(
                    "+60 {d} (where a bad value was used, not necessarily where it was made)"
                ));
            }
            if let Some(site) = e.feeds.iter().next() {
                score += 40;
                reasons.push(format!(
                    "+40 its result is used by {site}, where the panic occurred"
                ));
            }
            if let Some(site) = e.calls_symptom.iter().next() {
                score += 20;
                reasons.push(format!(
                    "+20 it calls {site}, where the panic occurred, and may pass it a bad value"
                ));
            }
            match e.depth {
                Some(1) => {
                    score += 40;
                    reasons.push("+40 called directly by the failing test or its helpers".into());
                }
                Some(2) => {
                    score += 20;
                    reasons.push("+20 called by a function the failing test calls".into());
                }
                Some(d) if d > 2 => {
                    let points = (40i64 >> (d - 1).min(6)).max(1);
                    score += points;
                    reasons.push(format!("+{points} at call depth {d} from the failing test"));
                }
                _ => {}
            }
            match e.best_match {
                Some(MatchKind::Qualified) => {
                    score += 15;
                    reasons.push(format!(
                        "+15 called by its qualified path ({})",
                        s.qualified
                    ));
                }
                Some(MatchKind::Unique) => {
                    score += 10;
                    reasons.push(format!("+10 the only function named {} in scope", s.name));
                }
                Some(MatchKind::Dispatch) => {
                    let points = (10 / e.alternatives.max(1) as i64).max(1);
                    score += points;
                    reasons.push(format!(
                        "+{points} one of {} in-scope implementations of {}::{}",
                        e.alternatives,
                        e.via.as_deref().unwrap_or("?"),
                        s.name
                    ));
                }
                Some(MatchKind::Shared) => {
                    let points = (10 / e.alternatives.max(1) as i64).max(1);
                    score += points;
                    reasons.push(format!(
                        "+{points} one of {} functions named {} in scope",
                        e.alternatives, s.name
                    ));
                }
                None => {}
            }
            match s.crate_name.as_ref() {
                Some(c) if own.contains(c) => {
                    score += 10;
                    reasons.push(format!("+10 in {c}, the crate the failing test belongs to"));
                }
                Some(c) if imported.contains(c) => {
                    score += 5;
                    reasons.push(format!("+5 in {c}, a crate the failing test uses"));
                }
                _ => {}
            }
            if let Some(t) = s.impl_type.as_ref().filter(|t| w.test_types.contains(*t)) {
                score += 5;
                reasons.push(format!("+5 a method of {t}, a type the failing test uses"));
            } else if let Some(t) = s.type_refs.iter().find(|t| w.test_types.contains(*t)) {
                score += 2;
                reasons.push(format!("+2 uses {t}, a type the failing test uses"));
            }
            for t in terms
                .iter()
                .filter(|t| s.body_idents.binary_search(t).is_ok())
                .take(2)
            {
                score += 10;
                reasons.push(format!(
                    "+10 its body uses {t}, which the failure output names"
                ));
            }
            let extra = e.callers.len().saturating_sub(1).min(3) as i64;
            if extra > 0 {
                score += 3 * extra;
                reasons.push(format!(
                    "+{} reached from {} functions on the failing path",
                    3 * extra,
                    e.callers.len()
                ));
            }
            if shared.contains(id) {
                score += 10;
                reasons.push(format!(
                    "+10 reached by every one of the {} failing tests",
                    tests.len()
                ));
            }
            if !checked.is_empty() && !passed_reach.contains(id) {
                score += 15;
                reasons.push(format!(
                    "+15 reached by none of the {} passing tests checked",
                    checked.len()
                ));
            }
            Some(Candidate {
                id: id.clone(),
                score,
                depth: if e.direct.is_empty() {
                    e.depth.unwrap_or(0)
                } else {
                    0
                },
                direct: !e.direct.is_empty(),
                symptom: !e.symptom.is_empty(),
                reasons,
            })
        })
        .collect();
    candidates.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    for pair in candidates.windows(2) {
        if pair[0].score == pair[1].score {
            notes.insert(format!(
                "{} and {} tie at {}; ordered by id",
                pair[0].id, pair[1].id, pair[0].score
            ));
        }
    }
    let mut frontier = frontier(index, &w, &candidates, budget, &unparsed_in_scope, &notes);
    // A test file that does not compile runs none of its tests: those without an error in them
    // were never observed, and nothing they call was followed.
    for file in &uncompiled {
        let unseen = index
            .symbols
            .values()
            .filter(|s| &s.path == file && !seeds.contains(&s.id))
            .count();
        if unseen > 0 {
            frontier.concealment.insert(
                0,
                format!(
                    "{file} does not compile, so none of its tests ran: only the functions holding its compile errors were followed; its other {unseen} functions (tests and helpers) were not observed"
                ),
            );
            frontier.could_conceal_target = true;
        }
    }
    let shape = if !shape_not.is_empty() {
        RepairShape {
            expressible: Expressible::NotExpressible,
            reasons: shape_not,
        }
    } else if !shape_unknown.is_empty() {
        RepairShape {
            expressible: Expressible::Unknown,
            reasons: shape_unknown,
        }
    } else {
        RepairShape {
            expressible: Expressible::Possible,
            reasons: vec![
                "nothing observed rules out a single function-body replacement (which does not show that one suffices)".into(),
            ],
        }
    };
    Ranking {
        candidates,
        seeds: seeds.into_iter().collect(),
        scope: scope.into_iter().collect(),
        notes: notes.into_iter().collect(),
        frontier,
        shape,
        per_test,
    }
}

/// What the walk could not see, and whether it could hide the target.
fn frontier(
    index: &SymbolIndex,
    w: &Walk,
    candidates: &[Candidate],
    budget: Budget,
    unparsed_in_scope: &[&String],
    notes: &BTreeSet<String>,
) -> Frontier {
    let ids: BTreeSet<&str> = candidates.iter().map(|c| c.id.as_str()).collect();
    let hidden: BTreeSet<&String> = w
        .plausible
        .iter()
        .chain(w.cut_off.iter())
        .filter(|p| !ids.contains(p.as_str()))
        .filter(|p| index.get(p).is_some_and(|s| !s.is_test))
        .collect();
    let mut excluded_counts: BTreeMap<String, usize> = BTreeMap::new();
    for e in &w.exclusions {
        *excluded_counts.entry(e.kind.label().into()).or_default() += 1;
    }
    // An exclusion conceals when some function it might reach is not a candidate.
    let conceals = |e: &Exclusion| {
        e.kind.may_conceal()
            && (e.plausible.iter().any(|p| !ids.contains(p.as_str()))
                || e.plausible_count > e.plausible.len())
    };
    let mut excluded: Vec<Exclusion> = w.exclusions.clone();
    excluded.sort_by(|a, b| {
        conceals(b)
            .cmp(&conceals(a))
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.from.cmp(&b.from))
            .then_with(|| a.call.cmp(&b.call))
    });
    excluded.dedup();
    excluded.truncate(MAX_FRONTIER_EXAMPLES);
    let stop = w.stop();
    let unresolved_dispatch = w
        .exclusions
        .iter()
        .any(|e| e.kind == ExclusionKind::UnresolvedDispatch && conceals(e));
    let mut concealment = Vec::new();
    if stop != StopReason::Complete {
        let which = match stop {
            StopReason::VisitBudget => format!("visit budget of {}", budget.max_visits),
            StopReason::CandidateBudget => format!("candidate budget of {}", budget.max_candidates),
            StopReason::DepthBudget => format!("depth budget of {}", budget.max_depth),
            StopReason::Complete => String::new(),
        };
        concealment.push(format!(
            "the walk was cut off by its {which} ({} functions kept out)",
            w.cut_off.len()
        ));
    }
    // One line per kind of missing link, naming its first examples and their causes.
    for (kind, what) in [
        (
            ExclusionKind::UnresolvedDispatch,
            "trait-method calls on a receiver of unknown type (unresolved trait implementation)",
        ),
        (
            ExclusionKind::ReturnTypeUnknown,
            "method calls on a value whose type comes from a call that could not be followed (missing return-type propagation)",
        ),
        (
            ExclusionKind::ImplicitOperator,
            "operators or methods that may call a user trait implementation (implicit operator dispatch)",
        ),
        (
            ExclusionKind::UntypedMethod,
            "method calls on a receiver with no visible type (unknown type)",
        ),
        (
            ExclusionKind::UnmatchedPath,
            "path calls matching no indexed module path",
        ),
    ] {
        let hits: Vec<&Exclusion> = w
            .exclusions
            .iter()
            .filter(|e| e.kind == kind && conceals(e))
            .collect();
        if hits.is_empty() {
            continue;
        }
        let examples: Vec<String> = hits
            .iter()
            .take(3)
            .map(|e| format!("{} → {}: {}", e.from, e.call, e.cause))
            .collect();
        concealment.push(format!(
            "{} {what} may reach non-candidates; e.g. {}",
            hits.len(),
            examples.join(" | ")
        ));
    }
    if !unparsed_in_scope.is_empty() {
        concealment.push(format!(
            "{} files in scope are not indexed: {}",
            unparsed_in_scope.len(),
            unparsed_in_scope
                .iter()
                .take(5)
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // A panic site's callers are where a bad argument would come from; unknown callers are unseen.
    for c in candidates.iter().filter(|c| c.symptom) {
        let reached = w.evidence.get(&c.id).is_some_and(|e| !e.callers.is_empty());
        if !reached {
            concealment.push(format!(
                "the panic site {} was not reached from a failing test, so its callers (which may pass it a bad value) were not followed",
                c.id
            ));
        }
    }
    if notes.iter().any(|n| n.contains("is not in the index")) {
        concealment
            .push("a failing test is not in the index, so its calls were not followed".into());
    }
    Frontier {
        budget,
        stop,
        explored: w.explored,
        edges: w.edges,
        max_depth_reached: w.max_depth,
        cut_off: w.cut_off.len(),
        excluded_counts,
        excluded,
        hidden: hidden.len(),
        hidden_examples: hidden.iter().take(10).map(|s| s.to_string()).collect(),
        unresolved_dispatch,
        budget_exhausted: stop != StopReason::Complete,
        could_conceal_target: !concealment.is_empty(),
        concealment,
    }
}

/// The decision a ranking supports. Proximity never selects one function on its own, and a repair
/// the loop cannot express is reported as such rather than given a plausible target.
pub fn decide(ranking: &Ranking) -> Target {
    if ranking.shape.expressible == Expressible::NotExpressible {
        return Target::NotExpressible(ranking.shape.reasons.clone());
    }
    let direct: Vec<&Candidate> = ranking.candidates.iter().filter(|c| c.direct).collect();
    let top = |list: &mut dyn Iterator<Item = &Candidate>| {
        list.take(MAX_CANDIDATES).map(|c| c.id.clone()).collect()
    };
    match (direct.len(), ranking.candidates.len()) {
        (1, _) => Target::Single(direct[0].id.clone()),
        (n, _) if n > 1 => Target::Ambiguous(top(&mut direct.into_iter())),
        (_, 0) => Target::None,
        // One candidate is a unique target only when nothing could be hiding another.
        (_, 1) if !ranking.frontier.could_conceal_target => {
            Target::Single(ranking.candidates[0].id.clone())
        }
        _ => Target::Ambiguous(top(&mut ranking.candidates.iter())),
    }
}

pub fn resolve(index: &SymbolIndex, run: &AcceptanceRun) -> Target {
    decide(&rank(index, run))
}
