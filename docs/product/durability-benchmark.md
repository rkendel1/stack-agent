# Does durable agent state make work more effective?

`work --state-db FILE --task-id ID` ([`agent-state.md`](agent-state.md)) makes work *safer*: tasks
survive restarts, competing workers are fenced, and an action whose outcome is unknown is not
reported as done. This benchmark treats "and therefore more *effective*" as a hypothesis. It
compares the same tasks with persistence off (arm **A**, legacy) and on (arm **B**, durable).

**Short answer, from the deterministic suite:** durability improves verified completion
**under contention** (two workers, or a stale worker that comes back), and removes false
successes there. **After a crash** it does not improve verified completion. It adds lease waits,
an extra run and, for an ambiguous action, an operator step. For an action whose effect cannot be
observed, it refuses to retry blindly, but **once the uncertainty is reconciled, the resumed run
performs the action again**, so the duplicate is deferred, not prevented. Normal execution has the
same outcomes in both arms and costs about 30–40 ms more per task (debug build). The real-model
paired run has **not been executed** (no credentials in this environment); see
[below](#real-model-paired-runs).

## Method

### Arms

| | |
| --- | --- |
| A (legacy) | Chip's work loop with no durable state, as `work` runs without `--state-db`. |
| B (durable) | The same loop composed as `work --state-db` composes it: `chip_agent_state::begin`, the durable executor wrapper, `DurableWork::finish(report, accepted)`, over a real file-backed FeltDB that is closed and reopened on every restart. |

Everything else is shared: the task, the initial world, the scripted model, the capabilities and
executor, the limits (16 turns, 12 executions), Chip's goal check, and the oracle.

### World, oracle and model (task set `chip.durability-bench.tasks.v1`)

* **World:** two append-only files. `effects.log` is **observable** (`world.inspect` reports it).
  `outbox.log` is **not observable**, like a sent e-mail or a charge: nothing can read it back.
* **Tasks:** `steps-3-observable` applies steps 1, 2 and 3, each exactly once.
  `notify-once-unobservable` applies step 1, then sends notification 1 exactly once.
* **Oracle:** the world on disk. Every required effect must appear exactly once. A report of
  success is never the criterion; a success report the world does not support is a **false
  success**.
* **Model:** a deterministic scripted model that decides only from the observations Chip sends
  it. It inspects after every action, applies the lowest step the last inspection does not show,
  sends the notification if none is among its observations, and then completes. It never reads
  the world. Chip completes the work only once an inspection shows every step.

### Scenarios

| Scenario | Fault |
| --- | --- |
| `normal` | none |
| `restart-crash-before-effect` | crash in the 4th execution, before its effect. In B, the intent is already durable. |
| `ambiguous-crash-after-effect` | crash in the 4th execution, after its effect, before its outcome is recorded |
| `restart-crash-between-actions` | crash at the 5th model call: the 4th action ran and its outcome was recorded |
| `contention-concurrent-start` | two workers start together on one task |
| `contention-stale-owner` | worker 1 stalls before the 4th action's effect, is presumed dead, worker 2 is started, then worker 1 resumes |

A **crash** drops the run and every handle, so arm B's database is reopened from its file.
**Recovery** restarts the work until it reports success, at most 5 runs per case:

* Arm A simply restarts.
* Arm B waits out the lease (60 s, on an injected clock: recorded as `lease_wait_ms_modelled`,
  not as wall-clock time). If FeltDB refuses with `OutcomeUncertain`, the **operator** verifies the
  action's outcome from the world and records it as an observation FeltDB can cite. The merged
  implementation has no command for this; the harness writes the same record `chip-agent-state`
  writes.

### Classification

Each case is classified as exactly one of:

* `success`: the world is verified and success was reported.
* `false_success`: success was reported but the world is not right. Every success report beyond
  the one the world supports counts, so two workers both claiming the same task count once each.
* `completed_unproven`: the world is right but no success was reported.
* `unresolved`: refused with uncertain outcomes and never finished.
* `failure`: anything else.

A safe refusal (`OutcomeUncertain`, `LeaseHeld`) is counted on its own and is never a success.

## Results (deterministic, 20 repeats per cell)

Run: `stack-agent --durability-bench --repeats 20` at `ab6c9b4` (main), rustc 1.97.0, debug build.
Per-attempt records: [`durability-bench/run1-deterministic/results.jsonl`](durability-bench/run1-deterministic/results.jsonl) (480 records).
Summary: [`summary.json`](durability-bench/run1-deterministic/summary.json).

All 20 repeats of each cell gave the same result in every field except timing. The cases are
deterministic, so n = 20 shows **repeatability, not sampling variance**. Read each cell as one
outcome, observed 20 times.

| scenario | task | arm | verified | false success | safe refusal | operator | duplicate effects | model calls | executions | runs | median wall ms | lease wait ms (modelled) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| normal | steps | A | 20/20 | 0 | 0 | 0 | 0 | 8 | 7 | 1 | 1.38 | 0 |
| normal | steps | B | 20/20 | 0 | 0 | 0 | 0 | 8 | 7 | 1 | 39.15 | 0 |
| normal | notify | A | 20/20 | 0 | 0 | 0 | 0 | 6 | 5 | 1 | 1.10 | 0 |
| normal | notify | B | 20/20 | 0 | 0 | 0 | 0 | 6 | 5 | 1 | 29.55 | 0 |
| crash before effect | steps | A | 20/20 | 0 | 0 | 0 | 0 | 10 | 9 | 2 | 1.53 | 0 |
| crash before effect | steps | B | 20/20 | 0 | 20 | 20 | 0 | 10 | 9 | 3 | 64.46 | 120002 |
| crash before effect | notify | A | 20/20 | 0 | 0 | 0 | 0 | 8 | 7 | 2 | 1.27 | 0 |
| crash before effect | notify | B | 20/20 | 0 | 20 | 20 | 0 | 8 | 7 | 3 | 55.51 | 120002 |
| ambiguous (crash after effect) | steps | A | 20/20 | 0 | 0 | 0 | 0 | 8 | 7 | 2 | 1.28 | 0 |
| ambiguous (crash after effect) | steps | B | 20/20 | 0 | 20 | 20 | 0 | 8 | 7 | 3 | 54.73 | 120002 |
| ambiguous (crash after effect) | notify | A | 0/20 | 20 | 0 | 0 | 20 | 8 | 7 | 2 | 1.34 | 0 |
| ambiguous (crash after effect) | notify | B | 0/20 | 20 | 20 | 20 | 20 | 8 | 7 | 3 | 54.19 | 120002 |
| crash between actions | steps | A | 20/20 | 0 | 0 | 0 | 0 | 9 | 7 | 2 | 1.36 | 0 |
| crash between actions | steps | B | 20/20 | 0 | 0 | 0 | 0 | 9 | 7 | 2 | 45.27 | 60001 |
| crash between actions | notify | A | 0/20 | 20 | 0 | 0 | 20 | 9 | 7 | 2 | 1.31 | 0 |
| crash between actions | notify | B | 0/20 | 20 | 0 | 0 | 20 | 9 | 7 | 2 | 45.58 | 60001 |
| concurrent start | steps | A | 0/20 | 40 | 0 | 0 | 60 | 16 | 14 | 2 | 2.21 | 0 |
| concurrent start | steps | B | **20/20** | **0** | 0 | 0 | 0 | **8** | **7** | 2 | 38.62 | 0 |
| concurrent start | notify | A | 0/20 | 40 | 0 | 0 | 40 | 12 | 10 | 2 | 1.70 | 0 |
| concurrent start | notify | B | **20/20** | **0** | 0 | 0 | 0 | **6** | **5** | 2 | 29.19 | 0 |
| stale owner | steps | A | 0/20 | 40 | 0 | 0 | 20 | 12 | 10 | 2 | 1.75 | 0 |
| stale owner | steps | B | **20/20** | **0** | 20 | 0 | 0 | 8 | 7 | 3 | 50.08 | 120002 |
| stale owner | notify | A | 0/20 | 40 | 0 | 0 | 20 | 10 | 8 | 2 | 1.51 | 0 |
| stale owner | notify | B | 0/20 | 20 | 20 | 0 | 20 | 8 | 7 | 3 | 49.33 | 120002 |

(Model calls, executions and runs are per case. "Verified" and "false success" are totals over 20
cases.)

## Findings

1. **Normal execution: same outcome, measurable overhead.** Both arms reach the same verified
   outcome with the same model calls and executions. B costs about 28–38 ms more per task in this
   debug build:
   * about 7 ms in `begin` and `finish` (median `persistence_ms`), and
   * about 4–5 ms per execution for the durable bracket (renew, intent, observation and outcome:
     four guarded commits).

   The database is about 60–78 KB per task. Against model calls that take seconds, the overhead
   would be small, but that is an inference: it has not been measured with a real model.
2. **Contention: a real effectiveness gain.**
   * *Concurrent start:* the second worker is refused (`LeaseHeld`) before doing anything. B
     completes 20/20 with no duplicates and half the model calls and executions. A completes 0/20:
     both workers act, duplicate effects, and both report success (40 false successes).
   * *Stale owner, observable task:* the returning worker is fenced (`FENCE_STALE`). Its success is
     not reported, and the next owner reconciles its in-flight action from the observation it left.
     B: 20/20. A: 0/20, with 40 false successes.
3. **Restart: no gain in verified completion, extra cost.**
   * *Observable task:* both arms recover 20/20. Arm A recovers because the world shows what was
     done.
   * B needs an extra run, up to two lease waits (60–120 s modelled) and, for a crash before
     the effect, an operator reconciliation that an observable world did not need.
4. **Ambiguous, unobservable actions: refusal without prevention.**
   * After a crash after the effect, B correctly refuses to resume (20/20 `OutcomeUncertain`, no
     blind retry).
   * Once the outcome is reconciled as executed, the resumed attempt runs Chip's loop from the
     goal. It has no access to that history, and the notification is sent again. The same happens
     after a crash between actions and in the stale-owner case.
   * B ends with the same duplicate and the same false success as A, later and at a higher cost.
5. **False successes:** B never reported more than A in any cell, and reported none under
   contention or in the observable stale-owner case. It still reports false successes where
   finding 4 applies.

**Is there evidence of improved verified effectiveness?** Only under contention: concurrent
starts and stale workers whose effects are observable. Under restarts and ambiguous outcomes, the
merged implementation improves *safety at the moment of resumption* (a refusal instead of a blind
retry), but not verified completion. For unobservable actions it does not prevent the duplicate.

## After trajectory resume (P1)

The tables and findings above are the baseline before a resumed attempt received its history. With
P1, `begin` reads every resolved intent's `chip.Observation` record back from FeltDB, and the
resumed loop starts from them (`WorkSpec::restored`, see [`agent-state.md`](agent-state.md)). The
bench's arm B passes them the same way `work --state-db` does. The same command (debug build, 20
repeats per cell) gives these changes. Every other cell, including all of arm A, is unchanged.

