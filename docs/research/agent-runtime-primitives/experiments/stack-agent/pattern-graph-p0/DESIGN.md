# Pattern graph P0: design

Written before the implementation (Phase 1 findings first). `README.md` reports what was built,
tested and measured, and supersedes this where they differ.

## What exists (inspected, not assumed)

| Question | Finding |
| --- | --- |
| "Project fingerprinting and initial graph construction" | `stack-agent init` (`graph_cmd.rs`) calls `chip_graph::analyze_with_stats` and writes a snapshot. There is no separate fingerprinting stage. |
| The graph | `chip-graph`: a **Rust-only**, `syn`-based, deterministic, content-addressed projection (`chip.architecture.v1`). Nodes are `{id, kind}` and edges `{from, kind, to}`: **no attributes**. Its docs: "a projection, never authority". |
| Parsing | `syn` 2 (`full`, `visit`) and `toml`. No tree-sitter, SWC or other parser is in `Cargo.lock`, and nothing parses TypeScript. |
| File discovery | one directory walk, symlinks skipped, a built-in skip list (`.git`, `target`, `.chip`, `node_modules`). **`.gitignore` is not read**, and `vendor/` is not skipped. |
| Reachability | only files reachable from a crate root through `mod` declarations are parsed. Orphan `.rs` files are `File` nodes with no contents. |
| Symbols | public items only; methods only from inherent `impl`s. Trait impls are recorded as `Implements` edges when both ends resolve uniquely. |
| Tests | `Tests` edges mean "this test body *mentions* an identifier that names exactly one symbol". A mention, not coverage. |
| Persistence | `.chip/graph/<snapshot-id>.json` plus a `latest` pointer, written atomically; a cache that "can be deleted and rebuilt". FeltDB (agent state) is not involved. |
| `project.read/list/search` | the model's capabilities for reading a project during a work; `init` does not go through them, and nothing here changes them. |
| Existing contracts | graph goldens pin the snapshot bytes for fixtures, so the graph's own nodes and edges must not change. |

## Target: Rust, not TypeScript

The brief suggests TypeScript API routes. This repository has no TypeScript parser and no TypeScript
analyzer, no web routes to analyze, and the only language-aware infrastructure is Rust through
`syn`. Adding a TypeScript parser would be a new dependency tree and a second analyzer for a language
this repository does not contain. P0 targets **Rust with `syn`**. The brief's route handlers,
request validation, database access and authorization have no Rust counterpart in the analyzer's
reach without picking a web framework; they are **not implemented**, and the report says so.

The Rust-native implementation structures implemented instead:

| Category | Structure | Family | Signature |
| --- | --- | --- | --- |
| `trait_impl` | `impl T for X` | the trait's name | the sorted method names the impl defines |
| `error_type` | a type named `*Error` | `error_type` | its derives and the traits implemented for it in the same crate |
| `test_shape` | a test function | `test_shape` | attribute, `async` or `sync`, result or unit, `unit` or `integration` placement |
| `call_order` | a function whose top-level statements call a `validate*` and an effect | `call_order` | the classes of those calls in evaluation order |

## Where the graph cannot represent it, and the extension

**Gap.** A pattern observation needs a signature, counts, a classification, provenance and coverage.
A graph node is an id and a kind. Encoding those as nodes and edges would put attributes in node ids
and change every snapshot id and golden.

**Extension.** A second *document*, `chip.patterns.v1`, in the same store, content-addressed the same
way, that **references** graph node ids (symbol, file, test) and records the graph snapshot it was
derived from. The architecture graph's nodes, edges and snapshot ids do not change. This is not a
parallel graph (it has no nodes or edges of its own), not a database, and not a new service.

**One parse.** The collector runs inside the existing `scan_file`, on the `syn::File` the graph
already parsed. The pattern pass costs no second parse and sees exactly the files the graph sees.

## Representation

`PatternReport`: schema, analyzer id and version, graph snapshot, coverage, source digests,
observations, patterns, report id (SHA-256 of the canonical bytes; no timestamps).

* **Observation:** id, category, family, name, signature, subject (a graph node id), file, line,
  flags, related symbols. **Identity excludes the line** (the line is evidence, not identity), so
  moving code does not change what it is.
* **Pattern:** a group of observations with the same category, family and signature: id,
  classification, supporting observations, counterexamples (the family's other signatures),
  counts, distinct files and crates, caveats.
* **Classification:** `observed` or `candidate_convention` only. There is no variant for policy,
  rule or requirement, so a promotion cannot be written. `authority` says `inferred_from_frequency`
  and `enforced` is always `false`. Basis is always `deterministic`; no model is involved.
* **Candidate rule:** a signature with at least 3 supporting observations. A count, not a judgement
  of correctness.

## Staleness

The report records the SHA-256 of every file it analyzed. `PatternReport::check(root)` re-hashes them
and re-walks the tree, and returns what changed, what disappeared and what is new. A stored report is
read only through a function that returns that check with it, so a report is never handed over as
current without having been compared with the source.

## Not done

LLM discovery; pattern-constrained planning; mutation rejection; policy generation; promotion; other
languages; a plugin framework; a second store; changes to the stuck guard, P5 or P6.
