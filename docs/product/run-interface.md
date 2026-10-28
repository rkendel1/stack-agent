# `work` run interface

| Need | Flag | Notes |
|---|---|---|
| Goal from a file / stdin | `--goal-file FILE`, `--goal-stdin` | One source only. Read bounded; UTF-8 text. |
| Longer goals | `--max-goal-bytes N` | Default 2000, at most 32768. `serve` keeps 2000. |
| Another directory | `--project DIR` | Replaces the current directory as the work's root. |
| Token budget | `--max-total-tokens N` | Provider-reported input + output tokens. Checked before every model call and execution. |
| Cost cap | `--max-cost-usd X --input-price-per-mtok A --output-price-per-mtok B` | You supply the prices; Chip knows none. |
| What ended the run | report `ended_by` | `turns`, `executions`, `context`, `deadline`, `token_budget`, `cancelled`, or `null` (it ended itself). |
| Slow local model | note on stderr | A loopback endpoint with the default call timeout gets a one-line note. Nothing is changed for you. |
| Stuck / rejected-reply bounds | `--stuck-repeats`, `--stuck-alternations`, `--max-rejected-replies` | Report `tuning` shows any non-default. |
| Test run | `--test-timeout-secs N`, `--test-arg ARG` (repeatable) | Operator-only; report `pax_test` says `whole_suite: false` for a narrowed run. |

Stated plainly:

- A provider that reports no usage adds nothing to the token budget; the report counts those calls in
  `budget.calls_without_reported_usage`, so the bound is never presented as tighter than the data allows.
  (Item 13, local estimation, is separate.)
- The write cap (32 KiB), the reply cap (4096 bytes) and the summary cap (1024 bytes) are part of the decision
  contract and the capability declarations; they are **not** made configurable here.
- A budget stop is `terminal_state: cancelled` with `ended_by: token_budget`; `verified` stays false unless an
  independent predicate said otherwise earlier.

## Observability and files (items 12-14)

| Need | Flag | Notes |
|---|---|---|
| Replies as text | `--record-replies FILE` (+ `--redact-env NAME`) | JSONL `chip.reply.v1`, owner-only, redaction is **best effort** and every line says so. The `--json` report itself is **not** redacted: it carries the model's own `reason`/`summary` text. |
| Usage when the server reports none | `--estimate-usage` | One token per four bytes, reported as `usage_estimate` with `estimated: true`, counted toward `--max-total-tokens`. The provider's `usage` stays `null`; a call that reported usage is never estimated. Not a tokenizer. |
| No files in the checkout | `--no-project-files` | No `.stack-agent/` learning or project identity; learning stays in the runtime data directory. |
| Live events | `--events FILE` | See `events-stream.md`. |
