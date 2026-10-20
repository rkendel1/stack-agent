# Capability selection S1: which AppBoundry/Synapse capabilities add something Stack-Agent lacks?

**Question.** AppBoundry onboards 302 Synapse WASM modules as callable capabilities. Before any
integration (no `CapabilitySource` crate, no AppPort code), which of them provide something Stack-Agent
cannot already do with its native operations (`project.*`, `project.git.*`, `pax.test`, chip-graph)?

**Answer: none clears the bar for a first integration.** Every coding-relevant module is either a line
heuristic that a native tool does better, a restatement of caller input, or duplicates Git, the
filesystem or chip-graph. One module (`skill-02-markdown-parser`) is accurate against an independent
reference, but what it enables (a documentation link check) is a few lines of native code, and this
repository already has one (`scripts/check-design-docs.py`, check 5). Labels below: **measured**
(run against an independent reference), **contract** (read from the source-grounded contract that
AppBoundry publishes), **not done**.

Nothing here is on a product path. No code in `crates/` changed.

## Inputs

| | |
| --- | --- |
| Synapse | `rkendel1/synapse` at `796a93d5f5368052a475cbe91745990a091dded6`, read only (the revision AppBoundry pins) |
| Classification | AppBoundry's own `classifyCorpus` (`packages/cli/test/synapse-corpus.ts`): 302 ready, 743 unsupported, 5 need portability work (`results/ready-302.json`). `pnpm synapse:inventory` (full conformance of every ready module through the real host and MCP) also passed |
| Execution | each module's pinned `browser_agent.wasm`, run through the Synapse skill ABI v2 directly (`probes/syn.mjs` checks the sha256 against `synapse.inventory.json`); the TOML guard check went through AppBoundry's packaged input schema |
| References | TypeScript 5.9.3 `ts.preProcessFile`; markdown-it-py 3.0.0 (CommonMark); Python `tomllib`; chip-graph snapshot of this repository |

## Selection criterion

A module is retained only if it (1) does real work on its input, (2) does something Stack-Agent's
native operations do not, and (3) agrees with an independent reference on real repository material.
A module whose output restates its input, or that duplicates a Git, filesystem, PAX or chip-graph
operation, is rejected.

## Screen

**First cut (contract).** 164 of the 302 are outside coding work: image, audio, video, web protocol,
crawling and feeds, NLP, knowledge and retrieval, data-frame statistics, causal analytics,
optimization. The 138 left are `skill-14` code analysis (36), `skill-02` parsers (18), `skill-16`
orchestration (18), `skill-07` data quality and SQL (16), `skill-10` documents (11), `skill-17`
security (11), `skill-09` text rules (9), and the repository-graph and change-impact modules (19).

**Rejected on contract alone.** AppBoundry's descriptions say outright what each module does:

* `skill-14`: line-prefix matching and counting. `source-ast-parser` and `symbol-indexer` match lines
  that start with `fn`, `class` and similar ("Despite the name…"); `linter` flags tabs, TODO and long
  lines; `secret-scanner` matches four substrings; `call-graph-builder` counts `name(`;
  `patch-generator` and `patch-validator` wrap or shape-check caller input and never diff or apply;
  `test-runner-adapter` echoes results it is given; `migration-planner` and `refactoring-planner`
  return fixed three-step templates. `pax.test`, `project.search` and the model's own reading cover
  these.
* `skill-16`/`skill-17`: compare caller-supplied strings or counts (`retry-controller`: attempt <
  maxRetries; `tamper-detector`: two given hashes differ; `permission-checker`: set membership).
  Chip already enforces its own bounds, authorization and recovery, and must not delegate them.
* `skill-09`: word-overlap heuristics (`verification-skill` marks a claim verified when one evidence
  string shares two words with it). Stack-Agent's completion check is observation-based; this would
  weaken it.
* `skill-02-json-schema-validator`: implements a keyword subset (no `$ref`, `oneOf`,
  `additionalProperties`, `pattern`, `format`). **Measured:** 0 of the 8 schema files in
  stack-agent, appboundry and authboundry stay within the subset.

## Measured: the candidates that could add something

### Repository graphs (`repository-graph-builder`, `repository-impact-traverser`, `graph-impact-differ`)

| Probe | Result |
| --- | --- |
| Builder from what `work` observes in a non-Cargo project (file paths and sizes) | Edges only from each file's immediate parent directory; no ancestor chain, no file-to-file edge. Traversal from `chip-experiments/src/plugin.rs` returns its 7 sibling files and none of its dependents outside `src/`: `project.list` gives the same. (`probes/a_list_only.mjs`) |
| Builder fed chip-graph's edges (9651 nodes, 31837 edges) | Upstream traversal finds all 6 files that use `CapabilitySource`/`CapabilityFixture` (grep), plus `stack-agent/src/experiment.rs`. The recall comes from chip-graph's `imports` edges; the module adds a breadth-first search. `truncated` is true on every run; direction `both` reaches 999 nodes at depth 3. AppBoundry's contract caps the graph at 200 nodes and 200 edges. (`probes/b_chipgraph.mjs`) |
| Differs on a README edit | `graph-impact-differ` compares ids only, so it reports nothing. `repository-graph-differ` reports the file as changed only when its size changed. Both report an unintended extra file, which `git diff --name-status` already does. (`probes/c_diff.mjs`) |

