# Stack Agent baseline feasibility: design and execution-path audit

Status: written and committed **before any trial**. The audit smoke run in §3 is an
audit observation (one run, one task), not a trial; it is not counted in any result.

Question: can Stack Agent's existing `work` interface, unmodified and without pattern
context, complete small bounded repository changes with the local model, and when it
cannot, which layer is responsible?

Nothing in this experiment changes Stack Agent. Every setting used is an existing,
documented command-line flag. The pattern-utility P1 artifacts
(`../pattern-utility-p1/`) are untouched.

## 1. Execution path (source references, at origin/main 7a9b640)

| Step | Where | Fact |
|---|---|---|
| Goal accepted | `crates/stack-agent/src/software_work.rs:48,2458` | goal is plain text, at most 2000 bytes, else exit before any model call |
| Goal wrapped | `software_work.rs:61-66,724` (`goal_text_for[_contract]`) | the model's first message is the goal plus fixed instructions for the kind (inspect/change/verify): capability names, "emit exactly one JSON object", the four decision shapes |
| Capability discovery | `crates/chip-project/src/lib.rs:440-470`; `crates/chip-core/src/model_decision.rs:644` (`question`) | the question text lists every offered capability with its declared inputs and description. The model is *told* the capabilities in-prompt; no tools parameter is sent (proxy: `tools: 0`) |
| Response -> request | `model_decision.rs` (`ModelDecisionBoundary`, `chip.work-decision.v1`) | strict parse; unknown fields, undeclared inputs, `"inputs": {}` for a no-input capability are rejected (Invariants 3, 6) |
| Execution | `crates/chip-core/src/work.rs:2218` (`perform`) | only Chip constructs the invocation; PAX/FX executor runs it |
| Read bound | `chip-project/src/lib.rs:70,356-382` | `project.read` at most 32 KiB per call (`MAX_READ_BYTES`) |
| Write bound | `chip-project/src/lib.rs:71,413` | `project.write` input at most 32 KiB (`MAX_WRITE_BYTES`) |
| Output limit | `software_work.rs:795,1151,2325`; `crates/fx-core/src/normalize.rs:563-575` | `--max-output-tokens` (default 2048, 1..131072) is sent as `max_tokens`; `finish_reason: length` becomes the typed failure `length_limited`, never a decision; no retry |
| Stuck guard | `work.rs` (`detect_stuck`), `crates/chip-core/src/stuck.rs:72`; enabled `software_work.rs:1160-1163` | work ends `Blocked` with reason beginning `stuck: ` when its own trajectory repeats without learning |
| Compile/test | `crates/chip-pax/src/lib.rs:596,813` | `pax.test` runs `cargo test` through real PAX; stdout JSON `pax.execution-result.v1`; failures return as observations |
| Inspect acceptance | `software_work.rs:569-585,203` (`GroundedAnswer`) | an inspect answer completes only if it cites a path observed by a successful project operation. It does **not** judge truth |
| Failure recording | `software_work.rs:1589-1617` | JSON report fields `outcome_reason`, `model_failures[]{kind,delivery}`, `invalid_decisions`, `invalid_inputs`, `executions_by_capability`, `terminal_state`, `goal_satisfied`, `grounded`, `writes`, `tests`, `measurement` |
| Optional persistence | `--state-db/--task-id` | FeltDB agent state; not needed for this experiment |

Settings that are configurable **without** semantic change (used as-is): `--provider
openai-compatible --provider-profile local --structured-output object --endpoint
--max-output-tokens --call-timeout-secs --deadline-secs --json --print-reply`.
`local` + `object` is the documented local cell (`docs/product/agent-bench.md`); the
canonical `schema` mode is known to be harmful for local Gemma.
Settings **not** touched: stuck guard (no flag exists and none is added), validation,
`MAX_READ_BYTES`/`MAX_WRITE_BYTES`, default turn/execution limits (12/8).

Unit note: `--provider openai` rejects the `local` profile; the local endpoint requires
`--provider openai-compatible`.

## 2. Instrumentation (no execution-behavior change)

- `logging_proxy.py`: a local stdlib pass-through between Stack Agent and the model
  server. It forwards bytes unmodified and records, per exchange: model, stream flag,
  `max_tokens`, `response_format` type, tool count, per-message role/char count/hash,
  `finish_reason`, usage (prompt/completion/reasoning tokens), content vs reasoning
  chars, chunk count, wall seconds. It never sends anything off the machine (upstream
  is `localhost`). Without `--full` it stores no message text (hash and size only).
- `stack-agent work --json --print-reply`: the existing report plus each raw model reply.
- `harness.py`: copies the fixture to a disposable directory, runs the trial, applies
  the deterministic acceptance, classifies, and writes one JSON trace per trial.

Because Stack Agent already emits `model_failures[].kind=length_limited`, the
terminal reason, per-call token usage and capability counters, no production
instrumentation is added.

## 3. Audit smoke (not a trial)

One `inspect` run on the fixture, via the proxy with full logging, model
`google/gemma-4-26b-a4b-qat`, `--max-output-tokens 8192`:

- Call 1 (62 s, 2103 prompt tokens, 25 completion): `project.list` of `.` — valid.
- Call 2 (2.5 s): `project.read` of `src/math.rs` — valid.
- Call 3 (136 s): began `{"decision": "complete", "summary": "In src/math.rs
  (Observation 2), the function triple(n: u32) -> u32 is defined as 3 * n, ...` and
  continued to 8191 completion tokens (39,055 content characters, 0 reasoning
  tokens, `finish_reason: length`). Chip recorded `length_limited`, terminal
  `failed`, exit 4. `goal_satisfied` was true at that point: the observations
  contained the answer.

What the model is shown (call 1, 10,293 bytes): the goal; fixed inspect instructions;
state `turn 1 of 12; executions 0 of 8`; an "inspection progress" block; a
manifest-first hint to `project.list "."`; and the capability list with declared
inputs (project.list/search/read, project.git.*, project.observe; `project.write` is
omitted for `inspect`, which the prompt states: "do not modify any file").

Audit conclusions (single observation; the experiment measures how general they are):
1. Capability discovery is **feasible through the interface**: the capabilities and
   their input shapes are in the prompt, and the model used two of them correctly.
2. The first-run failure was not discovery, not an invalid request, and not the stuck
   guard. It was the completion reply running to the output limit *after* a correct
   start, in the free-text `summary` field.
3. This is one sample at one limit; it does not establish that completion always
   runs away, or that a larger/smaller limit would change it.

## 4. Controls and what each failure can mean

Each non-accepted trial maps to exactly one category (see PREREGISTRATION §6) and the
evidence-supported conclusion in the report is limited to one of:
*not feasible through the interface*; *model failed to use an available capability*;
*guard or limit terminated the work*; *invalid capability request*;
*produced a change that failed verification*; *unresolved*.

A confound the design records rather than hides: the local model is a slow reasoning
model; call latency (2.5 s to 136 s here) is dominated by generation, so timeouts
(`--call-timeout-secs`) are a separate cause from output-limit termination and are
reported as such.
