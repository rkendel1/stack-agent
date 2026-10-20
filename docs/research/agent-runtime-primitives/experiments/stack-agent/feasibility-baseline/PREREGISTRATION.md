# Preregistration: Stack Agent baseline feasibility ladder

Committed before any trial. Deviations are listed in the report, not edited here.

## 1. Hypotheses (descriptive, not directional claims about patterns)

H-F1 the model can discover and validly use an offered capability for a trivial task.
H-F2 it can complete read-only inspection with a correct, fixture-checked answer.
H-F3 it can make a one-file exact source edit within the write limit.
H-F4 it can make an edit verified by a hidden compile/test acceptance.
H-F5 it can complete a bounded multi-step change (read test, read source, edit, run tests).

Each is falsified by the trial counts in §5. Nothing here tests pattern context.

## 2. Fixture

`fixture/` is a standalone Rust crate `tally` (a `[workspace]` table keeps it out of the
repo workspace). Every source file is under 500 bytes (write limit 32 KiB). Baseline
tests (4) pass. Each trial runs in a fresh disposable copy with its own `git init` and
base commit. Acceptance tests and reference solutions are **never** in the trial copy
unless listed as "visible".

| Task | Kind | Goal text (exact) | Precondition | Acceptance (deterministic, no LLM judge) |
|---|---|---|---|---|
| F1 | inspect | `List the files in the src directory of this project.` | fixture | `Completed`, and at least one successful `project.list` request with path `src` or successful `project.read` of a `src/` file. (Discovery = valid, executed, relevant request.) Answer content not judged. |
| F2 | inspect | `Which function in src/math.rs multiplies its argument by three? Answer with the function name, citing the file you read.` | fixture | `Completed`, summary contains `triple` and `src/math.rs`; no file changed. |
| F3 | change | `In src/greeting.rs change the GREETING constant from "hello" to "welcome". Change nothing else.` | fixture | tree diff against base equals `reference/f3/greeting.rs` for `src/greeting.rs` exactly, and no other file differs; `cargo test` passes. |
| F4 | change | `In src/math.rs add a public function double that takes a u64 and returns it multiplied by two as a u64. Do not change add or triple.` | fixture | after copying hidden `acceptance/f4.rs` to `tests/f4.rs`: `cargo test` passes; only `src/math.rs` differs; `add`/`triple` text unchanged. Unmodified fixture fails (compile error); reference passes (`validate_fixture.py`). |
| F5 | change | `The tests in tests/percent.rs describe the intended behaviour of clamp_percent in src/percent.rs. Make the project's tests pass without editing any test file.` | fixture + visible `overlays/f5/tests/percent.rs` (2 tests, 1 failing) | after adding hidden `acceptance/f5.rs`: `cargo test` passes; only `src/percent.rs` differs; `tests/percent.rs` byte-identical. Unmodified fails; reference passes. |

Reference solutions: `reference/f3`, `f4`, `f5`. `validate_fixture.py` (run and passing
before this commit) proves each acceptance discriminates.
All six checks passed: F3 baseline holds with the reference; F4 and F5 fail unmodified and pass with the reference.
Expected interactions: F1 list/read; F2 list, read; F3 list, read, write, (pax.test);
F4 list, read, write, pax.test; F5 list, read x2, write, pax.test.

Note on F4: `u64` is in the goal because the hidden test needs a width the goal must
state; the acceptance asserts a value above `u32::MAX`.

## 3. Configuration (identical across trials; flags only)

Model `google/gemma-4-26b-a4b-qat` at `http://localhost:1235` through the logging proxy;
`--provider openai-compatible --provider-profile local --structured-output object
--call-timeout-secs 240 --deadline-secs 900 --json --print-reply`; default
`--max-turns 12 --max-executions 8`; temperature default (0.0). No hosted model, no
network beyond localhost, no inference cost.

Two cells differing in exactly one setting, `--max-output-tokens`:
- **Cell A**: 2048, the product default.
- **Cell B**: 8192, the documented local-bench value (`docs/product/agent-bench.md`).
Neither changes any default or code; B only uses an existing flag. If B passes where
A fails, the report says the task needs a non-default limit, not that the interface
is feasible by default.

Stuck guard, validation and file-size limits: unchanged and not configurable here.

## 4. Order, repetition, stop rules

Order (fixed): for each cell, F1 x3, F2 x3; then (only if the gate in §5 allows)
F3 x3, F4 x3, F5 x3. Within a cell tasks run in this order, repetitions sequentially.
Cell A runs before cell B. No trial is dropped or rerun for any reason except a
harness crash with no model call (reported as `harness_error` and rerun once; the
count of reruns is reported).

- **Stop rule 1**: if a cell has 0 accepted of 3 on F1 *or* 0 of 3 on F2, stop that
  cell. No implementation task runs in that cell.
- **Stop rule 2**: stop a cell's ladder at the first task with 0/3 accepted.
- **Repeatable** = at least 2 of 3 accepted. A task passes the gate at >= 2/3.
- The prompt, fixture, goals, flags and acceptance are not changed after the first
  trial. If a trial reveals a harness or acceptance bug, the report records it and
  the affected trials are reported under both the buggy and fixed classification.

## 5. Decision gate (copied from the brief; applied mechanically per cell)

F1 or F2 fails -> investigate model-to-capability interaction. F1+F2 pass, F3 fails
-> source-edit behaviour. F3 passes, F4 fails -> implementation quality/verification.
F1-F4 pass, F5 fails -> multi-step coordination. Ladder passes -> smallest successful
task class is the baseline. No pattern-utility work proceeds until a task is
repeatable (>= 2/3) through the real interface.

## 6. Trace and failure categories

One JSON per trial: task, fixture hash, base commit, model/config, trial id, cell,
per-call proxy records (tokens, finish_reason, seconds), capability requests
(capability, executed, valid), file ops (reads/lists/writes counts, paths written),
output-limit events, stuck-guard events (`Blocked` reason `stuck: `), compile/test
(pax executions, final cargo test result), acceptance result, final category,
elapsed seconds, model calls, tokens. No secrets, no environment, no source text
beyond fixture paths and the model's own reply.

Exactly one final category, first match in this order:
1. `accepted` - acceptance passes (runtime terminal state is recorded separately).
2. `stuck_guard_terminated` - terminal reason begins `stuck: `.
3. `length_limited` - `model_failures` has `length_limited`.
4. `invalid_capability_request` - `invalid_decisions>0` or `invalid_inputs>0` ended the work
   (terminal reason names it).
5. `read_failure` - last failed observation was `project.read`/`project.list`.
6. `write_failure` - last failed observation was `project.write`.
7. `capability_discovery_failure` - no capability request was made and none was invalid, and no
   earlier category applies (the model never used an available capability).
8. `compile_or_test_failure` - change task, files written, final `cargo test` of the trial copy fails.
9. `implementation_incomplete` - change task ended without the required write, or inspect
   ended without an answer, no earlier category applies.
10. `acceptance_failure` - runtime reported completion but the fixture acceptance fails.
11. `unknown` - nothing above applies (reported, never merged into another).

Timeouts (`--call-timeout-secs`, `--deadline-secs`) have no category of their own in
the brief; they are mapped by the terminal reason into `unknown` and counted in a
separate `timeout` flag so they are visible.

## 7. Reported per cell

Per task: accepted/n, category counts, median/maximum seconds, model calls, tokens,
and for each failure the evidence-supported conclusion from the list in DESIGN §4.
Implemented / tested / measured / unresolved are listed separately.