### TS/JS import edges (the gap chip-graph leaves outside Rust)

160 TypeScript files in `appboundry/packages/*/src`, 568 import specifiers according to the TypeScript
compiler's own scanner (`probes/imports.mjs`, `probes/scp.mjs`):

| Module | Recall | Notes |
| --- | ---: | --- |
| `skill-14-import-graph-builder` | 0.616 | Returns the raw statement line, not a specifier or a resolved target. Misses every `export … from` (140), every multi-line import (66; it emits the line `import {`) and `import()` (12) |
| `skill-02-source-code-parser` | 0.438 overall (0.574 on accepted files) | Refuses the 23 files over 16384 characters; 19 returned specifiers are not imports |

The TypeScript compiler, already installed in any TypeScript project, gives the reference set by
definition, and `ts.resolveModuleName` resolves targets. **The TS/JS edge gap is real, and it should be
closed natively** (a chip-graph analyzer for TS/JS), not through these modules.

### Markdown structure (`skill-02-markdown-parser`)

Against CommonMark (markdown-it-py) on every tracked `.md` file in the five repositories
(`probes/md_ref.py`, `probes/md.mjs`):

| Corpus | Files | Refused (> 16384 chars) | Link recall | Link precision | Headings exact |
| --- | ---: | ---: | ---: | ---: | ---: |
| all five repositories | 1875 | 95 | 0.994 | 1.000 | 0.996 |
| excluding Synapse | 371 | 78 (21%) | 0.994 | 1.000 | 0.997 |

The only accurate module in the screen. Two limits: outside Synapse, the refused files hold 780 of the
934 links (83%); and 2 of 83 READMEs are refused. What it enables for a README task, checking that
relative links resolve, is a short native check (this repository's `check-design-docs.py` does it).
**Kept as a candidate only if a documentation task shows the model needs it**; not an integration
target.

### TOML (`skill-02-toml-parser`)

925 tracked `.toml` files, compared with `tomllib` (`probes/toml_ref.py`, `probes/toml.mjs`): 920 equal,
**5 silently misread through the bare ABI, all workspace-root `Cargo.toml` files** (multi-line
`members = [`). Through AppBoundry's packaged input schema (`TOML_DOCUMENT` pattern) those 5 and 2 more
are refused instead, so AppBoundry fails closed, but it refuses exactly the manifests an agent edits
most. For Rust, cargo (through PAX) and chip-graph already read these.

### The four checked-in modules (earlier probe)

`skill-04-citation-matcher` marked every claim supported, including a negation and "The moon is made
of green cheese" (0.418) (`probes/four_modules_citation.mjs`). `skill-06` ranks caller-supplied text
by term frequency; `change-context-resolver` maps concepts to caller-supplied business mappings;
`skill-08` Monte Carlo is correct but unrelated to repository work.

## Conclusions

1. **Callable is not valuable.** The 302 are well-contracted and conform, but almost all coding-relevant
   modules are line heuristics or restate their input. AppBoundry's descriptions already say so.
2. **Keep structure native.** chip-graph supplies the relationships that made traversal useful. The
   missing piece is a TS/JS analyzer in chip-graph, measured against the TypeScript compiler as above.
3. **No integration yet.** No module justifies an AppPort `CapabilitySource` crate (AGENTS.md §14):
   none passes criterion (2) and (3) together for a coding task.

## Not done

* **The deciding experiment** (verified completion with versus without a capability on the original
  failed task). It needs the original task: the repository holding `packages/workspace/README.md`,
  the exact goal text and the original Qwen model. None of the five repositories contains that path.
  The pipeline was checked once: `stack-agent work` with `alibaba/qwen3-coder-30b-a3b` through the AI
  Gateway, PAX on the path, a disposable copy of the feasibility fixture. The run was blocked
  ("completion refused: the proposed answer is not supported by the observations"), the audit was
  clean, and there were 0 false completions. One run establishes neither success nor benefit.
* When it runs, the first batch is a **pilot** (about 5 runs per arm, same starting state, model,
  prompt, deadline and ordinary tools) to estimate variability; the size of a confirmatory trial is
  decided from that, not assumed.

## Reproducing

Checkouts at `/home/user/{stack-agent,appboundry,synapse,...}` (paths are written into the probes),
AppBoundry dependencies installed, Node 22. Run each probe from an empty scratch directory: the
Python reference scripts write `md_ref.json` and `toml_ref.json` there, and `md.mjs` and `toml.mjs`
read them. `b_chipgraph.mjs` takes a chip-graph snapshot path (from `stack-agent init` on a copy) and a
start node.
