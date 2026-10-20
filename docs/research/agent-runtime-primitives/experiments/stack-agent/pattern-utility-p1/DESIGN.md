# Pattern utility experiment: design

Written before the selector and the harness. `README.md` reports what was built and measured and
supersedes this where they differ.

## What P0 gives us (read, not assumed)

* `chip.patterns.v1` (`PatternReport`): observations (id, category, family, name, signature, subject
  graph node, file, line, flags, test mentions), patterns (id, classification `observed` or
  `candidate_convention`, support, counterexamples, counts, caveats), coverage, per-file SHA-256s,
  and the graph snapshot id. No policy variant; `enforced` is always false.
* `read_latest_patterns(root)` returns `(PatternReport, Staleness)` and nothing else: a consumer
  cannot get a report without being handed whether it is current.
* P0's own caveats still hold and bound what this experiment may claim: call classes are
  name-based, a trait's method set may be compiler-required, traits and error types are matched by
  name, `test_shape` counterexamples are other styles.

## How planning and context work today (read, not assumed)

| Question | Finding |
| --- | --- |
| Does `work` use the architecture graph? | **No.** `chip_graph` is referenced only by `graph_cmd.rs` (the `init`, `graph`, `slice`, `impact` commands) and by the decision-state code for `slice`/`impact` tokens, never by `run_software_work_*`. |
| What does the model see? | The goal text, the capability list, and capability observations (`EscalationContext`): "Goal / State / Evidence / Prior decisions / Ruled out / Question". The goal is passed verbatim. |
| Is there a planner? | No. The model proposes one capability request per turn; Chip validates and executes it. |
| An existing seam for extra context? | `chip_experiments::GuidedContext` appends digest-pinned guidance to the goal under a label. `chip-experiments` is not wired to real providers ("Real providers are not wired in yet"), so it cannot run a real model. |
| So the baseline "existing graph and planning context" is | the goal and the project the capabilities can read. The graph is **not** part of it. |

## Smallest integration point

The **goal text**. A candidate condition is the baseline goal followed by one delimited block that a
read-only selector renders from the report. Nothing in `work`, `serve`, the context policies or any
default changes; the block reaches the model through the same field every goal reaches it through,
which is also how the baseline goal does. Nothing is persisted: the selection is computed from the
stored graph and report on demand.

The selector lives in `chip-graph` next to the report (it reads `ArchitectureGraph`, `PatternReport`
and `Staleness`, all of which are already there) and is exposed as
`stack-agent graph patterns select --task FILE`. Nothing calls it from `work`.

## The selector, bounded and explainable

Input is a **task representation** the caller writes: files, graph symbol ids and explicit pattern
families. The goal's prose is **never parsed** and names are **never compared for similarity**.
Signals, each a fact with its evidence:

| Signal | Fact |
| --- | --- |
| `file` | an observation is in a file the task names |
| `subject` | an observation's subject is a graph symbol the task names |
| `implements` | a task symbol has an `Implements` edge to a trait symbol whose name is a pattern's family |
| `family` | the task names the family explicitly |
| `verification` | a test observation mentions a task symbol |

Output states: `selected`, `no_applicable_pattern`, `insufficient_evidence` (the task names nothing,
or everything it names is unknown or unanalyzed) and `stale` (the report does not match the source:
nothing is returned). Each selected pattern carries its reasons, at most 3 supporting examples and 3
counterexamples (references, not source), its classification and caveats, and the coverage. Bounded
by count and by rendered size. Ranking is by signal strength, then pattern id: **frequency does not
rank**.

## Evaluation: two separated parts

1. **Selector evaluation (deterministic, offline).** A pre-registered corpus of tasks, each with the
   patterns I expect to be relevant, written before the selector ran. Measured: whether the selector
   returns them, what else it returns, the tokens it adds, and its behavior on the six failure cases.
2. **Model evaluation (real local model, small).** The same tasks run through the real `stack-agent
   work` with a local model, baseline against candidate, host-owned hidden acceptance tests, and the
   pattern analyzer itself used to say whether the result followed or departed from a pattern.
   Small enough to read every trial; no significance is claimed.

## Not done

LLM discovery or ranking, promotion, mutation rejection, any change to production defaults, new
analyzer vocabulary, a new store, a new agent framework.
