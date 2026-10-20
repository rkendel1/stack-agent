# Durable agent state through FeltDB

`stack-agent work --state-db FILE --task-id ID [--lease-secs N]` persists one piece of work in
FeltDB. It is opt-in: without `--state-db`, nothing is persisted and `work` behaves as before.

## What is used

| | |
| --- | --- |
| Source | `rkendel1/flow_db` at commit `b24481360f7c10564ae09fcc5bbf7802725eccd9`, vendored as `vendor/feltdb` (a path dependency of `crates/chip-agent-state`; see [`vendor/feltdb/VENDORED.md`](../../vendor/feltdb/VENDORED.md)). Building needs no access to that repository. |
| Crate | `feltdb` 0.2.0 (`crates/feltdb`) |
| API | `feltdb::agent_state::AgentState` (`new`, `with_clock`, `read`, `apply`) over `feltdb::FeltDb::open` (a file-backed log with a per-process ownership lock) |
| Contract | `feltdb::agent_transition_contract`, id `feltdb.agent.transition`, **version 1** |
| Records | `_feltdb.AgentTask`, `_feltdb.AgentCheckpoint`, `_feltdb.AgentIntent` (the contract's); `chip.Observation` (schema `chip.observation.v2`, which adds the validated request; `v1` records are still read), `chip.WorkResult` (Chip's, write-once, cited as evidence) |

No npm package, JavaScript binding or sidecar is involved. The FeltDB agent-transition contract v2
(`flow_db/adrs/agent-transition-contract-v2.md`) is a proposal and is not implemented; nothing here
depends on it, and `chip-agent-state` refuses any contract other than v1 (`check_contract`).

## Ownership

* **FeltDB** owns durable agent state: every transition is decided by its contract and committed
  through one guarded transaction carrying version, lease and fence preconditions.
* **Chip** (`chip-agent-state`) maps its work onto that contract. It decides no transition, keeps no
  state of its own, and never retries a refused or conflicting commit.
* **stack-agent** opens the database and composes it into `work`.
* **FX** is unaffected: model delivery failures are reported exactly as before.

## Lifecycle

```text
work starts       open FILE; create_task(ID) or, if it exists, acquire it (fence + 1)
                  reconcile every unresolved intent from its chip.Observation record
                  read back every recorded observation (the trajectory to resume from)
                  checkpoint -> running            (the attempt is this checkpoint's version)
each execution    renew; record_intent             (until this commits, the action does not start)
                  run the action
                  write chip.Observation (with the validated request); record_outcome citing it
work ends         write chip.WorkResult; checkpoint -> completed | blocked | cancelled
```

Chip's outcome maps to the contract's status: `completed` to `completed` (only if at least one
observation backs it and no persistence failure occurred), `cancelled` to `cancelled`, and escalated,
blocked, limit reached and failed to `blocked`, which a later run can resume.

## Restart and recovery

FeltDB, not process memory, decides what happens on the next run of the same task id:

* **Completed or cancelled**: the persisted history is reported and nothing is run (exit 0 for
  completed, 1 for cancelled).
* **Lease still held** (by another process, or by a process that died less than the lease ago): refused
  before anything runs (exit 3). The default lease is 900 s; there is no forced takeover in contract v1.
* **Lease expired**: the task is acquired, which advances the fence; the previous owner can commit
  nothing more.
* **An intent whose outcome is unknown** (the process died between recording the intent and recording
  its outcome): if its `chip.Observation` record exists, the intent is reconciled from it; if not, the
  run is refused with `OutcomeUncertain` (exit 3). The absence of an observation is not evidence that
  the action did not run.

A resumed attempt starts from the trajectory earlier attempts recorded, not from the goal alone:

