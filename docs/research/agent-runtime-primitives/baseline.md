# Baseline: stack-agent's own agent runtime

Phase 1 of the agent-runtime-primitives study. This is the architecture map that every candidate is
compared against. It follows [`protocol.md`](protocol.md): every finding carries one evidence tag,
and durability is split into D1 to D4.

| | |
| --- | --- |
| Repository | `rkendel1/stack-agent` (this worktree, `origin/main`) |
| Inspected commit | `366f6c13c243f8bd419e593e0bfbe67d071c9d3e` (2026-10-19, "Merge pull request #10 ... record the first real-model durable/legacy paired run") |
| Immutable link | https://github.com/rkendel1/stack-agent/tree/366f6c13c243f8bd419e593e0bfbe67d071c9d3e |
| Source references | `path:line` below means `https://github.com/rkendel1/stack-agent/blob/366f6c13c243f8bd419e593e0bfbe67d071c9d3e/<path>#L<line>` |
| FeltDB | `rkendel1/flow_db` at `b24481360f7c10564ae09fcc5bbf7802725eccd9`, crate `feltdb` 0.2.0, contract `feltdb.agent.transition` v1 (`crates/chip-agent-state/Cargo.toml:13`) |

Experiment status: `cargo test --offline -p chip-agent-state` was attempted to reproduce the
crash-window tests. It got as far as compiling `feltdb`, then failed with "No space left on device"
(the root filesystem was 99% full). **[blocked]** No `[experiment]` tag below comes from this phase.
The bench numbers in §8 are the repository's own recorded results. They are tagged `[claim]`,
because they were not reproduced here.

---

## 1. Execution loop (`chip-core`, `Agent::run_work*`)

### 1.1 Shape and turn boundaries

* Entry points are `Agent::run_work(spec, policy, boundary)` and
  `run_work_with_context_policy(..., context_policy)`. `run_work` uses `FullEscalationContext`.
  [source] `crates/chip-core/src/work.rs:2154`, `:2166`
* One iteration ("turn") makes exactly one `WorkDecision`. The four decisions are `Complete`,
  `Escalate`, `Block` and `RequestCapability`, and the last of these is data, not execution.
  [source] `crates/chip-core/src/work.rs:111-122`, module doc `:1-7`
* Capabilities are discovered once, before the loop. The loop never re-discovers.
  [source] `crates/chip-core/src/work.rs:2281-2294`
* Each turn runs in this order. [source] `crates/chip-core/src/work.rs:2298-2470`
  1. stop check (`WorkControl::stop_requested`)
  2. turn-limit check
  3. `DecisionStarted` event
  4. local proposal
  5. a model escalation, if the local side gave no decision
  6. `DecisionMade`
  7. one of: a terminal return, evidence reuse, or `perform` (validate, execute once, observe,
     record evidence, evaluate goal)
* Concurrency is sequential: one decision, and at most one execution, per iteration. Each is
  awaited before the next turn. There is no parallel tool dispatch. [source]
  `crates/chip-core/src/work.rs:2455-2469`, `:1998-2083`. The HTTP service runs several *works*
  concurrently, each with its own loop. [source] `crates/stack-agent/src/service.rs:3-5`

### 1.2 Local policy vs. model escalation

* `LocalWorkPolicy::propose(&WorkView) -> Option<WorkDecision>` is pure and synchronous. `None`
  escalates to the model. [source] `crates/chip-core/src/work.rs:268-272`
* A locally proposed `RequestCapability` is first assessed against evidence
  (`assess_evidence`). [source] `crates/chip-core/src/work.rs:2332-2389`
  * Reuse means nothing executes.
  * `Continue` means the request executes.
  * `Escalate` means the model is asked.
  * If no `LocalReasoner` is installed (the product configuration), a request without valid
    evidence goes to the model, not to execution (`:2373-2384`).
* A capability declared without evidence reuse skips the evidence check.
  [source] `crates/chip-core/src/work.rs:2319-2326`
* A local `Complete` that the required observations or answers do not support is refused. It
  becomes an ordinary escalation. [source] `crates/chip-core/src/work.rs:2393-2402`
* The learned "PR25" local model is not wired in as an authority. [source]
  `crates/chip-core/src/work.rs:13-15`
* Production policies are `CompleteWhenStateVerified` and `CompleteWhenVerified`. [source]
  `crates/stack-agent/src/software_work.rs:480`, `:702`

### 1.3 Decision contract `chip.work-decision.v1`

* The model's reply is one JSON object with one of four shapes. [source]
  `crates/chip-core/src/model_decision.rs:1-19`, `:31`
  * `request_capability {capability, inputs?}`
  * `complete {summary}`
  * `escalate {reason}`
  * `block {reason}`
* Limits [source] `crates/chip-core/src/model_decision.rs:34-37`
  * the reply is at most 4,096 bytes, raised by 6 bytes for each byte of the largest declared
    text input (`:550-556`);
  * a text field is at most 1,024 bytes;
  * at most 16 inputs.
* The parse is strict. These are all errors: prose around the object, unknown decisions, missing
  or extra fields, duplicate keys, arrays, null, floats, an undeclared or unavailable capability.
  One surrounding Markdown code fence is tolerated, and nothing else.
  [source] `crates/chip-core/src/model_decision.rs:17-19`, `:386`, `:405-435`, `:454-457`,
  `:600-609`
* The model never names an execution. The parser emits `ExecutionId::unassigned()`
  (`model_decision.rs:612-615`; field at `:614`). The loop then assigns `<work id>-exec-<n>`
  (`work.rs:1958-1962`, `:1971-1978`). [source]