| scenario | task | arm | verified | false success | duplicate effects | model calls | executions |
| --- | --- | --- | --- | --- | --- | --- | --- |
| crash between actions | notify | B | 0/20 → **20/20** | 20 → **0** | 20 → **0** | 9 → 7 | 7 → 5 |
| stale owner | notify | B | 0/20 → **20/20** | 20 → **0** | 20 → **0** | 8 → 6 | 7 → 5 |
| crash before effect | steps | B | 20/20 | 0 | 0 | 10 → 9 | 9 → 8 |
| crash before effect | notify | B | 20/20 | 0 | 0 | 8 → 7 | 7 → 6 |
| ambiguous (crash after effect) | notify | B | 0/20 (unchanged) | 20 | 20 | 8 | 7 |

What this shows, and what it does not:

* **Verified deduplication is not what fixed these rows.** Arm B now recovers 20/20 with no
  duplicate after a crash between actions and in the unobservable stale-owner case. The effect ran
  once and its observation was recorded, so the resumed loop sees it and does not ask for it again.
  That is a fewer-repeated-decisions result: the recorded history answered the question the goal
  would otherwise have re-asked. No executor de-duplicated anything; no key reaches one yet.
* **Fewer model calls and executions after a crash before the effect.** The steps already done are
  restored instead of redone.
