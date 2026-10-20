# Agno

## Candidate record

| | |
| --- | --- |
| Repository | https://github.com/agno-agi/agno. This is the canonical repository: it is the `Repository` URL in `libs/agno/pyproject.toml` (line 515) and the `origin` of the clone. |
| Inspected commit | `643f6e797312bb00a80a7bc2e445449b8106970d`, https://github.com/agno-agi/agno/tree/643f6e797312bb00a80a7bc2e445449b8106970d |
| License | Apache License 2.0 (`LICENSE` at that commit) |
| Last meaningful activity | 2026-10-19: `fix: retrieve focused passages from offloaded tool results (#11171)` (940065b), followed by `chore: Release v3.1.6 (#11170)`. The HEAD commit is a test-only change from the same day. |
| Package used | `agno==3.1.6` (PyPI; it matches `version = "3.1.6"` in the source tree), `SQLAlchemy==2.1.5`, `greenlet==3.5.6` (needed by `agno.db.sqlite`), `pydantic==2.14.1`, Python 3.13.16 |
| Harness | `experiments/agno/agno_harness.py`. Commands are in `experiments/agno/README.md`; results are in `experiments/agno/results.json`. |

All source links below use the prefix `https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/` (written `S/` below).

## Execution model

* **Run pipeline.** `Agent.run` dispatches to `_run`
  ([S/agent/_run.py#L367](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/agent/_run.py#L367)),
  a 16-step pipeline: read or create session, load session state, pre-hooks, resolve tools, build
  messages, start background memory/learning futures, then one call to `Model.response`, which
  *contains the whole tool loop*. After that come post-hooks, the summary and `cleanup_and_store`. `[source]`
* **The tool loop lives in the model abstraction, not the agent.** `Model.response`
  ([S/models/base.py#L651](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/models/base.py#L651))
  is a `while True` loop: invoke the provider, append the assistant message, parse tool calls, run all
  calls in the batch (`run_function_calls`), append the results, fire the optional `after_tool_results`
  callback, then break on `stop_after_tool_call`, confirmation, external execution, user input or an
  unresolved member requirement, or loop again. There is no closed decision contract: any assistant
  message without tool calls ends the run. `[source]`
* **Tool-call limit.** `tool_call_limit` is checked per call inside `run_function_calls`. Calls over the
  limit are not executed. Each one gets a synthetic tool message saying "Tool call limit reached … Don't try
  to execute it again", and the loop continues to the next model call
  ([S/models/base.py#L2222](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/models/base.py#L2222)). `[source]`
* **Tool-call parsing and validation.** `get_function_call`
  ([S/utils/functions.py#L10](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/utils/functions.py#L10)):
  * an unknown tool name produces the tool message "Error: The requested tool does not exist or is not available";
  * arguments are parsed with `json.loads`, falling back to `ast.literal_eval`;
  * non-object arguments are an error returned to the model;
  * string values `"none"`, `"null"`, `"true"` and `"false"` are silently coerced to `None`, `True` and `False`.

  Schema validation happens only at call time, through pydantic `validate_call` wrapping the entrypoint
  ([S/tools/function.py#L1739](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/tools/function.py#L1739)).
  The `ValidationError` is caught by `FunctionCall._execute`
  ([S/tools/function.py#L2604](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/tools/function.py#L2604))
  and returned to the model as a tool error. `[source]`
  * `[experiment]` E1 omitted the required `amount`. The pydantic error went back to the model with
    `tool_call_error=True` and nothing was written to the ledger. The model's corrected call charged
    once, over 4 model calls. The error text includes a repr of the injected `run_context` argument.
  * `[inferred]` Pre-hooks and tool hooks run before argument validation, so a hook used as an
    authorization gate sees unvalidated arguments.
* **Retries.**
  * `Model.retries` retries a provider call on retryable `ModelProviderError`s (429/5xx), with optional
    exponential backoff. `retry_with_guidance` re-invokes with an extra temporary user message
    ([S/models/base.py#L226](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/models/base.py#L226)). `[source]`
  * `Agent.retries` wraps the **entire run** in an attempt loop
    ([S/agent/_run.py#L426](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/agent/_run.py#L426),
    [#L727](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/agent/_run.py#L727)).
    On an exception it sleeps and re-runs from step 1 with the same `run_id`. `[source]`
  * `[inferred]` An exception raised after tools ran (for example a provider error on the final model
    call) re-executes those tools.
  * `FallbackConfig` switches models on error, rate limit or context overflow
    ([S/models/fallback.py#L203](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/models/fallback.py#L203)). `[source]`
* **Cancellation.** Cooperative only. `raise_if_cancelled(run_id)` is checked between pipeline steps and
  between stream events, through an in-memory or Redis cancellation manager
  (`S/run/cancellation_management/`). Team cancellation cascades to registered member run ids. A cancelled
  run is persisted with status CANCELLED, and `continue_run` refuses it (`RunNotContinuableError`). `[source]`
* **Concurrency.**
  * Tool calls in one batch run sequentially in sync mode. `arun_function_calls` runs them concurrently
    in async mode.
  * Memory and learning extraction run in a background thread pool *concurrently* with the main model
    call (`agent.background_executor.submit`, `S/agent/_managers.py#L213`).
  * Broadcast teams run members concurrently under `arun`. `[source]`
  * `[inferred]` A direct `continue_run` has no lease or fence, so two processes can resume the same
    RUNNING run. Fencing exists only in the AgentOS queue worker (see Durability).
* **Why instantiation is fast.**
  * `Agent.__init__` only assigns attributes ([S/agent/agent.py](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/agent/agent.py)).
  * Model defaulting, id, telemetry, checkpoint level, memory, summary and compression managers are all
    resolved in `initialize_agent`
    ([S/agent/_init.py#L476](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/agent/_init.py#L476)),
    which is called per run.
  * Tools are stored as given. `Function` derivation (signature, type hints, docstring parse, JSON schema,
    `validate_call` wrap) is deferred to `get_tools` at run time and memoized in identity-keyed bounded
    caches ([S/tools/function.py#L385](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/tools/function.py#L385)).
  * DB tables are created lazily on first use (`_get_or_create_table`).
  * Most cross-module imports are function-local.
  * The project's own benchmark docstring says schema extraction "is deferred to the first run". `[source]`
  * The deferred work is paid on the first run: see Overhead. `[experiment]`

## Durability (D1–D4)

The storage model: one row per session in `agno_sessions` (session_data/session_state, metadata, summary)
and, in v3, one row per run in `agno_runs` (the serialized `RunOutput`: messages, tools/`ToolExecution`s,
requirements, status, metrics). `persist_run_in_session` writes the session row, then the run row, through
`save_session` + `save_run`
([S/agent/_run.py#L6012](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/agent/_run.py#L6012)).
SQLite runs in WAL mode with `synchronous` at the default FULL
([S/db/sqlite/sqlite.py#L246-L260](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/db/sqlite/sqlite.py#L246)). `[source]`

When the run is written depends on `Agent.checkpoint`
([S/agent/agent.py#L153-L159](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/agent/agent.py#L153)):

* `"runs"` (the default) writes only at terminal or paused states (`cleanup_and_store`, `handle_agent_run_paused`).
* `"tool-batch"` additionally writes a RUNNING snapshot after each tool batch, through the
  `after_tool_results` callback (`checkpoint_run`,
  [S/agent/_run.py#L6354](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/agent/_run.py#L6354)).
* `"tools"` (per-tool) is reserved and raises. `[source]`

A failed checkpoint is logged and the run continues
([S/models/base.py#L845-L849](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/models/base.py#L845)). `[source]`
Nothing is written *before* a tool executes. `[source]`

| | Verdict | Evidence |
| --- | --- | --- |
| D1 Save | **Yes.** It covers sessions, runs (full messages and tool executions), paused requirements and approvals, on SQLite, Postgres, Mongo, Redis and others. With the default `"runs"`, nothing is saved until the run ends or pauses. | `[source]` + `[experiment]` (E2 `runs` W1–W4: no run row after the kill) |
| D2 Resume | **Yes, for paused runs and for `tool-batch` RUNNING snapshots.** `continue_run(run_id, session_id)` resumes from the stored messages with no manual reconstruction ("unified continue": RUNNING and ERROR runs resume in place, and COMPLETED runs auto-fork). With the default `"runs"`, a crashed run cannot be resumed. A team that crashed before its first leader batch has no row. | `[source]` [S/agent/_run.py#L3285](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/agent/_run.py#L3285); `[experiment]` E2 tool-batch W1, W3, W6 correct; E3 W5 stuck |
| D3 Safe recovery | **Partial.** Recorded progress is never lost or corrupted at tool-batch granularity: the resume always restarted from the last committed snapshot. Work after that snapshot is redone by re-asking the model. The session row and the run row are separate commits (W4-commit2 killed after the session-row commit and before the run-row commit; the run row stayed at the lookup checkpoint). | `[experiment]` E2 W4-commit1/2 |
| D4 Effect safety | **No.** No intent is recorded before execution, no idempotency key is forwarded, and there is no ambiguous-outcome state. The resumed model issues a new tool call with a new `tool_call_id`. Approvals are not persisted before the approved effect runs (W6b re-asked for approval and charged twice). | `[experiment]` E2 W2, W4, W6b: 2 charges |

The AgentOS durable job queue (`S/os/job_queue.py`, `QueueWorker`, line 617) is a separate layer for
`background=True` runs on Postgres or Redis. It provides SKIP LOCKED claims, heartbeated leases,
`attempt`-fenced status writes (`RunPersistOutcome.STALE_ATTEMPT` / `TERMINAL_REFUSED`,
[S/run/status_persist.py](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/run/status_persist.py)),
submission dedup by `idempotency_key` per user, and a DLQ/sweep. `[source]`
A reclaimed non-continuation job **re-executes the run** ("a re-execution is a fresh stream",
[S/os/job_queue.py#L1309](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/os/job_queue.py#L1309)), so it fences *state writes* but not *tool effects*. `[source]`
The queue was not exercised here. `[blocked]` It needs AgentOS plus Postgres or Redis, which is out of scope for this harness.

## Crash-window table (E2, SQLite, real SIGKILL, fresh-process `continue_run`)

Primary variant: `checkpoint="tool-batch"`. Every kill was confirmed as `returncode == -9`.

| Window | Resumed | Charges | State consistent with ledger | Progress lost | Manual intervention | Outcome |
| --- | --- | --- | --- | --- | --- | --- |
| W1 before charge | yes | 1 | yes | no | none | `correct` |
| W2 inside charge (after fsync) | yes | **2** | no (run records 1) | no | none | `duplicate_effect` |
| W3 after charge recorded | yes | 1 | yes | no | none | `correct` |
| W4 state write, 1st commit (session row) | yes | **2** | no | no | none | `duplicate_effect` |
| W4 state write, 2nd commit (run row) | yes | **2** | no | no | none | `duplicate_effect` |
| W5 during handoff (Team, E3) | no (no team row) | 0 | n/a | n/a | resubmit task | `stuck` |
| W6 suspended awaiting approval | yes (re-approve in new process) | 1 | yes | no | approve | `correct` |
| W6b inside approved charge | yes | **2** | no | no | approve again | `duplicate_effect` |

Default variant `checkpoint="runs"`:

* W1–W4 → `stuck`. `get_run_output` returns None and resubmission re-runs lookup and charge, so the
  ledger shows 2 charges for W2–W4.
* W6 → `correct`.
* W6b → `duplicate_effect`.

All `[experiment]`; raw data is in `experiments/agno/logs/e2_raw.json`.

## Handoffs / teams and approval (E3)

* **Modes.** `TeamMode` is `coordinate` (default supervisor), `route` (leader picks one member, whose
  response is returned directly), `broadcast` (same task to every member) and `tasks` (shared task list,
  loops until done)
  ([S/team/mode.py#L6](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/team/mode.py#L6)). `[source]`
* **Delegation is a tool call.** `delegate_task_to_member(member_id, task)` calls `member.run(...)`
  synchronously inside the leader's tool batch, using the same `session_id`, a fresh `run_id` and a
  *copy* of session_state, which is merged back afterwards
  ([S/team/_default_tools.py#L694](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/team/_default_tools.py#L694)). `[source]`
* **Context at the boundary.**
  * The member receives the leader-written `task` string as a fresh user message. The leader history is
    not passed unless `add_team_history_to_members` is set, and member interactions only through an option.
  * The member's result returns to the leader as the tool result string. `[source]` `[experiment]`: the
    Specialist saw `['user']` only.
* **Identifiers and ownership.**
  * The team run (`run_id=t-…`) owns state.
  * The member run is stored in the same session with `parent_run_id` set to the team run, and the
    leader's tool record gets `child_run_id`. `[source]` + `[experiment]`
* **Approval.**
  * `@tool(requires_confirmation=True)` (or `@approval`, which also creates an approvals-table record)
    pauses the loop. The run is persisted as PAUSED with `requirements`.
  * A member's requirements are copied onto the team run with `member_agent_id` and `member_run_id`
    (`_propagate_member_pause`). `[source]`
  * `[experiment]` The child process was SIGKILLed while the team was PAUSED. A new process loaded the
    team run from SQLite, confirmed the requirement and called `Team.continue_run`. The member run was
    found by `member_run_id` (session lookup, `S/team/_run.py#L6237`), and the Specialist resumed with its
    full own history (user, assistant, tool, assistant, tool). There was exactly **1 charge**. So approval
    survives a restart. W6b shows it is still not effect-safe.
  * The "already ran" guard is `confirmed and result is None`
    ([S/agent/_tools.py#L1083](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/agent/_tools.py#L1083)),
    and `_calls_that_already_ran` requires a stored tool message
    ([S/run/approval.py#L736](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/run/approval.py#L736)).
    Both depend on a result write that happens *after* the effect. `[source]`
* **Specialist failure.** `[experiment]` When the Specialist's model raised, the member run was stored as
  ERROR and its error text became the delegate tool result. The team run finished **COMPLETED**, with the
  error string as its content. The failure is not surfaced as a team failure.
* **W5.** `[experiment]` A kill inside the member before the leader's first batch finished left no team row. Nothing could be resumed.

## Workflows

* Workflow v2 steps cover Step, Steps, Parallel, Loop, Condition and Router, with a shared `session_state`.
* `_execute` persists the workflow run only on pause (`save_paused_session`) and at the terminal state
  ([S/workflow/workflow.py#L2932](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/workflow/workflow.py#L2932)). `[source]`
* `continue_run` resumes from `paused_step_index`, with per-step-type rules: re-execute, or `+1`. Loops
  resume from an iteration
  ([S/workflow/workflow.py#L6827-L6961](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/workflow/workflow.py#L6827)). `[source]`
* `[inferred]` A crash between steps (not paused) leaves no resumable workflow row. Not exercised. `[blocked: scope]`

## Provider / local-model notes

* There are more than 50 provider classes. `OpenAILike` is a thin subclass of `OpenAIChat` with
  overridable `base_url`. `LMStudio` is `OpenAILike` at `http://127.0.0.1:1234/v1` with
  `supports_json_schema_outputs=True`. vLLM, llama.cpp and Ollama (native chat API) are also present. `[source]`
* Ollama tool calls are parsed without an `id`
  ([S/models/ollama/chat.py#L376-L388](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/models/ollama/chat.py#L376)),
  so `FunctionCall.call_id` is None on that path. `[source]`
  * `[inferred]` Any idempotency key derived from the provider's call id is unavailable for Ollama, and
    it changes on re-ask for every provider anyway (E2 W2 ledger: different `call_id`s for the two charges).
* The custom-model boundary is clean: subclass `Model` and implement `invoke`/`ainvoke` (plus the stream
  variants) and the two parse methods. The harness's `ScriptedModel` is about 40 lines. `[experiment]`
* E4 `[blocked]`: no local inference endpoint.

## Memory

* User memories come from `MemoryManager.create_user_memories`
  ([S/memory/manager.py#L377](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/memory/manager.py#L377)).
  It is an LLM extraction that runs in a background thread concurrently with the run, or agentically as
  a tool. Memories are stored in the DB's memories table, keyed by user. `[source]`
* Session summaries come from `SessionSummaryManager.create_session_summary`
  ([S/session/summary.py#L242](https://github.com/agno-agi/agno/blob/643f6e797312bb00a80a7bc2e445449b8106970d/libs/agno/agno/session/summary.py#L242)),
  run after the response and before storage. `[source]`
* History injection is controlled by `add_history_to_context`, `num_history_runs` and
  `max_tool_calls_from_history`. Compaction and compression managers handle overflow. `[source]`
* All of these call the model and are not part of the effect path. Nothing here was exercised.

## Tools and extensibility

* Plain callables, `Toolkit`s, `@tool(...)` flags (`requires_confirmation`, `external_execution`,
  `requires_user_input`, `stop_after_tool_call`, `cache_results`, `tool_hooks`) and pre/post hooks.
* Framework objects are injected by parameter name: `agent`, `team`, `run_context`, `fc`.
* `cache_results` is an on-disk cache keyed by arguments. It is written after a successful result, so
  `[inferred]` it does not protect the W2 window.
* Guardrails exist as pre and post hooks. MCP tools are present. `[source]`

## Observability

* Typed run events (`RunOutputEvent` stream: tool_call_started/completed/paused, and so on).
* `RunOutput.metrics` with per-message timers.
* OpenTelemetry tracing through `openinference-instrumentation-agno`, with a `DatabaseSpanExporter`
  storing spans in the Agno DB (`S/tracing/`).
* Messages carry `checkpoint_status` markers for timelines. `[source]`
* Telemetry to `os-api.agno.com` is on unless `telemetry=False` or `AGNO_TELEMETRY=false`
  (`S/api/settings.py#L57`). The harness disabled it. `[source]`

## Overhead (E5)

### Measured on this machine `[experiment]`

4 vCPU VM, Python 3.13.16, agno 3.1.6. The task is E1 with the fake model and fake tools, including the
two ledger fsyncs per run; 1 warm-up, then N=50.

| Measurement | p10 | median | p90 |
| --- | --- | --- | --- |
| E1 run, no db | 2.14 ms | **2.61 ms** | 3.52 ms |
| E1 run, SqliteDb, `checkpoint="runs"` | 7.45 ms | **9.05 ms** | 12.52 ms |
| E1 run, SqliteDb, `checkpoint="tool-batch"` | 13.23 ms | **16.99 ms** | 22.57 ms |
| First run of a freshly built agent (no db) | 2.05 ms | 2.24 ms | 3.92 ms |
| `Agent(name, model, tools=[lookup, charge])` construction (N=1000) | 13.4 µs | **13.6 µs** | 15.0 µs |
| Bare `Agent(telemetry=False)` construction (N=1000) | 10.7 µs | 10.8 µs | 11.5 µs |
| Construction tracemalloc peak | 5.42 KiB | 5.42 KiB | 5.42 KiB |
| Cold `import agno.agent` (fresh interpreter, 10×) | 444 ms | **490 ms** | 513 ms |
| Cold `import agno.agent, agno.team, agno.db.sqlite` | 720 ms | 815 ms | 866 ms |

The first E1 run in a cold process took 37–44 ms, which includes deferred tool derivation and lazy imports.

### Project claims `[claim]` (not reproduced; not comparable)

`cookbook/performance/README.md` reports results on an Apple M4 Max, Python 3.12:

* agent construction with 1 tool: 4.7 µs;
* construction memory peak: 7.1 KiB;
* cold import: 147 ms;
* tool-call run with a mocked model: 327 µs;
* 25-turn durable SQLite conversation: 42.2 ms.

How these relate to this harness:

* Different hardware and a different task, so the numbers are not compared.
* The construction claim is consistent in *shape* with the source: construction is attribute assignment
  and schema work is deferred. The deferred cost is visible here as the cold first-run figure.
* The project's own README acknowledges that the tool-call run is where the deferred cost is paid.

## Primitives worth porting

1. **Tool-batch checkpoint with a post-gather barrier** (`after_tool_results` → `checkpoint_run`).
   * Guarantee: a resumable RUNNING snapshot of messages and tool results after every batch, with
     `last_checkpoint_at_message_index`. It proved to be the only thing that made W1/W3 resume correct. `[experiment]`
   * Rust: chip-core already records the observation and outcome after each capability. Port the idea
     of a *message-level resume cursor*. `FeltDB` stores `ObservationRecord { seq, capability_id,
     result_digest, message_index }`, and resume rebuilds the model context from it.
   * Fix Agno's gaps: the checkpoint must be fatal on failure (Agno logs and continues), and the session
     and run writes must be one transaction.
2. **Unified continue over persisted runs** (`continue_run(run_id)` for PAUSED, RUNNING or ERROR in place;
   COMPLETED auto-forks to a sibling run id with `forked_from_run_id`).
   * Guarantee: "1 run = 1 model loop" lineage. `[source]`
   * Rust: `fn resume(task_id) -> Result<Resume, Blocked>` reads the fence/lease and refuses on an
     unresolved intent, which stack-agent already has.
   * Add `fork(task_id, at_seq) -> TaskId` for time-travel and regenerate without mutating the original record.
3. **Requirement records for HITL** (`RunRequirement { tool_execution, confirmation, member_agent_id,
   member_run_id }`, propagated from member to parent).
   * Guarantee: a paused run is self-describing and survives a restart. `[experiment]` E3, W6.
   * Rust: `Decision::Escalate`/`Block` could persist a `PendingRequirement { intent_id, capability,
     args_digest, owner_task, child_task: Option<TaskId> }`.
   * Unlike Agno, **record the approval itself (an `ApprovalGranted` event) before executing**. Agno's W6b
     duplicate is the counter-example.
4. **Attempt-fenced state writes with a typed outcome** (`RunPersistOutcome::{Updated, Missing,
   StaleAttempt, TerminalRefused, Unavailable}`, with "finished-work-wins").
   * Guarantee: a zombie worker cannot clobber a newer attempt or a terminal row. `[source]`
   * Rust: maps directly onto the FeltDB fence/lease. Return an enum, never a bool, from
     `persist_outcome(fence, attempt)`, and treat `StaleAttempt` and `TerminalRefused` as final.
5. **Per-user submission idempotency key** on enqueue (`{accepted: false, reason: "duplicate"}`).
   Guarantee: dedup of *task submission*, not of effects. `[source]` Rust: `submit(task, idempotency_key, principal)`.
6. **Validation errors fed back as tool results** (unknown tool, non-JSON or non-object arguments,
   schema-invalid arguments). `[experiment]`
   * In chip-core this maps to a rejected `request_capability` → an observation of kind
     `InvalidRequest { reason }` that the model sees on the next turn.
   * Validate *before* hooks and authorization. Agno validates after them.

## Not worth porting

* **Whole-run retry** (`Agent.retries` re-runs the pipeline with the same `run_id`). `[source]`
  `[inferred]` It re-executes effects. This conflicts with stack-agent's no-retry, intent-first contract.
* **The tool loop inside the model abstraction** (`Model.response` owns execution). Chip separates
  decision, validation, authorization and execution, which is the stronger boundary.
* **Silent string coercion** of `"null"`, `"true"` and `"false"` in arguments, and the `ast.literal_eval` fallback.
* **Non-fatal checkpoint failures** and separate session-row and run-row commits (W4).
* **Delegation as a synchronous nested run inside a tool call**, with team status COMPLETED on member failure.
* **Approval state that is only in memory until the post-effect write** (W6b).
* **The `cache_results` file cache** as a substitute for idempotency.
* **Default-on telemetry.**

## Open questions

* Does the AgentOS queue worker on Postgres resume a reclaimed job from its last tool-batch checkpoint, or
  always re-execute? The source at `os/job_queue.py:1309` says re-execute for non-continuation jobs. This is
  unverified by experiment, which needs Postgres and AgentOS. `[blocked]`
* Is `checkpoint="tools"` (per-tool, "3.0 runs-table split") planned to record intent *before* execution,
  or only results? Currently it raises. `[source]`
* What happens with concurrent `continue_run` on the same RUNNING run outside the queue? There is no fence
  in that path. `[inferred]`, not tested.
* Team `tool-batch` W2–W4 inside a member was not run. `[inferred]` It behaves like the agent case, with the
  member run's own checkpoint level applying.