* The provider-facing schema has two projections of the same contract, `Full` (`anyOf`) and
  `Flat`. Whichever is sent, the runtime parse stays authoritative. [source]
  `crates/chip-core/src/model_decision.rs:110-121`, `:152-155`
* A malformed reply ends the work `Failed("the model's response is not a valid decision")`. There
  is no repair and no re-ask. [source] `crates/chip-core/src/work.rs:1951-1956`

### 1.4 Termination, limits, deadline, cancellation

* Terminal outcomes are `Completed`, `Escalated`, `Blocked`, `LimitReached{Turns|Executions|Context}`,
  `Failed{reason, delivery}` and `Cancelled{cause, reason, delivery, remote}`. [source]
  `crates/chip-core/src/work.rs:173-211`
* Each terminal outcome emits exactly one terminal event. `audit_safety` counts any event after it
  as `events_after_terminal`. [source] `crates/chip-core/src/work.rs:2099-2150`, `:2509-2510`
* `WorkLimits { max_turns: 8, max_executions: 4 }` is the default. The turn limit is checked
  before each turn (`:2300`). The execution limit is checked before each execution (`:1999`).
  [source] `crates/chip-core/src/work.rs:77-91`
* The context budget (`context_budget_bytes`) fails closed. A request over budget is not sent,
  trimmed or retried; the outcome is `LimitReached{Context}`. [source]
  `crates/chip-core/src/work.rs:1897-1911`
* Cancellation and deadline go through the `WorkControl` trait, which is cooperative. [source]
  `crates/chip-core/src/work.rs:229-241`, `:2298`, `:1912`, `:2012-2015`
  * It is polled before each turn, before each model call, and before each execution.
  * An execution already in flight is **not** interrupted.
* The product `RunControl` implements cancellation and a whole-work deadline (`--deadline-secs`).
  [source] `crates/stack-agent/src/software_work.rs:1765-1829`
  * `CancelBeforeModelCall` refuses new model calls after a stop.
  * It also hands the stop to FX (`chip_fx::with_call_cancel`), which abandons the call in flight
    and keeps its delivery certainty. [source] `crates/stack-agent/src/software_work.rs:1831-1860`,
    `crates/chip-fx/src/native.rs:56`
* The two timeouts live outside the loop:
  * a per-model-call wall-clock timeout (`--call-timeout-secs`, default 30 s), enforced by the
    native provider (`timeout_global`) [source] `crates/stack-agent/src/work_generation.rs:12-15`,
    `crates/fx-native-provider/src/provider.rs:333`;
  * per-executor timeouts (for example PAX 300 s, git, compute 30 s) [source]
    `crates/chip-pax/src/lib.rs:618`, `crates/chip-project/src/git.rs:207`,
    `crates/chip-compute/src/lib.rs:146`.
  * The `Executor` trait itself carries no timeout. [source] `crates/chip-core/src/lib.rs:419-424`

### 1.5 Retries and error handling

* There are **no retries**: one model call per escalation (`work.rs:18`, `:2405`), and no second
  attempt anywhere. Retries and repair loops are forbidden by `AGENTS.md:803-804` (§14), and
  "Recovery Is Not Retry" is `AGENTS.md:962-999` (§21). [source]
* A model failure ends the work `Failed{delivery}`. `delivery` is
  `definitely_unsent | possibly_sent | confirmed_response`, so a caller can decide whether its own
  retry is safe. If the failure arrived together with a stop, the outcome is `Cancelled` instead.
  [source] `crates/chip-core/src/work.rs:1942-1950`, `crates/chip-inference/src/lib.rs:118-128`
* `InferenceFailure.retry_after_ms` is recorded and never acted on. [source]
  `crates/chip-inference/src/lib.rs:179-180`
* Execution errors map as follows. [source] `crates/chip-core/src/work.rs:2004-2045`
  * `ExecutorUnavailable` gives `Blocked`.
  * Other execution errors give `Failed`.
  * Validation failure gives `Blocked`, and the executor is never called.
  * An execution whose result is `Failure` is recorded as an observation and noted in
    `ruled_out`. The loop then continues to the next turn (`:2073-2078`). This is the only
    "recovery", and it is a new bounded judgment by the model, not a retry.

### 1.6 Separating model claims from authoritative outcomes

* Only an observation of an execution that this loop performed becomes evidence. A request, a
  model response or a plan never does. A cancelled execution is never retained. [source]
  `crates/chip-core/src/work.rs:20-22`, `crates/chip-core/src/lib.rs:650-668`
* Goal evaluation reads observations only. [source] `crates/chip-core/src/work.rs:1754-1822`
  * `required_outputs` must match a successful observation's output exactly.
  * `required_observations` are `ObservationPredicate`s over the whole trajectory, and one can
    stop holding later.
* A completion is refused when a requirement is unmet or the answer is unsupported
  (`AnswerPredicate`). Either case ends the work `Blocked("completion refused: ...")`; it does
  not continue. [source] `crates/chip-core/src/work.rs:1725-1736`, `:2421-2439`
* Context condensation is checked before sending. Each condensed item must be one recorded
  successful execution: not repeated, not also sent in full, and with a correct byte count.
  Otherwise the work fails and nothing is sent. [source] `crates/chip-core/src/work.rs:1830-1878`,
  `:1889-1895`
