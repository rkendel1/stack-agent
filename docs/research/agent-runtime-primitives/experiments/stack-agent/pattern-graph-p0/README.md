# Pattern graph P0: a deterministic implementation-pattern inventory

Design and the Phase 1 findings: [`DESIGN.md`](DESIGN.md) (written before the code).
Raw measurements: [`measurements-origin-main-ec22d91.txt`](measurements-origin-main-ec22d91.txt).

Each section says what kind of claim it makes. **Implemented** is what the code does. **Tested** is
what an automated test asserts. **Measured** is a number from one run on one machine. **Unresolved**
is what I do not know or did not do.

## 1. Implemented

* **Target: Rust, with `syn`, not TypeScript.** The existing graph is Rust-only and the only
  language-aware infrastructure here is `syn`; there is no TypeScript parser in the dependency tree and
  no TypeScript in this repository. See `DESIGN.md`.
* **One extension point: the existing graph pass.** `stack-agent init` now calls
  `chip_graph::analyze_with_patterns`, which collects pattern facts inside the graph's own scan, on
  the `syn::File` it already parsed. No second parse, no model call, no process, no network. The
  architecture graph's nodes, edges and snapshot id are **unchanged** (asserted below).
* **A second document, not a second graph:** `chip.patterns.v1`, written beside the snapshots as
  `.chip/graph/patterns-<id>.json` with a `patterns-latest` pointer, using the same atomic write.
  It references graph node ids (symbol, file, test), names the graph snapshot it was derived from,
  and has no nodes or edges of its own. The gap that required it: graph nodes carry an id and a kind
  and nothing else.
* **Four structures** (the brief's route handlers, request validation, database access and
  authorization are web-framework concepts with no counterpart in what this analyzer can see, and
  are **not implemented**):

  | Category | Observed structure | Signature |
  | --- | --- | --- |
  | `trait_impl` | `impl T for X` | sorted method names |
  | `error_type` | a type named `*Error` | derives, and traits implemented for it in the same crate |
  | `test_shape` | a test function | attribute, async or sync, result or unit, unit or integration placement; plus the graph symbols it names |
  | `call_order` | a function with a `validate*` call and an effect call in its top-level statements | the classes in evaluation order |

* **Order is claimed only where the syntax establishes it.** Straight-line top-level statements, in
  Rust's evaluation order (operands before the operation; receiver, arguments, then the call; a call
  in an argument runs first). A call under `if`, `match`, a loop, a closure, a nested block or the
  right side of `&&`/`||` is counted and **not ordered**; a function that has one half in straight-line
  code and the other half only under a condition is recorded as an `order_not_established` exception.
* **Call classes come from names and say so.** `validate*`/`verify*` are validation; an explicit list of
  twelve names (`write`, `execute`, `rename`, ...) are effects. Every `call_order` observation is
  flagged `name_based`, and the report lists the vocabulary.
* **Identity:** an observation's id is a hash of category, file, name and signature, **not the line**.
  Two observations that would share an id are numbered `#2` in source order. A pattern's id is a hash
  of category, family and signature. Lines are evidence and move with the code.
* **Classification:** `observed` or `candidate_convention` only. A signature with at least 3
  supporting observations is a candidate; that is a count, not a judgement. There is **no policy,
  rule or requirement variant**, `enforced` is always `false`, `basis` is always `deterministic`, and
  every pattern carries the caveat that frequency is not proof. Counterexamples are the family's
  observations with a different signature (a different structure, not necessarily a violation).
* **Provenance and coverage:** analyzer id and version, graph snapshot, the SHA-256 of every analyzed
  file, every skipped file with its reason (`vendored tree`, `generated file`, `not reachable from a
  crate root`), parse failures, item macros whose expansion is invisible, and the ignore rules
  actually applied. `coverage.complete` is `true` only when nothing was skipped, failed or unexpanded.
* **Staleness:** `PatternReport::check(root)` re-hashes the analyzed files and re-walks for new Rust
  files; `read_latest_patterns` returns a stored report **only together with that check**.
  `invalidated()` names the observations and patterns resting on changed or removed files.
  `stack-agent graph patterns` prints the verdict first and exits 2 when stale.
