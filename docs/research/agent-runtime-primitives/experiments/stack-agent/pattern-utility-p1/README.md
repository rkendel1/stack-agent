# Pattern utility: does the P0 inventory help a task? A selector, a corpus, and a pilot

Design and integration point (written first): [`DESIGN.md`](DESIGN.md). Corpus, hidden acceptance
tests and the pre-registration with its one amendment: [`corpus/`](corpus/). Harness:
[`pattern_experiment.py`](pattern_experiment.py). Raw results: [`results/`](results/).

**The answer to the question this PR was opened to ask is: not established.** The selector works, is
deterministic and honest, and has moderate precision on a small pre-registered set. Whether pattern
context makes a model's implementation better was **not measured**, because no available model could
complete even the simplest task. Sections are labelled **implemented / tested / measured /
unresolved**.

## 1. Implemented

* **Integration point: the goal text.** `work` does not use the architecture graph at all (only
  `graph_cmd.rs` does), the model sees the goal verbatim, and `GuidedContext` is wired only to
  `chip-experiments`, which has no real providers. A candidate condition is the baseline goal plus one
  delimited block. No production default changed (`DESIGN.md`).
* **The channel is smaller than assumed.** `work` refuses a goal over **2000 bytes**
  (`MAX_GOAL_BYTES`). The six goals are 307 to 695 bytes, so the block gets **1303 to 1691 bytes**
  (about 330 to 420 tokens). I found this when the first candidate trial exited in 0 seconds. It is a
  production limit and I did not change it.
* **`chip_graph::select_patterns` (experimental, read-only),** exposed as `stack-agent graph patterns
  select --task FILE [--json] [--max-bytes N] [--max-examples N]`. Nothing calls it from `work`, `serve`
  or any default. Input is a task representation the caller writes: files, graph symbol ids, explicit
  families. **The goal's prose is never parsed and no names are compared for similarity.**
  * Signals, each a fact with its evidence: `file` (an observation is in a file the task names),
    `subject` (a named symbol is the subject of an observation), `implements` (a graph `Implements`
    edge names the family), `family` (named outright), `verification` (a test observation mentions a
    task symbol).
  * Ranking is by signal strength, then pattern id. **Frequency does not rank.**
  * Output: the pattern with its reasons, its own observations in the task, at most N supporting
    examples and N counterexamples (references with file and line, never source), every shape of the
    family with its count, caveats, coverage warnings.
  * Four statuses: `selected`, `no_applicable_pattern`, `insufficient_evidence`, `stale`. A stale
    report (any analyzed file changed, removed or added, or a different graph snapshot) returns
    nothing, with the changed files.
  * Rendering: a full form (default 4096 bytes) and a compact form for budgets of 2000 bytes or less,
    so a small budget shows every selected pattern briefly instead of the first few in full. Both say
    the block is **not a requirement**; neither contains source.
* **The harness** (`pattern_experiment.py`): corpus validation, selector evaluation, instrument
  validation, and model trials through the real `stack-agent work`.

## 2. Tested

* `chip-graph/tests/select.rs` (18) and one real-binary test: signals, supporting versus
  counterexamples kept apart, determinism and independence of input order, bounds (patterns, examples,
  bytes), stale and mismatched-graph refusal, insufficient evidence for a task that names nothing or
  only unknown things, no-applicable-pattern, a similar-looking name selecting nothing, every
  reference resolving in the report and graph, the selector changing nothing, compact rendering never
  exceeding a budget.
* **The corpus** (6 tasks, below): each task's hidden acceptance procedure **fails on the unmodified
  repository and passes on a reference solution, and the crate's own tests still pass**
  (`results/validate-corpus.json`). A task cannot edit a file larger than 32 KiB (checked).
* **The instruments** (`results/instruments.json`): the acceptance and the pattern check were run on
  the reference solutions and on deliberately wrong variants. For T3, the most-frequent
  `CapabilityProvider` shape (two methods, the text limit checked while executing) **fails acceptance
  and is classified `followed_counterexample`**; the reference passes and is `followed_relevant_pattern`.
  For T4, executing before validating fails acceptance and is `followed_counterexample`. So the pattern
  check, which is P0's own analyzer run on the result, can tell the two apart.

## 3. The corpus

Six tasks in this repository at `72d62d8`, one per required class, each with hidden acceptance tests (or
a mutation procedure for T5), the pattern observations I expected before running anything, and the known
ambiguity. Goals are in `corpus/tasks.json`.

