# Consequential-operation recovery matrix

What happens when a consequential capability call crosses a process death, traced through the
merged code and pinned by tests that run under `cargo test`. It builds on
[`README.md`](README.md) (parity with the reference) and does not replace its 22 scenarios.

"Effects" always means what the fake destination applied in **its own** files or counters, never a
record of stack-agent's.

## The lifecycle, and what is authoritative at each step

All paths are in `crates/chip-agent-state/src/lib.rs` (`DurableExecutor::perform`) unless noted.

| # | Step | Authoritative record | Commit point | Can the effect already have happened? | Can recovery prove it? |
| --- | --- | --- | --- | --- | --- |
| 1 | Model response received | `chip.ModelResponse` | `record_model_response` | no | — |
| 2 | Request validated (capability, typed inputs, descriptor) | none (`validate_request`) | — | no | — |
| 3 | Settlement check, key and binding claimed (`claim`) | none (memory) | — | no | — |
| 4 | **Intent** recorded (`before`: `renew`, `record_intent`) | FeltDB intent (`intent_key = a<attempt>.<execution id>`; for a keyed capability the action carries `ak1.` key and `b1.` binding) | the `record_intent` transition | no | an absent intent proves nothing was dispatched |
| 5 | **Dispatch** (`inner.execute`) | none: **nothing records that dispatch began** | — | from here on, yes | no. An open intent means "dispatched or not": recovery cannot tell |
| 6 | Result in hand | process memory only | — | yes | no |
| 7 | **Observation** written (`after`: `create chip.Observation` id `<task>~<intent key>`) | `chip.Observation` (create-only) | the record write | yes | yes: the record is the result |
| 8 | **Outcome** recorded (`record_outcome`, citing the observation) | FeltDB intent resolved | the `record_outcome` transition | yes | yes |

So the windows are: before 4 (nothing happened), 4 to 7 (an effect may or may not have happened,
and the result, if any, is in memory), 7 to 8 (happened and observed: `begin` reconciles the intent
from the observation record), after 8 (committed).

The design consequence, and the limit: **the runtime cannot distinguish "dispatched, result lost"
from "never dispatched" between steps 4 and 7.** It does not guess. Whether anything is performed
again is decided by what the capability *declared*, never by the process having restarted.

## Recovery decision for an open intent

From `begin` (`Reissuable`, `Reobservable`, `OutcomeUncertain`) and the descriptor the adapter
declares:

| Capability declares | Open intent after a death | Source |
| --- | --- | --- |
| `effect = observation` | performed again for a fresh observation; the old intent is reconciled citing it | `Inner::reobserve_one`, `effects.rs` |
| `idempotency = deduplicated` | asked again **under the same key**, if the request is the same binding | `Inner::claim`, `idempotency.rs` |
| anything else (consequential, unknown, `carried` only, or no declaration) | `OutcomeUncertain`; nothing is run; an operator settles | `begin`, `settlement.rs` |

## Matrix

Status key: **T** implemented and tested · **P** implemented, insufficiently tested · **M** missing ·
**N/A** not applicable · **U** unverified.