* **The ambiguous case is unchanged, as it must be.** After a crash after the effect, B still
  refuses to resume (`OutcomeUncertain`). The operator's reconciliation record carries no request,
  so the trajectory is not restored and the next attempt starts from the goal. That is where a
  key the executor can de-duplicate on (P2) and an operator settlement command (P3) apply.

The same windows on the real release binary, with real `SIGKILL`s, are in
`docs/research/agent-runtime-primitives/experiments/stack-agent/p1-trajectory-resume/`.

## What the results point to (not done here)

* ~~**Give a resumed attempt its task's history.**~~ Done (P1, above).
* **An operator reconciliation command.** Today, settling `OutcomeUncertain` means writing a FeltDB
  record by hand. The harness does that here.
* **Lease length.** It trades restart latency (every recovery in B waits for it) against the risk
  of taking over a live worker. 60 s was used here; `work` defaults to 900 s.

## Limits of this evidence

* **Synthetic world:** the world, the model and the faults are synthetic. The model is a reactive
  script, not a real model. Do not infer real-model effectiveness from these results.
* **One interleaving:** contention uses one deterministic schedule (a single-threaded runtime,
  where each model call yields), not a sample of schedules.
* **Timings:** from a debug build on a shared cloud machine. The lease waits are modelled, not
  measured.
