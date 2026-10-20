# Tier 3 screening: Rig, Pydantic AI, OpenHands, Google ADK, LlamaIndex Workflows, Rust-native runtimes

Screening depth: source-level reading at pinned commits, plus one cheap decisive experiment
(Pydantic AI StepPersistence under SIGKILL, `experiments/pydantic-ai/`). Evidence tags follow
`protocol.md`. All clones were shallow (`--depth 1`, deepened only to find the latest meaningful
commit) and no project scripts were run. GitHub API access was not available in this session, so
canonical URLs were verified by `git clone`/`git ls-remote` against the URL plus the project's own
`repository` fields and READMEs, not by API metadata. Star counts are not used.

Target context (only for portability judgments): stack-agent is a Rust, turn-based loop with a
closed JSON decision contract; Chip validates/authorizes/executes capabilities sequentially;
completion needs the required observations; FX crates own provider wires (OpenAI-compatible and
Anthropic); FeltDB is the durable contract (intent before execution, observation and outcome
after, reconcile on restart, an unresolved intent blocks). Retries/repair loops are excluded by
policy, and there are no handoffs or approvals today.

## Summary

| Candidate | Verdict | Most portable mechanism |
| --- | --- | --- |
| Rig (Rust) | **Deeper study justified** | Wire/Transport split; serializable sans-I/O `AgentRun` state machine; serializable `EffectRecord` with `Disposition`/`Decider`; restore marks started/settled tool calls `Interrupted` |
| Pydantic AI (+ Harness) | **Deeper study justified** (narrowly: StepPersistence's tool-effect ledger and deferred-tool contract) | `started → completed/failed` tool-effect ledger keyed `(run_id, tool_call_id)`, recovery "facts, not replay permission"; `DeferredToolRequests/Results` |
| OpenHands (software-agent-sdk) | **Deeper study justified** (event log, orphan handling, lease) | Fsynced per-event files; on stale RUNNING, unmatched action → synthetic error, not re-run; owner lease with generation; `Condensation` as an event |
| Google ADK | Not justified beyond what's noted here | `ResumeAction {CONTINUE, PAUSE, REPLAY_CALLS}` decision; stale-session optimistic check; explicitly at-least-once |
| LlamaIndex Workflows | Not justified | Tick → reduce → command control loop (journaled ticks); everything else is at-least-once step replay |
| goose (Rust; added) | **Deeper study justified** (small: `goose-agent` machine) | load → pure step → effects → apply loop over persisted conversation; operation notes on messages so a rebuilt pipeline decides the same |
| swarms-rs | Excluded | — |
| AutoAgents (Rust; added) | Excluded | — |
| langchain-rust (considered) | Excluded: last commit 2025-04-30 | — |

---

## Rig

**Record**
* Repository: https://github.com/0xPlaygrounds/rig (canonical; `Cargo.toml` `repository` matches).
* Commit: `98ed06d7f1c911e6e4c7fdb5d690093bc964c514` — https://github.com/0xPlaygrounds/rig/tree/98ed06d7f1c911e6e4c7fdb5d690093bc964c514
* License: MIT (`LICENSE`, "Copyright (c) 2024, Playgrounds Analytics Inc.").
* Last meaningful activity: 2026-10-19, "chore: release v0.45.0 (#2753)" (very active; frequent releases).
* Package for experiments: none (no build run).

Base for links: `R=https://github.com/0xPlaygrounds/rig/blob/98ed06d7f1c911e6e4c7fdb5d690093bc964c514`

**Inspected areas.** `crates/rig-core` (`wire.rs`, `driver.rs`, `effect/`, `serve/{recorder,layer}.rs`, `tool/`, `providers/`), `crates/rig-agent/src/run/` (sans-I/O run machine, invalid-call policy), `crates/rig-ecs/src/{restore,session_log}.rs`, `crates/rig-memory/src/dir.rs`, `crates/rig-cassette` (record/replay). The repository has changed a lot from the "CompletionModel/Tool/ToolSet" shape the brief describes: there is now a workspace of ~30 crates including its own coding-agent harness (`rig-harness`, Bevy ECS based).

**Provider boundary (most relevant to FX).**
* A model is `Model<W, T>`: a `Wire` (pure encode + decoder + reassembler for one operation) bound to a `Transport` (how payload bytes move) `[source]` — [`driver.rs#L184`]($R/crates/rig-core/src/driver.rs#L184), [`wire.rs#L743`]($R/crates/rig-core/src/wire.rs#L743). `Wire::encode` is documented as pure: it may read only `self`, the request and the mode `[source]` (same lines). Providers ship as wires (`providers/anthropic/wire.rs`, `providers/openai/...`), so a recorded stream or batch body can be decoded with no transport (`decode_reply`) `[source]`.
* Every model/tool/embedding/retrieval call is a serializable `EffectKind` routed to a handler by a string `HandlerKey` with a fixed grammar (`[owner/][kind:]label[#generation]`), answered with `Answered`, and recorded as an `EffectRecord` `[source]` — [`effect/mod.rs#L824`]($R/crates/rig-core/src/effect/mod.rs#L824). The record carries `parent`, `scope`, `purpose` (`"turn"`, `"compaction"`), `turn`, timestamps, a `reviewed` override, a `Disposition` (`Ran`, `AnsweredBeforeStart`, `AnsweredWhileRunning`, `Superseded`, `Cancelled`) and a `Decider` (`Person`, `Plugin(name)`) — [`#L899`]($R/crates/rig-core/src/effect/mod.rs#L899), [`#L919`]($R/crates/rig-core/src/effect/mod.rs#L919).
* `Recorder::begin` is called as each dispatch is handed to its handler, `resolve` when the outcome is known `[source]` — [`serve/recorder.rs#L40`]($R/crates/rig-core/src/serve/recorder.rs#L40). That is an in-process intent/outcome pair; durability depends on the recorder.
* Policy layers: `Intercept::before` returns `Proceed | Patch(EffectKind) | Deny(ErrorReport)` and `after` returns `Keep | Replace(...)`; replay re-runs layers rather than recording their decisions `[source]` — [`serve/layer.rs#L29`]($R/crates/rig-core/src/serve/layer.rs#L29), [`#L69`]($R/crates/rig-core/src/serve/layer.rs#L69). This is close to Chip's validate/authorize step expressed as a layer over a handler.

**Agent loop.**
* `rig_agent::run::AgentRun` is described as a "serializable, sans-I/O state machine": the driver calls `next_step()` and gets `CallModel{prompt, history, turn} | CallTools{calls} | Done` `[source]` — [`run/mod.rs#L76`]($R/crates/rig-agent/src/run/mod.rs#L76). `next_step` is idempotent while it waits for a response or results: a persisted run re-emits the same attempt (same `turn`, no extra budget), and a `CallTools` step lists only the unanswered calls `[source]` — doc at [`run/mod.rs#L829-L852`]($R/crates/rig-agent/src/run/mod.rs#L829). History is an append-only `CommittedLog` `[source]`.
* Invalid tool calls are classified (`UnknownTool`, `DisallowedByToolChoice`, `MalformedArguments{error}`), and the driver resolves them with `Fail | Retry{feedback} | Repair{tool_name} | Skip{reason} | Stop{reason}` `[source]` — [`run/policy.rs#L24`]($R/crates/rig-agent/src/run/policy.rs#L24), [`#L305`]($R/crates/rig-agent/src/run/policy.rs#L305). `Fail` and `Stop` fit stack-agent's no-repair policy, and the enum shows where a repair loop would plug in if one were ever wanted.

**D1–D4**
* D1 Save: `rig-memory` stores sessions (`DirStore`, `MemoryStore`, `AsyncStore`). Whole-file writes go through `write_durable` (temp file + `sync_all` + rename + folder sync) `[source]` — [`rig-memory/src/dir.rs#L141`]($R/crates/rig-memory/src/dir.rs#L141). Log appends (`append_file`) have no `sync_all` `[source]` — [`#L172`]($R/crates/rig-memory/src/dir.rs#L172). So the log survives a process crash (page cache) but power loss is not covered `[inferred]`. Log writes are compare-length-and-append, so the second of two saves that read the same log is refused (`SessionError::Changed`) `[source]` (`rig-memory/src/lib.rs` module doc).
* D2 Resume: in the ECS harness, `restore_session` respawns agents from their logs, then one reconcile pass settles anything half done `[source]` — [`rig-ecs/src/restore.rs#L1-L40`]($R/crates/rig-ecs/src/restore.rs#L1). The core `AgentRun` is serde, so resume means deserialize and call `next_step` `[source]`. Not run here.
* D3 Safe recovery: changes are saved "before a tool that may change something starts" `[source]` — [`session_log.rs#L7-L9`]($R/crates/rig-ecs/src/session_log.rs#L7). Restored calls that had started or settled are marked `Interrupted::{Started, Settled(result)}`. After every gate runs, the kernel answers `Started` with a "restart interrupted it" error result and `Settled` with its kept result, unless a plugin decided first or holds it for a person `[source]` — [`restore.rs#L205`]($R/crates/rig-ecs/src/restore.rs#L205), [`#L214`]($R/crates/rig-ecs/src/restore.rs#L214). "Nothing held or approved before the restart is kept": approvals are asked again `[source]` (module doc).
* D4 Effect safety: an interrupted call is **not re-run by default**. The model receives an error result and decides. That is surfacing, not idempotency: no idempotency key reaches the tool `[source]`, `[inferred]`. Tool `Footprint`/`Touches` metadata exists and is passed to `not_finished(RESTART, …, footprint)` `[source]`. Its effect on recovery wording was not traced.

**Handoffs/approval.** Approval lives in the harness: gates and holds (`HoldOf`), and `Decider::Person` on the record `[source]`. Subagents (`SpawnedBy`, restored child before parent) `[source]`. The core run machine has neither `[source]`.

**Provider neutrality and local models.** Ollama and llama.cpp providers, `rig-candle` (local inference), and many hosted providers `[source]` (`crates/rig-core/src/providers/`). Wires are per-provider types, not one lowest-common-denominator schema `[source]`.

**Uniquely valuable for stack-agent**
1. The **Wire/Transport split** with a pure `encode`. It maps directly onto FX crates: an OpenAI-compatible wire and an Anthropic wire over one transport, and a replay transport for tests `[inferred]`.
2. **`AgentRun` as a serde sans-I/O machine** whose re-issued steps are idempotent and unbudgeted. It is the closest external analogue to stack-agent's turn loop, and the "re-issue is the same attempt" contract is worth copying verbatim into FeltDB reconcile semantics `[inferred]`.
3. **`EffectRecord.disposition` and `decided_by`**: vocabulary for "answered before start / superseded / cancelled / decided by a person" that FeltDB outcomes currently lack `[inferred]`.
4. **`Interrupted::{Started, Settled}`** on restore: the same idea as FeltDB's unresolved intent. Rig answers with an error by default, while FeltDB blocks. Comparing the two is useful `[inferred]`.

**Verdict: deeper study justified.** Focus on `rig-core/src/{wire,driver,effect,serve}` and `rig-agent/src/run`. Caveat: rapid API churn (v0.45, many crates), so pin commits.

---

## Pydantic AI (with Pydantic AI Harness)

**Record**
* Repository: https://github.com/pydantic/pydantic-ai (canonical). Pydantic AI Harness now lives in the same repo under `src/pydantic_ai_harness`. The old `pydantic/pydantic-ai-harness` repository says it "keeps the history up to v0.36.0 and is no longer updated" `[source]` (its README at `4399efacd4755ff6feccc727d3180a97428e76cf`).
* Commit: `60c1d1a6cdbe51f7b80150f9e237488b51ade335` — https://github.com/pydantic/pydantic-ai/tree/60c1d1a6cdbe51f7b80150f9e237488b51ade335
* License: MIT (`LICENSE`, Pydantic Services Inc.).
* Last meaningful activity: 2026-10-19, "Fix `UseEnumMemberDocstrings` enums, `Choices` and `BoolCriteria` failing…".
* Experiment packages: `pydantic-ai-slim==2.57.0`, `pydantic-ai-harness==0.57.0`, `pydantic-graph==2.57.0` (PyPI).

Base: `P=https://github.com/pydantic/pydantic-ai/blob/60c1d1a6cdbe51f7b80150f9e237488b51ade335`

**Inspected areas.** `pydantic_ai_slim/pydantic_ai/{_deferred,exceptions,usage,_tool_execution}.py`, `durable_exec/{temporal,dbos,prefect}`, `pydantic_graph/`, `src/pydantic_ai_harness/.../step_persistence/`, `docs/{persistence,changelog,migration}.md`.

**Important correction to the brief.** `pydantic_graph.persistence` (`BaseStatePersistence`, `FileStatePersistence`, snapshot/node status) **was removed in V2**. "The builder API deliberately does not snapshot graph state", and the replacement for run state is Harness `StepPersistence` `[source]` — [`docs/changelog.md#L123`]($P/docs/changelog.md#L123) (PR #5470). There is nothing left to evaluate in pydantic-graph persistence. Durable execution for mid-step crashes goes through the Temporal/DBOS/Prefect wrappers (`TemporalAgent` etc.) `[source]` — [`durable_exec/temporal/_agent.py#L127`]($P/pydantic_ai_slim/pydantic_ai/durable_exec/temporal/_agent.py#L127). Those inherit the engine's semantics and were not evaluated.

**Validation and retries.** `ModelRetry` (from tools and output validators) and argument-validation failures go back to the model as `RetryPromptPart` until `max_retries`/`max_output_retries`, then `UnexpectedModelBehavior` `[source]` — [`exceptions.py#L55`]($P/pydantic_ai_slim/pydantic_ai/exceptions.py#L55), [`_tool_execution.py#L551-L581`]($P/pydantic_ai_slim/pydantic_ai/_tool_execution.py#L551). Confirmed: a scripted call missing `amount` produced one retry, and the tool never ran with bad arguments `[experiment]`. `UsageLimits` checks the request count before each step and token counts after each response `[source]` — [`usage.py#L492`]($P/pydantic_ai_slim/pydantic_ai/usage.py#L492). The retry loop is a policy stack-agent rejects. Strict argument validation with no execution on failure is the part that ports `[inferred]`.

**Deferred tools / approval.** A tool raising `ApprovalRequired` or `CallDeferred` (or registered with `requires_approval=True`) ends the run with output `DeferredToolRequests{calls, approvals, metadata}`. The next run passes `DeferredToolResults{approvals: id → bool | ToolApproved(override_args) | ToolDenied(message), calls: id → result}`, keyed by `tool_call_id` `[source]` — [`_deferred.py#L28`]($P/pydantic_ai_slim/pydantic_ai/_deferred.py#L28), [`#L104`]($P/pydantic_ai_slim/pydantic_ai/_deferred.py#L104), [`#L159`]($P/pydantic_ai_slim/pydantic_ai/_deferred.py#L159). Suspension is a *run end*, not a parked coroutine, so state is whatever the application stores `[source]`.

**StepPersistence (Harness).** "Append-only step log + continuable snapshots + tool-effect ledger" `[source]` — [`_capability.py#L50`]($P/src/pydantic_ai_harness/pydantic_ai_harness/step_persistence/_capability.py#L50). `before_tool_execute` writes `ToolEffectRecord(status='started')` before the tool body. `after_tool_execute` / `on_tool_execute_error` write `completed` / `failed` `[source]` — [`#L392`]($P/src/pydantic_ai_harness/pydantic_ai_harness/step_persistence/_capability.py#L392), [`#L611`]($P/src/pydantic_ai_harness/pydantic_ai_harness/step_persistence/_capability.py#L611). Reusing an explicit `run_id` raises, because the ledger keys on `(run_id, tool_call_id)` and providers reuse deterministic call ids `[source]` (`_capability.py` docstring, `run_id`). A tool can attach `idempotency_key` / `effect_summary` to its in-flight record `[source]` — [`_helpers.py#L93`]($P/src/pydantic_ai_harness/pydantic_ai_harness/step_persistence/_helpers.py#L93). `inspect_recovery` returns latest/settled snapshots and unresolved effects, explicitly "Recovery facts, not replay permission" `[source]` — [`recovery.py#L1-L24`]($P/src/pydantic_ai_harness/pydantic_ai_harness/step_persistence/recovery.py#L1).

**D1–D4** (FileStepStore; `experiments/pydantic-ai/results.json`)
* D1 Save: yes. JSONL events/effects plus JSON snapshots, temp file + `os.replace`, **no fsync** `[source]` — [`_store.py#L44`]($P/src/pydantic_ai_harness/pydantic_ai_harness/step_persistence/_store.py#L44). SQLite (WAL), Postgres and Mongo backends also exist `[source]`.
* D2 Resume: yes via the application. Load `latest_snapshot(...).messages` and pass it to `Agent.run(message_history=...)` under a new `run_id` `[experiment]`. W1, W2, W3 and W6 all resumed without reconstruction.
* D3 Safe recovery: snapshots are taken only at settled `CallToolsNode` boundaries. After a kill inside `charge`, the latest snapshot is the one after `lookup`, so the charge decision is redone `[experiment]` (W1/W2 `progress_lost: true`). W3 was correct `[experiment]`.
* D4 Effect safety: the **ambiguity is detected but not enforced**. W1 and W2 both left `charge started` unresolved, and the two cases are indistinguishable. A naive documented continuation produced **2 charges in W2** (`duplicate_effect`) `[experiment]`. Safety needs the caller to refuse when `list_unresolved_tool_effects` is non-empty, which is exactly FeltDB's "unresolved intent blocks" rule `[inferred]`. W6 (killed while awaiting approval) was correct, but only because the harness stored `all_messages()` and pending ids itself. StepPersistence's snapshot did not hold the pending call `[experiment]`.

**Handoffs.** None in core (delegation = calling an agent from a tool). Harness subagents record `parent_run_id` lineage `[source]`.

**Provider neutrality and local models.** Many providers, plus OpenAI-compatible endpoints (Ollama etc.) `[claim]` (docs/models). `FunctionModel`/`TestModel` make fake-model testing first-class `[source]`/`[experiment]`.

**Uniquely valuable**
1. The **tool-effect ledger state machine** (`started` → `completed|failed`, with optional `idempotency_key`/`effect_summary` annotated *by the tool*) and its explicit doctrine that recovery facts are not replay permission. It independently converges on FeltDB's intent/outcome design. The tool-supplied idempotency key annotation is the piece FeltDB could add `[inferred]`.
2. **`DeferredToolRequests/Results` keyed by tool_call_id with `ToolApproved(override_args)` / `ToolDenied(message)`**: a compact, serializable approval contract, if stack-agent ever adds approvals `[inferred]`.

**Verdict: deeper study justified, narrowly** (ledger and deferred contract). Graph persistence is gone, and retries conflict with policy.

---

## OpenHands

**Record**
* Repositories: `All-Hands-AI/OpenHands` now resolves to https://github.com/OpenHands/OpenHands (`git ls-remote` of both gives the same HEAD `8d39305538c1d17a30cbe477e57a0e18fcee7d95`). That repo is now the TypeScript "Agent Canvas" app. Its README says the runtime is https://github.com/OpenHands/software-agent-sdk ("Python SDK, Agent Server, agents, tools, conversations, workspaces, events") `[source]` (OpenHands `README.md#L148-L172`). The **evaluated repository is software-agent-sdk**.
* Commit: `33d8aa29c3078d074799e2fbf5e847aeb2d71360` — https://github.com/OpenHands/software-agent-sdk/tree/33d8aa29c3078d074799e2fbf5e847aeb2d71360
* License: MIT (`LICENSE`, "Copyright (c) 2026 OpenHands contributors"). OpenHands/OpenHands is also MIT.
* Last meaningful activity: 2026-10-19, "Release v1.56.0 (#6091)".
* Experiment package: none.

Base: `O=https://github.com/OpenHands/software-agent-sdk/blob/33d8aa29c3078d074799e2fbf5e847aeb2d71360`

**Inspected areas.** `openhands-sdk/openhands/sdk/{conversation/{event_store,state,stuck_detector}.py, conversation/impl/local_conversation.py, agent/agent.py, event/, context/condenser/, security/confirmation_policy.py, io/local.py, utils/files.py}`, `openhands-agent-server/.../{event_service,conversation_lease}.py`, and the `openhands-workspace` layout (docker, apptainer, remote sandboxes).

**Event model.** Frozen pydantic `Event`s (`ActionEvent`, `ObservationEvent`, `UserRejectObservation`, `AgentErrorEvent`, `MessageEvent`, `Condensation`, …) `[source]`. `EventLog.append` writes one file per event under a file lock `[source]` — [`event_store.py#L188`]($O/openhands-sdk/openhands/sdk/conversation/event_store.py#L188). Text writes use `atomic_write_text` with `os.fsync` `[source]` — [`utils/files.py#L21-L38`]($O/openhands-sdk/openhands/sdk/utils/files.py#L21).

**D1–D4**
* D1 Save: yes, fsynced per-event files plus base state `[source]`.
* D2 Resume: `ConversationState.create` creates or resumes from `persistence_dir` `[source]`. Not run.
* D3 Safe recovery: in the agent server, a conversation loaded with status `RUNNING` is treated as stale and set to `ERROR`. The first unmatched `ActionEvent` gets a synthetic `AgentErrorEvent` parented to the action, unless an observation for that `tool_call_id` already exists. The scan covers the full log, because "the process may have died between writing an event file and persisting the advanced HEAD" `[source]` — [`event_service.py#L1361-L1395`]($O/openhands-agent-server/openhands/agent_server/event_service.py#L1361). Split-brain is prevented by an owner lease (`owner_lease.json`, generation, TTL 45 s, takeover flag) `[source]` — [`conversation_lease.py`]($O/openhands-agent-server/openhands/agent_server/conversation_lease.py).
* D4 Effect safety: the server path surfaces an interrupted action as an error, which the model sees. It does not re-run it `[source]`. **But** the bare SDK's `Agent._step` executes any unmatched actions found at step start as "implicit confirmation" `[source]` — [`agent/agent.py#L714-L722`]($O/openhands-sdk/openhands/sdk/agent/agent.py#L714). A `LocalConversation` resumed directly from disk after a crash between `ActionEvent` and `ObservationEvent` would therefore re-execute the action `[inferred]` (decisive experiment: E2 W2 against `LocalConversation` with a file store). In-process interrupts backfill orphans with "Tool call interrupted before completion" `[source]` — [`local_conversation.py#L2733`]($O/openhands-sdk/openhands/sdk/conversation/impl/local_conversation.py#L2733). No idempotency keys `[source]`.

**Other mechanisms**
* Confirmation mode: `AlwaysConfirm | NeverConfirm | ConfirmRisky` policies, status `WAITING_FOR_CONFIRMATION`, `reject_pending_actions` → `UserRejectObservation`. A new user message while awaiting confirmation rejects the pending action first, so it cannot run on the next step `[source]` — [`confirmation_policy.py#L43`]($O/openhands-sdk/openhands/sdk/security/confirmation_policy.py#L43), [`local_conversation.py#L2693`]($O/openhands-sdk/openhands/sdk/conversation/impl/local_conversation.py#L2693).
* Condensers: `Condensation{forgotten_event_ids, summary, summary_offset}` is itself an event, and the LLM view is a projection of the log minus forgotten ids. The log is never rewritten `[source]` — [`event/condenser.py#L11`]($O/openhands-sdk/openhands/sdk/event/condenser.py#L11), [`llm_summarizing_condenser.py#L54`]($O/openhands-sdk/openhands/sdk/context/condenser/llm_summarizing_condenser.py#L54).
* Stuck detection: five patterns since the last user message (same action+observation, same action+error with a nudge, monologue, alternating A/B, context-window error loop) `[source]` — [`stuck_detector.py#L104`]($O/openhands-sdk/openhands/sdk/conversation/stuck_detector.py#L104).
* Sandboxed runtime: workspaces for docker, apptainer, cloud and remote API `[source]` (directory layout). Not evaluated.

**Handoffs.** Subagent registry/delegation (`sdk/subagent/`) `[source]`. Not traced.

**Provider neutrality and local models.** LiteLLM-based (`llm/` imports litellm) `[source]`, so any OpenAI-compatible local endpoint works `[inferred]`.

**Uniquely valuable**
1. **Recovery rule "stale RUNNING ⇒ ERROR; unmatched action ⇒ synthetic error parented to the action; scan the full log, not HEAD"**: a precise, cheap reconcile rule, and a counter-example (SDK implicit confirmation) of what not to do.
2. **Owner lease with generation and takeover**, comparable with FeltDB's lease/fence.
3. **Compaction as an event** (`forgotten_event_ids` + summary): keeps the durable log append-only.
4. **Stuck-pattern catalogue**: directly portable as a non-retrying guard that ends a run.

**Verdict: deeper study justified**, scoped to event log, recovery and lease, plus an experiment on the SDK-vs-server recovery discrepancy.

---

## Google ADK (adk-python)

**Record**
* Repository: https://github.com/google/adk-python (canonical).
* Commit: `1def14ba8efa662a5fa5d3b4c368ec8eecc9a2d6` — https://github.com/google/adk-python/tree/1def14ba8efa662a5fa5d3b4c368ec8eecc9a2d6 (`version.py` 2.12.0)
* License: Apache-2.0.
* Last meaningful activity: 2026-10-19, "fix(core): stop the unsafe local code executor waiting forever".
* Experiment package: none.

Base: `A=https://github.com/google/adk-python/blob/1def14ba8efa662a5fa5d3b4c368ec8eecc9a2d6/src/google/adk`

**Inspected areas.** `runners.py`, `sessions/{sqlite,database,in_memory}_session_service.py`, `events/event_actions.py`, `apps/_configs.py`, `flows/llm_flows/core/_resume.py`, `tools/{function_tool,long_running_tool,tool_confirmation,transfer_to_agent_tool,agent_tool}.py`, `models/lite_llm.py`, plus the `workflow/` graph package (only listed).

**Model.** `Runner` drives agents. A `Session` is a list of `Event`s, and each event's `EventActions` carries `state_delta`, `transfer_to_agent`, `escalate` and `requested_tool_confirmations` `[source]` — [`events/event_actions.py#L94`]($A/events/event_actions.py#L94), [`#L122`]($A/events/event_actions.py#L122), [`#L155`]($A/events/event_actions.py#L155). State changes only by appending events with deltas `[source]`.

**D1–D4**
* D1 Save: `SqliteSessionService`, `DatabaseSessionService` (SQLAlchemy async) and Vertex `[source]`. `append_event` refuses a stale session object: `StaleSessionError` when the stored `update_time` is newer `[source]` — [`sqlite_session_service.py#L484`]($A/sessions/sqlite_session_service.py#L484).
* D2 Resume: `ResumabilityConfig` lets a run "pause an invocation upon a long-running function call" and "resume an invocation from the last event, if it's paused or failed midway" `[claim]` — [`apps/_configs.py#L29`]($A/apps/_configs.py#L29). `decide_resume` returns `CONTINUE | PAUSE | REPLAY_CALLS`. An unanswered call in a resumable invocation → `PAUSE`, and a call "never executed" → `REPLAY_CALLS` `[source]` — [`_resume.py#L321`]($A/flows/llm_flows/core/_resume.py#L321).
* D3/D4: the project documents best effort: "Tool call to resume needs to be idempotent because we only guarantee an at-least-once behavior once resumed" and "temporary/in-memory state will be lost" `[claim]` — [`apps/_configs.py#L37-L40`]($A/apps/_configs.py#L37). No intent record separates "started" from "never executed" `[inferred]`. A crash inside a tool leaves a function-call event without a response, which reads either as PAUSE (awaiting an answer) or as replay, depending on matching `[inferred]`. Not run. The claim already settles D4 as "not protected".

**Handoffs/approval.** `transfer_to_agent` (control moves; the target sees the session's history) vs `AgentTool` (agent called as a tool, result returned) `[source]` — [`tools/transfer_to_agent_tool.py#L49`]($A/tools/transfer_to_agent_tool.py#L49), [`tools/agent_tool.py#L113`]($A/tools/agent_tool.py#L113). Confirmation: `FunctionTool(require_confirmation=bool|callable)` emits `requested_tool_confirmations`, and `ToolConfirmation` comes back as a function response. `LongRunningFunctionTool` pauses resumable invocations `[source]` — [`function_tool.py#L403`]($A/tools/function_tool.py#L403), [`long_running_tool.py#L27`]($A/tools/long_running_tool.py#L27).

**Provider neutrality and local models.** Gemini-first, with `LiteLlm` and `anthropic_llm` adapters `[source]` — [`models/lite_llm.py#L3912`]($A/models/lite_llm.py#L3912).

**Uniquely valuable (small).** The three-way `ResumeAction` names a decision stack-agent's reconcile also makes. `StaleSessionError` is an optimistic-concurrency check on append. The explicit at-least-once disclaimer is a useful contrast for FeltDB documentation.

**Verdict: deeper study not justified.** The useful ideas are captured above. Its durability is explicitly at-least-once, and the multi-agent/approval surface is broader than stack-agent needs.

---

## LlamaIndex Workflows

**Record**
* Repository: https://github.com/run-llama/workflows-py (the engine `llama-index-workflows` 2.26.0, plus `llama-agents-*` server/DBOS packages; README now titled "LlamaAgents"). `run-llama/llama_index` (HEAD `619d74664e8404612038bc15a1a40c686037277c`) consumes it as a dependency and was not cloned (size; its workflow code is the external package) `[inferred]`.
* Commit: `5c55ee37f424123fcdc1807a935f92a982d632e9` — https://github.com/run-llama/workflows-py/tree/5c55ee37f424123fcdc1807a935f92a982d632e9
* License: MIT.
* Last meaningful activity: 2026-10-13, "Test Python 3.15 compatibility across packages (#806)".

Base: `L=https://github.com/run-llama/workflows-py/blob/5c55ee37f424123fcdc1807a935f92a982d632e9/packages`

**Inspected areas.** `llama-index-workflows/src/workflows/{context/context.py, runtime/control_loop/reduce.py, runtime/types/{ticks,commands,internal_state}.py, events.py, retry_policy.py}`, `llama-agents-dbos/.../journal/task_journal.py`.

**Mechanism.** The control loop is a reducer: ticks (`TickStepResult`, `TickAddEvent`, `TickTimeout`, `TickSessionStart`, …) go through `_reduce_tick` and produce commands (`CommandRunWorker`, `CommandQueueEvent`, `CommandCompleteRun`, …). Ticks are journaled, and state can be rebuilt by replaying ticks `[source]` — [`reduce.py#L158-L257`]($L/llama-index-workflows/src/workflows/runtime/control_loop/reduce.py#L158), [`ticks.py#L137`]($L/llama-index-workflows/src/workflows/runtime/types/ticks.py#L137). The DBOS `TaskJournal` records task completion order for deterministic replay `[source]` — [`task_journal.py#L10`]($L/llama-agents-dbos/src/llama_agents/dbos/journal/task_journal.py#L10).

**D1–D4**
* D1: `Context.to_dict()` snapshots store, queues, buffers, active work and the running flag, and the application persists the dict `[source]` — [`context.py#L357`]($L/llama-index-workflows/src/workflows/context/context.py#L357). Server/DBOS packages persist ticks `[source]`.
* D2: `Context.from_dict(workflow, data)` → `workflow.run(ctx=...)` `[source]`.
* D3/D4: older in-progress work with no invocation id is put back on the queue `[source]` — [`internal_state.py#L551`]($L/llama-index-workflows/src/workflows/runtime/types/internal_state.py#L551). Steps are the unit of replay, so an interrupted step is re-run `[inferred]`. Human-in-the-loop `wait_for_event` "replays the entire step when the event arrives, so … make any preceding work safe to repeat" `[source]` — [`context.py#L536-L548`]($L/llama-index-workflows/src/workflows/context/context.py#L536). There is no effect ledger or idempotency key `[source]`.

**Handoffs/approval.** `InputRequiredEvent` / `HumanResponseEvent` `[source]`. Agent handoff lives in llama_index's `AgentWorkflow` (not inspected).

**Verdict: deeper study not justified.** The tick/reduce/command split is a clean sans-I/O pattern, but Rig's `AgentRun` is a closer, Rust-native fit. HITL depends on step replay, which conflicts with intent-before-execution.

---

## Rust-native runtimes

### goose (added; passes the screen)

**Record**
* Repository: https://github.com/aaif-goose/goose (`Cargo.toml` `repository`). `block/goose` serves the same HEAD (`git ls-remote`), consistent with a move or redirect.
* Commit: `9920efa16d078777a4d9715b2f00d8800a4da188` — https://github.com/aaif-goose/goose/tree/9920efa16d078777a4d9715b2f00d8800a4da188
* License: Apache-2.0. Workspace version 1.55.0.
* Last meaningful activity: 2026-10-20, "fix(desktop): show model changes when the provider reports no resolved model (#12914)".

Base: `G=https://github.com/aaif-goose/goose/blob/9920efa16d078777a4d9715b2f00d8800a4da188/crates`

**Inspected areas.** `goose-agent/src/{machine,operation}.rs`, `goose/src/agents/state_machine/*` (op list), `goose/src/session/session_manager.rs` (sqlx SQLite), `goose-provider-types/src/base.rs`, `goose-providers/src/`.

**Mechanism.** `StateMachine::run` loops: `load(session_id)` from the store → `step()` runs ordered `Operation`s (each `NotApplicable` or `Applied{effects}`; the first applied one wins) → `apply_effects` persists → repeat `[source]` — [`machine.rs#L211`]($G/goose-agent/src/machine.rs#L211), [`machine.rs#L31`]($G/goose-agent/src/machine.rs#L31). Operations attach "notes" to messages "so that a pipeline rebuilt from the persisted conversation reaches the same conclusion. Notes record past actions only" `[source]` — [`operation.rs#L89-L102`]($G/goose-agent/src/operation.rs#L89). Operations include tool approval, tool calling, compaction, tool-pair compaction, max-turns, retry and subagents `[source]` (`goose/src/agents/state_machine/`). On cancellation, unanswered tool requests are answered "Tool call was interrupted before completing" `[source]` — [`machine.rs#L69`]($G/goose-agent/src/machine.rs#L69).

**D1–D4.** D1: SQLite sessions `[source]`. D2: every step reloads from the store, so restart = run again `[source]`, but crash restart is not traced. D3/D4: the interrupted-answer path runs only on in-process cancellation `[source]`. After a crash, a persisted tool request without a response would likely be executed by the tool-calling op on the next pass `[inferred]`. No idempotency keys `[source]`.

**Provider neutrality.** `Provider` trait in `goose-provider-types` (`stream` required, `resume(session_id)` hook) `[source]` — [`base.rs#L532`]($G/goose-provider-types/src/base.rs#L532). Ollama, OpenAI-compatible and Anthropic providers, plus `local_inference` `[source]`. Tools are MCP (`rmcp`) `[source]`.

**Verdict: deeper study justified (small).** Only the `goose-agent` machine: "reload persisted state → pure ordered operations → effects → apply" and "operation notes for deterministic re-derivation" fit stack-agent's turn loop and FeltDB-as-truth. The rest is product code.

### swarms-rs — excluded

* https://github.com/The-Swarm-Corporation/swarms-rs @ `ee1e74cb6e2c94775db88dddb311f2235e3fe9e3`, Apache-2.0. Latest commit is a dependabot merge. Latest meaningful: 2026-10-13, "feat(providers): effort settings for Anthropic, OpenAI and OpenRouter".
* Excluded because: persistence is a plain `fs::write` of the agent's short-term memory per task at the end (`save_task_state`), with no run resume `[source]` — [`swarms_agent.rs#L1857`](https://github.com/The-Swarm-Corporation/swarms-rs/blob/ee1e74cb6e2c94775db88dddb311f2235e3fe9e3/swarms-rs/src/agent/swarms_agent.rs#L1857), [`persistence.rs#L18`](https://github.com/The-Swarm-Corporation/swarms-rs/blob/ee1e74cb6e2c94775db88dddb311f2235e3fe9e3/swarms-rs/src/structs/persistence.rs#L18). The loop is `max_loops` + `retry_attempts` `[source]`. Handoff (`run_handoff`: target gets the task, context and conversation; on error control stays) `[source]` is not unique. Providers: Anthropic, OpenAI, OpenRouter only `[source]`. D1 minimal, D2–D4 absent.

### AutoAgents — excluded

* https://github.com/liquidos-ai/AutoAgents @ `a85503b1e62b015f5bb774c1a5d90a775c0347bc`, dual MIT/Apache-2.0. Latest meaningful: 2026-10-17, "[BUG]: Cancel stream producers when their consumer is dropped". Real, active code (~340 .rs files).
* Excluded because: no durable run state (memory providers export messages; no checkpoint/resume) `[source]`. Its distinct pieces are ordinary: `AgentHooks::on_tool_call → HookOutcome` abort hook `[source]` — [`hooks.rs#L20`](https://github.com/liquidos-ai/AutoAgents/blob/a85503b1e62b015f5bb774c1a5d90a775c0347bc/crates/autoagents-core/src/agent/hooks.rs#L20), and an `LLMLayer` provider-middleware pipeline `[source]` — [`pipeline/mod.rs#L36`](https://github.com/liquidos-ai/AutoAgents/blob/a85503b1e62b015f5bb774c1a5d90a775c0347bc/crates/autoagents-llm/src/pipeline/mod.rs#L36). Both are covered better by Rig's `Intercept` layers. Actor runtime (ractor) and local backends (llama.cpp, mistral.rs, Ollama) noted.

### langchain-rust — excluded

* https://github.com/Abraxas-365/langchain-rust @ `cfc709a71fee02083ced078252d73c1f76fe9849`, MIT. Excluded because: last commit 2025-04-30 (~18 months before this screen), which fails the "actively maintained" bar.

---

## Mechanisms most worth porting (ranked)

1. **Tool-effect ledger with tool-supplied idempotency key** (Pydantic AI Harness): `started` before the body, `completed|failed` after, an optional `idempotency_key`/`effect_summary` annotated from inside the tool, and recovery as facts, never as permission. It validates FeltDB's design. The experiment shows the failure mode when the "unresolved blocks" rule is *not* enforced (W2 duplicate) `[experiment]`.
2. **Wire/Transport split with pure encode** (Rig), as the FX crate shape `[source]`.
3. **Serde sans-I/O run machine with idempotent re-issue** (Rig `AgentRun::next_step`) `[source]`.
4. **Effect dispositions / decider vocabulary** (Rig `EffectRecord`) for FeltDB outcomes `[source]`.
5. **Reconcile rule: stale RUNNING ⇒ ERROR, unmatched action ⇒ synthetic error parented to it, scan full log** and **owner lease with generation** (OpenHands agent server) `[source]`.
6. **Compaction as an append-only event** (OpenHands `Condensation`) `[source]`; **stuck-pattern guard** (OpenHands) `[source]`.
7. **Reload → pure ordered operations → effects → apply** with **operation notes for deterministic re-derivation** (goose) `[source]`.

Open experiments worth running if deeper study proceeds: OpenHands `LocalConversation` E2 W2 (suspected re-execution via implicit confirmation, `[inferred]`), and Rig `rig-ecs` restore with an `Interrupted::Started` charge.
