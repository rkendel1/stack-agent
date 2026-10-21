# durable-harness parity: what stack-agent already had, what it lacked, what was added

The reference is `vercel-labs/durable-harness` (read as the `rkendel1/durable-harness` fork at
`fb056a3`), together with the Workflow SDK World packages, pi-durable and libfx it builds on. Its
source-level analysis is in [`reference.md`](reference.md); every claim about it below cites that
report, which cites the reference's source. Every claim about stack-agent cites its source or a
test that runs under `cargo test`.

## What the reference actually does

* **The Workflow SDK's step engine is not used.** durable-harness uses the World only as an event
  log, queue and stream store. Its own kernel (`packages/harness-world`) does the memoising and
  replay ([reference §0](reference.md#0-the-headline-what-implements-durable)).
* **Every call goes through `effect()`.** The kernel writes an intent (a memo key, the drive's epoch
  and a replay policy, `'safe' | 'never'`), runs the body, then writes the outcome. On replay, a
  committed outcome is returned without running the body again.
* **An open intent after a crash:**
  * a `safe` one is re-run;
  * a `never` one is closed as failed, `interrupted: true`, with any partial output and the text
    "it is not safe to rerun. It may or may not have taken effect"
    ([§4.3](reference.md#43-what-happens-to-an-in-flight-call-after-a-crash--exact-code-path-and-text)).
* **No exactly-once.** "Never run twice" is not "ran once": the model is told the call may or may
  not have taken effect, and the turn goes on. No key reaches the external system, and nothing
  reconciles with it. The guarantee is only as good as each tool's `safe`/`never` label.
* **Model calls** ([§3](reference.md#3-model-requestresponse-persistence)):
  * kernel Pi saves a response before Pi sees it finish, so a response that was acted on is
    always in the log;
  * a crash after the provider answered but before that save calls the model again;
  * partial streams are discarded, except that pi-durable keeps them as aborted messages;
  * the CLI harnesses (Claude Code, Codex, OpenCode) do not record their model calls as steps.
* **Ownership** ([§6](reference.md#6-execution-ownership--concurrency)): queue redelivery,
  "anchors" judging liveness, epochs, first writer wins per key, and the World's writer fence. They
  stop a stale worker's writes, not its execution: a taken-over worker may still finish a `never`
  call.
* **Workspace** ([§8](reference.md#8-durable-workspace--virtual-filesystem-and-sandbox-state)): a
  virtual workspace whose files live in the log, atomic with each call's outcome. A sandbox's or a
  CLI's working directory is a plain disk, not transactional.

## Matrix

Parity is judged on behaviour (source and tests), not on names. "Validation" is a test that runs
under `cargo test`.

### 1. Existing parity

| Primitive | Reference | stack-agent | Validation |
| --- | --- | --- | --- |
| Durable intent before a side effect | `effect()` writes a born-running `step_started` before the body (`driver.ts:2398-2435`) | `DurableExecutor::perform`, `Inner::before`: renew, then `record_intent`; until it commits the action does not start (`crates/chip-agent-state/src/lib.rs`) | `feltdb_native.rs` `work_creates_a_task_records_each_execution_and_persists_its_completion`; `crash_windows.rs` `killed_after_the_intent_and_before_the_effect_it_is_uncertain_until_settled_not_run` |
| A committed outcome is not re-run | memo lookup in `effect()` (`driver.ts:2303-2305`) | resolved intents' `chip.Observation` records restored (`restore`, `WorkSpec::restored`) | `feltdb_native.rs` `a_crash_after_the_outcome_was_recorded_resumes_without_executing_it_again`; `crash_windows.rs` `a_recorded_run_followed_by_a_kill_in_the_next_model_call_is_restored_not_run_again` |
| Per-tool replay classification | `ReplayPolicy 'safe' \| 'never'`, declared per tool (`types.ts:9-19`) | `CapabilityDescriptor::effect` (`observation`, `consequential`, `unknown`, default `unknown`) and `idempotency` (`not_carried`, `carried`, `deduplicated`), declared by the adapter | `effects.rs` `an_interrupted_observation_is_performed_again_and_the_work_resumes_without_an_operator`, `an_interrupted_action_with_no_declaration_or_an_unknown_one_stays_uncertain` |
| An interrupted unsafe call is never run again on its own | `resolveAbandoned` closes a `never` intent as failed (`driver.ts:1527-1571`) | `begin` refuses with `OutcomeUncertain`; nothing is retried | `outcome_unknown.rs`; `crash_windows.rs` `killed_after_the_effect_it_is_uncertain_and_once_settled_executed_never_runs_again` (real `SIGKILL`, independent ledger) |
| Stale-worker writes are fenced | epochs, first-writer-wins, World fence | FeltDB `acquire` advances the fence; every contract op carries fence and lease | `feltdb_native.rs` `a_takeover_while_an_action_runs_fences_out_the_old_owner_and_its_success_is_not_reported`, `racing_owners_of_one_new_task_produce_exactly_one_runnable_attempt` |
| Two workers on one task | in-band fence test (`in-band-fence-processes.test.ts:22`) | lease plus FeltDB's per-process file lock plus `ProjectLock` | `feltdb_native.rs` `a_task_held_by_one_owner_is_refused_to_every_other`; `agent_state_cli.rs` `blocked_work_is_recorded_blocked_and_its_lease_refuses_a_second_process` |
| The model is told about interrupted calls | "[Recovery]" note / interrupted tool result (`recovery.ts:58-115`) | restored observations join the context; operator settlements are stated under "Ruled out" | `settlement.rs` `an_effect_settled_as_executed_is_resumed_from_and_never_performed_again` |
| Inconsistent history is refused | divergent replay ends `run_failed` after 8 restarts | `History`/`begin` refuse gaps, unknown schemas and contradictions (`InvalidHistory`) | `feltdb_native.rs` `incomplete_history_and_unknown_record_schemas_are_refused_not_read_around`, `a_recorded_execution_that_contradicts_its_intent_is_refused_not_restored` |
| Cancellation does not erase evidence | no mid-turn cancel ([§10](reference.md#10-cancellation--timeouts)) | cooperative stop before each model call and execution; a tool in flight finishes and is recorded | `cancellation.rs` `cancelling_while_a_tool_runs_lets_it_finish_reports_it_and_starts_nothing_else`; `effects.rs` `a_stop_requested_before_recovery_prevents_it_and_leaves_the_intent_open` |
| History is not authority | Agent Runs traces are best effort ([§11](reference.md#11-observability-agent-runs)) | `chip.WorkEvent` carries provenance; telemetry is exported from FeltDB, never read back | `work_events.rs`; `crates/chip-otel/tests` |

### 2. Incomplete parity (now closed)

| Primitive | Reference | Before | Failure mode | Change | Validation |
| --- | --- | --- | --- | --- | --- |
| **A completed model response survives a crash** | kernel Pi: `model:<n>` is a `safe` effect; a response saved before Pi saw it is replayed (§3.1) | "Model calls are not intents" (`agent-state.md` Limits). A response was never recorded, so a crash after it arrived and before its decision was acted on paid for the same turn again. | duplicate model charge; a different decision on resume (the model is not deterministic) | `chip.ModelResponse` (`chip.model_response.v1`), written as the loop receives the response, before it is interpreted. `begin` offers it back only under the conditions in [agent-state.md](../../product/agent-state.md#model-responses), and the loop takes it only if the model and observations are unchanged. | `model_response.rs` (7 tests); `crash_windows.rs` `killed_after_the_response_and_before_the_intent_the_turn_is_answered_from_the_record` (real `SIGKILL`: **0** model requests on resume; the tool ran once) |
| **A model call in flight at the crash is visible** | an open `model` step in the log | `ModelEscalation` was recorded, but nothing reported it as in flight | an operator could not tell that a call may have been charged and lost | `History::model_calls_in_flight`, `DurableWork::interrupted_model_call`; `agent-state show` and `work --json` (`model_call_in_flight`) report it | `model_response.rs` `a_model_call_that_never_returned_is_reported_in_flight_and_asked_again_not_replayed`; `crash_windows.rs` `a_recorded_run_followed_by_a_kill_in_the_next_model_call_is_restored_not_run_again` |
| **Real-kill tests of the tool-dispatch windows run in CI** | `retry.test.ts:183` (an unsafe tool crashed after its side effect: one ledger record) | Only the external Python/gdb harness (`docs/research/agent-runtime-primitives/experiments/stack-agent/`). `effects.rs` pointed to an `effects_crash.rs` that does not exist. | a regression in a crash window would pass `cargo test` | `crates/stack-agent/tests/crash_windows.rs`: the real binary, a `pax` stand-in with its own ledger that `SIGKILL`s its parent at the window, a fresh process to resume | 4 tests, below |

### 3. Missing primitives (deferred, with the reason)

| Primitive | Reference | Why not now |
| --- | --- | --- |
| Fencing Chip's own record writes (`AgentStore::create`: observation, event and response records are guarded only by "must not exist") | the World fence covers every step write | Unreachable in the product today: FeltDB's per-process file lock and `ProjectLock` keep a second live process off the file, and every record id carries its attempt. Fencing them would need a contract-level precondition FeltDB v1 does not offer for non-contract records. Recorded as a limitation. |
| Lease renewal while a long tool runs | anchors, plus a deadline on each drive | The lease is renewed before each intent. A tool that outlives it can only be taken over by another process, which the file lock prevents while this one lives. After a death, the open intent is `OutcomeUncertain`, never repeated. |
| Response replay in the durability bench (`durability_bench.rs` arm B) | — | The bench measures tool effects. Its model is scripted by observation, so a replayed response would change no cell. |
| Provider-side idempotency or response retrieval | none used (§3 (d)) | FX's providers expose none. A lost call is asked again, and the history says it was in flight. |

### 4. Architectural differences (a native design is preferable)

| Reference | stack-agent | Why |
| --- | --- | --- |
| Replay re-runs the harness's code and memoises every step by an ordinal key; the adapter must be deterministic | Resume reconstructs from records: restored observations, plus at most one given-back response. Decisions are not replayed. | No determinism is required of Chip's loop or of the model. A given-back response is accepted only if the work is provably where it was received; otherwise the model is asked. |
| An interrupted `never` call becomes a failed tool result, and the turn goes on | `OutcomeUncertain`: the work does not resume until an operator settles the call with evidence (`agent-state settle`) | AGENTS.md invariants 5–7: the runtime does not let the model judge whether reality changed. The model is never asked to "check before repeating". |
| No key reaches the external system | logical action keys (`ak1.`) bound to the request (`b1.`); a `deduplicated` capability is asked again under the same key | Turns "may or may not have run" into "answered once by the capability" where the capability supports it. |
| Liveness from queue redelivery and anchors; recovery is automatic | an expired lease plus an explicit `--task-id` resume | AGENTS.md §14: no background scheduler, and no hidden retry. |

### 5. Non-goals

* **A virtual workspace in the log** (§8). stack-agent works on the real project. Stale evidence is
  already caught at completion: `stale_test_evidence` compares the project fingerprint the passing
  `pax.test` recorded with the project now, and `tampered_test_files` catches replaced tests. A
  restored `project.read` may describe a file that has changed since. That observation is the
  model's input, never completion evidence.
* **Persisting partial streams** (pi-durable keeps them as aborted messages). An incomplete response
  is never a decision (`incomplete_response.rs`), and keeping it would not make one.
* **A Node runtime, a Workflow SDK World or a queue service.** Everything here is FeltDB in a file.

## The required crash scenarios

These are the 22 scenarios from the assignment. "Effects" counts what the fake external system
applied in its own ledger, not anything stack-agent recorded.

| # | Scenario | Test | Effects | Model calls | Authoritative state | Recovery |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | dies before recording the intent | `crash_windows.rs` `killed_after_the_response_and_before_the_intent_…` | 0, then 1 | 1, then **0** | no intent; completed | automatic |
| 2 | after the intent, before dispatch | `crash_windows.rs` `killed_after_the_intent_and_before_the_effect_…` | 0, then 1 after settlement | 1, then 1 | intent open → `OutcomeUncertain` | operator (`executed=false`) |
| 3–5 | after dispatch / while running / succeeded before the answer | `crash_windows.rs` `killed_after_the_effect_…`; `model_response.rs` `a_response_that_was_acted_on_…` | 1, never 2 | — | intent open → `OutcomeUncertain` | operator (`executed=true`); not repeated |
| 6 | result received; observation written, outcome not | `feltdb_native.rs` `a_crash_after_the_intent_blocks_resumption_until_evidence_settles_it` (an observation record written the way Chip writes one settles the open intent); the gdb harness in `docs/research/agent-runtime-primitives/` places real kills inside each commit | 1 | — | reconciled from the observation record | automatic |
| 7 | right after the commit | `crash_windows.rs` `a_recorded_run_followed_by_a_kill_…` | 1 | +1 (the lost call) | restored | automatic |
| 8 | completed model response recovered with no new call | `model_response.rs` `a_response_received_and_never_acted_on_…`; `crash_windows.rs` probe window | 1 | **no second call** | `chip.ModelResponse` offered and taken | automatic |
| 9 | model request interrupted before the response | `model_response.rs` `a_model_call_that_never_returned_…` | 1 | asked again | `model_calls_in_flight` lists it | automatic, reported |
| 10 | provider timeout after possibly accepting | `call_timeout.rs`; `cancellation.rs` (FX delivery certainty); in flight as #9 | — | not retried in the run | `WorkFailed` with delivery | next start asks again |
| 11 | read-only tool interrupted | `effects.rs` `an_interrupted_observation_is_performed_again_…`; `agent_state_cli.rs` `a_request_interrupted_inside_project_read_…` | re-observed | — | old intent reconciled citing the fresh observation | automatic |
| 12 | idempotent mutation | `idempotency.rs` `a_deduplicating_capability_is_asked_again_under_the_same_key_and_charges_once` | 1 | — | reissued under the same key | automatic (declared) |
| 13 | non-idempotent mutation | `idempotency.rs` `a_crash_after_the_effect_stays_uncertain_without_deduplication`; `crash_windows.rs` after-effect | 1 | — | `OutcomeUncertain` | operator |
| 14 | key reused with other inputs | `idempotency.rs` `reusing_the_key_for_a_different_payload_is_refused_before_anything_runs` | 0 | — | refused before the intent | — |
| 15 | operator resolves | `settlement.rs`; `agent_state_cli.rs`; `crash_windows.rs` | as settled | — | settlement record and reconcile | operator |
| 16 | two workers resume | `feltdb_native.rs` `a_task_held_by_one_owner_…`, `racing_owners_…` | 1 | — | `LeaseHeld` | — |
| 17 | stale worker commits | `feltdb_native.rs` `a_takeover_while_an_action_runs_fences_out_the_old_owner_…` | — | — | `FENCE_STALE` | — |
| 18 | tool errors after its side effect | `outcome_unknown.rs`: `OutcomeUnknown` keeps the intent open; `ExecutionFailed` is a definite outcome **by the executor's declaration** | 1 | — | open (unknown) or failed (definite) | trust assumption, see Limits |
| 19 | inconsistent durable state | `feltdb_native.rs` `incomplete_history_…`; `model_response.rs` `a_tampered_response_record_is_invalid_history_and_nothing_runs` | 0 | 0 | `InvalidHistory` | — |
| 20 | workspace modified before death | `agent_state_cli.rs` `died_inside_the_write` cases (`project.write` is consequential: uncertain); completion re-checks the project fingerprint | — | — | `OutcomeUncertain`; `evidence_invalidated` | operator |
| 21 | normal completed tool call not repeated | `crash_windows.rs` `a_recorded_run_followed_by_a_kill_…`; `feltdb_native.rs` `a_resumed_attempt_starts_from_every_recorded_observation_in_order_and_runs_none_again` | 1 | — | restored | automatic |
| 22 | cancelled where the outcome is unknown | `cancellation.rs` `cancelling_while_a_tool_runs_lets_it_finish_…`; `effects.rs` `a_stop_reached_before_the_executor_begins_runs_nothing` | as observed | — | a tool in flight finishes and is recorded | — |

The effect-to-result boundary, with real kills and an independent destination ledger, is in
[`recovery-matrix.md`](recovery-matrix.md).

## Limits that remain

* **No exactly-once in general.** A consequential capability that neither deduplicates nor can be
  observed stays `OutcomeUncertain` after a crash in its window. Only an operator, or the external
  system's own record, can say what happened. This is the same honest limit the reference has; the
  reference resolves it by telling the model, stack-agent by asking a person.
* **A declaration is trusted.** `effect = observation`, `deduplicated` and the executor's choice of
  `ExecutionFailed` (definite) over `OutcomeUnknown` are the adapter's word; Chip cannot check
  them. The reference's `safe`/`never` labels have the same trust model.
* **A given-back response is the model's judgment from an earlier process.** It is validated
  exactly like a fresh one. It is taken only for the same model and the same observations; the
  turn counter and the decision history it saw may differ, and that is accepted.
* **A crash between the `ModelCalled` event and the response record** leaves no record, and the
  model is asked again (as the reference does when its save does not land).
* **A response is given back once.** The attempt that takes it does not record it again, so a
  second kill in the same window asks the model.
