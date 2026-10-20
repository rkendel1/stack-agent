# Tier 2 screening: Microsoft Agent Framework, Mastra, CrewAI

Screening is source-level, following [protocol.md](../protocol.md). Two cheap E2-style `SIGKILL`
crash tests were run with no model (pure-function steps) and the shared ledger:

* MAF `FileCheckpointStorage` graph workflow: [experiments/maf/](../experiments/maf/).
* CrewAI Flow `@persist` (SQLite), `CheckpointConfig` (SQLite) and `@human_feedback` pending/resume:
  [experiments/crewai/](../experiments/crewai/).

Mastra was screened at source level only (Node).

Target reminder (used only for portability judgments): stack-agent records an intent before every
capability execution and an observation plus outcome after it (FeltDB task/fence/lease), and an
unresolved intent blocks resumption. So the bar for D3/D4 is "the runtime knows an effect *may*
have happened."

---

## 1. Microsoft Agent Framework (Python)

### Candidate record

* Repository: https://github.com/microsoft/agent-framework (canonical: Microsoft org, the
  `python/` and `dotnet/` trees ship `agent-framework-*` / `Microsoft.Agents.AI.*`).
* Commit: `809cc04f2d610bb48fa4274b6e7f0e96773d576d`,
  https://github.com/microsoft/agent-framework/tree/809cc04f2d610bb48fa4274b6e7f0e96773d576d
* License: MIT (`LICENSE`, "Copyright (c) Microsoft Corporation").
* Last meaningful activity: 2026-10-19, "`.NET: fix(dotnet): reset compiled workflow state between runs (#9540)`".
* Package used: `agent-framework-core==1.22.0` (matches `python/packages/core/pyproject.toml` at the commit).


### What was inspected

`core/agent_framework/_workflows/_checkpoint.py`, `_runner.py`, `_workflow.py`, `_functional.py`,
`_workflow_context.py`; `orchestrations/agent_framework_orchestrations/_handoff.py`, `_magentic.py`;
the `samples/03-workflows/checkpoint/` and `orchestrations/handoff_with_tool_approval_checkpoint_resume.py`
samples. I did not inspect the .NET workflow runtime or the Azure Functions / durabletask hosting
packages. Core's `all` extra references `agent-framework-durabletask` and `agent-framework-azurefunctions`
(`core/pyproject.toml` L46, L53), but neither package's source is in this repository at this commit.

### Execution model