| Task | Class | New code in | Why it is not just copy-the-neighbour |
| --- | --- | --- | --- |
| T1 | extend an existing implementation | `chip-graph/src/store.rs` | adding a variant does not change the type's pattern |
| T2 | new implementation, uniform family (`LocalWorkPolicy`, 33 impls, all `propose`) | `chip-experiments` | *is* copyable: the control |
| T3 | most frequent shape has a meaningful counterexample (`CapabilityProvider`) | `chip-experiments` | 47 impls (38 in `tests/`) do not validate inputs; 8 (6 in `src/`) do; the task needs it |
| T4 | ordering-sensitive (`validate` before `effect`) | `chip-experiments` | a request over the limit must not execute |
| T5 | tests following the repo's organization | `chip-agent-state/tests/` | acceptance includes four **mutants** the tests must kill |
| T6 | novel requirement, no pattern (`BoundedLog<T>`) | `chip-graph` | any nearby shape would be invented |

**Amended once, before any model trial.** T2 to T4 first edited `chip-core/src/lib.rs` (46.7 KB) and
`work.rs` (133 KB), which `project.write` (32 KiB per file) cannot do. No model could have done them
through `work`. I found this by running a trial, not by design; the tasks moved to `chip-experiments`
(2 KB `lib.rs`) with the same families. I had seen the selector's output for the first versions, so the
expectations are not independent of that run (`corpus/PREREGISTRATION.md`).

## 4. Measured: selector evaluation (deterministic, offline)

Run through the real CLI against a clean `72d62d8` with its report. Two runs gave byte-identical results.
"Expected" is what I pre-registered. The block is the compact form that fits the goal channel.

| Task | Status | Pre-registered pattern found | Selected | Not pre-registered | Block (bytes / ~tokens) | Goal (bytes) |
| --- | --- | --- | --- | --- | --- | --- |
| T1 | selected | yes | 4 | 3 | 1669 / 417 | 307 |
| T2 | selected | yes | 2 | 1 | 984 / 246 | 489 |
| T3 | selected | yes (both shapes) | 2 | 0 | 922 / 230 | 695 |
| T4 | selected | yes | 2 | 1 | 1119 / 279 | 595 |
| T5 | selected | yes | 2 | 0 | 1259 / 314 | 446 |
| T6 | no applicable pattern | (none expected) | 0 | 0 | 240 / 60 | 524 |

* **Recall:** every pre-registered expectation was returned (6 of 6). **Precision:** 7 of the 12
  patterns selected were pre-registered as relevant (58%). Six tasks, one author: no more than a
  description.
* **Added context:** where patterns were selected the block is 1.3 to 5.4 times the goal's size (a candidate goal reached 1978 of 2000 bytes for T1). The full rendering is 2.5 to 3.9 KB.

### Useful, misleading, and missing

* **Useful.** T3: both `CapabilityProvider` shapes side by side with their counts (47 against 8), their
  examples, and where each lives; this is the case the selector exists for. T6: it says there is no
  applicable pattern and that this is not an instruction to create one. Stale and unknown references
  are refused rather than matched.
* **Noise.** For T1 the `file` signal also selects the generic `Display` (54 impls) and `Error` (29)
  families, because `store.rs` holds one of each: true facts, not useful ones. For T2 and T4 the
  `verification` signal selects the generic async-test shape because tests merely *mention* a named
  symbol. These are the 5 unexpected selections. I did **not** change the selector to remove them: that
  would tune it on the tasks it is evaluated on.
* **Presentation can mislead.** Patterns tie-break by id, so for T3 the 47x shape is listed first. The
  selector does not rank by frequency, but a reader will tend to read first as best, and for T3 the
  first is the shape the task cannot use.
* **Missing evidence.** Nothing in a task representation says "validates its inputs", so the selector
  cannot tell T3's two shapes apart by relevance. It can only show both. The link from a requirement to
  a shape has to come from the task or the person, not from frequency.
* **Coverage.** Every selection carries "analysis coverage is partial": the repository has 143 skipped
  Rust files and 15 unexpanded item macros (P0), so an empty result is not evidence of absence.

### The six failure cases the brief names

| Case | What happened | Evidence |
| --- | --- | --- |
| Common but not appropriate | T3: the 47x shape is offered as a candidate convention beside the 8x shape the task needs; the instrument check shows following the common one fails acceptance | `selection-results.json`, `instruments.json` |
| Examples conflict | `ObservationPredicate`: two shapes (4 and 10) both returned, with counts | `failure_cases.conflicting_examples` |
| Stale report | after one file changed, `stale`, nothing offered, exit 2, the file named | `failure_cases.stale_report`, `select.rs` tests |
| Misleading name match | a nonexistent `Agent::execute_capability_limited`, whose name resembles an existing function, matches nothing: `insufficient_evidence`, listed as an unknown reference | `failure_cases.similar_name_does_not_match` |
| No relevant pattern | T6 | `no_applicable_pattern` |
| Legitimate departure | T6, and T3 (a task may need the minority shape) | rendered text states patterns are not requirements |

