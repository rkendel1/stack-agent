# The results contract

## Report (`work --json`)

`docs/schemas/work-report.v1.schema.json` (`schema: "stack-agent.work-report.v1"`). Fields listed there are
stable within version 1: not removed, renamed or reinterpreted; other fields may appear and are not part of
the contract. `terminal_state`, `verified`, `grounded`, `goal_satisfied` and `exit_status` are the runtime's
evaluation of authoritative observations; `outcome_reason` and `answer` are text and carry the model's
words. A host must not read success from `answer`.

## Events (`work --events`, `serve` SSE)

`docs/schemas/work-event.v1.schema.json` (`schema: "chip.work-event.v1"`): one object per event, with the
binary's `build`, the `work_id`, a gapless `seq` and `event.kind`. Kinds are the runtime's own events; model
prose is never one. `at_ms` is in `--events` lines only.

## Build identity

Every `--json` report carries `build` (`version`, `commit`, `tree` clean/dirty, `target`, `profile`,
`executable_sha256`); every event line and every `/v1/work` answer carries `build` as
`stack-agent VERSION+COMMIT` (`.dirty` when the tree had uncommitted changes). The commit is `unknown` for a
build outside a git checkout.

## Exit codes (stable)

| Code | Meaning |
|---|---|
| 0 | verified: an independent predicate over recorded observations established the outcome |
| 1 | not verified: the work ran and ended without that (blocked, limit, escalated, failed, cancelled, or completed without verification) |
| 2 | usage: nothing was run |
| 3 | unavailable: a required model, tool or file was not usable; nothing was run |
| 4 | runtime failure |
| 130 | a second interrupt: the process was ended without waiting |

Tested: `tests/report_contract.rs` checks a real report and event stream against the schemas and pins the
codes. Not published as a machine-readable schema: the `serve` HTTP responses other than events.
