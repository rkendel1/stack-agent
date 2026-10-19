# Durable agent state through FeltDB

`stack-agent work --state-db FILE --task-id ID [--lease-secs N]` persists one piece of work in
FeltDB. It is opt-in: without `--state-db`, nothing is persisted and `work` behaves as before.

## What is used

| | |
| --- | --- |
| Repository | `rkendel1/flow_db`, pinned by commit `b24481360f7c10564ae09fcc5bbf7802725eccd9` (a Cargo git dependency of `crates/chip-agent-state`) |
| Crate | `feltdb` 0.2.0 (`crates/feltdb`) |
| API | `feltdb::agent_state::AgentState` (`new`, `with_clock`, `read`, `apply`) over `feltdb::FeltDb::open` (a file-backed log with a per-process ownership lock) |
| Contract | `feltdb::agent_transition_contract`, id `feltdb.agent.transition`, **version 1** |
| Records | `_feltdb.AgentTask`, `_feltdb.AgentCheckpoint`, `_feltdb.AgentIntent` (the contract's); `chip.Observation`, `chip.WorkResult` (Chip's, write-once, cited as evidence) |

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
                  checkpoint -> running            (the attempt is this checkpoint's version)
each execution    renew; record_intent             (until this commits, the action does not start)
                  run the action
                  write chip.Observation; record_outcome citing it
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

A resumed attempt runs Chip's bounded loop again from the goal. Earlier attempts' intents, outcomes,
observations and results stay in FeltDB; intent keys carry the attempt (`a<attempt>.<execution id>`),
so a new attempt never collides with an old one.

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
* Chip's in-memory evidence is not restored into a resumed loop; the resumed attempt observes again.
* One process owns a database file at a time (FeltDB's ownership lock); a second process opening the
  same file is refused with a storage error.
* Model calls are not intents: the contract governs actions, and FX already reports each model call's
  delivery certainty.