* **CLI:** `stack-agent init` prints a pattern summary and writes the report; `stack-agent graph
  patterns [--json]` reads it back.

## 2. Tested

Everything below passes. Fixture: a small Rust crate (`crates/chip-graph/tests/pattern_fixture`, data,
not a workspace member) with known recurring structures, known exceptions, a file that does not
parse, a vendored copy and an unreachable file.

| Requirement | Test (`chip-graph/tests/patterns.rs` unless noted) |
| --- | --- |
| each supported structure, deterministically | `a_recurring_trait_impl_shape_…`, `error_types_are_matched_…`, `test_shapes_record_…`, `validation_before_a_write_is_found_…` |
| ordering-sensitive relationship | `validation_before_a_write_is_found_where_the_parser_establishes_the_order` (incl. a call nested in an argument; the reverse order is its own counterexample) |
| co-occurrence is not proof | `a_validation_that_may_not_run_is_not_claimed_to_precede_the_write` (validation under `if`, validation in a closure: both calls present, no ordered pair, recorded as exceptions) |
| duplicates and stable identity | `the_same_structure_in_two_places_…`, `moving_code_changes_its_line_and_not_its_identity`, `identical_structure_under_one_name_in_one_file_is_numbered_not_merged` |
| exceptions and incomplete analysis | `skipped_unparsed_and_unreachable_files_are_listed_…` (every discovered file accounted for), `item_macros_are_counted_…`, `a_repository_with_nothing_missing_says_it_is_complete`, `the_vendored_copy_is_not_part_of_…` |
| candidates are not promoted | `the_support_threshold_is_exact_and_a_candidate_is_never_a_rule` (2 observations stays `observed`, 3 is a candidate, nothing is `enforced`, the serialized report has no way to say policy or rule), `a_common_pattern_is_not_marked_mandatory_…` (40 of 40) |
| repeatability | `an_unchanged_repository_gives_the_same_report_every_time` (also from a copy at another path) |
| source changes, stale evidence | `a_changed_source_file_is_detected_…` (stored report read back as stale, which file, which observations and patterns are invalidated, support 4 to 3 after re-analysis), `added_and_removed_files_make_a_stored_report_stale`, `a_damaged_stored_report_is_refused` |
| integration with initial graph construction | `collecting_patterns_does_not_change_the_graph` (nodes, edges and snapshot id equal on two fixtures), `every_reference_in_a_report_is_a_node_of_the_graph_it_names`; `stack-agent/tests/graph_patterns.rs` (the real binary: `init` writes both, `graph patterns` is current, then stale after an edit and current after `init`, JSON, absent report) |
| no source copied into the report | `the_report_holds_references_and_digests_and_never_source_text` |

Existing `chip-graph` tests (including the golden snapshots) pass unchanged.

## 3. Measured

**Repository:** this repository at `origin/main` `ec22d91`, analyzed from a clean worktree (my
fixture is a crate inside the working tree and would contaminate it). 37 crates, 571 Rust files.
Release build, macOS arm64, one machine, one set of runs. Raw output in the file above.

| | |
| --- | --- |
| Rust files discovered / analyzed | 571 / 430 |
| Skipped, with reason | 141: 123 `vendored tree`, 18 `not reachable from a crate root` |
| Parse failures | 0 |
| Unexpanded item macros | 15 (so `coverage.complete` is `false`) |
| Graph: symbols / `imports` edges / test suites | 4513 / 5208 / 3267 |
| Observations | 2780: 553 `trait_impl`, 36 `error_type`, 2187 `test_shape`, 4 `call_order` |
| Patterns / candidate conventions | 80 / 41 |
| Exceptions | 28: 27 `order_not_established`, 1 `impl_for_non_path_type` |
| Tests that mention at least one graph symbol | 1953 of 2187 (a mention, not coverage) |
| Report size | 2.43 MB (49 KB of it source digests); the graph snapshot is 7.9 MB |

**What it found, in concrete terms.**

