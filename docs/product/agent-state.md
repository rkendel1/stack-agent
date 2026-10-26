# Durable agent state through FeltDB

Every `stack-agent work` and every `stack-agent serve` work is persisted in FeltDB: its intents,
observations and result, every event of its trajectory with the provenance of its fact, and the
host's verification verdict. `--no-state` turns it off.

| | Database | Task id | If the database cannot hold the work |
| --- | --- | --- | --- |
| default | the project's own, `<data>/agent-state/<token>.feltdb` (`<data>` as for learning; the token is a hash of the project's canonical path) | the work's own id, `work_<16 hex>` (random; unique across processes and restarts) | the work runs, says on stderr and in `agent_state.reason` that it was not persisted, and its exit status is unchanged: the default database is a record, like learning |
| `--state-db FILE` | `FILE` | the work's own id, or `--task-id ID` | the work is refused before it starts (exit 3), and a work FeltDB did not record as done is not reported as done |
| `--task-id ID` | `--state-db FILE`, or the default | `ID`: an existing task is resumed | as `--state-db` |

`serve` takes `--state-db FILE` and `--no-state` too, and holds its database for as long as it
runs. Before this, every CLI work was called `work` (so its execution ids, `work-exec-1`, repeated
from run to run), nothing was persisted without both `--state-db` and `--task-id`, and `serve`
persisted nothing.

## What is used