* Two after-the-fact auditors read only the event stream and the observations, never the loop's
  counters. [source] `crates/chip-core/src/work.rs:1155-1290`, `:2481-2780`
  * `verify_trajectory(events, limits)` checks structure and limits.
  * `audit_safety(report, spec, declared)` counts each of: unauthorized executions or completions,
    false completions, evidence without observation, observation without execution, limit
    violations, invariant violations, stale reuse, context-budget violations, and unjustified
    omissions.
* The summary counters are re-derived from events and `debug_assert`ed against the loop's
  counters. [source] `crates/chip-core/src/work.rs:2216-2244`

## 2. Tool dispatch

* `CapabilityProvider` declares descriptors, availability and `validate_inputs`. `Executor` runs
  an `ExecutionRequest { id, intent, inputs }`. [source] `crates/chip-core/src/lib.rs:246-264`,
  `:309-316`, `:421-424`
* `CapabilitySet` routes by declared capability id to exactly one backend. [source]
  `crates/chip-core/src/capability_set.rs:1-7`, `:36-59`
  * It never falls back.
  * Two backends declaring one id is an error, and the set fails closed.
* `AllowlistedBackend` only narrows. It refuses an id that is not on its list in each of
  declare, availability, validate and execute. [source]
  `crates/chip-core/src/capability_set.rs:114-179`
* The product composes one filtered set. That set is what the model is offered, what Chip
  validates against and what executes. An inspect goal is always narrowed to read-only
  capabilities. [source] `crates/stack-agent/src/software_work.rs:1124-1144`
* Input validation in `validate_capability_request` checks, in order: [source]
  `crates/chip-core/src/lib.rs:879-927`
  1. the capability is declared;
  2. it is available;
  3. there are no undeclared inputs;
  4. an input-less capability carries no `inputs` member;
  5. required inputs are present;
  6. the backend's own `validate_inputs` passes.
* An execution counts only if `ExecutionStarted` was emitted. [source]
  `crates/chip-core/src/lib.rs:997-1040`, `crates/chip-core/src/work.rs:2019-2028`
* Evidence reuse uses an **in-memory, per-`Agent`** store, keyed by capability plus normalized
  inputs. [source] `crates/chip-core/src/evidence.rs:1-8`, `:16-22`, `:34-52`,
  `crates/chip-core/src/work.rs:1980-1996`, `:2448-2454`
  * The key excludes the execution id.
  * An optional `StateToken` marks evidence stale when the state changes.
  * A capability declared `without_evidence_reuse` is always re-executed.
  * A model request gets the same evidence check as a local one.
* The model provider and the executor are separate injections. Model output reaches an executor
  only as a declared id and typed inputs. [source] `crates/chip-core/src/lib.rs:541-551`,
  `crates/chip-core/src/model_decision.rs:6-7`

## 3. Durable state (`crates/chip-agent-state`, FeltDB `feltdb.agent.transition` v1)

### 3.1 Records and lifecycle

* FeltDB is the authority. Chip "decides nothing": it maps work onto the contract's operations
  and reports what FeltDB said. A contract other than v1 is refused (`check_contract`). [source]
  `crates/chip-agent-state/src/lib.rs:1-6`, `:196-205`
* The records are FeltDB's `_feltdb.AgentTask`, `AgentCheckpoint` and `AgentIntent`, plus Chip's
  write-once `chip.Observation` and `chip.WorkResult`, which are cited as evidence. [source]
  `crates/chip-agent-state/src/lib.rs:62-68`, `docs/product/agent-state.md:14`
* Mapping of work events to FeltDB operations [source] `crates/chip-agent-state/src/lib.rs:8-16`,
  `docs/product/agent-state.md:31-39`
  * **Start:** `create_task`, or `acquire` for an existing task (this advances the fence); then
    reconcile open intents; then `checkpoint -> running`.
  * **Per execution:** `renew`, `record_intent`, the action, a `chip.Observation` create, then
    `record_outcome` citing that observation.
  * **End:** a `chip.WorkResult` create, then `checkpoint -> completed | blocked | cancelled`.
* Fencing and leases. [source] `crates/chip-agent-state/src/lib.rs:74`, `:706-747`,
  `docs/product/agent-state.md:51-52`
  * Every owner operation carries the `fence` and the `lease_id` (`:851-860`).
  * The default lease is 900 s, and there is no forced takeover in v1.
  * An unexpired lease held by another owner gives `LeaseHeld`.
* The intent key is `a<attempt>.<execution id>`, with a SHA-256 fallback. The attempt is the
  checkpoint version. [source] `crates/chip-agent-state/src/lib.rs:240-250`, `:802`
* Completion is persisted as `completed` only with all three: [source]
  `crates/chip-agent-state/src/lib.rs:1010-1036`
  * at least one observation from this attempt;
  * no persistence failure;
  * the host's `accepted` verdict (stack-agent's verification).
  Otherwise the checkpoint is `blocked`, with `completion_refused`. `Escalated`, `Blocked`,
  `LimitReached` and `Failed` all checkpoint as `blocked` (`:1016-1024`).
* If `record_intent` fails, the inner executor is not called. If `record_outcome` fails after the
  action, the execution reports failure and the intent stays unresolved. [source]
  `crates/chip-agent-state/src/lib.rs:1141-1160`
* History reads are strict. Missing checkpoint versions, an intent-count mismatch or an unknown
  record schema are refused (`InvalidHistory` / `UnsupportedRecord`), never read around. [source]
  `crates/chip-agent-state/src/lib.rs:491-583`

### 3.2 `begin` and resume

