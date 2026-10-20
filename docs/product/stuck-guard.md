# Stuck guard

`stack-agent work` ends a work that is repeating itself without learning anything, instead of
leaving it to run into `max_turns` or `max_executions`. It is a predicate over the trajectory the
work loop already keeps. It asks no model, executes nothing and stores nothing.

Design, measurements and limits: [the research report](../research/agent-runtime-primitives/experiments/stack-agent/p7-stuck-guard/README.md).

## What it detects

A *pair* is a request and what it observed. Two pairs are equal when

* the requests are the same capability with the same complete typed inputs (compared as Chip
  recorded them, never as a model worded them), **and**
* the observations are the same reality: the same kind, status and output.

Execution ids, receipts and runtime evidence differ by construction and are not compared. Only the
end of the trajectory counts, and only consecutive pairs.

| Pattern | Fires when | Default |
| --- | --- | --- |
| Repeated | the last 3 pairs are all equal | 3 |
| Alternating | the last 4 pairs are `X Y X Y`, with `X` and `Y` different | 4 |

Both are unproductive for the same reason: the request was answered the same way again, so nothing
the model could do with the answer is new. Different inputs, a different capability in between, or
a changed observation (a read after a write, a test that now passes) is not a repeat.

The guard reads at most the last 4 pairs, however long the trajectory is. One evaluation measured
about 30 ns at every length from 4 to 100,000 pairs.

**Not implemented: repeated context-window failures.** FX has no structured context-window error
kind, so a provider's context-length error is text, and recognising it would be the vague text
matching this guard does not do. Chip's own byte budget (`ContextLimit`) is already terminal at its
first occurrence and is never retried, so it cannot repeat inside a run.

## What the caller sees

The work ends `Blocked` with a reason that begins `stuck: `, for example

```text
stuck: project.read was requested 3 times in a row with the same inputs and returned the same
observation each time. Nothing new was learned, so the work stopped here: no further capability was
executed and the model was not asked. Everything recorded before it is kept.
```

In `work --json`: `terminal_state: "blocked"`, `outcome_reason: "stuck: …"`, exit status 1. There is
no separate terminal state. The reason prefix (`chip_core::STUCK_REASON_PREFIX`) is how a caller
tells this stop from any other `Blocked`. Every observation recorded before it stays in the report
and, with `--state-db`, in FeltDB.

## How it relates to the other bounds

* **Cancellation and deadlines come first.** The guard is evaluated after the stop check at the top
  of each iteration. A work that is stuck and told to stop ends `Cancelled`.
* **`max_turns` and `max_executions` still apply.** The guard is evaluated before the turn limit
  and names a loop when both would apply at once. A run that hits a limit without a repeating end
  is reported as the limit.
* **Evidence reuse** is unchanged. It suppresses re-execution of a reusable capability; a model that
  keeps re-asking for one is still a repeated pair, because the reused observation is recorded with
  its origin like any other.
* **Durable runs.** A stuck work is recorded `blocked`. A fresh process that resumes it restores
  the same trajectory and ends with the same reason, with no model call and no execution (the
  restored end already repeats), so a resume does not retry a loop. With P4, a request interrupted
  inside the repeating read is performed again once, and the guard then sees it.
* **No retry, no repair.** The guard never prompts the model, changes a request or tries another
  capability.

## Configuration

`WorkSpec::stuck_guard` (`None` by default, so chip-core callers are unchanged) holds the
`StuckThresholds`. `stack-agent work` sets the defaults above. There is no flag to turn it off.

## Commands

```sh
cargo test -p chip-core --test stuck_guard
cargo test -p chip-agent-state --test stuck_guard
cargo test -p stack-agent --test stuck_guard
cargo test --release -p chip-core --test stuck_guard_overhead -- --ignored --nocapture
python3 -I docs/research/agent-runtime-primitives/experiments/stack-agent/p7-stuck-guard/stuck_experiment.py \
    BASELINE_BIN CANDIDATE_BIN WORK_DIR results.json
```
