# LangGraph

## Candidate record

| | |
| --- | --- |
| Repository | https://github.com/langchain-ai/langgraph. This is the canonical repo: the `langgraph` PyPI package and the LangChain docs point to it, and it is the `langchain-ai` org's monorepo (`libs/langgraph`, `libs/checkpoint*`, `libs/prebuilt`, `libs/sdk-*`, `libs/cli`). |
| Inspected commit | `1400d25b7b88cfa055c55b8a82bd95b7c25caef1`, at https://github.com/langchain-ai/langgraph/tree/1400d25b7b88cfa055c55b8a82bd95b7c25caef1 |
| License | MIT, per `LICENSE` at that commit ("Copyright (c) 2024 LangChain, Inc.") |
| Last meaningful maintenance | `b0cabd9`, 2026-10-16, "fix(langgraph): let other tasks finish when a resume value is invalid (#9265)". The HEAD commit (2026-10-19) is a CLI release. |
| Packages used | `langgraph==1.2.16`, `langgraph-checkpoint==4.3.0`, `langgraph-checkpoint-sqlite==3.1.2`, `langgraph-prebuilt==1.1.1`, `langchain-core==1.6.12`, all from PyPI. These versions equal the `pyproject.toml` versions at the inspected commit. |
| Experiments | [`../experiments/langgraph/`](../experiments/langgraph/) (README, harnesses, `results.json`, `raw/`) |

All source links below are immutable links at the inspected commit.

## Execution model

