# Targeting v6: reachability and uncertainty audit (development, NOT held out)

**Nothing here is a held-out result.**
- **Fixtures.** The v3 fixtures (f01–f12), the v5 fixtures (g01–g12), T1 and T3 are all
  development evidence. v6 was written after studying the v5 failures, and its ranking changes were
  chosen on these same fixtures. Good numbers on them do not show generalization.
- **Untouched.** The v5 run and its artifacts are unchanged.
- **How it was produced.** Commands are in `COMMANDS.md`, hashes in `SOURCES.sha256`. The base is
  revision `054175e` plus uncommitted changes.

## 1. Audit of the v5 architecture (before any change)

1. **Implicit calls lost their operands.**
   - Operators and `.max()`/`.sort()` were resolved against every type the caller names anywhere,
     not the operand's type.
   - There was no model at all for `for` loops (`IntoIterator`/`Iterator::next`), iterator
     adapters, `.to_string()`, the format macros (`Display`/`Debug`), or map-key `Hash`/`Eq`.
2. **Format strings were not interpreted.** Macro arguments were parsed as expressions, but not the
   format string, inline captures (`{name}`), named arguments, or `{}` versus `{:?}`.
3. **Call references had no source line,** so explanations could not cite locations.
4. **A receiver that is a call result** (`o.status().to_string()`) had no type.
5. **The frontier reported by kind, not by materiality.**
   - It truncated excluded calls to 25 and their possible callees to 5.
   - It grouped warnings by kind and had no notion of materiality.
   - The P1 judgment could be satisfied by an unrelated call (v5's g07 and g10).

## 2. What changed

### A. Implicit reachability, resolved from the operand's type

**Operand typing.** Every call reference records its line, and its receiver or operand as a chain
(`self`, a local, a field chain) or as the call that produces it. An operand's type comes from:
- a written type (an annotation, parameter, struct literal, cast or literal);
- a field declaration;
- the declared return type of the producing call, with `-> Self` mapping to the impl's type;
- an associated function of a library type (`String::new()`), which gives that library type.
  Wrappers such as `Box::new(x)` stay unknown.

**Rules.**
- **Resolved to a user type (or `dyn` trait) with an implementation:** follow only those
  implementations.
- **Resolved to a library type:** no edge and nothing recorded. It calls only library code.
- **Unknown:** fall back to the types the caller names (labeled as such). If that finds nothing,
  record a material unresolved edge with its reason.
- **Iterator adapters and map-key operations on an unknown operand:** not modeled (see §6).

Bounded: operand typing follows at most three links, and binding resolution is depth-bounded on
every path.

| Construct | Implementations it can reach |
|---|---|
| `for x in e` | `IntoIterator::into_iter` of `e`'s type, `Iterator::next` of `e`'s type or of the type its `into_iter` returns; `dyn Iterator` → every in-scope `Iterator::next` |
| `e.count()`, `.map(…)`, `.collect()` and 27 other adapters | `Iterator::next` of `e`'s type (only when `e`'s type is known) |
| `e.to_string()` | `Display::fmt` of `e`'s type |
| `format!`/`print!`/`println!`/`eprintln!`/`write!`/`writeln!`/`panic!`/`assert!` … | each placeholder: positional, numbered, named (`s = e`) or inline capture (`{name}`); `{}` → `Display::fmt`, `{:?}` → `Debug::fmt` |
| `assert_eq!`/`assert_ne!` | `PartialEq::eq` of the first operand's type |
| `==`, `<`, `+`, … (non-literal operands) | `PartialEq`/`PartialOrd`/`Ord`/`Add`… of the left operand's type |
| `.max()`/`.min()`/`.sort()`/`.contains()`/… | `Ord`/`PartialOrd`/`PartialEq` of the receiver's type |
| `map.insert/get/get_mut/contains_key/remove/entry/contains` | `Hash`/`PartialEq`/`Ord` of the key type, only when the receiver's type is a `HashMap`/`HashSet`/`BTreeMap`/`BTreeSet` |

### B. Evidence-based missing-path explanations

Every unresolved edge records:
- its file, line and enclosing function;
- the construct (`.admit`, `format! {}`, `for loop`);
- the expected dispatch mechanism (for example "implicit Display (fmt) via format! {}" or
  "trait-object or generic dispatch");