* **What is restored.** Every resolved intent's `chip.Observation` record, read back from FeltDB in
  execution order (attempt, then the time FeltDB recorded the intent, then Chip's execution sequence).
  Each becomes an observation of the resumed loop before its first turn (`WorkSpec::restored`), with
  its original execution id, capability and inputs. It is derived by the same observer, joins the
  same notes and is evaluated against the same requirements as when it was first made. The model's
  context carries it as an observation like any other.
* **What is not done.** A restored observation is not executed, validated or recorded again: no
  second intent, no new result. Execution numbering continues after the highest restored id, so a new
  intent never names an earlier execution. A completion that restored observations back is persisted
  as complete, citing their records.
* **What is not restored.** An execution whose executor returned an error was never an observation
  and is not one now. Decisions and turn counts are not restored; limits apply per attempt.
* **Where the position comes from.** Only from the records. Nothing else is persisted for it: no
  cursor, checkpoint file or session log.
* **When nothing is restored.** If any observation record lacks its request (an operator's
  settlement, or a `chip.observation.v1` record from an earlier version), or one execution id was
  recorded by two attempts, the history is consistent but cannot be restored whole. None of it is,
  and the attempt starts from the goal as before (`agent_state.restored.not_restored` in `--json`
  says why). A record that contradicts its intent is `InvalidHistory`, and nothing runs.

Earlier attempts' intents, outcomes, observations and results stay in FeltDB; intent keys carry the
attempt (`a<attempt>.<execution id>`), so a new attempt never collides with an old one.

## Idempotency keys

An intent key names one *attempt* at an action, so it changes on every resume. FeltDB uses it for
reconciliation and fencing, and that does not change. A capability needs a second key that names
the *logical action*: the same whenever the action is performed again, and different for every other
action.

* **The logical key.** `action_key(task, execution id)` = `ak1.` + 128 bits of
  SHA-256(`chip.action-key.v1`, task id, execution id). It is derived from the task and the action's
  place in the trajectory, never from the attempt, the process id or a retry count, so every process
  derives it again. Execution numbering continues across restored attempts, so a new action gets a
  new key.
* **The binding.** `binding(request)` = `b1.` + 128 bits of SHA-256 of the validated request (the
  capability and its typed inputs) as the observation record writes it. Within a task, a key is
  bound to one request. A request that would reuse a bound key for a different request is refused
  before anything is recorded or executed (`InvalidRequest`: "already names another request").
* **Where it goes.** `ExecutionRequest::idempotency_key`, set by the durable executor and passed
  through `CapabilitySet`, `AllowlistedBackend` and the remote wire (`idempotency_key`, sent only
  when set) to the capability's executor. It is set only for a capability that can carry it. Without
  `--state-db` nothing identifies the action durably, so no key is set.

Each capability declares what it does with the key (`CapabilityDescriptor::idempotency`, declared by
its adapter, never by a requester or the model):

| Declaration | Meaning | Today |
| --- | --- | --- |
| `not_carried` (default) | The key does not reach the effect. | Every production capability. `project.*` (`ProjectExecutor`) and `pax.*` (`PaxExecutor`, fixed argv) have no field for it, `ComputeExecutor` runs fixed arguments, and remote capabilities are declared by the worker, whose capabilities are those same executors. |
| `carried` | The key reaches what performs the effect, which promises nothing. | No production capability. The remote backend's transport carries it to the worker, but the declaration is the worker's capability's own. |
| `deduplicated` | What performs the effect applies at most one effect per key and answers a repeated key with the first effect's outcome. | No production capability. The test service in `crates/chip-agent-state/tests/idempotency.rs` and the benchmark's `notify-once-deduplicated` service are fakes. |

A remote worker that predates the declaration, or declares a name this build does not know, is read as
`not_carried`, never as `deduplicated`.

**After a crash.** An intent with no observation is an action that may or may not have run:

* **For a `deduplicated` capability**, when the intent recorded it as one and the capability still
  declares it, the intent does not block the restart. It stays open and is listed as reissuable
  (`DurableWork::reissuable`). If the model asks for exactly the same request again, the request is
  performed under the intent's logical key, wherever the trajectory now numbers it. The capability
  then answers for that key without applying the effect a second time. Once its answer is observed,
  the open intent is reconciled `executed`, citing that observation. The conclusion means the
  capability now answers for the effect; it does not mean this particular attempt applied it.
  * If the answer is an error, it is not an answer, and the intent stays open.
  * If a later attempt's observation already answered for the key, the next start reconciles the
    intent from it.
  * The task cannot be completed while any intent is open: a completion is checkpointed `blocked`
    ("intent(s) are still unresolved"), as the contract requires.
* **For every other capability** (`not_carried`, `carried`, one whose intent predates keys, or one
  that no longer declares `deduplicated`), nothing changes. The restart is refused with
  `OutcomeUncertain` (exit 3) until the outcome is settled with evidence. Nothing is retried
  automatically and no receipt is made up.

**Compatibility.** No breaking change and no new durable schema.

* **Records.** The logical key, binding and declaration are kept in the intent's `action` text, which
  contract v1 leaves to its caller (`<capability> key=<ak1…> binding=<b1…> idempotency=<declaration>`).
  This is done only for a capability that carries the key. Every other intent's `action` is the bare
  capability id it always was, so every production record is unchanged. An intent written before keys
  existed has no tag and is never reissued. `chip.Observation` stays `v2`: the binding is recomputed
  from the request it already records.
* **Types.** `ExecutionRequest` and `CapabilityDescriptor` each gain one field, defaulted by their
  constructors.
* **Wire.** On the remote wire, the request's `idempotency_key` and the descriptor's `idempotency` are
  additive. A peer that predates them ignores them, or reads them as absent.

## Failures

* The intent cannot be recorded: the action is not started; the execution, and so the work, fails.
* The outcome cannot be recorded after the action ran: the execution is reported as failed and the
  intent stays unresolved in FeltDB, which is the truth (the action may have happened).
* The terminal checkpoint is refused or fails: the run's report says the outcome was not recorded,
  and a run that would otherwise exit 0 exits 4.
* A completion with no observation behind it is checkpointed `blocked` and exits 1.
* Stored history that is incomplete (a missing checkpoint version, an intent count that does not match)
  or written under an unknown Chip record schema is refused, never read around.

## Limits

* **Sessions and conversational turns are not stored in FeltDB.** Their owner is the Zig-compatible
  session store (`chip-session`, `chip-turns`, [`session-format.md`](session-format.md)); a second
  store would compete with it, and contract v1 has no record for them. Each execution's decision turn
  is kept in the attempt's `chip.WorkResult`.
* Contract v1 records are closed (`deny_unknown_fields`), so Chip's richer data lives in its own
  write-once records, cited by the checkpoints and outcomes.
* Restored observations join the trajectory but not the agent's in-memory evidence store, so they
  never answer a later request by reuse. A request made after a restart is performed again.
* One process owns a database file at a time (FeltDB's ownership lock); a second process opening the
  same file is refused with a storage error.
* Model calls are not intents: the contract governs actions, and FX already reports each model call's
  delivery certainty.
* **A `deduplicated` declaration is trusted, not checked.** Chip cannot verify that a capability
  deduplicates, just as it cannot verify that a capability does what it describes. The guarantee
  is the capability's own.
* **A key names a place in the trajectory, bound to a request.** When the trajectory cannot be
  restored (an operator's settlement, for example), the next attempt numbers its actions from the
  start again.
  * The same request at a numbered place gets the same key, which a deduplicating capability answers
    from the earlier effect.
  * A different request at that place is refused if its capability carries keys.
  * The same is true after an execution that ended in an executor error, because that execution is
    not restored and its number is used again.
  * These refusals fail closed: nothing is executed.
* There is no exactly-once guarantee in general. With no deduplicating capability, nothing about
  OutcomeUncertain changes.