* Graph workflows run Pregel-style supersteps. Each superstep delivers queued messages to executors.
  At the end of a superstep, pending state is committed and then a checkpoint is written
  ([`_runner.py#L123`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/core/agent_framework/_workflows/_runner.py#L123),
  [`#L166`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/core/agent_framework/_workflows/_runner.py#L166),
  [`#L173`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/core/agent_framework/_workflows/_runner.py#L173)) `[source]`. Before the first
  superstep, an entry checkpoint captures the seeded input
  ([`_workflow.py#L765`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/core/agent_framework/_workflows/_workflow.py#L765)) `[source]`.
* A checkpoint holds the queued inter-executor messages, the committed state (including
  `_executor_state` from `on_checkpoint_save`), the pending request-info events, the iteration
  count, a `previous_checkpoint_id` lineage and a graph-signature hash
  ([`_checkpoint.py#L35`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/core/agent_framework/_workflows/_checkpoint.py#L35)) `[source]`. A restore
  refuses a checkpoint whose graph signature differs from the workflow's
  ([`_runner.py#L323`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/core/agent_framework/_workflows/_runner.py#L323)) `[source]`.
* The functional API (`@workflow` / `@step`) memoizes each `@step` result and saves a checkpoint
  after every completed step (`_on_step_completed`,
  [`_functional.py#L2150`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/core/agent_framework/_workflows/_functional.py#L2150)) `[source]`. This
  is journal-style: results are recorded after completion, and nothing is recorded before execution.

### Durability

| | Verdict | Evidence |
| --- | --- | --- |
| D1 Save | **Yes** | `FileCheckpointStorage.save` writes JSON to a unique temp file, then `os.replace`s it into place ([`_checkpoint.py#L624`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/core/agent_framework/_workflows/_checkpoint.py#L624), [`#L652`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/core/agent_framework/_workflows/_checkpoint.py#L652), [`#L617`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/core/agent_framework/_workflows/_checkpoint.py#L617)) `[source]`. There is **no `fsync`** of the file or the directory anywhere in `_workflows/` `[source]`, so the write is atomic against a process crash but not guaranteed against power loss `[inferred]`. A Cosmos DB storage is also available (`azure-cosmos/.../_checkpoint_storage.py`) and was not inspected. |
| D2 Resume | **Yes** | A fresh process calls `storage.get_latest()` and then `workflow.run(checkpoint_id=…)`, adding `responses=` for pending requests. It resumed without manual reconstruction in every window `[experiment]`. `get_latest` picks the checkpoint with the maximum wall-clock `timestamp` ([`_checkpoint.py#L845`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/core/agent_framework/_workflows/_checkpoint.py#L845)), although the dataclass docstring says ordering is defined by the lineage chain `[source]`. That is clock-dependent `[inferred]`. |
| D3 Safe recovery | **Partial** | Recorded progress was never lost: W1 and W3 resumed at the right superstep with one `lookup` `[experiment]`. However, **a failed checkpoint write is swallowed**. `create_checkpoint_if_enabled` logs a warning and the run continues, with the note "this does not fail the workflow run" ([`_runner.py#L278`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/core/agent_framework/_workflows/_runner.py#L278)) `[source]`. So a run can proceed past effects whose checkpoint never landed. The granularity is the superstep: everything an executor did inside a superstep is redone on resume `[experiment]` (W2, W4). |
| D4 Effect safety | **No** | No intent is recorded before an executor runs, no idempotency key is offered, and no ambiguous outcome is surfaced. Kill windows W2 (after the effect, before the tool returned) and W4 (before the checkpoint that records the charge) both produced **2 charges** in the ledger, and the checkpoint's view (one charge result) disagreed with the ledger `[experiment]`. |

E2 results (`experiments/maf/results.json`): W1 correct, W2 duplicate_effect, W3 correct,
W4 duplicate_effect, W6 correct, W5 not run `[experiment]`.

### Handoff and delegation

* **Handoff** (`HandoffBuilder`): each agent receives synthetic `handoff_to_<target>` tools. A function
  middleware intercepts those calls and short-circuits them with a deterministic result
  `{"handoff_to": target}`, so the tool never executes
  ([`_handoff.py#L142`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/orchestrations/agent_framework_orchestrations/_handoff.py#L142),
  [`#L397`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/orchestrations/agent_framework_orchestrations/_handoff.py#L397)) `[source]`.
* After every agent run, the response is **broadcast to all participants**: agents are connected
  fan-out, and each one keeps its own copy of the full conversation
  ([`#L415`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/orchestrations/agent_framework_orchestrations/_handoff.py#L415),
  [`#L543`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/orchestrations/agent_framework_orchestrations/_handoff.py#L543)) `[source]`. The
  context passed across a handoff is therefore the full history.
* Without autonomous mode, a turn that does not hand off ends in a `request_info(HandoffAgentUserRequest)`,
  which suspends for user input
  ([`#L501`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/orchestrations/agent_framework_orchestrations/_handoff.py#L501)) `[source]`. Executor
  state such as `_autonomous_mode_turns` is checkpointed `[source]`.
* Sequential, concurrent, group-chat and Magentic orchestrations are builders over the same graph
  runtime ([`_magentic.py#L274`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/orchestrations/agent_framework_orchestrations/_magentic.py#L274)
  task ledger, [`#L492`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/orchestrations/agent_framework_orchestrations/_magentic.py#L492)
  stall/reset limits) `[source]`. Their durability is the superstep checkpoint above `[inferred]`.

### Approval and suspend

* Executors call `ctx.request_info(request_data, response_type)`
  ([`_workflow_context.py#L404`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/core/agent_framework/_workflows/_workflow_context.py#L404)); the
  answer arrives in a typed `@response_handler`. Pending requests are stored in the checkpoint, and
  the run ends `IDLE_WITH_PENDING_REQUESTS` `[source]`.
* Tool-level approval is `approval_mode="always_require"` on a tool
  ([`_tools.py#L755`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/core/agent_framework/_tools.py#L755)), which surfaces
  `function_approval_request` content `[source]`.
* A pending approval survived `SIGKILL`, and a new process approved it by `request_id`, producing
  one charge (W6) `[experiment]`.
* No claim or compare-and-set guards consuming a response. Two processes restoring the same
  checkpoint with the same response would each execute the approved effect `[inferred]`.

### Uniquely valuable

* **The superstep commit boundary** gives a clean, explainable unit: state changes are staged and
  then committed or discarded per superstep
  ([`_runner.py#L166-L170`](https://github.com/microsoft/agent-framework/blob/809cc04f2d610bb48fa4274b6e7f0e96773d576d/python/packages/core/agent_framework/_workflows/_runner.py#L166)) `[source]`.
* **Typed request/response ports**, where the request carries its original payload to the response
  handler, are a good shape for stack-agent approvals: the pending request lives in durable state,
  keyed by request id.
* **Graph-signature validation** on restore prevents resuming a checkpoint against a changed topology.
  Stack-agent could apply the same check to a "decision-contract version" check on resume `[inferred]`.

### Deeper investigation?

**Not for durability.** D4 is absent and was demonstrated absent. Stack-agent's intent-before-effect
contract is already strictly stronger. **Worth a short design read (no port)** of two pieces: the
request_info/response_handler port shape, and the commit/discard staging, as models for adding
approvals to Chip. Neither needs an experiment.

---

## 2. Mastra (TypeScript)

### Candidate record

* Repository: https://github.com/mastra-ai/mastra (canonical monorepo: `@mastra/core`, stores,
  `workflows/inngest`, `workflows/temporal`).
* Commit: `8ea807fc7cc79c9e05d72093098396a95face0cc`,
  https://github.com/mastra-ai/mastra/tree/8ea807fc7cc79c9e05d72093098396a95face0cc
* License: Apache-2.0 outside `ee/` directories; `ee/` has its own license (`LICENSE.md`).
* Last meaningful activity: 2026-10-19, "`fix(inngest): run call-time and defaultOptions scorers on the Inngest engine (#27719)`" (HEAD itself is a test move).
* Package versions at the commit: `@mastra/core` 1.79.0-alpha.5, `@mastra/inngest` 1.10.7-alpha.3,
  `@mastra/temporal` 0.4.17-alpha.5 (README: "Experimental … not ready for production"). Nothing
  was installed.


### What was inspected

`packages/core/src/workflows/{default.ts,workflow.ts,execution-engine.ts,run-ownership.ts,handlers/entry.ts,step.ts,types.ts}`,
`packages/core/src/storage/run-fencing.ts`, `stores/pg/src/storage/domains/workflows/index.ts`,
`packages/core/src/agent/durable/{run-recovery.md,leases.md}`, `packages/core/src/agent/{agent.ts,subagent.ts,tool-approval.ts}`,
and `workflows/inngest/src/execution-engine.ts`.

### Execution model

* Workflows are step graphs. The default engine persists a `WorkflowRunState` snapshot (status,
  step results, active and suspended paths, resume labels, state) through `persistStepUpdate` at
  step transitions, including a `waiting` snapshot before a step runs
  ([`handlers/entry.ts#L169`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/workflows/handlers/entry.ts#L169),
  [`#L633`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/workflows/handlers/entry.ts#L633)) `[source]`. The default predicate
  is `shouldPersistSnapshot ?? (() => true)`
  ([`workflow.ts#L1956`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/workflows/workflow.ts#L1956)) `[source]`.
* Every step and persist call goes through `engine.wrapDurableOperation(operationId, fn)`. This is
  a no-op on the default engine
  ([`default.ts#L174`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/workflows/default.ts#L174)). On Inngest it wraps
  `step.run` (memoized, durable;
  [`workflows/inngest/src/execution-engine.ts#L225`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/workflows/inngest/src/execution-engine.ts#L225))
  `[source]`. The workflow definition is therefore separate from the durability substrate.
* **Workflow state is separate from model interaction.** Agents run their loop as internal
  workflows, and durable agents (`agent/durable/`) persist that loop's snapshot `[source]`.

### Durability

| | Verdict | Evidence |
| --- | --- | --- |
| D1 Save | **Yes** (storage-adapter dependent) | `persistWorkflowSnapshot` upserts the snapshot row, e.g. pg `INSERT … ON CONFLICT DO UPDATE` ([`stores/pg/…/workflows/index.ts#L641`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/stores/pg/src/storage/domains/workflows/index.ts#L641)) `[source]`. More than 20 store adapters exist (`stores/`) `[source]`. |
| D2 Resume | **Yes** | Suspended runs resume through `run.resume({step, resumeData})` ([`workflow.ts#L4853`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/workflows/workflow.ts#L4853)). Crashed `running` runs restart through `restart()` and `restartAllActiveWorkflowRuns()` ([`#L4932`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/workflows/workflow.ts#L4932), [`#L3433`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/workflows/workflow.ts#L3433)) `[source]`. Durable agents get `recover()` / `recoverActiveRuns()` / boot-time `recoverAllDurableAgents()` `[claim]` ([`run-recovery.md`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/agent/durable/run-recovery.md)). Not exercised `[blocked]`: no Node experiment was run. |
| D3 Safe recovery | **Partial, and the most engineered of the three** | **Run ownership with leases and generation fencing**: `claimRunOwnership` atomically increments `generation` and sets the owner and lease on the store's clock ([pg `#L405-L440`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/stores/pg/src/storage/domains/workflows/index.ts#L405)), and fenced writes are checked atomically with the write and rejected with `RunFenceConflictError` ([`storage/run-fencing.ts#L1-L25`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/storage/run-fencing.ts#L1)) `[source]`. For plain workflows, "writes stay unfenced: the lease only tells the boot-time recovery sweep … that the run is not orphaned" ([`workflows/run-ownership.ts#L15`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/workflows/run-ownership.ts#L15)) `[source]`; durable-agent writes are fenced `[claim]` (leases.md L36–38). The suspended-snapshot guard keeps a later `running` write from overwriting `suspended` ([`entry.ts#L208`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/workflows/handlers/entry.ts#L208)) `[source]`. The redo re-runs everything after the last snapshot, and message ids come from the snapshot, so the redo upserts rather than appends ([run-recovery.md L50–54](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/agent/durable/run-recovery.md#L50)) `[claim]`. The docs list known gaps: the evented engine re-runs finished steps, and evented recovery fails at some crash points ([L108](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/agent/durable/run-recovery.md#L108)) `[claim]`. |
| D4 Effect safety | **No** (honestly documented) | "Neither recovery nor takeover can undo a side effect. A tool that was running when the process died runs again on the redo … Tools in durable agents should be idempotent. Declaring which tool calls are safe to replay is tracked in #25780" ([run-recovery.md#L98](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/agent/durable/run-recovery.md#L98)) `[claim]`. No pre-effect intent record was found in the step path `[source]`. On Inngest, `step.run` memoizes completed steps but still re-executes a step that crashed mid-flight `[inferred]` (Inngest semantics, not verified here). |

### Handoff and delegation

* **Subagents are tools**: each entry in `agents` becomes a tool `agent-<name>`
  ([`agent.ts#L5654`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/agent/agent.ts#L5654)) `[source]`. The supervisor keeps
  control; there is no transfer of control.
* Delegation hooks: `onDelegationStart` can modify or reject a delegation, which is a natural
  policy/authorization point ([`#L5795`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/agent/agent.ts#L5795),
  [`#L5915`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/agent/agent.ts#L5915)), and is followed by `messageFilter` (a context
  filter, [`#L6064`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/agent/agent.ts#L6064)) and `onDelegationComplete` `[source]`.
  Options also cover whether subagent tool results enter the supervisor's context, and
  deterministic `<agent>-<n>` result references reusable via `contextFromRefs`
  ([`delegation-refs.ts`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/agent/delegation-refs.ts)) `[source]`.
* `agent.network()` is a routing-agent loop over agents, workflows and tools
  ([`agent.ts#L9194`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/agent/agent.ts#L9194); `loop/network/`) `[source]`; its
  internals were not inspected further.

### Approval and suspend

* Steps call `suspend(payload)`, and the run persists `suspended` with `suspendedPaths` and
  `resumeLabels` ([`step.ts#L53`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/workflows/step.ts#L53)) `[source]`. Tool approval
  in agents works through `requireApproval` plus `approveToolCall` / `declineToolCall` with a
  persisted decline reason ([`agent.ts#L10965`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/agent/agent.ts#L10965),
  [`tool-approval.ts`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/agent/tool-approval.ts)) `[source]`.
* **The resume claim is a compare-and-set** `suspended → running` in storage
  (`updateWorkflowState({status:'running', expectedStatus:'suspended'})`), so of two concurrent
  `resume()` calls only one enters the engine
  ([`workflow.ts#L4988-L5044`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/workflows/workflow.ts#L4988)) `[source]`. The code
  warns explicitly when the store or predicate cannot de-duplicate: "Concurrent calls may execute
  downstream steps more than once" `[source]`. A crash between the claim and the resumed step is
  handled by handing a claim-only `running` snapshot back to `suspended`
  ([`#L3252`](https://github.com/mastra-ai/mastra/blob/8ea807fc7cc79c9e05d72093098396a95face0cc/packages/core/src/workflows/workflow.ts#L3252)) `[source]`.

### Uniquely valuable

1. **Generation-fenced run ownership**: claim with `expectedGeneration`, lease on the store clock,
   renew by heartbeat, write rejected on fence mismatch, and a memory fence raised to the claim's
   generation. This maps almost one-to-one onto FeltDB task/fence/lease and is the best external
   reference found so far for the takeover and recovery-sweep protocol and its error taxonomy
   (`run-recovery.md` refusal table).
2. **Approval consumption as a CAS state transition** with a crash-repair rule. Stack-agent will need
   exactly this once approvals exist: an approval must authorize one execution.
3. **The durability-substrate seam** (`wrapDurableOperation(operationId, fn)` with deterministic
   operation ids), where the same workflow runs on in-process, Inngest or Temporal engines.

### Deeper investigation?

**Yes, targeted.** Study (1) and (2) at source level against FeltDB's contract: the lease-expiry
timing, `force` takeover, the unfenced-workflow gap, and the evented-engine gaps the docs admit. If
disk allows, run one Node E2 test of the CAS resume claim and fenced takeover using the libsql
store. Mastra does not solve D4, so it informs fencing and approvals, not effect safety.

---

## 3. CrewAI

### Candidate record

* Repository: https://github.com/crewAIInc/crewAI (canonical; monorepo `lib/crewai`, `lib/crewai-tools`, …).
* Commit: `c6e500ab3730ccf3badb640c6e0170e5c6aa3882`,
  https://github.com/crewAIInc/crewAI/tree/c6e500ab3730ccf3badb640c6e0170e5c6aa3882
* License: MIT (`LICENSE`, "Copyright (c) 2025 crewAI, Inc.").
* Last meaningful activity: 2026-10-19, "`feat(rag): add read-only notebook text loader (#8188)`".
* Package used: `crewai==1.15.28` (`__version__` at the commit).


### What was inspected

`flow/runtime/__init__.py` (the Flow engine; `flow/flow.py` is now a re-export shim),
`flow/persistence/{sqlite.py,decorators.py}`, `state/{checkpoint_config.py,checkpoint_listener.py,provider/*.py}`,
`crew.py`, `tools/agent_tools/base_agent_tools.py`, and `flow/async_feedback/`. Experiment:
[experiments/crewai/](../experiments/crewai/) (`results.json`).

### Two persistence mechanisms

1. **`@persist` (Flow state persistence)**. After each persisted method completes, the method is
   added to `_completed_methods` and the state is written
   (`await asyncio.to_thread(self._persist_method_completion, …)`,
   [`flow/runtime/__init__.py#L3030-L3032`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/flow/runtime/__init__.py#L3030)) `[source]`.
   `SQLiteFlowPersistence` appends one row `(flow_uuid, method_name, timestamp, state_json)` per
   save in WAL mode ([`sqlite.py#L110`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/flow/persistence/sqlite.py#L110),
   [`#L147`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/flow/persistence/sqlite.py#L147)). `load_state` returns only the latest row's
   `state_json` ([`#L217`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/flow/persistence/sqlite.py#L217)) `[source]`.
2. **`CheckpointConfig` (event-driven runtime checkpoints)** for Crew, Flow and Agent. A checkpoint
   is written when configured events fire (default `task_completed`; flow events, tool events and
   LLM events are also available), using a JSON-file or SQLite provider, and is resumed with
   `kickoff(from_checkpoint=…)` / `Flow.from_checkpoint`
   ([`state/checkpoint_config.py#L160`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/state/checkpoint_config.py#L160)) `[source]`.

### Durability

| | Verdict | Evidence |
| --- | --- | --- |
| D1 Save | **Yes** | `@persist` writes to SQLite in WAL mode with a commit per save `[source]`; the default `synchronous` is not overridden `[source]`, so commits are durable under SQLite's default `[inferred]`. The `CheckpointConfig` JSON provider uses a plain `open(path,"w"); f.write(data)`, with no temp file, rename or fsync ([`state/provider/json_provider.py#L64-L68`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/state/provider/json_provider.py#L64)) `[source]`, so a torn checkpoint file is possible on a crash mid-write `[inferred]`. |
| D2 Resume | **@persist: state reload only. CheckpointConfig: yes, when a checkpoint landed.** | With `@persist`, `kickoff(inputs={"id": uuid})` reloads the latest state. A fresh process, however, has an empty `_completed_methods`, so "the flow executes from scratch" from `@start`, as the code comment says ([`flow/runtime/__init__.py#L2309-L2331`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/flow/runtime/__init__.py#L2309), [`#L2378`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/flow/runtime/__init__.py#L2378)) `[source]`. In every `@persist` window the resumed process re-ran all five methods, with 3 model calls and the lookup redone `[experiment]`. Skipping completed work is left to user code that inspects the state. `CheckpointConfig` restores `checkpoint_completed_methods` and replays recorded events ([`#L741-L760`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/flow/runtime/__init__.py#L741)); a Crew restarts at the first task without output and marks mid-task agent executors `_resuming` with their saved messages ([`crew.py#L480-L515`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/crew.py#L480), [`#L1563`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/crew.py#L1563)) `[source]`. In 5 of 9 `ckpt` kills, **no checkpoint row existed yet** although 2–4 methods had finished, so there was nothing to resume from `[experiment]`. |
| D3 Safe recovery | **Weak** | `@persist` W1 lost progress: the completed lookup was redone `[experiment]`. Checkpoint writes run from a sync event handler, which the event bus dispatches on a `ThreadPoolExecutor` ([`state/checkpoint_listener.py#L247-L253`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/state/checkpoint_listener.py#L247)) `[source]`, so a checkpoint is not ordered before the next step. This was confirmed experimentally: a clean run writes 5 rows, while kills after 3–4 finished methods left 0–1 rows `[experiment]`. A failed auto-checkpoint is logged and swallowed ([`#L229-L244`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/state/checkpoint_listener.py#L229)) `[source]`. The JSON provider is not atomic (above). By contrast, a `@persist` failure raises `RuntimeError` ([`flow/persistence/decorators.py#L132`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/flow/persistence/decorators.py#L132)) `[source]`. |
| D4 Effect safety | **No** | No intent record, no idempotency key, no ambiguous-outcome state in either mechanism `[source]`. `@persist` W2, W3 and W4 each produced **2 charges**. W3 duplicated even though `do_charge`'s state row was committed, because the resume re-runs from `@start`. `ckpt` W2/W3 duplicated in 3 of 6 runs; the other 3 had no checkpoint to resume `[experiment]`. |

### Handoff and delegation

* `Process.sequential` runs tasks in order. `Process.hierarchical` requires a `manager_llm` or
  `manager_agent`, which executes every task and delegates
  ([`crew.py#L726`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/crew.py#L726), [`#L497`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/crew.py#L497)) `[source]`.
* Delegation tools (`Delegate work to coworker`, `Ask question to coworker`) match a coworker by
  sanitized role string. Each call constructs a **new `Task(description=task)`** and runs
  `selected_agent.execute_task(task, context)`, where `context` is a free-text string the
  delegating LLM writes. Errors are returned as tool-result text
  ([`tools/agent_tools/base_agent_tools.py#L46-L125`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/tools/agent_tools/base_agent_tools.py#L46))
  `[source]`. Context across the boundary is therefore an LLM-authored summary, not history.
  Delegation is synchronous and nested, not a transfer of control, and is not individually
  checkpointed `[inferred]`.

### Approval and suspend

* `@human_feedback` methods on Flows can raise `HumanFeedbackPending`
  ([`flow/async_feedback/types.py#L163`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/flow/async_feedback/types.py#L163)). The pending context
  and state go to the `pending_feedback` table (UNIQUE per flow, `INSERT OR REPLACE`;
  [`sqlite.py#L131`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/flow/persistence/sqlite.py#L131)). A new process calls
  `Flow.from_pending(flow_id)` and then `resume(feedback)`
  ([`flow/runtime/__init__.py#L1257`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/flow/runtime/__init__.py#L1257),
  [`#L1305-L1325`](https://github.com/crewAIInc/crewAI/blob/c6e500ab3730ccf3badb640c6e0170e5c6aa3882/lib/crewai/src/crewai/flow/runtime/__init__.py#L1305)) `[source]`. Providers (console, or custom such
  as Slack) are pluggable `[source]`.
* A pending approval survived `SIGKILL`, and `from_pending(...).resume("approved")` finished with one
  charge (W6) `[experiment]`.
* There is no claim when consuming feedback: two `resume()` calls would both proceed `[inferred]`.

### Memory

There is a unified memory with a LanceDB backend (a hard dependency, `lancedb`) `[source]`; it was
not inspected further. It is not relevant to durability.

### Uniquely valuable

Little that stack-agent lacks. The one transferable idea is the **event-selectable checkpoint
trigger** (`on_events=["tool_usage_finished", …]`): durability granularity becomes configuration
rather than code. Stack-agent's per-capability intent/outcome records already sit at the finest
useful granularity. `from_pending`/`resume` is a straightforward suspended-for-human pattern and
weaker than Mastra's CAS claim.

### Deeper investigation?

**No.** Both persistence paths lack D4, which was demonstrated. `@persist` lacks real D2: it
restores state and re-runs from `@start`, which was also demonstrated. The checkpoint path writes
asynchronously, lags behind execution, can fail silently, and its default JSON provider is
non-atomic.

---

## Cross-candidate summary

| Candidate | D1 | D2 | D3 | D4 | Primitive worth deeper study |
| --- | --- | --- | --- | --- | --- |
| MAF | Yes `[source]` (no fsync) | Yes `[experiment]` | Partial `[experiment]`/`[source]` (superstep redo; checkpoint failure swallowed) | No `[experiment]` (W2, W4 duplicate) | request_info/response_handler ports; superstep commit/discard (design read only) |
| Mastra | Yes `[source]` | Yes `[source]` (not run) | Partial, best engineered `[source]`/`[claim]` | No `[claim]` (docs: tools run again) | **Generation-fenced run ownership + CAS resume claim** |
| CrewAI | Yes `[source]` (JSON provider non-atomic) | @persist: state only, reruns from `@start` `[experiment]`; CheckpointConfig: only if a row landed `[experiment]` | Weak `[experiment]` (async checkpoint lag, lost progress) | No `[experiment]` (2 charges) | none |