| Scenario | Required behaviour | Status | Evidence |
| --- | --- | --- | --- |
| Dies before the intent | nothing was dispatched; the recorded model response answers the turn | **T** | `crash_windows.rs` `killed_after_the_response_and_before_the_intent_…` (real `SIGKILL`: tool ledger 0 then 1, model requests 0) |
| Dies after the intent, before dispatch | cannot be proved; retry only if the contract makes it safe | **T** | `crash_windows.rs` `killed_after_the_intent_and_before_the_effect_…`: ledger 0, `OutcomeUncertain` (exit 3), nothing run until `executed=false`. A `deduplicated` or observation capability is retried: `idempotency.rs`, `effects.rs` |
| Dies right after dispatch | reconcile if supported, else keep the uncertainty | **T** | `crash_windows.rs` `killed_after_the_effect_…`: ledger 1, uncertain, never run again. Reconciliation exists only through a deduplicating key |
| External effect succeeds, response lost | do not resubmit a non-idempotent operation | **T** | `outcome_unknown.rs` (in memory); `kill_boundaries.rs` (real kill, destination files) |
| Result received, dies before durable commit | recover without blindly repeating | **T** (new) | `kill_boundaries.rs` `a_result_received_and_lost_with_the_process_is_not_charged_again_without_a_key`: the destination returned a success to the executor, the process was `SIGKILL`ed holding it; destination effects 1; two fresh processes both refuse as uncertain and call the destination **0** more times; 0 outcomes committed; intent still open. With a deduplicating key, `…_asked_again_under_the_same_key_when_deduplicated`: second call carries the **identical** key, effects stay 1 |
| Dies after the result is committed | reuse the result; do not invoke again | **T** | `crash_windows.rs` `a_recorded_run_followed_by_a_kill_…` (ledger 1, `restored.observations` 1); `feltdb_native.rs` `a_crash_after_the_outcome_was_recorded_resumes_without_executing_it_again` |
| Dies between observation and outcome (steps 7 to 8) | reuse the observation | **T** | `feltdb_native.rs` `a_crash_after_the_intent_blocks_resumption_until_evidence_settles_it`; `effects.rs` `a_fresh_observation_recorded_before_its_intent_was_settled_…`. Placing a real kill *inside* these two commits is in the gdb harness (`docs/research/agent-runtime-primitives/`), which `cargo test` does not run |
| Read-only call interrupted | retry per the declared read-only contract | **T** | `effects.rs` `an_interrupted_observation_is_performed_again_…`; `kill_boundaries.rs` `a_read_interrupted_by_a_kill_is_read_again_…` (real kill; destination read twice, changed nothing, no operator, no open intent) |
| Idempotent mutation interrupted | retry only under the documented contract, same logical identity | **T** | `idempotency.rs` `a_deduplicating_capability_is_asked_again_under_the_same_key_and_charges_once`; `kill_boundaries.rs` (real kill, independent ledger: two calls, one key, one effect) |
| Non-idempotent mutation, unknown outcome | `OutcomeUncertain`; reconcile or operator | **T** | `idempotency.rs` `a_crash_after_the_effect_stays_uncertain_without_deduplication`; `effects.rs` `an_interrupted_consequential_action_is_not_performed_again_…`; `kill_boundaries.rs` |
| Key reused with different inputs | reject | **T** | `idempotency.rs` `reusing_the_key_for_a_different_payload_is_refused_before_anything_runs`; `claim` returns `InvalidRequest` before the intent |
| Same logical operation keeps its identity; a new one gets a new one | | **T** | `idempotency.rs` `a_logical_key_names_the_task_and_the_action_…`; `kill_boundaries.rs` (identical key across processes) |
| A key does not grant permission to invoke | | **P** | Structural: the key is attached inside `perform`, to a request that already passed `validate_request`; recovery re-validates against what the capability declares *now* (`effects.rs` `recovery_re_validates_against_what_the_capability_declares_now`, `a_declaration_does_not_move_an_intent_to_another_capability`). No test asserts the negative for a forged key as such |
| Two workers resume one execution | no conflicting transitions, no unsafe duplicate dispatch | **T** | `feltdb_native.rs` `racing_owners_…`, `a_task_held_by_one_owner_…` (in-process: the lease). `kill_boundaries.rs` `two_processes_resuming_…` (real processes, released together): the loser is refused either as `uncertain` or at the file, with `FeltDB database … is already owned by another process`; destination calls stay at 1 (non-idempotent) or 2 with exactly one `ran` (deduplicating) |
| A stale worker returns after ownership changed | reject its commit; keep the authoritative outcome | **T** | `feltdb_native.rs` `a_takeover_while_an_action_runs_fences_out_the_old_owner_…` (`FENCE_STALE`); `stale_worker.rs` (new): the old worker's action ran once at the destination, its late observation is refused (`Conflict`: the id is already the operator's settlement), the work is a `Failed`, the intent keeps `not_executed`, the next owner is given the settlement and restores nothing |
| Operator settles an uncertain operation | durable; resume only per the contract | **T** | `settlement.rs` (including survives a restart, two operators racing); `crash_windows.rs` (`executed=false` runs once; `executed=true` never runs again) |
| Capability errors after applying its effect | the error is not proof of no effect | **T** for built-ins after this change; **trust assumption** for external executors | see the gap below |
| Interrupted model call, restored/discarded responses | unchanged | **T** | `model_response.rs` (7), `crash_windows.rs`: untouched by this change |

