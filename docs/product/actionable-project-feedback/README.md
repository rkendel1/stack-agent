# Actionable project feedback

Runtime feedback, not authority. The benchmark in `../coding-agent-benchmark-qwen36-omlx/` showed a model repeating a
nonexistent path after being told `not_found`, and a whole-project search that silently stopped at its 500-file cap.
Everything the runtime needed to help was already known to it. This change makes that knowledge visible to the model.

## What changed

| Where | Before | After |
| --- | --- | --- |
| `project.read` / `list` / `search` on a path that does not exist | `{"error":"not_found","path":"src"}` | also `nearest_existing_directory`, `missing_component`, `entries` (the nearest directory's entries, bounded to 40, exactly as `project.list` shows them), `entries_total`, `similar` (names resembling the missing one) and a one-sentence `hint` |
| `project.read` of a directory, `project.list` of a file | bare `not_a_file` / `not_a_directory` | the same plus a `hint` naming the capability to use |
| `project.search` | `truncated: true` for three different causes | `exhaustive`, `stopped_by` (`file_limit`, `match_limit`, `output_limit`), `file_limit`, `match_limit`; when it stopped, `searched_fully`, `stopped_inside`, `not_reached` (the entries of the directory searched); `excluded_by_rule`; a `hint` that says whether a missing match proves anything |
| `project.search` with matches | rows `path:line: text` | also `files_with_matches` and `read_hints`: per file its `matches`, `first_line` and a `read` object that is the exact `project.read` input; for a file too large for one read, an `offset` at the first matching line and a `length` of whole lines |
| The same failure repeats | one `its execution failed` line per failure in the context's "Ruled out" list | one entry per request that says how many times it failed with the identical result, what the observation said, that sending it again cannot change it, and to change the request or use another capability |
| The stuck guard stops the work | the reason said nothing was learned | the reason adds a `Recovery:` sentence (see `../stuck-guard.md`) |

All head fields are additive; no existing field changed. Existing `chip-project`, `chip-core` and `stack-agent` tests pass unmodified.

## What did not change (AGENTS.md section 22)

* **Authority.** No capability, input, permission or path rule changed. A hint is derived from the same `visible_entries`
  that `project.list` uses, so symlinks, reserved names (`.env`, `.git`, credential stores) and unaddressable names are
  excluded exactly as they are there. `project.list` is offered wherever `project.read` and `project.search` are
  (`INSPECT_CAPABILITIES`, the change and verify sets), so a hint exposes nothing the model could not already list.
  A refused path (absolute, `..`, reserved, through a symlink) is refused as before and gets no hint (tested).
* **No hidden retry or fallback.** Nothing is re-executed, repaired or substituted. A hint is text in an observation.
* **The stuck guard.** Thresholds (3 and 4), comparison (same request, same observation) and its stop are unchanged.
  The repeat note is added to the model's context only; the recorded observations of identical requests stay
  byte-identical, which is what the guard compares. Hints are deterministic for a given tree, so identical requests still
  get identical observations (tested). The guard still fires at the third identical failure with no fourth execution or
  model call (tested, including a script that would ask 50 times).
* **Durable record.** The stuck reason quotes no output and no input.

Falsifiable: the hypothesis is that actionable feedback turns the observed failed-path loop into progress. It would be
falsified by no reduction in failed observations or reads in the live comparison below.

## Evidence

* `chip-project/tests/feedback.rs` (12): the hint equals the listing, bounded, deterministic, refusals unchanged, truncation
  and exhaustiveness reported, **and every `read_hints` entry is a `project.read` that succeeds when executed**, including
  the offset range for a file over 32 KiB.
* `chip-core/tests/failure_feedback.rs` (6): first failure noted as before, the repeat explained, one note per request,
  recorded observations identical, guard unchanged, no fourth execution.
* Mutation checks: disabling the repeat note fails 2 of those tests; corrupting `entries_total` fails 2 of the others.
* Regression: `cargo test -p chip-project -p chip-core -p chip-pax -p stack-agent`: 418 passed, 2 failed. Both failures are
  `chip-pax/tests/pax_boundary.rs` (executing a file held open for writing) and fail identically on unmodified `main` (which
  also failed a third, flaky test in that file that passed in my run). Not caused by this change.
* clippy: no new warnings in changed code (the crates' existing style backlog, D-31, is untouched).

## Live comparison (`results/ab-summary.json`)

The frozen task of `../coding-agent-benchmark-qwen36-omlx/` (Qwen3.6-35B-A3B-4bit on oMLX, `--structured-output object`,
same limits), 10 runs per arm, alternating the pre-change and post-change binary, each run from a fresh checkout.

| | Before | After |
| --- | --- | --- |
| Failed observations (total, 10 runs) | 44 (every run hit a nonexistent path) | 3 (1 run) |
| Runs that narrowed into `crates/` | 2 | 10 |
| Runs that reached a defect crate | 0 | 3 |
| Runs with at least one `project.read` | 0 | 3 (all three read `chip-fx-native` or `chip-fx`, neighbours of the defect, not the defect files) |
| Files written / task verified / acceptance passed | 0 / 0 / 0 | 0 / 0 / 0 |
| Ended by the stuck guard | 10 | 10 |

Reading it: the loop on nonexistent paths is gone and the model now follows the narrowing guidance, a real change. The
**task is still not completed**: all 20 runs end stuck, and the evidence is n=10 on one task against one model.

## Remaining failure (observed, not fixed here)

The post-change runs stop on three identical *successful* searches: after narrowing to `crates/fx-native-provider` the
model re-sends `cache_creation_input_tokens` although the previous result says exhaustively that no file contains it.
The note added here covers repeated failures only; an identical successful repeat gets no explanation (the observation's
own `hint` was already there and was not acted on). That is the next, separate experiment: extend the repeat note to an
identical successful repeat and measure it on this same task. Not done here because it would be a second change.