* **Operator stand-in:** the operator always reports the true outcome. A real operator could
  be wrong or slow.

## Real-model paired runs

`--agent-bench` takes `--state-arms legacy,durable`. Each trial then runs the real `work` command
in both arms, paired by trial number. The durable arm adds `--state-db FILE --task-id ID` with a
fresh database outside the fixture. Both arms use the same fixture, model, settings and oracle.
The report adds `state_arm` and `agent_state` to each trial, groups aggregates by arm, and adds a
`paired` section with these fields:

* `both_complete`, `legacy_only`, `durable_only` and `neither`;
* `durable_false_success` (FeltDB says `completed` but the oracle does not);
* per-pair latency and model-call differences.

**Status: executed once (run `1792452743`, 2026-10-19).** Archive:
`docs/product/agent-bench/run5-durable-pair/`. Model `claude-sonnet-5-5`, provider profile `anthropic`,
`max_output_tokens` 8192, call timeout 120 s, 3 trials per cell, pax v0.4.1 (674f3b3) on `PATH`.

| task | pairs | both complete | legacy only | durable only | neither | durable recorded | durable false success | median latency diff (durable − legacy) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| A | 3 | 3 | 0 | 0 | 0 | 3 | 0 | +122 ms |
| B | 3 | 2 | 0 | 0 | 1 | 3 | 0 | −687 ms |
| C | 3 | 0 | 2 | 0 | 1 | 3 | 0 | +1561 ms |
| D | 3 | 3 | 0 | 0 | 0 | 3 | 0 | −3574 ms |
| E | 3 | 2 | 0 | 1 | 0 | 3 | 0 | −1325 ms |

Reading it:

* **No durable false success.** In all 15 durable trials FeltDB recorded the task, and no task was
  recorded `completed` when the oracle disagreed.
* **Task C, legacy only ×2:** both durable trials reached a final answer that Chip's existing
  completion check refused ("the proposed answer is not supported by the observations"), and the
  task was recorded `blocked`. The third pair failed in both arms (`invalid_decision`). Both arms
  made the same number of model calls in two of the three pairs. With three pairs, this is model
  variance on a long task (30 replies) until a larger sample shows otherwise; it is not attributed
  to the durable arm.
* **Task E, durable only ×1:** the legacy trial ended on an invalid decision.
* **Latency:** per-pair differences range from −3.6 s to +2.5 s and are dominated by model time.
  This run does not measure the durable arm's FeltDB overhead separately.
* `report.md` names the arm of every trial and aggregate row (it was regenerated from
  `report.json` after the renderer gained the arm column; the measurements are unchanged).

To reproduce:

```sh
cargo build --release --bin stack-agent
# pax v0.4.1 must be on PATH for task E (see .github/workflows/ci.yml).
# The harness reads keys only from an env file; process substitution keeps the key off disk.
./target/release/stack-agent --agent-bench \
  --providers anthropic --tasks A,B,C,D,E --trials 3 \
  --state-arms legacy,durable \
  --env-file <(printf 'ANTHROPIC_API_KEY=%s\n' "$ANTHROPIC_API_KEY") \
  --out agent-bench-out/durable-pair
```

The real-model arm covers uninterrupted execution only. It measures normal-path overhead and
outcome equivalence with a real model. Interruption and contention with a real model are not
implemented: their mechanism is what the deterministic suite isolates. Three trials per cell is a
small sample, so report the per-pair counts rather than rates.

## Reproduce

```sh
cargo run --bin stack-agent -- --durability-bench --repeats 20 --out durability-bench-out
cargo test -p stack-agent --test durability_bench
```

The command writes `results.jsonl` (one record per case: arm, scenario, task, classification, world,
every worker run), `summary.json` (both arms per scenario and task, with B − A) and `summary.md`.