* **Pregel / BSP supersteps** `[source]`. In the main loop, `while loop.tick(): runner.tick(...); loop.after_tick()` ([B/libs/langgraph/langgraph/pregel/main.py#L3054](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/main.py#L3054)). `tick()` plans the next tasks from channel versions with `prepare_next_tasks` ([_loop.py#L650](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L650)). The runner executes those tasks concurrently, and `after_tick()` applies all their writes at the barrier and then saves a checkpoint ([_loop.py#L737-L803](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L737)). Writes from step N become visible only in step N+1 (comment at main.py#L3052).
* **Typed state and reducers** `[source]`. State keys are channels. A plain key is a `LastValue`, and two writes to it in one superstep raise `InvalidUpdateError` ([channels/last_value.py#L56](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/channels/last_value.py#L56)). `Annotated[T, reducer]` makes the key a `BinaryOperatorAggregate` ([graph/state.py#L1917](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/graph/state.py#L1917)). For example, `MessagesState.messages` uses `add_messages`, which merges by message id ([graph/message.py#L64](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/graph/message.py#L64), [#L310](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/graph/message.py#L310)). `Overwrite` bypasses a reducer ([types.py#L1049](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/types.py#L1050)). Within a step, writes are applied in sorted task-path order, so the merge is deterministic ([_algo.py#L254](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_algo.py#L254)).
* **Concurrency** `[source]`. All tasks of a superstep run in parallel: a thread pool in sync mode ([_executor.py#L40](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_executor.py#L40)), asyncio tasks in async mode. `ToolNode` additionally runs one model turn's tool calls in parallel with `executor.map` ([prebuilt/tool_node.py#L829](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/prebuilt/langgraph/prebuilt/tool_node.py#L829)). `Send(node, arg)` fans out dynamically ([types.py#L738](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/types.py#L738)).
* **Control flow** `[source]`. There are static edges, conditional edges, and `Command(goto=..., update=..., resume=..., graph=Command.PARENT)` ([types.py#L833](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/types.py#L833)). The functional API offers `@entrypoint` and `@task` ([func/__init__.py#L132](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/func/__init__.py#L132)). Each task returns a future, and its result is saved as that task's write.
* **Task identity** `[source]`. A task id is a hash of (checkpoint id, namespace, step, node name, trigger path) ([_algo.py#L549](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_algo.py#L549), [#L615](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_algo.py#L615)). It is exposed to nodes and tools as `runtime.execution_info.task_id` ([runtime.py#L27](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/runtime.py#L27)). `[experiment]`: a task re-executed after a crash, in a different process, gets the **same** task_id (E2 W2/W4b ledger lines, E3 W5).

**Contrast with chip-core** `[inferred]`. A LangGraph superstep is roughly a chip turn, and a node is roughly a policy step or a capability execution. LangGraph has no closed decision contract: control flow is whatever the node code returns.

## Checkpoint boundaries and pending writes

* **When a checkpoint is written** `[source]`:
  * The input is checkpointed first (`source: "input"`, [_loop.py#L1179](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L1179)).
  * One checkpoint is written per superstep, after **all** tasks of the step finish and their writes are applied (`source: "loop"`, [_loop.py#L803](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L803)).
  * A time-travel run forks (`source: "fork"`, [_loop.py#L1102](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L1102)).
  * Nothing is checkpointed *before* a node or a tool call runs. A tool call inside a node is not a boundary at all: the whole node is the unit.
* **Pending writes** `[source]`. When a task finishes, errors, or interrupts, `PregelRunner.commit` calls `put_writes(task_id, writes)` ([_runner.py#L575-L615](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_runner.py#L575)). The loop hands those writes to `checkpointer.put_writes`, keyed to the current checkpoint id ([_loop.py#L515-L540](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L515)). On resume, a task that has an output write in that log is treated as finished, and its writes are re-applied instead of re-running it ([_loop.py#L827](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L827), rules in [_task_status.py#L1](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_task_status.py#L1)). This is what saves the finished siblings of a failed task in a parallel step.
* **Durability modes** `[source]`. `Durability = Literal["sync", "async", "exit"]`, default `"async"` ([types.py#L98](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/types.py#L98), [main.py#L2696](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/main.py#L2696)).
  * `sync` waits for the step's checkpoint future before the next step ([main.py#L3078](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/main.py#L3078)).
  * `async` saves while the next step runs.
  * `exit` skips `put_writes` and intermediate checkpoints entirely and saves once, on loop exit ([_loop.py#L515](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L515), [#L1283](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L1283), [#L1494-L1512](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L1494)).
* **Ordering gap in `sync`** `[source]` + `[experiment]`. `put_writes` is **submitted to the background executor** and is not awaited ([_loop.py#L524-L540](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L524)). The checkpoint save waits only for the previous checkpoint and delta-channel write futures, not for ordinary task writes ([_loop.py#L1706-L1722](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L1706)). So even in `sync` mode, a finished task's result is durable only once *either* its pending write or the next checkpoint commits, and the two race for SqliteSaver's lock. E2 W4 shows the consequence: 2 of 5 runs re-executed `charge`.

## Durability D1–D4

| | Verdict | Evidence |
| --- | --- | --- |
| D1 Save | **Yes**, with a durable checkpointer. `InMemorySaver` is explicitly for "debugging or testing" ([checkpoint/memory/__init__.py#L41](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/checkpoint/langgraph/checkpoint/memory/__init__.py#L42)). `SqliteSaver` uses WAL and commits per call ([checkpoint-sqlite/.../__init__.py#L146](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/checkpoint-sqlite/langgraph/checkpoint/sqlite/__init__.py#L146), [#L188-L207](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/checkpoint-sqlite/langgraph/checkpoint/sqlite/__init__.py#L188)). Postgres is similar ([checkpoint-postgres/.../__init__.py#L268](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/checkpoint-postgres/langgraph/checkpoint/postgres/__init__.py#L268)). | `[source]` `[experiment]` |
| D2 Resume | **Yes**. `graph.invoke(None, {"configurable": {"thread_id": t}})` in a fresh process continues from the latest checkpoint plus its pending writes, with no reconstruction. This worked in every `sync`/`async` window. With `durability="exit"` there is nothing to resume after SIGKILL: `EmptyInputError`, all progress lost. | `[experiment]` E2 |
| D3 Safe recovery | **Mostly**. Recorded progress was never lost or corrupted in any window: no `progress_lost` in 30 sync + 30 async runs, and the SQLite transactions are atomic. Two caveats. (a) The granularity is a whole node, so the unrecorded in-flight node always re-runs. (b) The ordering gap above can push the boundary back a step, so a completed tool's result can be dropped. That is not "lost recorded progress": the result was never durably recorded, but it widens the effect window. There is **no owner/lease/fence** on a thread: two resumers both ran, and the state then disagreed with the ledger (1 ToolMessage vs 2 charges). | `[source]` `[experiment]` |
| D4 Effect safety | **No**. There is no intent record before an effect, no idempotency key is forwarded, and an ambiguous outcome is never surfaced. A node killed after its effect is silently re-executed (W2, W4b: duplicate charge, 5/5 runs). Guidance is to make side effects idempotent or isolate them in `@task`s (`[claim]`, interrupt and task docstrings). That narrows the re-execution unit but does not change the crash-after-effect window. What it does offer: a stable `task_id` plus model `tool_call_id`, which an app can use as an idempotency key (`[experiment]`: identical across the duplicate executions). | `[source]` `[experiment]` |

**Where guarantees come from** `[source]` `[inferred]`. The runtime owns *what* is saved and when: checkpoint per superstep, pending-write reapply, task ids, interrupt/resume bookkeeping. The checkpointer owns *atomicity and survival*. SqliteSaver: WAL, one transaction per `put`/`put_writes`, `INSERT OR REPLACE` for checkpoints and `INSERT OR IGNORE` for ordinary writes, so re-sent writes do not overwrite ([#L439](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/checkpoint-sqlite/langgraph/checkpoint/sqlite/__init__.py#L439), [#L476-L478](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/checkpoint-sqlite/langgraph/checkpoint/sqlite/__init__.py#L476)). InMemorySaver: none across processes. Postgres: the same upsert/insert split ([#L352](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/checkpoint-postgres/langgraph/checkpoint/postgres/__init__.py#L352)). No saver in this repo implements a lease or lock on a thread: a grep for lease/lock/fence in pregel and the savers finds none `[source]`. Run serialization ("multitask strategies") is a feature of LangGraph Platform, not the OSS runtime `[claim]`.

## Crash-window results (E2)

Store: SqliteSaver file, `durability="sync"`. Each run is a real SIGKILL followed by a fresh-process `invoke(None)`, with 5 repetitions per window. Ground truth is the ledger. W5/W6 come from the E3 graph (1 run each).

| Window | Kill point | Outcome (5 reps) | Charges | Notes |
| --- | --- | --- | --- | --- |
| W1 | `charge` entry, before ledger write (decision checkpointed) | correct ×5 | 1 | The tools node re-runs once. |
| W2 | inside `charge`, after ledger fsync | **duplicate_effect ×5** | 2 | The node never committed, so it re-ran. Same task_id and tool_call_id on both lines. |
| W3 | right after the charge result committed as a pending write | correct ×5 | 1 | Pending write (or step checkpoint) reapplied; node not re-run. |
| W3b | start of model call 3 (after step checkpoint) | correct ×5 | 1 | Only the agent node re-runs. |
| W4 | inside `SqliteSaver.put` of the checkpoint that carries the charge result (uncommitted) | correct ×3, **duplicate_effect ×2** | 1/2 | The kill point is deterministic; the outcome is not: `put_writes` vs `put` ordering race. |
| W4b | inside `put_writes` of the charge result (uncommitted) | **duplicate_effect ×5** | 2 | Result lost, so the node re-ran. |
| W5 | after triage's `Command(goto)` handoff write committed | correct | 1 | Specialist re-runs from its start with the same task_id. |
| W6 | suspended in `interrupt()` awaiting approval | correct | 1 | Approval in a fresh process works. A repeated `Command(resume)` after completion is a no-op. |

No window was `stuck` or `crashed_on_resume` in `sync` mode, and no manual intervention was needed beyond the human's approval in W6.

Supplementary runs (`results.json → e2.supplementary`) `[experiment]`:
* **`async`**: W3b duplicated 4/5 and W3 1/5. The step checkpoint and the pending writes were both still in flight when the next step began.
* **`exit`**: every killed window gave `EmptyInputError` on resume. Nothing was saved, so this is effectively total `lost_progress`, and a re-run repeats every effect.
* **Concurrent resume**: two processes resumed one thread after a W1 crash. Both charged (same task_id). The final state shows one charge.

## Handoffs and approval (E3)

The E3 graph is `triage --Command(goto="specialist")--> specialist --(charge proposed)--> approval[interrupt()] --> ToolNode --> specialist`. The approval node is kept apart from the model call on purpose. `interrupt()` re-executes its node from the top on resume ("The graph resumes from the start of the node, **re-executing** all logic", [types.py#L916](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/types.py#L918) `[claim]`; mechanism in [types.py#L1019-L1046](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/types.py#L1019): resume values are matched to `interrupt()` calls by their per-task index `[source]`). If the model call sat in the same node as `interrupt()`, it would be re-issued on resume. `[experiment]`: the approval node was entered twice, once in each process, with the same task_id.

* **Context at each boundary** `[experiment]`. The full shared state goes across: one `messages` list (add_messages reducer). The specialist saw 3 messages: the human message, triage's tool call, and the transfer ToolMessage. Filtering, summaries, or private per-agent state need app code, such as subgraphs with their own schema.
* **State ownership** `[source]`. The run's Pregel loop owns state. Nodes only return writes, and the checkpointer persists them per `thread_id/checkpoint_ns/checkpoint_id`. "Active agent" is not a runtime concept: E3 uses an app field.
* **Execution ids**: `thread_id`, chosen by the caller; `checkpoint_id`, one per step; `task_id`, deterministic; interrupt `id`, an xxh3 of the namespace; and the model's `tool_call_id`.
* **Specialist failure** `[experiment]`. Without a retry policy, `invoke` raised. The error was persisted as a pending ERROR write (`get_state().tasks[].error`), `next=["specialist"]`, and `invoke(None)` in a fresh process re-ran it and finished (1 charge). With `RetryPolicy(max_attempts=2)` the retry ran in-process with the same task_id and `node_attempt=2` ([types.py#L427](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/types.py#L427)). The retry counter is not persisted across processes `[inferred]`, from `ExecutionInfo` being per-run.
* **Approval survives restart**: yes (W6).
* **Supervisor/swarm**: these patterns live in the separate `langgraph-supervisor` / `langgraph-swarm` packages, built from handoff tools that return `Command(goto=..., graph=Command.PARENT)` `[claim]`. They were not inspected. The OSS runtime primitives are `Command.goto` and `Command.PARENT` ([types.py#L882](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/types.py#L882)), plus `map_command` ([_io.py#L56](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_io.py#L56)).

## Provider and local-model notes

* **Model-agnostic** `[source]` `[experiment]`. The runtime never calls a model. Nodes are arbitrary callables, and the harness's fake is a plain `BaseChatModel` subclass. Tool calling depends on langchain-core's `bind_tools`/`AIMessage.tool_calls` and on the prebuilt `ToolNode`.
* **Invalid arguments** `[experiment]`. A call that omits `amount` is rejected by pydantic validation *before* the tool body runs. The error goes back to the model as a `ToolMessage(status="error")` ([tool_node.py#L391](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/prebuilt/langgraph/prebuilt/tool_node.py#L391)).
* Small or local models get no special support. The model can call any bound tool; there is no closed decision contract.
* E4 is `[blocked]` (no local inference endpoint).

## Memory

* Short-term memory is the per-thread checkpointed state. Every checkpoint is kept, which enables `get_state_history`, `update_state` (fork), and replay from a `checkpoint_id` ([main.py#L1514](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/main.py#L1514), [#L2617](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/main.py#L2617); replay re-executes finished tasks and forks, [_loop.py#L974](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L974), [#L1102](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L1102)) `[source]`.
* Long-term memory is a separate `BaseStore` (key-value, optional vector search) passed to nodes through the runtime `[source]` (`runtime.store`). It was not exercised.

## Tools and extensibility

* `ToolNode` supports `wrap_tool_call` interceptors for retries, caching, or request modification, plus injected `ToolRuntime` (state, store, config, execution_info, tool_call_id) ([tool_node.py#L630](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/prebuilt/langgraph/prebuilt/tool_node.py#L630), [#L1697](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/prebuilt/langgraph/prebuilt/tool_node.py#L1697)) `[source]`.
* Tools can return a `Command` to update state or route.
* **Retries**: `RetryPolicy` per node or task, with backoff and jitter, `retry_on` predicate ([types.py#L427](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/types.py#L427)).
* **Timeouts**: `TimeoutPolicy`, async only. Sync tasks "cannot be safely cancelled in-process" ([func/__init__.py#L160](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/func/__init__.py#L169)).
* **Caching**: `CachePolicy(key_func, ttl)` with an in-memory, sqlite, or redis `BaseCache`. A cache entry is written only *after* a task finished without an error or interrupt ([_loop.py#L164](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L164), [#L1783-L1801](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L1783)), so it does not protect the crash-after-effect window `[source]`.
* **Cancellation**: a cooperative drain through `RunControl.request_drain()`, which is checked between supersteps ([runtime.py#L79](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/runtime.py#L79), [_loop.py#L723](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L723)). An async task cancelled mid-step gets an ERROR write ([_runner.py#L580](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_runner.py#L580)) `[source]`.

## Observability

* Stream modes are `values`, `updates`, `checkpoints`, `tasks`, `debug`, `messages` (token stream), and `custom` (a node's `stream_writer`) ([types.py#L131](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/types.py#L131)) `[source]`.
* Every checkpoint carries metadata (`source`, `step`, `parents`, `run_id`).
* `get_state()` exposes `next`, `tasks` (with `error`, `interrupts`, `result`), and `interrupts`. The E2/E3 harnesses used these to inspect a crashed thread before resuming `[experiment]`.
* LangSmith tracing goes through langchain callbacks `[claim]`.

## Overhead (E5)

Setup: same machine (4 CPUs, Python 3.13.16), E1 task, 1 warm-up + N=50, ms per run, with one ledger fsync per tool call in every variant `[experiment]`.

| Store | p10 | median | p90 |
| --- | --- | --- | --- |
| no checkpointer | 8.4 | 9.1 | 10.6 |
| InMemorySaver | 13.0 | 16.0 | 20.4 |
| SqliteSaver, `sync` | 18.7 | 25.3 | 31.1 |
| SqliteSaver, `async` | 17.5 | 20.9 | 25.8 |

Cold import of `langgraph.graph` + `prebuilt` + `checkpoint.sqlite` + `langchain_core.messages` takes a median of 858 ms (5 runs). The orchestration cost of durability is about 16 ms over no persistence for 5 supersteps, about 3 ms per step.

## Primitives worth porting

1. **Per-task pending writes keyed by (checkpoint, deterministic task id), reapplied on resume.**
   * **What it is**: when a unit of work finishes, its result is recorded against the current step *before* the step's checkpoint exists. On resume, finished units are not re-run; only unfinished ones are ([_runner.py#L575](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_runner.py#L575), [_loop.py#L461](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L461), [#L827](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L827), [_task_status.py](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_task_status.py), [_algo.py#L615](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_algo.py#L615)).
   * **What it guarantees**: no completed sibling is re-executed when another one fails or crashes (W3).
   * **Rust boundary**: FeltDB already records intent → observation → outcome per capability, which is stronger. The piece to adopt is the **deterministic execution id**: `ExecutionId = hash(task_id, turn_no, intent_seq, capability)`. It is derived, not random, so a re-planned turn after restart maps to the same intent row. It is also forwarded to executors as an idempotency key, which stack-agent lacks today. The ledger showed LangGraph's id is stable across a crash yet never forwarded. Port the stability, and also forward it.
2. **Interrupt/resume as a persisted, addressable suspension.**
   * **What it is**: `interrupt(value)` persists an INTERRUPT write with a stable id. `Command(resume=v | {interrupt_id: v})` supplies the answer from any process, and resume values are matched by per-task index ([types.py#L903-L1046](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/types.py#L903), [_loop.py#L1013-L1050](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/_loop.py#L1013)).
   * **What it guarantees**: approval survives restart (W6). A late or duplicate resume after completion is a no-op.
   * **Rust boundary**: add a `Decision::AwaitApproval { request_id, capability, args }` terminal-for-now outcome. It is stored in FeltDB as an intent in state `AwaitingApproval`, keyed by a deterministic `request_id`. `resume(task_id, request_id, verdict)` records an `ApprovalObservation` that cites the request, and only then may the executor run. The intent-before-effect rule stays intact.
   * **Lesson to keep**: do *not* copy "re-execute the node from the top". Approval must be its own step, so the model is not re-asked.
3. **Explicit durability mode per run (`sync` / `async` / `exit`) and fresh-process inspection of a crashed run** (`get_state().next/tasks/interrupts`) ([types.py#L98](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/types.py#L98), [main.py#L3078](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/main.py#L3078), [main.py#L1464](https://github.com/langchain-ai/langgraph/blob/1400d25b7b88cfa055c55b8a82bd95b7c25caef1/libs/langgraph/langgraph/pregel/main.py#L1464)).
   * **What it gives**: an honest, named knob for the cost/safety trade-off, plus a read-only "what will resume do" view.
   * **Rust boundary**: stack-agent is effectively `sync`-plus. Expose a `chip inspect <task>` view that lists the pending intents, which ones are OutcomeUncertain, and the next step, reading FeltDB without taking the lease.
   * **Ordering note**: LangGraph's `sync` does *not* order task writes before the checkpoint (W4). FeltDB's observation→outcome ordering already avoids that gap; keep it as a written invariant and a test.

Also of interest:
* `RetryPolicy` as declarative per-node data (only for executors that declare themselves idempotent).
* Time-travel forks from any checkpoint (useful for debugging and evaluation, not for the core loop).

## Primitives NOT worth porting

* **Whole-node re-execution as the recovery unit, with silent re-run of effects (W2/W4b)** `[experiment]`. stack-agent's OutcomeUncertain is strictly safer. Keep it.
* **`async` and `exit` durability as defaults** `[experiment]`. They duplicate effects or lose everything under SIGKILL, so they are incompatible with the intent-before-effect contract.
* **Thread without owner/lease** `[experiment]`. Two resumers double-charged. FeltDB's owner/fence/lease is the right design.
* **Open graph / arbitrary-callable control flow and shared mutable message list as the handoff medium.** These conflict with chip's closed decision contract and validation step. A handoff in stack-agent, if ever added, should be an explicit `Decision` with a declared context projection, not "goto + full history".
* **Result caching (`CachePolicy`)** for consequential capabilities. It is written after completion only, so it gives no crash protection, and it risks replaying stale observations.
* The thread-pool background executor for checkpoint I/O. It is the source of the W4 ordering race.

## Open questions

* Whether Postgres saver plus `sync` shows the same W4 race. The code path is identical (`put_writes` submitted, not awaited), so this is `[inferred]` yes. Not run here.
* What LangGraph Platform adds on top of OSS (run queue, double-texting strategies, thread locking) `[claim]`, not inspected.
* Whether `@task`-level granularity in the functional API changes W2-style windows for multi-effect nodes. `[inferred]`: it narrows them to one effect per task, but the crash-after-effect window remains.
* Exact behaviour of `Command(resume=...)` racing with a second resumer on the same interrupt (no fence). Not tested.