| | |
| --- | --- |
| Source | the published `feltdb` crate from crates.io, version 0.2.0 (`rkendel1/flow_db`, `crates/feltdb`), a registry dependency of `crates/chip-agent-state`; `Cargo.lock` pins the release and its checksum. Building needs no access to the FeltDB repository. |
| Crate | `feltdb` 0.2.0 |
| API | `feltdb::agent_state::AgentState` (`new`, `with_clock`, `read`, `apply`) over `feltdb::FeltDb::open` (a file-backed log with a per-process ownership lock) |
| Contract | `feltdb::agent_transition_contract`, id `feltdb.agent.transition`, **version 1** |
| Records | `_feltdb.AgentTask`, `_feltdb.AgentCheckpoint`, `_feltdb.AgentIntent` (the contract's); `chip.Observation` (schema `chip.observation.v2`, which adds the validated request; `v1` records are still read), `chip.WorkResult` (Chip's, write-once, cited as evidence; schema `chip.work_result.v2` adds the host's verdict, `v1` records are still read), `chip.WorkEvent` (schema `chip.work_event.v1`, one per work event, write-once), `chip.ModelResponse` (schema `chip.model_response.v1`, one per model response, write-once; see [Model responses](#model-responses)), `chip.Episode` (schema `chip.episode.v1`, one per finished attempt, write-once; see [episodic-memory.md](episodic-memory.md)) |

No npm package, JavaScript binding or sidecar is involved. The FeltDB agent-transition contract v2
(`flow_db/adrs/agent-transition-contract-v2.md`) is a proposal and is not implemented; nothing here
depends on it, and `chip-agent-state` refuses any contract other than v1 (`check_contract`).

## Ownership

* **FeltDB** owns durable agent state: every transition is decided by its contract and committed
  through one guarded transaction carrying version, lease and fence preconditions.
* **Chip** (`chip-agent-state`) maps its work onto that contract. It decides no transition, keeps no
  state of its own, and never retries a refused or conflicting commit.
* **stack-agent** opens the database and composes it into `work` and `serve`.
* **FX** is unaffected: model delivery failures are reported exactly as before.

## Lifecycle

```text
work starts       open FILE; create_task(ID) or, if it exists, acquire it (fence + 1)
                  reconcile every unresolved intent from its chip.Observation record
                  read back every recorded observation (the trajectory to resume from)
                  checkpoint -> running            (the attempt is this checkpoint's version)
each event        write chip.WorkEvent             (as the loop records it, before the next step)
each response     write chip.ModelResponse         (as the loop receives it, before it is interpreted)
each execution    renew; record_intent             (until this commits, the action does not start)
                  run the action
                  write chip.Observation (with the validated request); record_outcome citing it
work ends         write chip.WorkResult (with the host's verdict); checkpoint -> completed | blocked | cancelled
after the end     write chip.Episode citing the result and observations (work only; changes nothing above)
```

Before the first turn, `work` also recalls verified episodes of other works of the same project as
evidence for its model requests ([episodic-memory.md](episodic-memory.md)); `--no-recall` turns that off.

## Work events and provenance

Intents, observations and the result say what was executed and how the work ended. `chip.WorkEvent`
says everything else, one record per event of Chip's trajectory, in order (`<task>~a<attempt>~e<index>`):
the start, each decision and who made it, each model call and the usage the provider reported, each
capability request, each execution, observation and piece of evidence, each evaluation of the goal,
each limit, and the terminal state. A record is written as the loop records the event, through the
same guarded "must not exist yet" transaction every Chip record uses, so a killed process leaves
every event it had recorded.

Each record says where its fact came from (`chip_agent_state::Provenance`):

| Provenance | Meaning | Events |
| --- | --- | --- |
| `host_observed` | the host saw it happen | capability availability, `ExecutionRequested`/`Started`/`Completed`/`Failed`, `ObservationRecorded`/`Restored`, `EvidenceRecorded`/`Reused`, `GoalEvaluated`, `ModelCalled` (that the call returned; its token counts are the provider's report) |
| `model_inferred` | the model said it; a claim, never evidence | `DecisionMade`, `CapabilityRequested` |
| `runtime_decided` | Chip decided it | `WorkStarted`, `DecisionStarted`, `LocalDecision`, `ModelEscalation`, `ContextLimit`, and every terminal event (whose reason may quote the model, as text) |

An event carries no prompt and no execution output. The goal is recorded as its SHA-256 and size
(the objective is on the task), and an execution's output is its `chip.Observation` record, which
the event names (`observation_record`) with the output's size and SHA-256.

A trajectory is whole: positions 0, 1, 2, ... of each attempt without a gap. If a record cannot be
written, recording stops there (the trajectory ends early, it never skips), the failure is a
persistence failure, and like every persistence failure it keeps the attempt from being persisted
as complete. A history whose events skip a position is refused as invalid.

## The host's verdict

`chip.work_result.v2` adds `verification` to the result: `accepted` (the host accepted the outcome:
exit status 0, which is what a completion is persisted on), `verified` (PAX verification passed on
evidence the host observed), `goal_satisfied`, `grounded`, `evidence_invalidated` (why evidence the
work relied on was refused: a replaced test file, a change after the passing run) and `exit_status`.
It is the host's, never the model's: the model's completion claim is a `model_inferred` decision
event.

## Interrupted work

A task still `running` that this process is not running was interrupted: its process ended before
it finished. A running work holds the project lock (`chip_project::ProjectLock`), so in one project
no other process is running one. A new `work` reports each interrupted task on stderr (`note: work
<id> was interrupted ...`) and in `agent_state.interrupted`; `serve` reports them when it starts.
Nothing is resumed or rerun on its own: the new work is a new task. `--task-id <id>` resumes an
interrupted task, through the recovery below, once its lease has expired.

A restarted `serve` answers `GET /v1/work/{id}` and `GET /v1/work/{id}/events` for a work it does
not hold from FeltDB: `persisted: true`, the task's status (`interrupted` for a task still
`running`), the result and verdict if there is one, and the recorded events with their provenance.
Before, every earlier id answered 404 (audit HA-18).

What a restarted service says about earlier work, and what it does not do (pinned by
`crates/stack-agent/tests/service_recovery.rs`, which kills and restarts the real binary):

| Earlier work | A restarted `serve` answers |
| --- | --- |
| Finished (`completed`, `blocked`, `cancelled`) | `200`, `persisted: true`, the recorded outcome, the host's verdict as `result.verified`, `result.goal_satisfied`, `result.grounded` (and in full as `result.verification`; `null` if no verdict was recorded), and every event in order with its provenance. Nothing is recomputed; a historical `verified: true` is a fact about that attempt only. |
| Running when its process ended | `200`, `status: interrupted`, `result: null`. It is not resumed, replayed or verified by being recovered, and its events up to the kill are all there. |
| Held by FeltDB but not readable consistently (a gap in its events, a record that does not parse) | `500 work_record_unreadable`, never `404`. Nothing is inferred from what could be read; other works are unaffected. |
| Only accepted, still queued (`ready`) | `200`, `status: queued`, and the work is queued again at startup and started once (see [Durable admission](#durable-admission)); until it is scheduled, `scheduled: false` and a `reason`. A cancel of it is recorded before it is reported. |
| Never issued | `404 work_not_found`. |
| Evicted from the live registry in this process | `410 work_expired`, as before. |

`POST /v1/work/{id}/cancel` on a recovered work is `409` (`work_already_finished`, or `work_not_running`
for an interrupted one) and changes nothing: only a work this process is running can be cancelled.
Startup reconciliation reads the database (the same scan `work` does) and reports interrupted tasks; it
writes nothing, so running it again changes nothing.

There is no automatic resumption. An interrupted work may have written files or run tests whose
completion no receipt records, and most capabilities declare neither an idempotency key nor a safe
re-observation, so replaying it could repeat an effect. Continuing is the explicit `--task-id` path
below, which refuses (`OutcomeUncertain`) whenever it cannot show that an action did not run.
A submission carries no idempotency key: posting the same request twice starts two works. Neither is
exactly-once execution.

## What the database holds

The goal, as the task's objective (at most 1,024 bytes; a longer goal is cut for the default
database), and each observation's output: a file a work read, a test run's output. That is what a
resumed attempt is restored from, and it is what `--state-db` always held. Learning documents, by
contrast, never hold the goal or file contents (`learning.md`); the two stores are separate. The
default database's directory, `<data>/agent-state/`, is created mode 0700, and withheld files
(`chip_project::policy`) are never read, so never recorded. Work events hold neither the goal nor any
output, only their hashes and sizes.

## Inspecting it

`stack-agent agent-state list` lists the database's tasks and which were interrupted;
`stack-agent agent-state show --task-id ID` shows a task's intents, results and verdicts, and its
events with their provenance. `stack-agent agent-state export --task-id ID` prints the task's record
as an OpenTelemetry trace (`--send` delivers it); `work` and `serve` export each finished work
when `OTEL_*` asks for it ([`telemetry.md`](telemetry.md)). Without `--state-db` they read the
current project's default database.

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
  its outcome, or the executor answered `ExecutionError::OutcomeUnknown`: it started the action and
  cannot say how it ended, for example a request sent with no answer back; durable state then records
  no outcome for it, since it has none): if its `chip.Observation` record exists, the intent is reconciled from it; if not, the
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

## Durable admission

A work is a FeltDB task from the moment `POST /v1/work` accepts it, not from the moment it starts.
`chip_agent_state::admit` writes a write-once `chip.Admission` record (the work's kind and its goal
whole: the contract's objective holds neither) and then `create_task`, which leaves the task `ready`:
FeltDB's own status for a task that exists and has not started. Only then does the service put the
work in its queue and send the 202. There is one task and one identity; the admission record is keyed
by the same id and adds no state. If either write fails the request is refused (`503 admission_failed`)
and nothing is queued; an admission record with no task was never acknowledged and is ignored.

The task is created holding its lease for the contract's shortest term, 1 ms, so the next owner takes
it over with `acquire` once that has passed. Starting is the ordinary `begin` of any existing task,
`ready -> running` ("resumed from ready"), and FeltDB grants one lease, so two starters cannot both
run it. A work whose `begin` fails is not run unpersisted (FeltDB would still hold it as waiting, and
a restart would run it again); the service fails it closed.

`chip_agent_state::cancel_admitted` is `ready -> cancelled` through the contract's `acquire` and
`checkpoint`. A queued cancellation is reported only once it is recorded, and a cancelled task is not
`ready`, so a restart does not start it.

**Startup.** `chip_agent_state::waiting` lists the `ready` tasks, oldest first, each with what is
known of its admission. They provably never started: `begin` checkpoints `running` before the first
model call. The service queues them again under the limits a new submission faces (workers plus
queue; the rest wait in an id-only backlog and move in as room opens). A `ready` task with no
admission record, an unreadable one, or a `change`/`verify` one while no test runner can be found is
left exactly as it is and reported; nothing is repaired or deleted. A `running` task is
`interrupted` and is never rerun, a finished or cancelled one is never queued. Reading and queueing
write nothing, so a repeated startup changes nothing.

**Every entry point admits; only a service admission is started by a restart.** `stack-agent work`
admits the task it is about to run, the same way the service does, with `origin: cli`, and then
starts it (`ensure_admitted`, then `begin`). `origin` is `service` by default and omitted from the
record, so records written before the field existed, and readers that predate it, are unaffected.
What differs is who may start a task again after a crash. A `service` admission was acknowledged to a
client (a `202`), so a restarted service queues it. A `cli` admission was not: nobody was told it
was queued, and running a stale command line later would surprise the person who typed it. It is
continued only explicitly, with `stack-agent work --task-id`. A continuation (`--task-id` naming a
task that exists) never admits again and never creates a second identity, and it is held to the kind
the task was admitted as: an inspection is not continued as a change.

What a crash can leave, and what each entry point does with it:

| Durable state | Meaning | Restarted service | `work --task-id ID` |
| --- | --- | --- | --- |
| record only (no task) | a crash after the record, before the task; nobody was told | ignored; it names no task | the same request completes the admission and runs; a different one is refused, nothing runs |
| `ready` + `service` admission | accepted (`202`), never started | queued again, once, under the usual limits | continues it (same kind, same goal) |
| `ready` + `cli` admission | made by `work` for its own run, never started | reported, not started: `scheduled: false`, reason says so | continues it |
| `ready` + admission, lease still held | someone may be starting it | left until the lease runs out, reported | refused while the lease is held |
| `ready` + no admission | made below this layer (a direct `begin`), or the record was lost; what it was asked is unknown | reported, not started | continues it with an explicit goal |
| `ready` + unreadable admission | damaged | reported, not started, not repaired | continues it |
| `running` | its process ended; effects may have happened | `interrupted`, never rerun | refused while its lease is held; then the existing recovery (`OutcomeUncertain`, settlement, re-observation) |
| `blocked` | stopped before finishing | reported as it ended | resumes it |
| `completed`, `cancelled` | finished | reported; never queued again | reports it; runs nothing |

The only callers that create a task without admitting it are the lower-level ones that call
`begin` directly (the durability benchmark and one example); they deliberately exercise the state
machine and are not entry points. `work` and `serve` cannot produce a `ready` task without a record.

**Not guaranteed.** Without durable state the 202 says `durable: false`. There is no idempotency
key on a submission: a client that retries a POST starts another work, and two requests with the same
words are two works. Neither is exactly-once execution.

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

**Cancellation and deadlines apply.** `reobserve` is given the run's `WorkControl`, the one the work
loop asks before every execution. It asks before it records the new intent and again at the
executor's door. A cancellation request or an expired deadline at either point means nothing is
executed, the intents stay open, the lease is given back and the run exits 3. (A stop at the
executor's door leaves the recovery's own intent recorded as a failed execution that did not run.)
Nothing retries on its own; the next attempt tries once.

**What it records, and what it does not claim.** The earlier response is gone. The new observation
is the capability's answer *now*, which may differ from the lost one, and no record says it is the
lost one.

* The new intent's `action` is `<capability> binding=<b1…> reobserves=<old intent key>[,<key>…]
  inputs=<JSON>`. The new `chip.Observation` is under a new execution id
  (`<task>-reobs-<attempt>-<n>`) and a new intent key, and is an ordinary record.
* The old intent has no observation record of its own. It is resolved with a citation of a
  *different* execution's observation.
* Contract v1 allows only `executed` or `not_executed` as a `reconcile` conclusion, and neither is
  known of the lost execution. The old intent is reconciled `not_executed`. **That value is not
  evidence that the lost execution did not run.** Here it means only that the lost execution is not
  credited with an effect; whether it ran is unknown, and is not claimed either way. What the
  records say happened is the `reobserves` marker and the cross-execution citation. Nothing reads an
  intent's `conclusion` to decide anything: FeltDB stores it unread, and in Chip only `agent-state
  settle`'s refusal message and `agent-state show` print it. Anything that did read it as "proven not
  executed" would misread this path, and would need a contract change, not a reinterpretation. An
  operator settlement is never used.
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
model's decisions: the model's own requests are unchanged. It is bounded by the number of open
intents the earlier attempt left (in practice one).

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

## Reconciliation

`chip_agent_state::reconcile` is the second way out of an unknown outcome, beside operator
settlement. For a capability that carries an action key and has a `Reconciler`, it asks the
destination what happened to the **original** operation, by its key and binding, and never
dispatches it. Strong evidence resolves the intent (`Completed`: the destination committed it and
its business effect is present; `NotExecuted`: it did not apply and the destination guarantees it
cannot); anything weaker is `Indeterminate`, the intent stays open, and the next start still
refuses it. Each query is recorded (`chip.Reconciliation`) with what the destination said and why it
was not enough, and `agent-state show` lists it under `reconciliations` and `unresolved`. Nothing
has a `Reconciler` except `project.write` (`stack-agent agent-state reconcile`, which locks the project while it asks); see
[the recovery matrix](../research/durable-harness/recovery-matrix.md#reconciliation).

## Model responses

A model response is the model's judgment, never evidence. It is still worth keeping, because the
call was paid for. Each response a model call returns is recorded as a `chip.ModelResponse`
(`<task>~a<attempt>~m<turn>`), written once, as the loop receives it and before it is interpreted
or anything it asks for is attempted.

The record holds:

* the model the work asked, the provider's response id, the text and its SHA-256, and the usage
  the provider reported;
* the executions whose observations the work held when it asked;
* the intents the attempt had recorded by then.

**What a restart does with it.** Before this, a process that died after a response arrived and
before anything it asked for was attempted paid for that turn again on resume, and might have
decided differently. Now `begin` offers the interrupted attempt's last response back, but only when
every record agrees nothing was attempted after it:

* the attempt has no work result;
* its last `ModelCalled` event is that response's turn and succeeded;
* every event after it is the decision and the request it made (no execution, evidence reuse, new
  turn or terminal event);
* the attempt's intents are exactly the ones it had when the response arrived;
* no intent of the task is open.

The loop (`WorkSpec::restored_response`) then takes it for its first escalation only if the work is
still exactly where the response was received: the same model, the same observations in the same
order, and nothing executed or reused yet in this run. Otherwise it records `ModelResponseDiscarded`
with the reason and asks the model, as before.

**Taken, it changes nothing about authority.** No model call is made (`ModelResponseRestored`,
`runtime_decided`, then the response's own `DecisionMade`, `model_inferred`). The response is
interpreted, validated and authorized exactly like a fresh one, and an action it asks for goes
through the durable executor: an intent first. `work --json` reports what `begin` decided in
`agent_state.model_response`.

**What it does not do.**

* **A response that was acted on is never given back.** Once an intent was recorded after it, the
  intent is the authority. An interrupted action stays `OutcomeUncertain`, or is re-observed or
  reissued exactly as before.
* **A call that never returned is not replayed**, because there is nothing to replay. The last
  `ModelEscalation` of an unfinished attempt with no `ModelCalled` after it is a call whose outcome
  (received, answered, charged) is unknown. `agent-state show` lists it under
  `model_calls_in_flight`, `work --json` reports it in `agent_state.model_call_in_flight`, and the
  resumed attempt asks again.
* **A response cut off mid-stream is never recorded.** FX reports it as a failed call, and an
  incomplete response is not a decision.
* **It is given back once.** The attempt that takes it does not record it again. If that attempt
  is also killed in the same window, the next one asks the model, as without a record.
* **No determinism is assumed.** The given-back response was made when the work's turn counter and
  decision history may have been different. Only the model and the observations are required to
  match.

**Tests.**

* `crates/chip-agent-state/tests/model_response.rs`: crashes at gates in-process, and a reopened
  database.
* `crates/stack-agent/tests/crash_windows.rs`: a real `SIGKILL` of the binary as the `pax.test` the
  model chose is validated; the resumed process makes no model request and runs the test once.

**Compatibility.** A new collection, read only when present: a database written before it has no
model responses and behaves as before. A record whose text does not match its digest, or that
names another task or place, is `InvalidHistory`, and nothing runs.

## Failures

* The intent cannot be recorded: the action is not started; the execution, and so the work, fails.
* The outcome cannot be recorded after the action ran: the execution is reported as failed and the
  intent stays unresolved in FeltDB, which is the truth (the action may have happened).
* The terminal checkpoint is refused or fails: the run's report says the outcome was not recorded,
  and, with `--state-db` or `--task-id`, a run that would otherwise exit 0 exits 4. With the default
  database the exit status is unchanged.
* A completion with no observation behind it is checkpointed `blocked` and, with `--state-db` or
  `--task-id`, exits 1.
* A work event cannot be recorded: recording stops, and the attempt is checkpointed `blocked`.
* Stored history that is incomplete (a missing checkpoint version, an intent count that does not match)
  or written under an unknown Chip record schema is refused, never read around.

## Limits

* **Zig `fx` conversational turns are not stored in FeltDB.** Their owner is the Zig-compatible
  session store (`chip-session`, `chip-turns`, [`session-format.md`](session-format.md)). A work's own
  events are not conversation turns; they are `chip.WorkEvent` records here, next to the rest of the
  work's durable state.
* **One database per project, one process at a time.** A `work` that starts while `serve` holds the
  project's database (between that service's works) cannot open it, and runs unpersisted with a note.
* **The default database grows**: nothing prunes finished tasks yet.
* Contract v1 records are closed (`deny_unknown_fields`), so Chip's richer data lives in its own
  write-once records, cited by the checkpoints and outcomes.
* Restored observations join the trajectory but not the agent's in-memory evidence store, so they
  never answer a later request by reuse. A request made after a restart is performed again.
* One process owns a database file at a time (FeltDB's ownership lock); a second process opening the
  same file is refused with a storage error.
* Model calls are not intents: the contract governs actions, and FX already reports each model call's
  delivery certainty. A received response is recorded, and given back after a restart only as
  [Model responses](#model-responses) says; a call that never returned is asked again.
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
* **`ExecutionFailed` is a definite outcome, by the executor's word.** Durable state records it as a
  resolved, failed outcome (with `executed: true` unless the request was refused before it ran), and a
  later attempt may ask for the action again. An executor that started an action and then cannot
  establish how it ended must answer `OutcomeUnknown` instead. The built-in consequential executors
  now do: `pax.test` when it outlasts its time limit or its result is oversized or unreadable, and
  `project.write` when the blocking write task does not return. Chip cannot check this for an
  external executor; see [the recovery matrix](../research/durable-harness/recovery-matrix.md).
* **A lease that has expired is not a stopped worker.** An action in flight when the lease ran out
  can still finish at its destination. Its late record is refused (the fence, and the observation
  id the operator's settlement may already hold), so it cannot change the authoritative outcome;
  but an operator who settles an intent while the previous worker may still be running can be
  wrong. Within the product one process owns the database file, which is what rules this out.
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