* `begin(store, task_id, objective, config)` returns `Ready(DurableWork)` or `Finished(History)`.
  [source] `crates/chip-agent-state/src/lib.rs:630-846`
  * A terminal task (`completed` or `cancelled`) is not run again (`:711-717`).
  * A different objective gives `ObjectiveMismatch`.
* Each unresolved intent is reconciled from its `chip.Observation`. The conclusion is
  `executed` or `not_executed`, from `ObservationRecord.executed`. An intent without an
  observation gives `OutcomeUncertain`, and nothing runs. [source]
  `crates/chip-agent-state/src/lib.rs:752-795`, `:276-279`
* **Resume re-runs the loop from the goal.** [source] `docs/product/agent-state.md:60-62`, `:83`,
  `crates/stack-agent/src/software_work.rs:2652-2667`
  * The resumed attempt starts with an empty `Run`, and no observations or decisions are restored.
  * The in-memory evidence store starts empty too.
  * The work id equals the task id, so execution ids (`<task>-exec-n`) repeat across attempts.
    Only the intent key is unique, because it carries the attempt.
* There is no operator reconciliation command. Settling `OutcomeUncertain` means writing a FeltDB
  record by hand; the bench harness does it for the operator. [source]
  `docs/product/durability-benchmark.md:59-63`, `:164-165`,
  `crates/stack-agent/src/durability_bench.rs:628-660`

### 3.3 Idempotency of external effects

* **No idempotency key reaches an executor.** `DurableExecutor` computes the intent key but
  passes the original `ExecutionRequest` (`id`, `intent`, `inputs`) to the inner executor
  unchanged. `ExecutionRequest` has no idempotency field. [source]
  `crates/chip-agent-state/src/lib.rs:1143-1151`, `crates/chip-core/src/lib.rs:309-316`
* The execution id the executor receives repeats across attempts, and its value depends on the
  model's decision order. An executor that de-duplicated on it would be unreliable. No executor in
  the repo de-duplicates. [inferred]
* Model calls are not intents and are not persisted; FX reports each call's delivery certainty
  instead. [source] `docs/product/agent-state.md:86-87`

### 3.4 What is not persisted

* Loop state is not persisted: observations, decisions, `ruled_out`, the evidence store and
  escalation contexts are all in memory. The attempt's `chip.WorkResult` keeps only decision
  labels, per-execution turn numbers, the outcome and the persistence failures. [source]
  `crates/chip-agent-state/src/lib.rs:303-321`, `:1103-1131`
* Model attempts are not persisted: no requests, responses, tokens or delivery certainty. [source]
  `crates/chip-agent-state/src/lib.rs:303-321`, `docs/product/agent-state.md:86-87`
* Sessions and conversational turns are deliberately out of scope. They belong to `chip-session`
  and `chip-turns` (§4). [source] `crates/chip-agent-state/src/lib.rs:33-38`,
  `docs/product/agent-state.md:77-80`
* The HTTP service registry is in memory only. [source] `crates/stack-agent/src/service.rs:12-16`
* `--state-db` is opt-in, on `work` only. [source] `docs/product/agent-state.md:3-4`

### 3.5 Crash windows W1 to W6 (from the code path; since reproduced)

> **Reproduced [experiment]:** `experiments/stack-agent/` drives the release binary at this commit
> with a real `SIGKILL` at every window, including torn FeltDB records. It confirms the table below
> and adds two findings:
>
> * a resume refused with `OutcomeUncertain` has already acquired the task under a new lease, so
>   recovery after the operator reconciles waits for a second lease expiry;
> * a `SIGKILL` can tear a FeltDB record, and FeltDB truncates the torn tail when it next opens the
>   file.
>
> The experiment labels W1a `lost_progress` (protocol vocabulary), not "correct". The entries
> below remain the source reading that the experiment checked.

`intent` is FeltDB's `_feltdb.AgentIntent` record (open until `record_outcome` or `reconcile`
resolves it). `obs` is the `chip.Observation` record. All entries are [inferred] from the
`[source]` code path in §3.1–3.2, and match the bench scenarios named in brackets [claim]
(`docs/product/durability-benchmark.md:44-53`).

| Window | Persisted state at the kill | What the next `begin` does | Protocol outcome (inferred) |
| --- | --- | --- | --- |
| W1a: model chose `charge`, before `record_intent` committed | no intent for this execution | lease wait, then `acquire`; nothing to reconcile; a new attempt re-runs the loop from the goal | one charge (`correct`), but every earlier model call and observation is redone: `progress_lost` |
| W1b: after `record_intent`, before the inner executor ran [`crash-before-effect`] | open intent, no `obs` | `OutcomeUncertain`; refuses to run | `stuck` until an operator writes an `obs` (`executed:false`); then a re-run from the goal gives one charge |
| W2: inside `charge`, after the ledger fsync, before return [`ambiguous-crash-after-effect`] | open intent, no `obs` | `OutcomeUncertain` | `stuck`, then operator reconciliation (`executed:true`). The re-run attempt has no history, so for an unobservable effect it charges again: `duplicate_effect` (bench notify: 20/20 duplicates) |
| W3: after `charge` returned and `record_outcome` committed [`crash-between-actions`] | resolved intent, `obs` | nothing uncertain; a new attempt re-runs from the goal | `duplicate_effect` for an unobservable effect (bench notify: 20/20); correct only if the model can re-observe the effect |
| W4a: between the `obs` create and `record_outcome` | open intent, `obs` present | `reconcile` from `obs` (`executed`), then re-run | same as W3 |
| W4b: between the `chip.WorkResult` create and the terminal checkpoint | task still `running` under a lease; result record for attempt N | re-run as attempt N+1, with no id collision (`attempt = checkpoint_version+1`, `lib.rs:802`) | completed work may be redone. Atomicity of each FeltDB commit is FeltDB's guarantee [claim] (`agent-state.md:22-23`), not verified here |
| W5: handoff | n/a: no handoffs exist (§4) | n/a | `not_applicable` |
| W6: awaiting approval | n/a: no approval state exists (§4) | n/a | `not_applicable` |

