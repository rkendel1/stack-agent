# OpenHands software-agent-sdk (deeper study)

Follow-up to the Tier 3 screen ([`tier3.md`](tier3.md#openhands)). It is scoped to these parts:
* the event stream and its per-event fsynced log;
* conversation resume;
* the agent server's recovery rule and owner lease;
* condensation, the stuck detector and confirmation mode.

It also reports the decisive experiment the screen left open: E2 under real `SIGKILL`, through
the bare SDK and through the agent server ([`experiments/openhands/`](../experiments/openhands/)).
Evidence tags follow [`protocol.md`](../protocol.md).

## Candidate record

| | |
| --- | --- |
| Repository | https://github.com/OpenHands/software-agent-sdk. The OpenHands README names it as the runtime ("Python SDK, Agent Server, agents, tools, conversations, workspaces, events"), and `All-Hands-AI/OpenHands` resolves to `OpenHands/OpenHands` (screen, [`tier3.md`](tier3.md#openhands)). The SDK's `pyproject.toml` `Source` URL matches. |
| Inspected commit | `33d8aa29c3078d074799e2fbf5e847aeb2d71360`, at https://github.com/OpenHands/software-agent-sdk/tree/33d8aa29c3078d074799e2fbf5e847aeb2d71360 |
| License | MIT ("Copyright (c) 2026 OpenHands contributors", `LICENSE`) |
| Last meaningful maintenance | 2026-10-19, "Release v1.56.0 (#6091)" (HEAD) |
| Packages used | `openhands-sdk==1.56.0`, `openhands-agent-server==1.56.0`, `openhands-tools==1.56.0`, `litellm==1.105.1` from PyPI. The exercised modules are byte-identical to the pinned source (`cmp`). |
| Experiments | [`../experiments/openhands/`](../experiments/openhands/): `harness.py`, `driver.py`, `assemble.py`, `logs/`, `results.json`, README |

Source links are at the pinned commit (reference list at the end). stack-agent `path:line`
references are at `366f6c1`, the [baseline](../baseline.md) commit.

## Summary

* **Decisive question, answered `[experiment]`.** The bare SDK does **not** re-execute an
  interrupted tool through "implicit confirmation" in normal mode. The screen's `[inferred]`
  mechanism was wrong. The cause is subtler: the conversation's HEAD lags the fsynced events.
  * HEAD is `leaf_event_id` in `base_state.json`, and it is saved only when the step's state
    block exits.
  * So after a crash mid-step, the action and everything else written in that step are on disk
    but **off the active branch**.
  * The resumed run asks the model again from the pre-step history. A model that re-decides
    charge duplicates it (W2, W4a, W4b: `duplicate_effect` under the history-reactive script).
  * Under the protocol's count-indexed script the charge happens once, but the conversation
    shows no charge (state ≠ ledger). In W1 it finishes with a false completion and no charge.
* **The agent server path differs, and it is better, but not safe.**
  * `start()` sets RUNNING to ERROR, scans the full log, and appends a synthetic `AgentErrorEvent`
    parented to the first unmatched action. That re-attaches the branch and nothing re-runs
    (W1, W2, W4a).
  * It records "did not complete" even when the effect happened (W2), so the ambiguity becomes a
    false negative.
  * It misses an observation that is on disk but off-branch (W4b: duplicate under the reactive
    script).
* **Confirmation mode has a re-execution window in both paths `[experiment]` (W2c).**
  * Approval is "call `run()`". `run()` flips the status to RUNNING inside the same deferred-save
    block as the step.
  * A crash inside the approved tool therefore persists `WAITING_FOR_CONFIRMATION`. After the
    restart the action looks un-approved, the server rule (which keys on RUNNING) does not fire,
    and approving again re-executes it. Result: 2 charges in both the SDK and the server, with no
    model involved.
* **Portable ideas for stack-agent:**
  * an owner lease with a generation that fences writes (FeltDB already has this, and is
    stronger);
  * compaction as an append-only event;
  * a stuck-pattern catalogue usable as a non-retrying terminal guard;
  * three counter-examples: a separately saved HEAD pointer, approval-as-status, and
    "interrupted ⇒ did not complete".

## 1. Event stream

* Every state change is an immutable pydantic `Event` with an `id` and a `parent_id` (a tree,
  with HEAD = `leaf_event_id`) `[source]` [state.py#L176][o-leaf]. Agent-facing kinds:
  * `ActionEvent` (a validated tool call with its `Action`, `tool_call_id`, `llm_response_id`)
    [action.py#L24][o-action];
  * `ObservationEvent` (paired by `action_id` and `tool_call_id`) [observation.py#L32][o-obs];
  * `UserRejectObservation` [#L86][o-reject];
  * `AgentErrorEvent` (paired by `tool_call_id`) [#L138][o-err];
  * also `MessageEvent`, `SystemPromptEvent`, `Condensation`, `ConversationStateUpdateEvent`,
    `ConversationErrorEvent`.
  `[source]`
* **Order of an action** `[source]`:
  1. `Agent._get_action_event` validates the call. On a pydantic/JSON error it emits
     `AgentErrorEvent` and no action, so the tool never runs (E1 invalid: `[experiment]`).
  2. Otherwise it emits the `ActionEvent` (`on_event(action_event)`), which appends and fsyncs it,
     **before** execution [agent.py#L1342][o-get-action], [#L1494][o-emit-action].
  3. `_handle_tool_calls` checks confirmation; if confirmation is needed, the step ends there
     [response_dispatch.py#L154-L198][o-dispatch].
  4. Otherwise `_ActionBatch.prepare` runs the tools, by default one at a time
     (`tool_concurrency_limit = 1`) [agent.py#L226][o-batch], [agent/base.py#L302][o-concurrency].
  5. `emit` appends the observations in order.
* The persisted `ActionEvent` is therefore an **intent record written before the effect**. That
  is the shape of FeltDB's `record_intent` (`chip-agent-state/src/lib.rs:1141-1160`) `[inferred]`.
  The difference is what recovery does with an unmatched one (§3, §4).
* **No idempotency key reaches a tool.** `ToolExecutor.__call__(action, conversation)` has no call
  id `[source]` [tool/tool.py#L283][o-executor]. The same holds in stack-agent: `ExecutionRequest`
  has none (`chip-core/src/lib.rs:309-316`) `[source]`.

## 2. Persistence (D1)

* **One file per event.** `EventLog.append`, under a `FileLock`:
  * re-syncs from disk unless a length marker proves no other writer appended;
  * refuses a duplicate id or a missing parent;
  * writes `event-<idx>-<id>.json` inside the optional write guard;
  * then advances the in-memory index and the `.eventlog-len-N.marker`.
  `[source]` [event_store.py#L188-L238][o-append], [#L261][o-marker]
* **The write is fsynced.** `LocalFileStore.write` → `atomic_write_text`: temp file in the same
  directory, `flush`, `os.fsync`, `os.replace`. There is **no directory fsync** after the rename,
  so the event's name is durable only once the directory entry reaches disk `[source]`
  [utils/files.py#L21-L39][o-atomic], [io/local.py#L63][o-local-write]. Under `SIGKILL` this did
  not matter: in every W1, W2, W2c, W4 and W6 run, the charge `ActionEvent` written before the
  kill was on disk afterwards `[experiment]` (`persisted_before_resume.kinds`). Power loss was not
  tested.
* **`base_state.json` holds the non-event state.** That is the agent, the execution status,
  confirmation policy, stats, and HEAD (`leaf_event_id`). It is written with the same writer, on
  every public-field change. But "inside a context-manager block, defer the save until `__exit__`"
  `[source]` [state.py#L600-L622][o-autosave], [#L753-L765][o-exit].
  * The run loop holds `with self._state:` around each `agent.step()`
    [local_conversation.py#L1981-L2045][o-runloop].
  * So `append_event` sets `leaf_event_id = event.id` on every event [state.py#L315-L337][o-append-event],
    but the new HEAD (and any status change made in the step) reaches disk only when the step ends
    `[source]`.
  * The agent server's own comment names the hazard: "the process may have died between writing
    an event file and persisting the advanced HEAD" `[source]` [event_service.py#L1361-L1371][o-recover].
  * Measured: in W1, W2, W4a and W4b, the interrupted step's events were on disk and off the
    reloaded active branch `[experiment]` (`pending_in_full_log_at_load: ["charge"]`,
    `pending_on_active_branch_at_load: []`).

## 3. Resume (D2) in the bare SDK

* `ConversationState.create` with an existing `base_state.json` validates it, checks the id,
  attaches the `EventLog` and "cold-loads" the view with full property enforcement `[source]`
  [state.py#L526-L567][o-resume].
* `LocalConversation._run` moves `IDLE`, `PAUSED`, `ERROR` and `STUCK` to `RUNNING` and leaves
  a stale `RUNNING` as it is. `FINISHED` exits the loop `[source]` [local_conversation.py#L1961-L2000][o-run].
  There is **no crash-recovery step in the SDK**.
* `Agent._step` executes any action that `get_unmatched_actions(state.active_branch())` returns
  ("implicit confirmation") before sampling `[source]` [agent.py#L714-L723][o-implicit],
  [state.py#L677-L716][o-unmatched]. Because it reads the **active branch**, an action written
  during the step that crashed is invisible to it. It fired after a restart only where the
  pending action's step had ended: W6 (awaiting approval) and W2c `[experiment]`.
* **View enforcement on cold load drops an unmatched action from the LLM view**
  (`ToolCallMatchingProperty`: "removing actions without matching observations, and vice versa")
  `[source]` [view/properties/tool_call_matching.py#L15][o-tcm], [view.py#L74][o-enforce].
  * In W6 the pending charge was then executed (approved). Its observation was appended to an
    incrementally maintained view that no longer held the call.
  * So the next model request contained a charge tool result **without its tool call** (1
    assistant message, 2 tool results) `[experiment]` (`W6-sdk-*` traces).
  * A provider that checks pairing would reject that history `[inferred]`.

## 4. Agent server recovery rule and lease

* **Lease first.**
  * `EventService.start()` claims `ConversationLease` (`owner_lease.json`: owner, generation,
    expiry, host, pid; default TTL 45 s) before building the conversation `[source]`
    [event_service.py#L1179-L1196][o-start], [conversation_lease.py#L20][o-ttl].
  * `claim()` refuses a live, unexpired owner. It takes over an expired lease, or a dead pid on
    the same host, with `generation + 1` [#L122-L164][o-claim], [#L166][o-dead].
  * Every event and base-state write goes through `guarded_write(generation)`, which verifies the
    owner and generation under the lock [#L197][o-guarded].
  * `[experiment]` (`logs/lease.json`): a second claimant is refused while A is live. After
    expiry B gets generation 2, and A's `guarded_write` and `renew` both raise
    `ConversationOwnershipLostError`. In E2 every server resume took over a dead pid's lease
    immediately (generation 1 → 2).
  * **Weakness:** an unparseable lease file is "treated as stale", so the next claim restarts at
    **generation 1** `[source]` [#L266][o-stale-lease], `[experiment]`. The owner-id check still
    fences a different instance, but the generation is no longer monotonic.
  * The lease file write is `write_text` + `replace`, with no fsync `[source]` [#L271][o-write-payload].
* **Then recovery** `[source]` [event_service.py#L1361-L1400][o-recover]:
  1. `RUNNING` at load is stale: set it to `ERROR`.
  2. `get_unmatched_actions(state.events)` scans the **full log**, not the branch.
  3. For the **first** unmatched action only, unless an observation-like event with its
     `tool_call_id` exists, append `AgentErrorEvent(parent_id=first_action.id, error="A restart
     occurred while this tool was in progress. … The tool execution was interrupted and did not
     complete.", classification=INTERNAL, retryable=False)`.
  * Parenting to the action re-attaches the lagging branch `[experiment]` (W1, W2, W4a:
    `final_state_consistent` judged on the branch).
* **Gaps the experiment and source show:**

  | Gap | Evidence |
  | --- | --- |
  | **Interrupted is not "did not complete".** In W2 the ledger has the charge and the record says it did not complete. stack-agent's FeltDB refuses to guess here (`OutcomeUncertain`, `chip-agent-state/src/lib.rs:752-795`) | `[experiment]` |
  | **W1 and W2 leave identical recovery facts.** A model that re-issues after the error is correct in W1 (1 charge) and duplicates in W2 (2 charges). No model policy is right in both | `[experiment]` (`*-retry-on-error`) |
  | **An off-branch *matched* pair is not repaired (W4b).** The observation file exists, so the full-log scan sees the action as answered. The branch still ends before both, and the reactive script charged again | `[experiment]` |
  | **The rule keys on `RUNNING` only.** In confirmation mode the persisted status after an approved-then-crashed tool is `WAITING_FOR_CONFIRMATION` (W2c). The rule does not fire, and accepting (`respond_to_confirmation(accept=True)` → `run()`) re-executes | `[source]` [event_service.py#L2015][o-confirm-accept], `[experiment]` |
  | **Only `unmatched_actions[0]` is answered.** With several calls in flight (parallel batches), the others stay unmatched. After `ERROR` → `run()`, the SDK's implicit confirmation would execute those on the branch | `[source]`; consequence `[inferred]`, not run (the harness issues one call per turn) |

## 5. Condensers

* `Condensation{forgotten_event_ids, summary, summary_offset, llm_response_id}` is an ordinary
  appended event. The LLM view is the log minus the forgotten ids, with the summary inserted at
  the offset. The log itself is never rewritten `[source]` [event/condenser.py#L11-L37][o-condensation],
  [llm_summarizing_condenser.py#L272][o-summarizer].
* A context-window or malformed-history error from the provider triggers `CondensationRequest`
  (when the condenser handles requests) instead of failing `[source]` [agent.py#L1040-L1085][o-condense-req].
  That is a recovery-by-rewrite loop that stack-agent excludes `[inferred]`.
* Compared with stack-agent: Chip's context condensation is validated before sending (each
  condensed item must be one recorded successful execution, `chip-core/src/work.rs:1830-1878`),
  and `chip-session` refuses `compacted_summary` turns (`chip-session/src/turn.rs:8-10`)
  `[source]`. The portable part is the *representation*: compaction as an event that names what
  it hides. That keeps a FeltDB-style log append-only and auditable. The LLM-written summary is
  not portable, because it is model text entering context as if it were history `[inferred]`.

## 6. Stuck detector

* Five patterns are evaluated over the events since the last user message, scanning at most 20
  events `[source]` [stuck_detector.py#L104-L154][o-stuck]:
  1. the same action and observation, repeated (threshold 4);
  2. the same action and an error (3, with **one nudge** first, a `MessageEvent` to the model);
  3. an agent monologue (3);
  4. an alternating A/B action-observation pattern (6);
  5. a context-window-error loop (≥ 10 events).
  Thresholds: [conversation/types.py#L150-L162][o-thresholds].
* Equality ignores ids. Actions compare `source`, `thought`, `action` and `tool_name`, and
  observations compare their content `[source]` [#L325][o-eq].
* A detection sets `STUCK` and ends `run()` `[source]` [local_conversation.py#L761-L784][o-check-stuck].
* For stack-agent:
  * Patterns 1, 4 and 5 are deterministic predicates over Chip's own trajectory, and fit as an
    additional terminal guard (`Blocked("stuck: …")`). They would matter in the reliability-
    over-horizon experiment, where `max_turns` alone allows up to 8 identical requests `[inferred]`.
  * The nudge (pattern 2) is a model-facing retry prompt and conflicts with AGENTS.md §21
    `[inferred]`.
  * Evidence reuse already suppresses identical re-executions of reusable capabilities
    (`chip-core/src/evidence.rs:16-52`) `[source]`.

## 7. Confirmation mode

* Policies: `AlwaysConfirm`, `NeverConfirm`, `ConfirmRisky(threshold, confirm_unknown)` over a
  `SecurityRisk` from an optional analyzer. A lone `FinishAction` or `ThinkAction` never asks
  `[source]` [confirmation_policy.py#L27-L61][o-policy], [agent.py#L1131-L1205][o-requires].
* The suspension is a **persisted status**, `WAITING_FOR_CONFIRMATION`, not a record of a pending
  request. It works as follows `[source]`:
  * The pending action is just an unmatched `ActionEvent`.
  * Approval is `run()`, which flips the status to RUNNING [local_conversation.py#L2025-L2031][o-approve-flip]
    and then executes unmatched actions.
  * Rejection is `reject_pending_actions()`, which appends a `UserRejectObservation` for each
    one [#L2693-L2731][o-reject-fn].
  * Who approved, and when, is not recorded.
* W6: SIGKILL while waiting. After the restart the status and the pending action are intact.
  Approving (SDK `run()`, server accept) charged once, and rejecting charged zero
  `[experiment]`.
* W2c: approve, then crash inside the tool. The deferred status save leaves the status saying
  "still waiting", and the second approval re-executes: **2 charges in both paths**
  `[experiment]`. A durable approval event *before* execution would separate "approved, may have
  run" from "awaiting approval" `[inferred]`.
* **Correction to the screen.** "A new user message while awaiting confirmation rejects the
  pending action first" holds only in the async `_arun` path, and only when the message arrives
  during a step [local_conversation.py#L2341-L2360][o-supersede]. `send_message` and the sync
  `run()` do not reject `[source]`.

## 8. Durability D1–D4

| | Verdict | Evidence |
| --- | --- | --- |
| D1 Save | **Yes.** Per-event fsynced files, plus `base_state.json`. There is no directory fsync, and HEAD and status saves are deferred to the end of each step | `[source]` §2; `[experiment]` (every pre-kill event present after `SIGKILL`) |
| D2 Resume | **Yes, without manual reconstruction**, in both paths: 31/31 resumes ran to `finished`, and none crashed on resume. But the SDK continues from the **last completed step**, so the interrupted step's recorded events are silently orphaned | `[experiment]` |
| D3 Safe recovery | **No in the bare SDK, partial in the server.** SDK: `progress_lost` in W1, W2, W4a and W4b (recorded action and observation dropped from the branch), and the orphan stays unmatched forever. Server: the branch is re-attached for unmatched actions (W1, W2, W4a), but not for an off-branch matched pair (W4b), and only the first unmatched action is answered | `[experiment]`, `[source]` §4 |
| D4 Effect safety | **No.** No idempotency key. The SDK lets the model redo an orphaned charge (duplicate). The server converts ambiguity into "did not complete" and lets the model decide (duplicate when it retries). Confirmation mode re-executes an approved-then-interrupted tool in both paths, with no model in the loop (W2c) | `[experiment]` |

## 9. Crash-window results (E2)

One run per case, with a real `SIGKILL` from inside the process at a deterministic point and a
fresh-process resume. Ground truth is the ledger. The `count` script is the protocol fixture
(model calls in this process plus replies already recorded in the log). The `visible` script
replies from what the model is shown. Full records: `results.json` → `e2.windows`.

| Window | Kill point | bare SDK `count` | bare SDK `visible` | server `count` | server `visible` |
| --- | --- | --- | --- | --- | --- |
| W1 | charge entry, before ledger | lost_progress: 0 charges, false completion | correct (1), decision redone | lost_progress (0), error "did not complete" | lost_progress (0) |
| W2 | inside charge, after ledger fsync | correct (1), branch shows no charge | **duplicate_effect (2)** | correct (1), record says "did not complete" | correct (1), same |
| W2c | as W2, after the person approved | **duplicate_effect (2)** | **duplicate_effect (2)** | **duplicate_effect (2)** | **duplicate_effect (2)** |
| W3 | start of model call 3 | correct | correct | correct | correct |
| W4a | write guard, before charge observation file | correct (1), state ≠ ledger | **duplicate_effect (2)** | correct (1) | correct (1) |
| W4b | after observation file, before HEAD/marker | correct (1), state ≠ ledger | **duplicate_effect (2)** | correct (1), state ≠ ledger | **duplicate_effect (2)** |
| W5 | handoff | not_applicable (not exercised) | | | |
| W6 | awaiting approval of charge | correct (approve → 1; reject → 0) | correct | correct | correct |

Sensitivity (server, a model that re-issues charge after the restart error): W1 is correct (1)
and W2 is `duplicate_effect` (2) `[experiment]`.

`manual_intervention` was none, except approve or reject in W6/W2c. `resumed: true` in all 31 cases.

## 10. What stack-agent can take, and what it already does better

| OpenHands mechanism | stack-agent today | Take? |
| --- | --- | --- |
| `ActionEvent` fsynced before the tool runs | FeltDB `record_intent` before the executor; the executor is not called if that fails (`chip-agent-state/src/lib.rs:1141-1160`) | covered |
| Unmatched action on restart → synthetic error to the model | Open intent without an observation → `OutcomeUncertain`, nothing runs (`lib.rs:752-795`) | **no**. FeltDB's block is the right rule, and W1/W2 show why |
| Full-log scan, not HEAD | `begin` reconciles every open intent from the store (`lib.rs:752`) | covered. Lesson: never keep a second, separately saved pointer (HEAD) that can lag the log. Derive it |
| Owner lease + generation + guarded writes | FeltDB lease, fence on every operation, `FENCE_STALE` (`lib.rs:706-747`, `:851-860`) | covered, and stronger (no corrupt-file reset). OpenHands's same-host dead-pid fast takeover is a possible addition for the 900 s lease wait in the restart bench rows (baseline §3.7) `[inferred]` |
| `Condensation{forgotten_event_ids}` as an event | validated condensation, recomputed per request; nothing persisted | the representation only, if context state is ever persisted |
| Stuck patterns 1, 4, 5 | turn and execution limits, evidence reuse | **yes**, as a deterministic terminal guard (no nudge) |
| Approval = status flag + `run()` | no approval primitive (baseline §4) | if approval is ever added: record the approval as a durable event before execution, keyed to the intent, so "approved, maybe ran" is distinct from "awaiting" (W2c) |

### Boundary sketch: stuck guard (the one direct port)

```rust
// chip-core: deterministic, read-only over the run's own trajectory. Never prompts the model.
pub enum StuckPattern { RepeatedRequestObservation { n: usize }, Alternating { n: usize }, ContextLimitLoop }
pub fn stuck(view: &WorkView, thresholds: &StuckThresholds) -> Option<StuckPattern>;
// run_work: checked after each observation, before the next turn ->
//   WorkOutcome::Blocked(format!("stuck: {pattern:?}")); counted by verify_trajectory.
```

* **Already covered:** the limits, and `ruled_out`.
* **New:** the predicates.
* **Complexity:** S.
* **Dependencies:** none. The trajectory and observations already exist in `WorkView`.

## 11. Recommendations

1. **Do not adopt "interrupted ⇒ error to the model".** Keep FeltDB's `OutcomeUncertain` block.
   The experiment gives the evidence: the same recovery fact is right in W1 and wrong in W2.
2. **Derive every position pointer from the log.** OpenHands's HEAD and status lag produced
   orphaned progress and duplicates in four of six windows in the SDK. Chip's planned resume
   (see [`rig.md`](rig.md) §2.6) must restore loop state from the same FeltDB records it
   reconciles, not from a separately saved snapshot.
3. **If approvals arrive, make approval an intent-level record written before execution.** It
   must not be a status bit cleared by the next `run()`.
4. **Add the stuck guard (patterns 1, 4, 5)** as a bounded, non-retrying terminal check.
5. **Fence generations monotonically**, even across a corrupt lease record (FeltDB already does).

[o-leaf]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/state.py#L176
[o-action]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/event/llm_convertible/action.py#L24
[o-obs]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/event/llm_convertible/observation.py#L32
[o-reject]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/event/llm_convertible/observation.py#L86
[o-err]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/event/llm_convertible/observation.py#L138
[o-get-action]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/agent/agent.py#L1342
[o-emit-action]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/agent/agent.py#L1494
[o-dispatch]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/agent/response_dispatch.py#L154-L198
[o-batch]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/agent/agent.py#L226
[o-concurrency]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/agent/base.py#L302
[o-executor]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/tool/tool.py#L283
[o-append]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/event_store.py#L188-L238
[o-marker]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/event_store.py#L261
[o-atomic]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/utils/files.py#L21-L39
[o-local-write]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/io/local.py#L63
[o-autosave]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/state.py#L600-L622
[o-exit]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/state.py#L753-L765
[o-runloop]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/impl/local_conversation.py#L1981-L2045
[o-append-event]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/state.py#L315-L337
[o-recover]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-agent-server/openhands/agent_server/event_service.py#L1361-L1400
[o-resume]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/state.py#L526-L567
[o-run]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/impl/local_conversation.py#L1961-L2000
[o-implicit]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/agent/agent.py#L714-L723
[o-unmatched]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/state.py#L677-L716
[o-tcm]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/context/view/properties/tool_call_matching.py#L15
[o-enforce]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/context/view/view.py#L74
[o-start]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-agent-server/openhands/agent_server/event_service.py#L1179-L1196
[o-ttl]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-agent-server/openhands/agent_server/conversation_lease.py#L20
[o-claim]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-agent-server/openhands/agent_server/conversation_lease.py#L122-L164
[o-dead]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-agent-server/openhands/agent_server/conversation_lease.py#L166
[o-guarded]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-agent-server/openhands/agent_server/conversation_lease.py#L197
[o-stale-lease]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-agent-server/openhands/agent_server/conversation_lease.py#L266
[o-write-payload]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-agent-server/openhands/agent_server/conversation_lease.py#L271
[o-confirm-accept]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-agent-server/openhands/agent_server/event_service.py#L2015
[o-condensation]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/event/condenser.py#L11-L37
[o-summarizer]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/context/condenser/llm_summarizing_condenser.py#L272
[o-condense-req]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/agent/agent.py#L1040-L1085
[o-stuck]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/stuck_detector.py#L104-L154
[o-thresholds]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/types.py#L150-L162
[o-eq]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/stuck_detector.py#L325
[o-check-stuck]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/impl/local_conversation.py#L761-L784
[o-policy]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/security/confirmation_policy.py#L27-L61
[o-requires]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/agent/agent.py#L1131-L1205
[o-approve-flip]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/impl/local_conversation.py#L2025-L2031
[o-reject-fn]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/impl/local_conversation.py#L2693-L2731
[o-supersede]: https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360/openhands-sdk/openhands/sdk/conversation/impl/local_conversation.py#L2341-L2360