* A recurring structure with a real divergence: **47** `CapabilityProvider` impls define
  `availability` and `capabilities`; **8** also define `validate_inputs`. Both are candidates, each
  listing the other as counterexamples. The 8 are the production adapters (`CapabilitySet`,
  `AllowlistedBackend`, `ProjectExecutor`, `PaxExecutor`, `PaxObserve`, `RemoteCapabilityBackend`) and two
  test doubles. Of the 47, 38 are in `tests/` directories and 9 in `src/` (not individually
  classified). **The most common shape here is the one that validates no inputs.** The report states both and promotes neither, which is exactly why frequency
  cannot decide a requirement.
* `call_order [validate, effect]`: 4 observations (`Agent::execute_capability`,
  `Agent::obtain_with_state`, `SessionWriter::create`, `SessionWriter::open`), 0 counterexamples.
  A candidate on 4 observations in 2 files of 2 crates. 27 further functions have a validate-like or effect-like
  call in straight-line code and the other only under a condition, and are recorded as
  `order_not_established` instead of being claimed.
* `error_type`: three candidates, with 20, 30 and 33 counterexamples (16, 6 and 3 supporting): error types do not share one
  shape here, and the report does not say which is right.

**Cost (15 alternating runs, release, median):** graph alone **509 ms**; graph with patterns
**541 ms** (+6.4%, of which pattern collection and the report are about 31 ms). Peak resident memory
(15 runs, two repetitions each): about 286 and 293 MB without; 315 and 317 MB with (about +29 MB,
+9%). Storage: +2.43 MB per snapshot of the report. A report is rewritten on every `init`.

**Determinism:** three analyses of the unchanged repository gave the same report id. A copy at another
path gave the same id (test).

**After a source change** (a copy of the repository, one file edited, one new `CapabilityProvider`
impl added): the stored report was reported stale with exactly that file; the three-method pattern's
support went 8 to 9; 2780 observation ids were kept, 1 added, 0 removed; re-analysis was current.

**Not measured:** task completion, model tokens, human correction effort (out of scope by design),
other machines, a second real repository, memory other than peak RSS.

## 4. Validation

Run on macOS arm64 (`pax` 0.4.1 on `PATH`).

* `cargo fmt --all -- --check`: clean.
* New: 22 tests in `chip-graph/tests/patterns.rs`, 4 in `stack-agent/tests/graph_patterns.rs`, one
  ignored measurement harness (`patterns_measure.rs`).
* Workspace and Linux CI: see the PR description; the numbers are reported there.

## 5. Unresolved

* **`.gitignore` is not read.** Neither the graph nor the pattern pass honours it. The pass skips the
  graph's built-in list and `vendor/`, and the report says so. A repository with generated trees outside
  those names would have them analyzed.
* **Syntax only.** `syn` sees unexpanded code: anything defined by a macro is invisible (15 item macros
  here, counted), and `#[cfg]` is not evaluated.
* **Names, not types.** A trait is identified by its last path segment (`Error`, `Display`, `Default`
  could be a local trait with the same name), and an error type is matched to impls by name within one
  crate. Cross-crate type resolution is not attempted, and ambiguity is recorded as an exception.
* **The call vocabulary is untuned.** `validate*`/`verify*` and twelve effect names were chosen by me,
  not derived from the repository or evaluated for recall or precision. Only 4 functions matched here.
* **`trait_impl` signatures may be forced.** A method set that a trait requires of every implementor
  repeats by construction; the analyzer cannot tell required from provided methods of a trait defined
  elsewhere. The caveat is stated on every such pattern, and the threshold of 3 is a starting count
  that nothing here validates.
* **`test_shape` has no meaningful counterexamples.** Different test styles are not violations of each
  other; the numbers (for example 1070 counterexamples) are tallies of other signatures.
* **Staleness is per file.** A change marks the report stale and `invalidated()` names what rests on the
  changed files, but there is no incremental update: `init` rebuilds the whole report (about 31 ms
  of added work here).
* **Report size.** 2.4 MB for this repository, mostly observations. A consumer that needs less would
  need a filtered view; none exists.
* **A pattern is not tied to a policy.** There is no place where a person authored policy, so "explicit
  architectural constraint", "security invariant" and "verified requirement" exist in the design as
  categories that must stay separate, and nowhere in the code. P0 only guarantees that the inferred
  category cannot be mistaken for them.
* **Not implemented:** route handlers, request validation, database access, authorization, TypeScript,
  model-assisted discovery, planning, mutation checks, other languages.