## Gaps confirmed, and what was done

### Confirmed and fixed: consequential executors reported an unknown ending as a definite failure

`DurableExecutor::perform` records an `ExecutionError::ExecutionFailed` as a resolved, failed outcome
(`Inner::after`, `executed: true`); only `OutcomeUnknown` keeps the intent open
(`outcome_unknown.rs`: *"An executor's definite failure is still an outcome … a later attempt may
start"*). That is the right contract for "the card was declined". It is wrong for an executor that
has started the action and cannot say how it ended. `chip-appport-source` already honours it (the
fix before this one). Two built-in consequential executors did not:

| Executor | Ending | Was | Now |
| --- | --- | --- | --- |
| `pax.test` (consequential: runs the project's own code) | outlasts its time limit; the child is killed with the run half done | `ExecutionFailed` | `OutcomeUnknown` |
| `pax.test` | stdout larger than a result may be; the run is over | `ExecutionFailed` | `OutcomeUnknown` |
| `pax.test` | stdout is not a valid `pax.execution-result.v1`; the run is over | `ExecutionFailed` | `OutcomeUnknown` |
| `project.write` | the blocking write task did not return | `ExecutionFailed` | `OutcomeUnknown` |

Before the change the next attempt began, and the model could ask for the run again
(`consequential_recovery.rs`, red on the unmodified code: *"execution failed: pax did not finish
within the time limit"*, no open intent). After: the intent stays open, the next start is refused
as `OutcomeUncertain`, and the stand-in's own ledger shows one run
(`consequential_recovery.rs`, 2 tests). Not changed: "pax could not be started" and an
unavailable executor stay `ExecutorUnavailable` (nothing ran); `pax.observe` and `project.read`
stay `ExecutionFailed` (observations have no effect to be unsure of). The `project.write` branch has
no test of its own: a panic inside the blocking task is not something a test can induce cleanly.

### Confirmed, not a defect: result in memory only

Between steps 4 and 7 the result exists only in the process. There is no other durable source, so
**a result lost in a crash is not recovered, and nothing claims it is**. What recovery does with it
is what the capability declared (table above).

### Confirmed limit, documented: a lease running out does not stop the worker

`stale_worker.rs` shows the whole of what fencing buys: the late record is refused and the
authoritative outcome is untouched. It also shows what it cannot: with a library caller (not the
product), an operator who settled "not executed" while the old worker's action was still running
was wrong, the destination charged, and the next owner is told `not_executed`. `settle` refuses only
while a lease is *held*. In the product one process owns the database file at a time (FeltDB's
ownership lock), which `kill_boundaries.rs` exercises with real processes, so a second process
cannot even open the file to take over or settle a live worker. Fencing alone is not an
exactly-once mechanism and is not claimed to be.

## Trust assumptions

* **`effect = observation`, `idempotency = deduplicated`, and an executor's choice between
  `ExecutionFailed` (definite) and `OutcomeUnknown`** are the adapter's declarations. Chip cannot
  check them. A false one defeats the matching row of the table above. The tests use fakes that keep
  the promise; none verifies a third-party service keeps it.
* **The destination's own record is the only ground truth.** Nothing reads it unless a capability
  has a `Reconciler` ([Reconciliation](#reconciliation)). No built-in capability has one, so in the
  product every open intent still ends in `OutcomeUncertain` and an operator.
* **Operator settlements are a person's conclusion** (`ObservationRecord::is_settlement`), recorded with
  who and on what evidence, never an observation or a receipt.

## Not needed from FeltDB

Nothing in this change needs a FeltDB API change. The one gap that would need one is already in the
parity README ("Fencing Chip's own record writes"): the create-only observation guard is what
refused the stale write in `stale_worker.rs`, but a contract-level precondition on non-contract
records would be needed to fence them by owner. It stays unreachable in the product, for the reason
in the lock above.

## Reconciliation

`chip_agent_state::reconcile` asks a destination what happened to an open intent's **original
operation** and resolves the intent only on sufficient independent evidence. It is the other way out
of `OutcomeUncertain`; it is not a retry. It never dispatches the operation, never mints a key
(it asks about the key and binding the intent recorded), and a lookup is never permission to
repeat anything. Code: `crates/chip-agent-state/src/reconcile.rs`.

### The contract a destination must provide

A `Reconciler` answers two reads about one operation, named by the intent's action key and
binding:

| Read | Answer | What Chip takes it to mean |
| --- | --- | --- |
| `status` (the operation's own record) | `Committed{key, binding, receipt, reported}` | the destination applied *that* operation; a different key or binding is `Indeterminate` |
| | `NotApplied{barred: true}` | no record, **and** the destination guarantees the operation can no longer apply (refused, fenced or expired) |
| | `NotApplied{barred: false}` | no record, and nothing stops it arriving later: **not proof** |
| | `Unknown` | the destination could not say |
| `verify` (the business effect in the destination's state) | `Present` / `Absent` / `Unavailable` | read independently of the operation's record |

Chip, not the reconciler, combines them (`decide`, tested over every pair):

| Status | Business effect | Finding | Consequence |
| --- | --- | --- | --- |
| committed, same key and binding | present | `Completed` | writes the observation the lost result would have been (marked "reconciled from the destination's own record", the destination's reported text and receipt); the intent is reconciled `executed`; the next start restores it and judges the goal on it |
| committed | absent or unreadable | `Indeterminate` | the report is not the outcome; intent stays open |
| not applied, barred | absent | `NotExecuted` | the intent is reconciled `not_executed`; no more is permitted than the normal path permits |
| not applied, barred | present or unreadable | `Indeterminate` | contradiction or unconfirmed absence |
| not applied, not barred / unknown / another operation | any | `Indeterminate` | intent stays open; the next start still refuses it |

Consistency assumptions a destination must meet for each finding:

* `Completed`: its operation record is keyed by the idempotency key Chip sent and stores the request
  binding; the business state it reads for `verify` is not written from the same record that `status`
  reads (otherwise the second read is not independent).
* `NotExecuted`: it can *bar* a key (or bound its validity) so that a delivery still in flight
  cannot apply after the answer. Without that, absence is not evidence.
* Both reads are free of side effects.

### What is recorded, and where

* `chip.Reconciliation` (`chip.reconciliation.v1`), create-only, one per query, id
  `<task>~<intent>~r<n>`: the action key and binding, the finding, what the destination said,
  what the business check said, the receipt, and **why the evidence was insufficient**. The request's
  inputs are not copied into it. It is the record of what was asked; the intent's own state is the
  authority on whether it was resolved.
* The resolution is the same kind of write `begin` and `settle` make: a create-only
  `chip.Observation` at `<task>~<intent>`, then the contract's `reconcile` transition and a
  checkpoint. Because that id is create-only, an operator's settlement or the original worker's own
  late observation that was recorded first stays authoritative, and a reconciliation after either is
  refused or loses the write. `agent-state show` lists each open intent's reconcilability and every
  query (`reconciliations`, `unresolved`).
* The task's lease is held while it queries and records, so a worker or another reconciliation
  cannot interleave (`LeaseHeld`); across processes FeltDB's file ownership refuses the second
  opener first.

### Evidence available in each tested scenario

All in `crates/stack-agent/tests/reconciliation.rs` (real `SIGKILL`; the destination's own files are
the assertions) unless noted.

| Scenario | Destination invocations / applied / business effect | Status said | Business effect | Finding | Intent afterwards | Run |
| --- | --- | --- | --- | --- | --- | --- |
| A. effect committed, result lost | 1 / 1 / 1 | committed (same key, binding) | present | `Completed` | resolved `executed` | completes on the reconciled observation; destination still called once |
| B. died before delivery, key barred | 0 / 0 / 0 | not applied, barred | absent | `NotExecuted` | resolved `not_executed` | no longer refused; the next request is dispatched by the normal path (see below) |
| B'. same, key **not** barred | 0 / 0 / 0 | not applied | absent | `Indeterminate` | open | refused as uncertain |
| C1. status service down | 1 / 1 / 1 | unknown | not asked | `Indeterminate`, twice | open | refused; two queries on record; no dispatch |
| C2. destination answers about another binding | 1 / 1 / 1 | committed, other binding | | `Indeterminate` | open | refused |
| C3. capability carries no key | 1 / 1 / 1 | not asked | not asked | `Indeterminate` (nothing written) | open | refused |
| D. destination reports done, effect absent | 1 / 1 / **0** | committed | absent | `Indeterminate` | open | refused; operator can still `settle` |
| E. reconcile again / after an operator settled | unchanged | | | refused (resolved) / nothing open | unchanged | operator's `not_executed` stands |
| F. two processes reconcile | 1 / 1 / 1 | | | exactly one `Completed`; the other refused | resolved once | completes |
| `chip-agent-state/tests/reconciliation.rs` | | | | | | |
| lease contention (in process) | | | | second refused with no query made | | |
| worker returns after a reconciliation | 1 | | | `Completed` | stays `executed`; the worker's write is refused | worker `Failed`; next owner restores the reconciled observation |

### What this does not give

* **A barred key is final for that logical action.** Action keys are derived from the task and the
  execution's place in the trajectory, not the attempt, so after `NotExecuted` the model's next
  request for the same place carries the *same* key, and a destination that barred it refuses it
  (scenario B: invocations 1, applied 0). Continuing needs a destination that distinguishes the
  barred delivery from a fresh one, or a new logical action identity, which this change does not
  introduce.
* **Concurrency across processes is mostly decided by the file lock**, not the lease: the loser
  usually reports `already owned by another process`. The lease path is pinned in process.
* **The fixture proves the protocol, not any destination.** The destination in the tests is a fake
  that keeps the promise.

### Capabilities that cannot be reconciled today, and why

| Class | Why |
| --- | --- |
| Any capability with `idempotency = not_carried` (the default) | the original operation has no key a destination could be asked about |
| `pax.test` | runs the project's own code; no operation record is defined for it here, so there is nothing to ask. Stays uncertain |
| `project.write` | a local file write: its "destination" is the project directory, no operation record and no bar. Stays uncertain; a future adapter would need a write journal keyed by the action key |
| AppPort / MCP tools | consequential; this repository defines no provider status endpoint for them, so no `Reconciler` exists |
| Anything whose destination cannot bar a key | `NotExecuted` is unreachable; only `Completed` is |

What a future destination adapter must provide: the key carried in and stored with the operation, a
status read keyed by it, a separate read of the business effect, and a way to bar a key.

### Who guarantees what

| Fact | Guaranteed by |
| --- | --- |
| an intent was recorded before dispatch; its key and binding | Stack-Agent's durable records (FeltDB) |
| the request that the intent recorded is the one asked about | Chip (recorded inputs must hash to the binding) |
| the operation was applied | the destination's record, **and** its contract that the record is keyed by what was sent |
| the business effect is present | the destination's state, read independently |
| the operation cannot still apply | the destination's contract (`barred`) |
| `idempotency`, `effect`, `ExecutionFailed` vs `OutcomeUnknown` | merely asserted by the adapter; not used as evidence here |
| what happened when no `Reconciler` exists | unknown |
