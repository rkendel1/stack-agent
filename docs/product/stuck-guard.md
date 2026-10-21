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

The reason then adds a `Recovery:` sentence that says only what the trajectory establishes: whether the repeated
requests all failed (the same request cannot succeed; a different request is needed) or all succeeded (asking again
cannot add anything), and that the stop retried, changed and asked nothing. It quotes no output text and no request
input, because the reason is recorded durably and observations are kept there only as sizes and hashes. The thresholds
and the comparison are unchanged. Before the guard fires, a repeated identical *failure* is explained to the model
(see [actionable-project-feedback](actionable-project-feedback/README.md)); that note is context only and never alters
a recorded observation.

In `work --json`: `terminal_state: "blocked"`, `outcome_reason: "stuck: …"`, exit status 1. There is
no separate terminal state. The reason prefix (`chip_core::STUCK_REASON_PREFIX`) is how a caller
tells this stop from any other `Blocked`. Every observation recorded before it stays in the report
and, with `--state-db`, in FeltDB.

## When the work is already complete

What establishes that a work is complete: authoritative observations that satisfy the goal's
requirements (`required_outputs`, `required_observations`), each decided by a predicate over the
recorded observations, never by a model's words; **and** a completion decision. Chip's own local
policy makes that decision for `change` and `verify` work the moment the predicate holds. For
`inspect` work only the model's accepted, grounded answer does.

The guard never completes a work and never refuses one that Chip can complete itself:

* **Chip can complete it now.** If the local policy would complete the work this iteration and the
  goal and answer rules accept it, the guard does not fire; the work completes. This holds when the
  repetition is what satisfied the goal (a goal such as "the reading is stable: three identical
  reads") and for a trajectory restored from FeltDB that is both a repetition and already satisfied.
  Before this rule the guard was evaluated first, and returned such a work `Blocked`.
* **Only the model can complete it** (`inspect`, or any work with no local policy), and it keeps
  repeating instead. The guard stops it, `Blocked`. The goal's observations being met is no
  completion: it needs a decision, and none was made. The reason says so ("The goal's required
  observations were already satisfied, but …"), and the `GoalEvaluated` events are kept. This is a
  contract gap, not a decision to downgrade: there is no outcome for "satisfied, undecided".
  Not firing here instead would disable the guard for every inspection, since `inspect`'s
  observation requirement is met by the first successful read-only observation.
* **The goal is not met.** The guard fires as before, and the reason says nothing was learned.

## Goal met is not completed, and a restart does not change what is said about it

A goal being met is reported separately from a work being completed. `work --json` reports
`goal_satisfied` (`true`, `false`, or `null` when nothing was evaluated or the work has no
requirement) next to `terminal_state`, and the two are independent: a work can be `blocked`,
`limit_reached` or `cancelled` with `goal_satisfied: true`. `Completed` still comes only from an
accepted completion decision.

`goal_satisfied` is derived from the evaluation of the recorded observations, so a resumed work
reports what the uninterrupted one would. (Before P9 a resume that restored all its observations
reported `null`, in the same report whose reason said the goal had been met.) The goal's status is
not stored as a field in FeltDB: it is re-derived from the durable observations, by the same
predicates, on every attempt. The only durable statement of it is text, in `WorkResult.detail` for
the stuck stop.

## How it relates to the other bounds

* **Cancellation and deadlines come first.** The guard is evaluated after the stop check at the top
  of each iteration. A work that is stuck, or satisfied, and told to stop ends `Cancelled`.
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

## False positives

The thresholds (3 and 4) are hypotheses. What is and is not known about them is in
[the validation report](../research/agent-runtime-primitives/experiments/stack-agent/p8-stuck-guard-validation/README.md):

* **Recorded real-model trials (243):** none can be shown to have triggered the guard, none can be
  shown to be a genuine loop, and 52 cannot be classified because the records hold no inputs or
  observations. The real-model false-positive rate is **unmeasured**.
* **Synthetic boundary corpus (25):** the guard does what it is specified to do. Two kinds of
  legitimate work are stopped, by construction: *unchanged polling that would have changed on the
  next poll*, and *monitoring that is unchanged for two cycles*. The guard compares what was
  observed and cannot know the next observation.

## Configuration

`WorkSpec::stuck_guard` (`None` by default, so chip-core callers are unchanged) holds the
`StuckThresholds`. `stack-agent work` sets the defaults above. There is no flag to turn it off.

## Commands

```sh
cargo test -p chip-core --test stuck_guard
cargo test -p chip-core --test stuck_guard_replay -- --nocapture
python3 -I docs/research/agent-runtime-primitives/experiments/stack-agent/p8-stuck-guard-validation/replay_agent_bench.py
cargo test -p chip-agent-state --test stuck_guard
cargo test -p stack-agent --test stuck_guard
cargo test --release -p chip-core --test stuck_guard_overhead -- --ignored --nocapture
python3 -I docs/research/agent-runtime-primitives/experiments/stack-agent/p7-stuck-guard/stuck_experiment.py \
    BASELINE_BIN CANDIDATE_BIN WORK_DIR results.json
```