## 5. Measured: the model pilot (local Gemma; 12 trials)

Baseline against candidate, six tasks, one trial each, through the real `stack-agent work` with the
benchmark's `local-object` configuration (the canonical decision schema is harmful for this model,
`docs/product/agent-bench.md`). Model, task, repository, limits (16 turns, 12 executions, 300 s per
call, 1500 s deadline) and verification are identical; the **only** difference is the appended block.
Unavoidable differences from production: the model's `pax.test` is a stand-in that runs `cargo check -p
<crate> --all-targets` (the real PAX runs the whole project's tests, which here includes five tests that
fail on this host); compile errors are fed back through stderr.

**Result: 0 of 12 passed acceptance, in both conditions.** No trial regressed anything, because only one
changed a file.

| Task | Baseline | Candidate |
| --- | --- | --- |
| T1 | failed, `length_limited`, 4 calls | failed, `length_limited`, 2 calls |
| T2 | failed, `length_limited`, 5 calls | failed, `length_limited`, 8 calls; **wrote one file** |
| T3 | failed, `length_limited`, 11 calls | blocked, an invalid `project.observe` scope, 4 calls |
| T4 | blocked by the stuck guard (read/search alternating), 7 calls | blocked by the stuck guard (same read 3 times), 6 calls |
| T5 | failed, `length_limited`, 6 calls | blocked by the stuck guard (same read 3 times), 6 calls |
| T6 | failed, `length_limited`, 5 calls | failed, `length_limited`, 4 calls |

* **Why they failed:** 8 of 12 stopped at the 8192-token output limit while the model was still
  reasoning (`length_limited`); 3 were ended by the stuck guard (P7) for repeating a read or alternating;
  1 was a malformed capability request. **None of these is about patterns.** The repository's own docs
  record the same `length_limited` behaviour for this model.
* **The one file written** (T2, candidate) does not compile: it imports through the private `chip_core::work`
  module and was never declared in `lib.rs`. A model mistake that no pattern block addresses.
* **Followed the relevant pattern / followed a counterexample: unmeasurable.** 11 trials produced no code,
  and the one that did produced code the analyzer cannot reach.
* **Tokens and calls** (baseline 133,283 tokens and 38 calls, candidate 122,991 and 30) are the cost of
  failing, not of the block, and mean nothing. **Human correction: none** (the harness is autonomous).
* A second local model (`meta/muse-glimmer`) timed out at 300 s per call on T1. Two earlier dry runs of
  T1 with Gemma failed the same ways.
* **No hosted model was run.** That would send this repository's source to a third party and spend
  money; I did not have that authorization.

## 6. Recommendation

**Do not proceed to a controlled planning experiment yet. Proceed to one only after four things, in this order:**

1. **A model that completes the baseline.** With a 0% baseline there is nothing to improve. A hosted
   model is the realistic route; it needs your decision on cost and on sending source out. Gate: the
   baseline passes at least half the corpus, so a difference can show.
2. **A context channel that fits.** 1.3 to 1.7 KB in the goal is enough for 2 to 4 compact patterns and no
   more. Either accept that as the design, or give pattern context its own bounded field (a production
   change, which this PR avoided). Decide before the experiment, not during it.
3. **Fix the selector's two noise sources on a different task set.** The `file` signal selecting generic
   std-trait families, and `verification` selecting the generic test shape, account for all 5 unexpected
   selections. Changes must be validated on tasks that were not used to find them.
4. **More tasks like T3 and fewer like T2.** Only tasks where the answer differs from the most frequent
   shape can show that pattern context *changes* a decision; uniform-family tasks can be done by
   copying a neighbour. And the task must carry the requirement that distinguishes the shapes
   ("validates its inputs"), because the selector cannot infer it.

What the evidence supports today is narrow: **a deterministic, read-only, explainable mechanism that
refuses stale evidence, says "no applicable pattern" and "insufficient evidence", and is correct on its
instruments.** It does not support "pattern context improves implementation".

## 7. Unresolved

* Whether pattern context improves outcomes: unmeasured.
* The corpus is six tasks written by one person, amended once after seeing the selector's output on its
  first version; the model pilot is one trial per cell on one model that fails everything.
* Task representations are written by hand. A real planner would have to produce them, and nothing here
  shows it can do so reliably.
* `T5`'s pre-registered expectation ("any test pattern selected through a verification signal") is weak,
  and the existing tests that mention `intent_key` are mostly async FeltDB tests, the wrong model for
  tests of a synchronous function.
* Pattern identity and caveats are P0's: names not types, an untuned call vocabulary, a method set that a
  trait may require.
* Not done: hosted-model trials, a second local model beyond one dry run, repeated trials, selector
  changes, a larger channel, any use of pattern context in `work`.
