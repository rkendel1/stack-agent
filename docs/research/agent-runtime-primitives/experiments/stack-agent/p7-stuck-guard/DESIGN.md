# Stuck guard: design, written before the implementation

Status: proposal (Phase 1). It is superseded by the measured report in `README.md` where they differ, and by P8 (`../p8-stuck-guard-validation/README.md`) on the order of the guard and completion: the guard yields to a completion Chip's local policy can already justify.
Base: `main` at `db6f0f7` (P1 to P4 merged).

## What the code gives us

* **The trajectory already holds both halves of a pair.** `Run::observations` and `Run::origins` are
  parallel vectors pushed together by `perform` (an execution), `reuse` (reused evidence) and
  `restore` (observations recorded by earlier attempts, P1, and P4's fresh re-observation).
  `ObservationOrigin::invocation` is the capability and its complete inputs, assigned by Chip and
  never read from a model. `same_reality` already says what "the same observation" means: kind,
  status and output, never ids or receipts.
  No new representation is needed, and nothing needs to be stored.
* **The loop head is the insertion point.** In `Agent::drive` the order is: stop request, turn limit,
  decide, execute. A guard between the stop check and the turn limit runs after every observation
  (the previous iteration's) and before any model call or execution. It also runs first on a
  resumed attempt, so it sees restored history before anything happens.
* **Terminal outcome.** `WorkOutcome::Blocked { reason }` already means "the work stops and a person
  must decide", it is what `completion refused` uses, and the durable layer persists it as
  `blocked`, which a later `begin` can resume. Contract gap: there is no distinct terminal state
  for "stuck". The discriminator is the reason, which begins `stuck: `. A new `WorkEvent` is not
  added: `WorkBlocked { reason }` already encodes it (AGENTS.md section 19). A caller that needs
  to count stuck stops matches the prefix.
* **Limits.** `stack-agent work` defaults to 12 turns and 8 executions (`DEFAULT_MAX_TURNS`,
  `DEFAULT_MAX_EXECUTIONS`); `--max-turns` and `--max-executions` change them, and `--auto` selects
  its own. (chip-core's `WorkLimits::default()` is 8 and 4, but the product does not use it.) At
  the product defaults an identical request can run 8 times before `LimitReached(executions)`,
  after a ninth model call. The guard's saving is therefore the difference between the loop's
  length and the threshold, and grows with the limits.
* **Evidence reuse** (`chip-core/src/evidence.rs`) suppresses re-execution of a *reusable*
  capability, and every product capability declares `reuse_evidence = false`, so it never applies
  to them. A reused observation is still pushed with its origin, so a model re-asking for a reusable
  capability forever is still a repeated pair.
* **Cancellation and deadlines** are asked at the top of each iteration (`Run::stopped`) and before
  each execution. The guard is synchronous and pure, runs after `stopped`, and cannot delay it.

## Patterns, thresholds, evidence, outcome

A *pair* is `(origins[i].invocation, (observations[i].kind, .status, .output))`. Two pairs are equal
when the invocation strings are equal (same capability, same complete typed inputs) and the
observations are the same reality. Execution ids, receipts and evidence are not compared.

| | Pattern | Fires when | Initial threshold | Why |
| --- | --- | --- | --- | --- |
| A | Repeated request and observation | the last `repeated` pairs are all equal | 3 | The third identical answer to an identical question taught nothing. |
| B | Alternating | the last `alternating` pairs are `X Y X Y ...` with `X != Y`, every pair equal to the one two back | 4 (two cycles) | After two full cycles with the same answers the world is not moving. |
| C | Context-window failures | see below | n/a | |

Thresholds are *starting hypotheses*. OpenHands uses 4 and 6 over at most 20 events. 3 and 4 are
chosen here as the smallest counts at which the repetition is unambiguous: three identical answers
to an identical question, and two full cycles of two different questions. They are not derived from
stack-agent's limits and they are not tuned on a corpus: no real-model trajectories were examined,
so the false-positive rate on real work is **unmeasured**. Both are fields of `StuckThresholds`,
tested at the boundary, and reported with the experiment.

**Bounded history.** Only the last `max(repeated, alternating)` pairs are read (4 at the defaults).
The cost does not grow with the trajectory.

**Outcome.** `WorkOutcome::Blocked { reason: "stuck: ..." }`, ended through the ordinary terminal
path. The guard does not execute anything, does not call the model, does not touch the recorded
observations, and runs once per loop iteration.

**False-positive protections.** Consecutive only (any different pair breaks a run). Full typed inputs
are compared, so `project.read a` then `project.read b` is not a repeat. The observation must be the
same reality, so a read after a write, or a test that now passes, is not a repeat. The guard is a
predicate over what happened and never reads the model's text.

## Pattern C: not implemented, and why

"Repeated context-window failures" needs a structured signal that a failure was a context-window
failure, and that it recurs within a run. Neither exists.

* FX's `ErrorKind` has no context-window variant (`Timeout`, `Cancelled`, `ProtocolViolation`,
  `ResourceLimit`, `Internal`, and the transport kinds). A provider's context-length error reaches
  Chip as text. Recognising it would be exactly the vague text matching this work forbids.
* The one structured signal, `WorkEvent::ContextLimit` (Chip's own byte budget refusing to send a
  request), already ends the work at its first occurrence with `LimitReached(Context)`. It is not
  retried, trimmed or substituted, so it cannot repeat inside a run.

So within a run the pattern cannot occur, and a repeat across attempts is a durable-task question,
not a trajectory one. It will be tested as an *absence*: one `ContextLimit` is terminal, makes no
model call, and is not reported as stuck. Adding a structured `ContextWindow` error kind to FX is a
separate change with its own justification, and is not made here.

## Not changed

The decision contract, the executor, the durable layer, authorization, completion rules, retries
(none are added), model-facing prompts (none are added), the FeltDB contract. `WorkSpec` gains one
optional field, `stuck_guard`, which is `None` by default so every existing chip-core test is
unchanged; `stack-agent work` turns it on.

## What the experiment must show

Identical scripted scenarios, guard off against guard on, and the `main` binary against this one:
a genuine loop, an alternating loop, a productive sequence with legitimate repeated capability calls,
an ordinary task, a turn-limit run with no loop, a loop after progress, and the same terminal result
after a restart. Counts of model calls, executions, repeated executions and turns, and whether any
productive scenario was stopped. Scripted models only: nothing here measures a real model.