A worker whose lease was taken over is fenced. Its later commits are refused (`FENCE_STALE`), and
its success is not reported. [source] `crates/chip-agent-state/tests/feltdb_native.rs:520`;
[claim] `docs/product/durability-benchmark.md:133-135`. The lease is renewed before every
execution, and an expired lease stops the next execution before it starts. [source]
`crates/chip-agent-state/src/lib.rs:867-874`, `crates/chip-agent-state/tests/feltdb_native.rs:648`

### 3.6 D1 to D4

| | Property | Verdict | Basis |
| --- | --- | --- | --- |
| D1 | Save | **Yes**, for task, lease, fence, intents, outcomes, checkpoints and observations, in a file-backed FeltDB. **No**, for loop state, model attempts and evidence | [source] §3.1, §3.4 |
| D2 | Resume | **Partial.** `begin` takes over the same task without manual reconstruction, with task identity and versions continuous. It does not continue the *execution*: the loop restarts from the goal with no prior observations | [source] `agent-state.md:60-62`, `:83`; test `feltdb_native.rs:353` |
| D3 | Safe recovery | **Recorded progress is not corrupted:** strict history, fences and leases. **It is not reused:** every recorded step is redone by a new attempt, so `progress_lost` holds in every window | [source] §3.1; [inferred] §3.5 |
| D4 | Effect safety | **Partial.** Intents are recorded before effects, and an ambiguous outcome is surfaced (`OutcomeUncertain`, no blind retry). Missing: no idempotency key reaches the executor, and once uncertainty is reconciled or the outcome recorded, the re-run can repeat an unobservable effect | [source] §3.3; [claim] `durability-benchmark.md:141-147` |

### 3.7 Deterministic durability bench

* Harness: `crates/stack-agent/src/durability_bench.rs`, two arms. [source]
  `crates/stack-agent/src/durability_bench.rs:1-25`
  * Arm A is the legacy loop.
  * Arm B is `begin`, then `DurableExecutor`, then `finish`, over a real FeltDB file that is
    reopened on every restart.
* A "crash" drops the future and closes every handle **in-process**. It is not a `SIGKILL` of a
  process, so it does not meet the protocol's E2 standard. [source]
  `crates/stack-agent/src/durability_bench.rs:20-21`
* Leases (60 s) run on an injected clock. [source] `crates/stack-agent/src/durability_bench.rs:22-24`
* Setup: the scripted model reacts to observations; the world has an observable `effects.log`
  and an unobservable `outbox.log`; limits are 16 turns and 12 executions; 20 repeats, which show
  repeatability, not variance. [claim] `docs/product/durability-benchmark.md:22-42`, `:84-86`
* Results [claim] `docs/product/durability-benchmark.md:88-155`
  * **Normal execution:** same outcome in both arms. B costs about 28–38 ms more per task (debug
    build): about 7 ms in begin/finish and 4–5 ms per execution. The database is 60–78 KB per
    task.
  * **Contention:** the only effectiveness gain.
    * Concurrent start: B 20/20 with 0 false successes; A 0/20 with 40 false successes.
    * Observable stale owner: B 20/20; A 0/20.
  * **Restart:** no gain. B needs an extra run, 60–120 s of (modelled) lease wait and an operator
    step.
  * **Unobservable notify after a crash after the effect, or between actions:** both arms 0/20,
    with 20 duplicates each. B only defers the duplicate.

## 4. Handoffs, delegation, approval; sessions and turns

* There is no handoff, delegation, multi-agent transfer or approval primitive. A search for
  approval or handoff terms in `crates/**.rs` finds only test strings ("no approval") and a
  corpus feature flag. [source] `crates/chip-core/tests/work_loop.rs:424`,
  `crates/chip-reasoning-corpus/src/corpus.rs:350-351`
* Attn integration is forbidden without justification (`AGENTS.md:807`). Architecture tests
  forbid an `Attn` dependency in chip-core. [source] `crates/chip-core/tests/forbidden_dependencies.rs:67`,
  `crates/chip-core/tests/architecture.rs:35`
* `Escalate` is a **terminal outcome**, not a suspension. [source]
  `crates/chip-core/src/work.rs:2441`, `crates/chip-agent-state/src/lib.rs:1018`,
  `crates/stack-agent/src/service.rs:16`, `:305-307`
  * In FeltDB it checkpoints `blocked`.
  * The service keeps escalated work in memory and never evicts it.
  * There is no approve or continue API. A later `begin` re-runs from the goal.
* `chip-session` reads and writes the Zig `fx` `event_log_v1` session directory: `events.jsonl`,
  a committed watermark, `authority.json`, and projections. [source]
  `crates/chip-session/src/lib.rs:1-9`, `docs/product/session-format.md:1-24`
  * Only execution-memory schema 3 is interpreted.
  * `background_command` and `compacted_summary` turns are refused. [source]
    `crates/chip-session/src/turn.rs:8-10`
