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
  the action did not run. An operator settles it with `stack-agent agent-state settle` (see
  [Operator settlement](#operator-settlement)). The one exception is a capability that declares
  itself an observation, which is performed again to obtain a fresh observation (see
  [Capability effects](#capability-effects-and-safe-re-observation)).
  * **The refusal holds no lease.** To find the intent, the refusing run had to acquire the task, which
    advanced the fence and took a new lease. It gives that lease back before it returns, so the task
    can be settled and resumed at once instead of after the lease runs out (900 s by default).
  * **How the lease is given back.** Contract v1 has no release operation. The refusing owner renews
    its own lease to the shortest the contract grants, 1 ms, under the same owner, fence and lease-id
    checks as any renewal. A lease someone newer holds is therefore never touched.
  * **If it cannot be given back.** The refusal says so (still exit 3), and the task stays held until
    that lease expires.

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
* **What an operator settled is not restored.** It is told to the work instead (see
  [Operator settlement](#operator-settlement)). It no longer stops the rest of the trajectory from
  being restored.
* **When nothing is restored.** If the record of an execution lacks its request (a
  `chip.observation.v1` record from an earlier version), or one execution id was recorded by two
  attempts, the history is consistent but cannot be restored whole. None of it is,
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
  This is done only for a capability that carries the key. Since P3 every intent also records its
  binding and inputs (see [Operator settlement](#operator-settlement)). An intent written before keys
  existed has no tag and is never reissued. `chip.Observation` stays `v2`: the binding is recomputed
  from the request it already records.
* **Types.** `ExecutionRequest` and `CapabilityDescriptor` each gain one field, defaulted by their
  constructors.
* **Wire.** On the remote wire, the request's `idempotency_key` and the descriptor's `idempotency` are
  additive. A peer that predates them ignores them, or reads them as absent.

## Capability effects and safe re-observation

Each capability declares what performing it does to the world (`CapabilityDescriptor::effect`,
declared by the adapter that performs it, never by a requester or the model, never inferred from a
name, a description or a parameter list). It is the experiment seam's `Effect`, promoted to
`chip_core::Effect`; there is one vocabulary.

| `effect` | Meaning | An interrupted request with no recorded observation |
| --- | --- | --- |
| `observation` | Performing it again changes nothing; it yields a fresh observation. | Performed again by the next attempt; no operator. |
| `consequential` | Performing it again may repeat a change. | `OutcomeUncertain` (exit 3); the operator settles it. |
| `unknown` | The adapter does not say. The default. | `OutcomeUncertain` (exit 3); the operator settles it. |

**Fail closed.** A descriptor that declares nothing is `unknown`. On the remote wire, a missing,
non-string or unrecognized `effect` is read as `unknown`, and only the exact name `observation` is
read as one. A remote worker that predates the declaration therefore promises nothing.

**Product classification.** Checked against each implementation, not its name.

| Capability | `effect` | Why |
| --- | --- | --- |
| `project.read` | `observation` | Opens a located file, seeks and reads at most a bounded range. It writes nothing. |
| `project.write` | `consequential` | Creates or replaces a file. |
| `pax.test` | `consequential` | Runs the project's own tests, which may do anything. |
| every other capability | `unknown` | Nothing established it. `project.list`, `project.search` and the `project.git.*` capabilities are read-only in their descriptions, but a description is not a declaration. |

The effect says nothing about the idempotency key, and the key says nothing about the effect. A
capability that is `consequential` and `deduplicated` is still not performed by recovery; P2's rule
(the work may ask for it again, under its key) is unchanged. An `observation` is performed again
without a key.

**What recovery does with an `observation`.** At `begin`, an open intent with no observation record
whose capability the work's own capability set declares `observation` becomes a re-observation, if
the request it recorded can be reconstructed exactly: its `action` records the inputs and they hash
to the binding it records. Then `DurableWork::reobserve`, called by `stack-agent work` between
`begin` and the first model call:

1. validates that request again, through `chip_core::validate_request`, the validation a model's
   decision passes through, against what the capability set declares now. The capability must still
   be declared, available, declare `observation` and accept the inputs;
2. executes it through the same durable executor as any other execution: a new intent, the
   capability's answer, an observation record and an outcome;
3. reconciles the old intent citing the new observation;
4. joins the new observation to the restored trajectory, before the first turn.

If any step fails, nothing is retried: the intents stay open, the attempt gives its lease back, and
the run is refused with `OutcomeUncertain` (exit 3) like any other.

**What it records, and what it does not claim.** The earlier response is gone. The new observation
is the capability's answer *now*, which may differ from the lost one, and no record says it is the
lost one.

* The new intent's `action` is `<capability> binding=<b1…> reobserves=<old intent key>[,<key>…]
  inputs=<JSON>`. The new `chip.Observation` is under a new execution id
  (`<task>-reobs-<attempt>-<n>`) and a new intent key, and is an ordinary record.
* The old intent has no observation record of its own. It is resolved with a citation of a
  *different* execution's observation.
* Contract v1 allows only `executed` or `not_executed` as a `reconcile` conclusion, and neither is
  known of the lost execution. The old intent is reconciled `not_executed`, the reading that does not
  credit it with an effect, which an observation does not have. **This is the one place the
  contract's vocabulary does not fit.** What the records say happened is the `reobserves` marker and
  the cross-execution citation, not the conclusion. An operator settlement is never used.
* `work --json` reports it as `agent_state.reobserved` (`fresh_observation: true`), and chip-core's
  `ObservationRestored` event carries `reobserved: true`. Once recorded, a later attempt restores it
  as any recorded observation, and the event says `false`: that attempt performed nothing.

**A recovery that is itself interrupted.** If the process dies inside the re-observation, the next
attempt finds two open intents that name one request (the old one, and the re-observation's, whose
`reobserves` names the old one) and performs it once, settling both. If it dies after the
re-observation's outcome was recorded and before the old intent was reconciled, the next attempt
reconciles the old intent from that observation and performs nothing.

**What it does not do.** It does not retry a re-observation that failed, make a consequential or
unknown request repeatable, infer anything from a name, or make the re-observation part of the
model's decisions: the model's own requests are unchanged. It runs before the work's cancellation
and deadline control applies, and it is bounded by the number of open intents the earlier attempt
left (in practice one).

## Operator settlement

```text
stack-agent agent-state show   --state-db FILE --task-id ID [--json]
stack-agent agent-state settle --state-db FILE --task-id ID --intent KEY \
                               --executed true|false --decided-by ID --evidence TEXT [--json]
```

When a run is refused with `OutcomeUncertain`, an operator establishes outside the agent whether the
action's effect occurred (a ledger, a log, the external system) and records their conclusion.

**What `settle` records.** Nothing is executed and no receipt is made up.

1. **A `chip.Observation` at the intent's id, created only if absent.** It is the shape the durability
   bench and `sa-state reconcile` already wrote: execution `operator` of attempt 0, no request, no
   receipt, and `executed` holding the conclusion. Its `status` is `error`, never `success`: no
   capability answered. Its `output` says what it is:
   `operator settlement (an operator's conclusion, not an execution observation): decided_by=<ID>
   executed=<bool> evidence=<TEXT>`.
2. **A `reconcile` of the intent** citing that record, with conclusion `executed` or `not_executed`.
   The intent keeps its key, action and history.
3. **A checkpoint by `--decided-by`** (the contract records the actor) whose summary states the
   conclusion and the evidence. The task's status is unchanged.

`settle` acquires the task to record all of this, as the contract requires, and releases it when
done.

**Who decided.** stack-agent has no authenticated or configured operator identity, so
`--decided-by` is required. It is an identifier (1 to 128 bytes of `A-Z a-z 0-9 _ . : @ -`) and is
never defaulted or guessed. `--evidence` is required too: 1 to 512 bytes on one line. It is
recorded as the operator gave it and is not checked by anyone else.

**What it refuses.** Each of these exits 1 and writes nothing:

- a malformed identity, evidence or conclusion (a missing flag is a usage error, exit 2);
- an unknown task or intent;
- a finished task;
- an intent that is resolved already, including a second settlement either way;
- an intent that has an observation of its own (the next start reconciles it from that);
- a task a worker still holds.

Two operators settling one intent at once cannot both be recorded. The record is created only if
absent, and ownership is fenced.

**What a resumed run does with it.**

* **Settled `executed=true`.** The action is not performed again by this work. The model is told,
  under "Ruled out", that an operator concluded its effect occurred and that its outcome was never
  observed. If it asks for the same request anyway, the durable executor refuses before recording
  an intent, and the work stops with that reason.
  * A settlement is never an observation. It joins no trajectory, satisfies no requirement and backs
    no completion.
  * So a goal that needs the action's result (for example verifying that tests pass) stops rather
    than complete on an operator's word or repeat the effect.
* **Settled `executed=false`.** The action may be performed again by the normal validated path,
  if the work still asks for it. That is a new execution decision, not part of the settlement.
* **Either way.** The rest of the recorded trajectory is restored as before. Settlements are
  reported in `work --json` (`agent_state.settled`) and by `agent-state show`.

**Compatibility.** No contract revision, no new store and no new record schema.

* **Settlement records.** The settlement is the operator record shape that already existed. Earlier
  records of that shape (`status: success`, no `decided_by`) are read as settlements too, with
  "an unrecorded operator".
* **Intent `action` text.** It now records the request for every capability, not only for one that
  carries a key: `<capability> binding=<b1…>`, the P2 key fields where they apply, then
  ` inputs=<JSON>` when everything fits the contract's 1024-byte text limit. This is what lets a
  settlement name exactly the request it settles.
  * An intent written before this records no binding. A settlement of it rules out every request
    of that capability in the task.
  * Contract v1 leaves `action` to its caller; FeltDB stores and returns it unread.

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
* **A settlement is as good as the operator's judgment.** Stack-agent records the conclusion and its
  basis, and acts on it. It cannot check the conclusion.
  * A wrong `executed=false` lets the effect be performed again.
  * A wrong `executed=true` keeps an action from being performed that never was.
  * Only the effect's own record (a ledger, the external system) can say which is true.
* **The refusal releases only its own lease.** A process that died holding a lease still holds it
  until it expires: contract v1 has no forced takeover.
* A settlement's own writes are separate commits. The record is created first; if a later step
  fails, the next start reconciles the intent from that record.