- the specific reason resolution failed (for example "the operand comes from .map(), which resolves
  to no indexed function");
- whether it is **material**: some possible callee is not a candidate.

Benign edges (every possible callee is already a candidate, or no user code is reachable) are
counted, not listed.

**Reach closures** are bounded at 2,000 functions each, over the call graph: resolved edges plus
one more hop of uncertainty at a time. For each non-candidate function, the frontier records which
material edges might reach it (`hidden_reach`). It also records what a budget cut-off reaches
(`cutoff_reach`) and what tests that never ran (an uncompiled test file) would reach
(`unobserved_reach`).

**`target::explain_miss(ranking, id)`** explains a miss only by:
- an unresolved edge that might reach `id`;
- a budget cut-off from which `id` is reachable (or whose reach was not fully computed);
- tests that never ran, from which `id` is reachable.

It reports every applicable reason, and always states that reachability through an unresolved edge
"is not established". A warning about an unrelated call never explains a miss; this is tested.

### C. Ranking changes (each with its rationale and evidence)

All were chosen with `ranking_ablation.py`, which rescores the existing candidates, on the outputs
in `ranking-basis/` (the v6 code before any ranking change). Candidate sets, and so recall, are
unaffected.

| Change | Rationale | Development effect (all / deep+dispatch MRR) |
|---|---|---|
| Depth weights 40/20/10/5/… → 10/8/6/4/2 | Distance from the test is a weak prior, not evidence of a defect; the old weights let the test's API surface (constructors, builders) outrank the code doing the work | 0.192 → 0.221 / 0.080 → 0.144 |
| +1 per decision or computation point in the body, up to +10 | A defect needs a branch, comparison, arithmetic or indexing to be wrong in; plain constructors and accessors rarely hold one | with the above: 0.298 / 0.189 |
| Match-confidence points capped at 5 (qualified 15→5, unique 10→5, shared/dispatch 10/k→5/k) | How confidently a call names a function measures edge certainty, not suspicion | with both above: 0.334 / 0.231 |

**Measured and not adopted:**
- an Ochiai spectrum score over static reach (0.195 / 0.087: static reach does not discriminate);
- replacing the two binary test signals with Ochiai (worse);
- also removing the "type the failing test uses" bonus (mixed: deep+dispatch improved, but
  multi+declaration top-5 fell).

I stopped at these three changes to limit fitting to the development set. No ranking threshold is
set.

## 3. Development diagnostics

### Recall and ranking: v4 (v5-evaluated code) against v6 on the same fixtures

| Set | Code | Recall | Top-1 | Top-5 | Top-10 | MRR | Median rank found |
|---|---|---|---|---|---|---|---|
| v3 f01–f12 | v4 | 17/18 | 0.111 | 0.278 | 0.556 | 0.223 | 8 |
| v3 f01–f12 | v6 | 17/18 | 0.167 | 0.556 | 0.667 | 0.357 | 3 |
| v5 g01–g12 | v4 | 14/19 | 0.053 | 0.158 | 0.368 | 0.139 | 11 |
| v5 g01–g12 | v6 | 17/19 | 0.105 | 0.474 | 0.684 | 0.287 | 5 |

| Category (v3 + v5) | v6 recall | v6 top-5 | v6 top-10 | v6 MRR | v4 MRR |
|---|---|---|---|---|---|
| deep | 6/7 | 3/7 | 4/7 | 0.21 | 0.06 |
| dispatch | 6/6 | 1/6 | 5/6 | 0.18 | 0.08 |
| multi | 12/12 | 7/12 | 8/12 | 0.40 | 0.30 |
| declaration | 10/12 | 8/12 | 8/12 | 0.38 | 0.18 |

(Per-set category metrics are in `dev-analysis.json`.)

**The v5 failure modes now resolve:**
- g07 `StockLine::fmt`: missed in v4, rank 13 in v6;
- g10 `RotatedSource::next`: missed → 8;
- g12 `OrderStatus::fmt`: missed → 18.

**Also improved:**
- g01's producer `page_bounds` moved 15 → 2;
- T3's `Policy::intersect` moved 7 → 1;
- T1's targets moved 66/124 (v4) → 19/34.

### Every development miss, and whether the report explains it

| Fixture | Missed target | Explanation (`explain_miss`) |
|---|---|---|
| v3 f12 | `Job::from_line` | Explained twice: the test file does not compile, so the tests that reach it never ran; and an unresolved `.parse` at `src/duration.rs:82` may reach it (not established). |
| v5 g04 | `parse_backoff` | Explained: reachable only from tests that never ran (uncompiled test file). |
| v5 g05 | `stage_21::r14` | Explained: reachable beyond the candidate-budget cut-off (400). |

**Every development miss (3 of 37 targets) has an explanation that connects to the missed function.
No miss is unexplained.**

### Material unresolved edges, budgets, runtime and size

- **Fixtures with at least one material unresolved edge:**
  - v3: 10/12;
  - v5: 6/12;
  - T3: 69 material and 62 benign edges;
  - T1: 238 material and 617 benign edges.
- **`could_conceal_target` set:** 19 of 24 fixtures. It remains a weak signal, but misses are now
  judged by `explain_miss`, not by the flag.
- **Budget cut-offs:** g05 (candidate budget 400) and T1 (candidate budget 400, 52 functions kept
  out). Both are reported. No reach closure hit its 2,000 bound.
- **Runtime:**
  - fixtures: ranking 1–36 ms, indexing 16–330 ms;
  - T3: 113 ms ranking (v4: 39 ms);
  - T1: 729 ms ranking (v4: 179 ms).

  The increase is the reach closures, still far below the 10-second bound.
- **Candidate counts:** about the same as v4 (v3 fixtures 5–38, v5 fixtures 5–30, g05 and T1 400,
  T3 45).
- **Decisions:** no `single` decision on any fixture. scorer v4's P1–P5, computed on these
  development sets, all pass (not a held-out claim).

## 4. Tests and guards (`logs/gates.log`)

- **chip-symbol-loop: 77 tests pass:**
  - contract 6, machine 13, source_safety 7, targeting 7, targeting_frontier 17,
    targeting_safety 11;
  - **targeting_reachability 16 (new).**
- **The new tests cover:**
  - `.to_string()` on a call result (g12's mode);
  - format placeholders: inline capture, positional `{:?}`, named argument;
  - `Display` through `format!` in a loop over a field (g07's mode);
  - map-key `Hash`/`Eq`;
  - a `for` loop over a factory-made `Box<dyn Iterator>` (g10's mode);
  - an iterator adapter on a typed receiver;
  - `IntoIterator` → `next`;
  - library-typed operands recording nothing;
  - unknown operands as material edges that explain the miss;
  - **the negative case:** an unrelated unresolved call does not explain a miss reached only
    through a function pointer;
  - benign edges;
  - budget and uncompiled-test explanations that cover only what they reach;
  - multiple reasons;
  - self-referential operands;
  - the ranking priors.
- **Guards:** inventory 8, product_path 5, forbidden_dependencies 3. The `symbol_loop` example
  builds, clippy reports 0 warnings, and fmt is clean.
- **Scripts:** scorer v4 tests 17 OK, v5 analyzer tests 7 OK.
- **Adjusted existing tests (wording only):**
  - three tests asserted the old grouped concealment wording, and now assert the per-edge material
    report;
  - one now expects the operand-specific cause.

## 5. Implicit call paths now resolved, and still unresolved

**Resolved when the operand's type is known:**
- `for` loops (`IntoIterator`/`Iterator::next`, including `dyn Iterator` from factories);
- iterator adapters;
- `.to_string()` and the format macros (`Display`/`Debug`, all placeholder forms);
- `assert_eq!`/`assert_ne!`, comparison, equality and arithmetic operators;
- ordering and equality methods;
- map-key `Hash`/`Eq`/`Ord` when the receiver is a std map or set;
- explicit method and trait-object dispatch, as before.

**Unresolved, and recorded as material edges when they could reach non-candidates:** implicit calls
whose operand type is unknown, for example an operand behind a library adapter chain, or a wrapper
such as `Box::new(x)`.

**Not modeled, and not recorded:**
- iterator adapters and map-key operations on operands of unknown type;
- calls through closures, function pointers and `Fn` trait objects (the negative test shows such a
  miss is reported as **unexplained**, correctly);
- `Drop`, `Deref`, `From`/`Into` conversions, `Default` via `..Default::default()`;
- operator overloading on references or generic parameters without a visible bound;
- macros other than the format and assert family;
- method chains more than three links deep.

## 6. Limitations

- **Every improvement was measured on development fixtures,** some of which motivated the change.
  The ranking changes in particular were selected on them.
- **`hidden_reach` is a static over-approximation.** "May reach; not established" is the most it
  can say. The closure follows each unresolved edge's possible callees, so it can attribute a miss
  to an edge that is real but not the operative one. f12 shows two valid reasons. The explanation
  is "could have", not "did".
- **The concealment flag is set on 19 of 24 fixtures.** It is a weak signal by itself.
- **T1 still hits the 400-candidate budget,** and ranking cost grew about 4× there (729 ms).
- **Deep and dispatch ranks are still mediocre:** deep MRR 0.21, dispatch 0.18, with no deep or
  dispatch target ranked first.

## 7. Decision

**Is another held-out evaluation justified? Yes, under these conditions:**
- the v6 code and binary are frozen, along with scorer v4 and a new analyzer;
- the P1 judgment uses `explain_miss` semantics (a miss counts as reported only through an edge,
  budget or uncompiled test that connects to it), preregistered before any fixtures exist;
- a new independent author writes the fixtures, with the same safety criteria (P2, P3, P6) and P4
  at 80%;
- the fixtures add constructs v6 does not model (closures and function pointers, `Drop`/`Deref`,
  wrappers), so unexplained misses can be observed;
- ranking is reported without thresholds.

Repair generators stay off until such an evaluation passes.