* `chip-turns::run_turn` runs one turn over a session. [source] `crates/chip-turns/src/run.rs:96-136`
  1. lock the session;
  2. reduce history;
  3. refuse if a turn is `PendingRecovery`;
  4. compose the goal from at most 20 prior turns and 512 bytes of output each
     (`context.rs:6-18`);
  5. run `run_work` once;
  6. append `history_turn_committed`.
  The result is reported as `Committed`, `NotCommitted` or `Uncertain`.
* What a session persists is the turn's result **after** the run, never mid-run. A crash inside
  a turn loses the turn. [inferred] from `run.rs:120-125`
* Neither crate is a dependency of the `stack-agent` binary. `chip-session` is used by
  `chip-wasm-host`. [source] `crates/stack-agent/Cargo.toml:26-58`,
  `crates/chip-wasm-host/src/abi.rs`

## 5. Providers (FX)

* Chip's model port is `ModelProvider::complete(ModelRequest)`. [source]
  `crates/chip-inference/src/lib.rs:240-241`
* The authoritative executor is FX: `fx-core`, a provider-neutral contract with validation,
  normalization and a one-attempt executor, plus `fx-single-attempt`. These are workspace path
  crates, described as coming from `rkendel1/fx`. [source] `crates/fx-core/src/lib.rs:1-27`,
  `crates/fx-single-attempt/src/lib.rs:1-11`, `crates/chip-fx/Cargo.toml:12-13`
* `chip-fx::FxModelProvider` is one FX attempt per call: no retry, fallback or routing. [source]
  `crates/chip-fx/src/lib.rs:1-21`
* `chip-fx-native` composes the native provider, and credentials are acquired in Chip's host
  layer. [source] `crates/chip-fx-native/src/lib.rs:1-24`
* Wires: `fx-native-provider` has `OpenAiCompatible` (OpenAI, OpenRouter, LM Studio) and
  `Anthropic` (Messages). **Both stream** over SSE, and OpenAI asks for
  `stream_options.include_usage`. [source] `crates/fx-native-provider/src/lib.rs:1-11`,
  `crates/fx-native-provider/src/openai.rs:118-119`, `crates/fx-native-provider/src/anthropic.rs:64`
  * The stream is consumed whole. Partial output of an interrupted stream is kept only as
    `InferenceFailure.partial_output`, "never a completion". The loop has no streaming. [source]
    `crates/chip-inference/src/lib.rs:183-184`
  * An Anthropic stream without a stop reason, or an OpenAI stream without a finish reason, is an
    error. [source] `crates/fx-native-provider/src/anthropic.rs:164`,
    `crates/fx-native-provider/src/openai.rs:173`
* Structured output on the OpenAI-compatible wire uses `response_format: json_schema`. Anthropic
  gets none. [source] `crates/fx-native-provider/src/openai.rs:135-143`,
  `crates/chip-fx/src/native.rs:151-202`
  * The `chip-fx-native` module doc still says structured output "never reach[es] the wire"
    (`crates/chip-fx-native/src/lib.rs:23-24`). That line looks stale against the code above.
    [inferred]
* Provider profiles are explicit operator choices, never inferred from a model name. [source]
  `crates/stack-agent/src/work_generation.rs:118-162`, `:346-383`

  | Profile | Token-limit field | Temperature | Structured output |
  | --- | --- | --- | --- |
  | `openai` | `max_completion_tokens` | omitted | schema-flat |
  | `anthropic` | `max_tokens` | omitted | none |
  | `local` | `max_tokens` | 0.0 | full schema |

  Defaults are 2048 tokens, temperature 0.0, `object`, and 30 s (`:10`).
* Accounting [source] `crates/chip-inference/src/lib.rs:66-70`,
  `crates/chip-core/src/work.rs:1931-1940`, `:2472-2479`, `:1918-1929`;
  [claim] `docs/product/agent-bench.md:159-160`
  * `Usage{prompt, completion, total}` is recorded per `ModelCalled` event, but only when the
    provider reported non-zero counts. Totals are summed from events.
  * Model latency is accumulated in `WorkLatency.model`.
  * Per-call latency and cost are not recorded.
* Context. [source] `crates/chip-core/src/lib.rs:1068-1088`, `crates/chip-core/src/work.rs:500-660`,
  `crates/stack-agent/src/software_work.rs:421-436`, `crates/stack-agent/src/inspect_context.rs:58-87`
  * Every escalation is **stateless**: the observations are sent as `System` messages, plus one
    `User` message holding the rendered `EscalationContext`. There is no chat history.
  * `EscalationContextPolicy` implementations:
    * `FullEscalationContext` (`full-v1`)
    * `DeduplicatedEscalationContext` (`dedup-v1`, omits named repeats)
    * `InspectProgressContext` (`inspect-progress-v1`)
    * `InspectBoundedContext`
    * `InspectCoveredContext` (`inspect-bounded-members-v1`, which condenses parsed manifests)
* A malformed reply ends the work `Failed` (§1.3). A token-cap cut-off or a timeout is a typed FX
  failure with delivery certainty. [source] `crates/stack-agent/src/work_generation.rs:22-24`

## 6. Memory: what is model-generated vs. host-observed

* **Evidence store:** in memory, observations only (host-observed), and lost at process exit. The
  doc says "This is not memory." [source] `crates/chip-core/src/evidence.rs:1-8`
* **Context:** built by Chip from observations, decisions and `ruled_out`, deterministically.
  Model text enters later contexts only as decision labels and prior-decision lines, never as
  evidence. [source] `crates/chip-core/src/work.rs:340-361`, `:1803-1826`
* **Learning** (`crates/stack-agent/src/learning`) [source]
  `crates/stack-agent/src/learning/mod.rs:1-13`, `crates/stack-agent/src/learning/store.rs:1-12`
  * It writes run documents and "behaviors" per project (`<project>/.stack-agent/learning`) and
    per runtime (`<data>/runtimes/...`).
  * Inputs are host-observed only (the autoconfig outcome record and the work's counters). "No
    model is asked anything."
  * With `--auto`, the records only rank catalog configurations.
  * Writes are atomic, and the directory is unreachable by capabilities.
* **Session history** (§4) replays prior turns' *outputs* as data into the next goal, bounded and
  clipped. [source] `crates/chip-turns/src/context.rs:32-40`
* There is no persistent agent memory, embedding or vector store, by policy. [source]
  `AGENTS.md:792-802`

## 7. Extensibility

* `chip-experiments` is opt-in and off the production path. [source]
  `crates/chip-experiments/src/lib.rs:1-17`, `crates/chip-experiments/src/plugin.rs:1-9`, `:45`, `:90`
  * A `Strategy` is a `LocalWorkPolicy` plus an `EscalationContextPolicy`.
  * A `CapabilityFixture` contributes a `CapabilityBackend` to the run's `CapabilitySet`.
  * Neither gets an executor, authorization, the evidence store or the model boundary.
* The autoconfig catalog (`catalog.v1`) is compiled in. It holds combinations of the existing
  decision policy, context policy and capability subset, and it never adds a capability, executor
  or verifier. [source] `crates/stack-agent/src/autoconfig/catalog.rs:1-6`, `:16-30`, `:160`
* Seams that exist today: `LocalWorkPolicy`, `WorkDecisionBoundary`, `EscalationContextPolicy`,
  `WorkControl` (including `on_event` streaming of events), `CapabilityBackend`/`Executor` (wrap
  them as `DurableExecutor` does), `ModelProvider`, `ObservationPredicate`, `AnswerPredicate` and
  `ObservationInvariant`. [source] `crates/chip-core/src/work.rs:234-241`, `:270`, `:305`, `:500`,
  `:1470-1510`
* **Pending, not in main:** a capability-source seam proposed on branch
  `claude/stoic-meitner-cqyud1`. A search for `CapabilitySource` or `capability_source` at this
  commit finds nothing. (The merged PR #10 from that branch was the run5 bench archive only.)
  [source] `git show --stat HEAD`

## 8. Benchmarks and baselines (recorded results, not reproduced)

### 8.1 Agent bench (`docs/product/agent-bench.md`)

* Tasks A to E: [claim] `docs/product/agent-bench.md:72-79`
  * A: grounded read;
  * B: multi-step config discovery with a distractor;
  * C: a 28-member workspace with a coverage contract and a 24,000-byte context budget;
  * D: a write refusal under inspect;
  * E: a verified code change.
* All tasks have independent oracles. Every run uses 3 trials per cell, `max_output_tokens 8192`
  and a 120 s call timeout.
* **Run 2** (45 trials): OpenAI `gpt-5.5` 15/15; Anthropic `claude-sonnet-5-5` 8/15 (6 refusals
  for the 1,024-byte summary limit, which the prompt did not state); local
  `gemma-4-26b-a4b-qat` (LM Studio, `localhost:1235`) 6/15. Safety: 0 violations in 45/45.
  [claim] `docs/product/agent-bench.md:44`, `:95-117`
* **Run 4**, after the decision contract (60 trials): [claim] `docs/product/agent-bench.md:189-227`
  * OpenAI 15/15.
  * Anthropic 13/15.
  * **Local with the canonical schema: 3/15.** In 12 of 15 trials the first reply was a
    schema-valid `complete` with nothing observed, which was refused.
  * Local with `object`: 6/15. Task B failed on duplicate JSON keys, task C gave an ungrounded
    answer, and task E hit token or call timeouts.
  * Schema-valid replies were 152/152, 137/138, 15/15 and 147/147.
  * Safety: 0 violations in 60/60.
  * Local latency: A 3.6 s; C 26.2 s; E 123.9 s with timeouts.
* Gemma caveats. [claim] `docs/product/agent-bench.md:198-202`, `:216-217`, `:238`
  * 3 trials per cell, and short local runs are bit-for-bit repeatable at temperature 0.
  * The full `anyOf` grammar *hurt* this model.
  * Task D "passes" vacuously.
* E4 (the protocol's small-model test) is **[blocked]** here: no local endpoint is reachable.
  [source] `docs/research/agent-runtime-primitives/protocol.md` (E4)
* There is no OpenAI or local paired durable run.

### 8.2 Run 5: real-model durable/legacy pair

Anthropic `claude-sonnet-5-5`, 3 pairs per task, uninterrupted execution only. [claim]
`docs/product/durability-benchmark.md:192-236`, `docs/product/agent-bench/run5-durable-pair/report.md`

* Results per task:
  * A: 3 pairs, both complete.
  * B: 2 both complete, 1 neither.
  * C: 2 legacy only (the durable answer was refused by Chip's answer check), 1 neither
    (`invalid_decision`).
  * D: 3 both complete (safe).
  * E: 2 both complete, 1 durable only.
* There were 0 durable false successes. All 15 durable trials were recorded in FeltDB.
* Latency differences ran from −3.6 s to +2.5 s and were dominated by model time. FeltDB overhead
  was not separated.
* The differences are attributed to model variance (n = 3). No interruption or contention was
  tested with a real model.

### 8.3 Durability bench

See §3.7. It is synthetic: a scripted model, one interleaving, an in-process crash, debug
timings, modelled lease waits and an always-correct operator. [claim]
`docs/product/durability-benchmark.md:169-178`

## 9. Capability map

| Capability | Exists? | Where | Authoritative owner | Known gap | Integration point for a future primitive |
| --- | --- | --- | --- | --- | --- |
| Bounded decision loop | yes | `chip-core/src/work.rs:2166-2470` | Chip | sequential only; no suspension state | `Agent::run_work_with_context_policy` (a caller, not a replacement) |
| Decision contract | yes, strict | `chip-core/src/model_decision.rs` | Chip | no native tool-calling; one decision per reply | `WorkDecisionBoundary` |
| Local decisions | yes | `LocalWorkPolicy`; `software_work.rs:480,702` | Chip | learned model not an authority | `LocalWorkPolicy` |
| Tool dispatch and validation | yes | `capability_set.rs`, `lib.rs:879-927` | Chip (validation), backend (effect) | no executor-level timeout or idempotency field | `CapabilityBackend` in a `CapabilitySet`, wrapped like `DurableExecutor` |
| Cancellation and deadline | yes, cooperative | `WorkControl`, `software_work.rs:1765-1860` | Chip (loop), FX (in-flight call) | executions in flight are not interrupted | `WorkSpec::with_control` |
| Retries | **no, by design** | `AGENTS.md` §14, §21 | n/a | caller-side retry only, using `Delivery` | `WorkOutcome::Failed.delivery` |
| Durable task, intent, outcome | yes, opt-in | `chip-agent-state` | **FeltDB** | resume restarts the loop; no operator command | `begin`, `DurableWork::executor`, `finish` |
| Resume with history | **no** | `agent-state.md:83` | FeltDB holds it; Chip does not read it | duplicate unobservable effects (bench 20/20) | `begin` → seed `Run.observations` (needs a loop/resume-contract change) |
| Effect idempotency keys | **no** | `chip-agent-state/src/lib.rs:1143-1151` | none | key computed, not forwarded | `ExecutionRequest` (no field today); `DurableExecutor::execute` |
| Model-attempt persistence | no | `agent-state.md:86-87` | FX (certainty, not stored) | no audit after a restart | `WorkControl::on_event` (`ModelEscalation`/`ModelCalled`) |
| Handoff and delegation | **no** | — | — | forbidden without justification | none; would be a new `WorkOutcome` or caller-level composition |
| Human approval and suspend | **no** | `Escalate` is terminal | — | no approve or continue path | `WorkOutcome::Escalated` + `begin` (a new contract state would be needed) |
| Sessions and turns | yes (Zig format) | `chip-session`, `chip-turns` | Zig `fx` session store | not wired into `stack-agent work`; persists only after the turn | `chip_turns::run_turn` |
| Providers | yes, streaming | `fx-*`, `chip-fx*` | **FX** | no per-call latency or cost; text only | `ModelProvider` (via `chip-fx`) |
| Structured output | yes | `work_generation.rs:61-162`, `openai.rs:135-143` | provider requests it; Chip's parse decides | the full schema hurts gemma | `StructuredOutput` / profile |
| Context policy and budget | yes | `work.rs:500-660`, `inspect_context.rs` | Chip | stateless per call; byte budget, not token budget | `EscalationContextPolicy`, `with_context_budget_bytes` |
| Evidence store | yes, in memory | `chip-core/src/evidence.rs` | Chip | not persisted or restored | `Agent::assess_evidence` / `record_evidence_under` |
| Learning | yes, host-observed | `stack-agent/src/learning` | stack-agent | ranks configurations only | `Learning::prepare/record` |
| Experiments | yes, opt-in | `chip-experiments` | Chip (runs), host (criteria) | not on the production path | `Strategy`, `CapabilityFixture` |
| Capability source seam | **pending** | branch `claude/stoic-meitner-cqyud1` | — | not in main | (as proposed) |
| Audit and verification | yes | `audit_safety`, `verify_trajectory` | Chip | post hoc; tests and bench only | `WorkReport.events` |

### Must remain authoritative

* **FeltDB** for durable agent state: task, lease, fence, intents, outcomes, checkpoints. No
  second store and no in-memory fallback. [source] `crates/chip-agent-state/src/lib.rs:18-23`
* **Chip** (`chip-core`) for:
  * decisions: parsing, validation, execution identity;
  * authorization: declared or allowlisted capabilities;
  * observations and evidence;
  * goal and completion evaluation;
  * limits, budget and cancellation boundaries;
  * the audit.
  [source] §1, §2
* **FX** for model delivery: one attempt per call, delivery certainty, cancellation semantics,
  request validation, stream normalization. [source] `crates/fx-core/src/lib.rs:12-27`,
  `crates/chip-fx/src/lib.rs:1-21`
* **Capability backends and executors** (PAX, project, git, compute) for the effects themselves
  and their timeouts. [source] §1.4
* **The Zig session format** (`chip-session`) for sessions and conversational turns. [source]
  `crates/chip-agent-state/src/lib.rs:33-38`
* **The host** (stack-agent) for verification (`accepted` in `finish`) and for oracles.
  [source] `crates/chip-agent-state/src/lib.rs:1003-1010`
* **A model's output is never evidence or authority.** A primitive must not let a framework's
  model or tool-call layer execute, complete or retry on its own. [source]
  `crates/chip-core/src/work.rs:20-22`, `AGENTS.md:379`, `:803-807`
