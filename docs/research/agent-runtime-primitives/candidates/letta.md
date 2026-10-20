# Letta

## Candidate record

| | |
| --- | --- |
| Repository named in the brief | https://github.com/letta-ai/letta. Its `main` branch is now a landing page. `main` at [`8816d3a8`](https://github.com/letta-ai/letta/tree/8816d3a8516893bfdc0fa14bfb1235226012e295) points to `letta-ai/letta-code` as "the current source", and `AGENTS.md` marks the old server retired [source] |
| Canonical current repository | https://github.com/letta-ai/letta-code (TypeScript: agent harness, TUI, App Server, local backend) |
| Inspected commit, current | [`e8073d4e`](https://github.com/letta-ai/letta-code/tree/e8073d4e475ab7f56e76f9aaeea81a7618a92575), `package.json` version 0.34.18 |
| Inspected commit, historical V1 server | `letta-ai/letta` branch `archive` at [`56ba9c25`](https://github.com/letta-ai/letta/tree/56ba9c25552605eec89de8ed3dc6394b625c1993), `pyproject` 0.16.8. Inspected read-only as a design reference. It is retired upstream and was not run |
| License | Apache-2.0 for both (the `LICENSE` file at each commit) [source] |
| Last meaningful activity | letta-code, 2026-10-19: "chore(skills): remove curating-memory-palace builtin skill (#5174)". The archive branch was last touched 2026-08-13 (issue-template chore). Its last runtime work predates that |
| Package used in experiments | `letta==0.34.12` from PyPI. This is now the Letta Code wheel, with Node bundled. It was run with `--backend local` |

Link bases: `LC = https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575`,
`LA = https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993`.

**Scope note.** The brief describes the V1 server: Postgres/SQLite ORM, `core_memory_*` tools,
heartbeats, archival passages. That code is now history. Current Letta keeps memory as
git-tracked Markdown files ("MemFS"), runs tools on the client, and ships a local file-backed
backend. This dossier covers both:

* **V1** is where the memory hierarchy and step loop were designed most explicitly.
* **letta-code** is what actually ships today. Every experiment ran against it.

## Execution model

### letta-code (current)

* **Turns are client-driven.** The backend streams the model output. Every model tool call
  becomes an `approval_request_message` and the turn ends with `stop_reason: requires_approval`.
  The client then runs a permission check, executes the tool locally, and sends the results back
  as an approval message, which starts the next model call. See
  [`LC/src/backend/dev/provider-turn-executor.ts#L496-L525`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/backend/dev/provider-turn-executor.ts#L496-L525) [source].
  So "approval" is the general tool-execution handoff, not only a human-in-the-loop feature.
* **Permissions** are an ordered rule cascade: deny rules, CLI disallow, always-ask, mode, CLI
  allow, workspace reads, session/settings allow, ask, then the tool default
  ([`LC/src/permissions/checker.ts#L100-L112`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/permissions/checker.ts#L100-L112)) [source].
  The default mode is **`unrestricted`**
  ([`LC/src/permissions/mode.ts#L10`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/permissions/mode.ts#L10)) [source].
* **Execution order.** Read-only tools and Task tools run in parallel. Write tools are grouped by
  target resource and run sequentially within a group
  ([`LC/src/agent/approval-execution.ts`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/agent/approval-execution.ts), around L326–L482) [source].
* **There are three orchestration paths**: TUI, headless `-p`, and the websocket listener
  (App Server / remote computers). The project's own `AGENTS.md` says they are separate [claim].
  This matters for durability, covered below.
* **Malformed tool calls** become tool results, not crashes. Example:
  `validateRequiredParams` produces "Bash tool missing required parameter: command…"
  ([`LC/src/tools/impl/validation.ts#L1-L13`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/tools/impl/validation.ts#L1-L13)) [source].
  The E1 run with a missing argument reproduced this, and the loop recovered [experiment].
  No retries are counted.

### V1 server (historical)

* **Step loop.** `LettaAgentV3.step` loops up to `max_steps`
  ([`LA/letta/agents/letta_agent_v3.py#L328-L395`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/agents/letta_agent_v3.py#L328-L395)) [source].
  V3 dropped heartbeats: "loops happen on tool calls" (L98–L108). V2 still pops a
  `request_heartbeat` argument from every tool call
  ([`LA/letta/agents/letta_agent_v2.py#L1126-L1139`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/agents/letta_agent_v2.py#L1126-L1139)) [source].
* **Continuation is decided by host-side tool rules** (`_decide_continuation`,
  [`L1968-L2030`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/agents/letta_agent_v3.py#L1968-L2030)).
  The rule types are `run_first`, `exit_loop`, `continue_loop`, `conditional`,
  `constrain_child_tools`, `max_count_per_step`, `parent_last_tool`, `required_before_exit` and
  `requires_approval`
  ([`LA/letta/schemas/enums.py#L182-L198`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/schemas/enums.py#L182-L198)) [source].
  * `required_before_exit` refuses to end the loop and injects "You must call X at least once to
    exit". This is the nearest analogue to Chip's "completion refused unless required
    observations exist", but it is keyed on *tool called*, not on *observation recorded*.
  * A rule violation is returned to the model as a tool result, and the tool is not executed
    (L1810–L1830).
* **Approval**: a tool that `requires_approval`, or that is a client-side tool, stops the step with
  `requires_approval` and persists an approval-request message
  ([`L1686-L1709`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/agents/letta_agent_v3.py#L1686-L1709)) [source].
* **Sandboxes**: local, E2B and Modal
  ([`LA/letta/services/tool_sandbox/`](https://github.com/letta-ai/letta/tree/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/services/tool_sandbox)) [source].

## Memory hierarchy and authority (the core of this dossier)

### V1: the three tiers

| Tier | Storage | Who writes it | In context? |
| --- | --- | --- | --- |
| **Core memory blocks** | `block` table: `label`, `value`, `limit` (default `CORE_MEMORY_BLOCK_CHAR_LIMIT` = 100000), `read_only`, `version` used as the SQLAlchemy optimistic-lock column, history pointer ([`LA/letta/orm/block.py#L39-L61`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/orm/block.py#L39-L61)). Blocks can be shared by many agents through `blocks_agents` [source] | **The model**, via `core_memory_append/replace`, `memory_replace/insert/rethink` ([`LA/letta/functions/function_sets/base.py#L246-L520`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/functions/function_sets/base.py#L246-L520)), plus users through the API | Always; rendered into the system prompt along with chars-used/limit |
| **Recall memory** | `messages` table. `agent.message_ids` holds the in-context subset ([`LA/letta/orm/agent.py#L71`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/orm/agent.py#L71)) | Host: every model output and tool return | Partly. Evicted messages stay searchable with `conversation_search` |
| **Archival memory** | `passages` with a pgvector or SQLite `CommonVector` embedding ([`LA/letta/orm/passage.py#L21-L40`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/orm/passage.py#L21-L40)) | **The model**, via `archival_memory_insert`; also documents a user loads | Only through search |

How V1 validates memory edits [source,
[`LA/letta/services/tool_executor/core_tool_executor.py#L319-L371`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/services/tool_executor/core_tool_executor.py#L319-L371),
[`#L743-L771`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/services/tool_executor/core_tool_executor.py#L743-L771)]:

* **Checked:** whether the block is read-only, exact-match uniqueness for `replace`, and rejection
  of line-number prefixes pasted back from the display.
* **Not checked, so far as I found:** the **character limit is not enforced on these write
  paths**. The bulk-update docstring promises "ValueError if any new value exceeds its block's
  limit", but the body only assigns the value
  ([`LA/letta/services/block_manager.py#L800-L835`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/services/block_manager.py#L800-L835)).
  The `Block` validator only strips NUL bytes ([`LA/letta/schemas/block.py#L51-L64`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/schemas/block.py#L51-L64)).
  At this commit the limit is shown to the model but not enforced [source; a negative finding
  over the paths named here].
* **Possible bug:** `memory_rethink` reads `get_block(label).read_only` *before* its `KeyError`
  "create block" branch, so that branch looks unreachable for missing labels [source, L743–L767;
  not executed].
* **Write semantics:** memory writes are last-writer-wins at block granularity
  (`update_memory_if_changed_async` writes the blocks whose value changed). The ORM version column
  catches concurrent ORM flushes, but no compare-against-expected-value step exists at the tool
  level [source, `agent_manager.py` ~L1769-L1790].

### letta-code: MemFS

* **Core memory is now a git repository of Markdown files**, kept per agent at
  `~/.letta/lc-local-backend/memfs/<agent>/memory/` (`MEMORY.md`, `persona.md`, …). A local run
  created it [experiment]. Root files go into the system prompt. Deeper files load on demand
  through the read-only `Memory` tool
  ([`LC/src/tools/impl/memory.ts#L1-L8`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/tools/impl/memory.ts#L1-L8)) [source].
* **The agent edits memory with its ordinary file tools.** Validation happens at **git commit
  time** in a pre-commit hook [source,
  [`LC/src/agent/memory-git-hooks.ts#L25-L37`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/agent/memory-git-hooks.ts#L25-L37),
  [`LC/src/memory-constraints.ts#L6-L19`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/memory-constraints.ts#L6-L19)]:
  * frontmatter is required, and `description` is the only key the agent may edit;
  * `read_only` is protected, so a file whose HEAD version is `read_only: true` cannot be changed;
  * `.memfs.config.json` can cap depth, per-file and total core characters, and mark read-only
    globs, checked against the accepted (HEAD) tree.
* **Limits:** a commit-time hook constrains *committed* history. It does not stop the working
  tree, which the prompt may read, from holding uncommitted edits [inferred].
* **Isolation between agents:** unattended memory subagents run under a fail-closed kernel
  sandbox. They can write their own memory and cannot read other agents' memory
  ([`LC/src/memory-confinement.ts#L13-L21`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/memory-confinement.ts#L13-L21)) [source].
* **Recall** is the JSONL transcript, searched with `/search` or `letta messages search`.
  "Sleeptime" / "dreaming" runs a background reflection that rewrites memory [claim, README].

### Authority analysis for stack-agent

Classification of every Letta memory tier:

* **Model-authored:** core blocks or MemFS files, archival passages, compaction summaries and
  reflection output.
* **Host-observed:** only the raw message/transcript log (model outputs and tool returns) and the
  ledger of executed tools.

Letta *deliberately* puts model-authored text in the most privileged spot, the system prompt, and
lets it persist across sessions. It is "self-improving" by design [claim]. Neither generation
guards facts with provenance: once written, a model claim in a block reads the same as a
user-provided one.

For stack-agent this is the pattern to **avoid as evidence and to fence if it is ever adopted as
advice**:

1. Never let a memory tier satisfy a required observation or feed an outcome classification. An
   observation must reference a host-recorded tool result by id.
2. If agent-editable notes are ever added, keep them in a separate tier labeled `model_note`. Keep
   that tier out of `EscalationContextPolicy`'s "condensed observations", which are host-checked
   facts. Render it under an explicit "unverified notes" heading.
3. Two Letta mechanisms are worth keeping:
   * **host-enforced write policy** (read-only labels or globs checked against the *accepted*
     prior state, plus size caps), with V1's lesson that a limit shown to the model but not
     enforced is no limit;
   * **versioned, diffable memory** (git history in MemFS; `block_history` in V1), so every
     change has provenance and can be reverted.
4. Letta's "memory blocks shared across agents" is an unmediated shared-write channel. Any
   stack-agent equivalent must be host-written only.

### Compaction and summarization

* **letta-code, local backend:**
  * The default mode is `sliding_window` at 30%
    ([`LC/src/backend/local/compaction.ts#L43-L44`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/backend/local/compaction.ts#L43-L44)).
  * It triggers when estimated context exceeds `contextWindow − reserve`
    ([`provider-turn-executor.ts#L241-L272`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/backend/dev/provider-turn-executor.ts#L241-L272)),
    or after a provider context-overflow error.
  * The cutoff must fall on an assistant message and never splits off a pending tool call
    ([`compaction.ts#L625-L691`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/backend/local/compaction.ts#L625-L691)).
  * Tool returns are clipped to 2000 characters before they are summarized (L25).
  * **The summary is LLM-generated.** It is injected as a `system_alert` saying "N messages … have
    been hidden … The following is a summary" (L723–L749) and *replaces* the evicted messages in
    context.
  * The raw messages are not deleted. The transcript gets an appended `compaction` entry with
    `firstKeptEntryId`
    ([`LC/src/backend/local/local-store.ts#L3000-L3028`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/backend/local/local-store.ts#L3002-L3027)) [source].
* **V1:** `compact_messages` summarizes and persists a `summary`-role message through the same
  checkpoint path
  ([`LA/letta/agents/letta_agent_v3.py#L1265-L1272`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/agents/letta_agent_v3.py#L1265-L1272)).
  The trigger is 100% of the context window, or 90% for GPT-5
  ([`LA/letta/services/summarizer/thresholds.py#L27-L41`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/services/summarizer/thresholds.py#L27-L41)) [source].
* **Verdict:** in both, summaries *replace evidence in context*. Evidence survives only on disk.
  Chip's "condensed observations are host facts, never model summaries" is stricter, and should
  stay that way. Two pieces are portable:
  * the **cut-point invariant**: never separate a tool call from its result, and never compact a
    pending call;
  * the **append-only compaction marker** pointing at the first kept entry.

## Durability (D1–D4)

| | letta-code headless / TUI, local backend | letta-code websocket listener (App Server) | V1 server |
| --- | --- | --- | --- |
| **D1 Save** | Yes. Per-agent JSON plus an append-only `messages.jsonl` per conversation, written with `appendFileSync`/`writeFileSync`, **no fsync and no temp+rename** for agent records ([`local-store.ts#L2746-L2756`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/backend/local/local-store.ts#L2746-L2756), [`#L3030-L3036`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/backend/local/local-store.ts#L3030-L3036)) [source]. The assistant tool call reached disk before tool execution in W1/W2 [experiment] | Yes, plus an interrupted-turn record written atomically (temp file, flush, rename, fsync of the directory) ([`interrupted-turn-storage.ts#L14-L42`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/websocket/listener/interrupted-turn-storage.ts#L14-L42)) [source] | Yes, in Postgres/SQLite. Messages are persisted **only once a step has completed**: "ONLY place where messages are persisted" ([`letta_agent_v3.py#L758-L800`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/agents/letta_agent_v3.py#L758-L800), [`#L1402-L1410`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/agents/letta_agent_v3.py#L1402-L1410)) [source] |
| **D2 Resume** | Partial. A new process continues the conversation, but needs a new user message (`-p ... --conversation <id>`) [experiment] | The listener reconciles pending approvals against its record on reconnect or restart [source] | No step-level resume. A step dying mid-way loses its unpersisted messages, and the client re-sends [inferred from L758 "only after the step completes"] |
| **D3 Safe recovery** | Recorded progress was not lost (W1–W3) [experiment]. Writes are not crash-atomic, so a torn last JSONL line is possible; a corrupt-agent test exists, the torn-line case is untested [inferred] | Revision-CAS on the record (`expectedRevision`), and claim/lineage fencing ([`interrupted-turn-record.ts#L245-L280`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/websocket/listener/interrupted-turn-record.ts#L245-L280)) [source] | The step is effectively a transaction for messages, but tool effects fall outside it [inferred] |
| **D4 Effect safety** | **No.** On resume, any pending call is closed with `STALE_APPROVAL_RECOVERY_DENIAL_REASON`, which tells the model that no result was recorded and it should "Re-issue the tool call if you still need it" ([`LC/src/headless.ts#L1771-L1779`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/headless.ts#L1771-L1779), [`turn-recovery-policy.ts#L464-L465`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/agent/turn-recovery-policy.ts#L464-L465)) [source]. **W2 produced a duplicate charge** [experiment]. No idempotency keys | **Partially yes. This is the most valuable primitive here.** Before each approved tool runs, the listener durably checkpoints the call as `unknown` and removes it from `unstartedToolCallIds`. After the tool settles it checkpoints the exact result ([`recovered-approval-checkpoint.ts#L50-L150`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/websocket/listener/recovered-approval-checkpoint.ts#L50-L150); fence call site [`approval-execution.ts#L206-L207`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/agent/approval-execution.ts#L206-L207)). Recovery sorts each call three ways: exact saved result → replay it without re-running; provably unstarted → execute; started without a result → **deny with "it may or may not have run. Check whether its effect already happened"** ([`recovery-sync.ts#L183-L216`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/websocket/listener/recovery-sync.ts#L183-L216), [`turn-recovery-policy.ts#L467-L468`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/agent/turn-recovery-policy.ts#L467-L468)) [source; not exercised]. The ambiguous case is surfaced to the *model* as text and is not blocked | No. A tool effect can complete, the step can die, and nothing records that the effect happened [inferred] |

E2 on the headless local backend [experiment]:

| Window | Outcome |
| --- | --- |
| W1 | `correct` |
| W2 | `duplicate_effect` (2 charges) |
| W3 | `correct` |
| W4 | `not_applicable` |
| W5, W6 | not measured |

See [`experiments/letta/README.md`](../experiments/letta/README.md).

**Note.** The `beforeToolExecution` fence is wired only from the websocket listener
(`turn-approval.ts`, `recovery.ts`). A grep finds no other callers [source], so the safer recovery
does not cover the TUI or headless paths.

## Handoffs and multi-agent

* **letta-code subagents** run as **separate child processes**
  ([`LC/src/agent/subagents/subagent-process.ts#L118-L123`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/agent/subagents/subagent-process.ts#L118-L123)),
  limited by `MAX_SUBAGENT_DEPTH = 2`
  ([`subagent-depth.ts#L11`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/agent/subagents/subagent-depth.ts#L11)) [source].
* **Context passed to a subagent** depends on the kind:
  * a "fork" subagent receives the **full parent transcript** "as reference", with a prompt
    telling it that it is not the primary agent (`manager.ts` L773–L789);
  * a memory worker gets **only its assignment**, with parent history available on demand as a
    temporary transcript file that is deleted when the child exits
    ([`memory-handoff.ts#L8-L30`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/agent/subagents/memory-handoff.ts#L8-L30)) [source].
* **Agent-to-agent messaging:** `SendAgentMessage` appears in the tool list sent to the model
  [experiment: fake-server request log], along with `--from-agent` headless injection.
* **V1:** `send_message_to_agent_and_wait_for_reply`, `send_message_to_agent_async` and
  `send_message_to_agents_matching_tags`
  ([`LA/letta/functions/function_sets/multi_agent.py#L60-L163`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/functions/function_sets/multi_agent.py#L60-L163)),
  plus groups and shared blocks (`groups_blocks`) [source]. These are message passing between
  agents on one server, with no durable handoff record.
* **Ownership:** each agent owns its transcript. Shared blocks are shared mutable state with
  last-writer-wins semantics [inferred].

## Provider and local-model notes

* letta-code's local backend uses `@earendil-works/pi-ai` providers, including dedicated
  **Ollama, LM Studio, llama.cpp and generic OpenAI-compatible** providers
  ([`LC/src/backend/dev/`](https://github.com/letta-ai/letta-code/tree/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/backend/dev),
  e.g. `pi-openai-compatible-provider.ts`). Models come from `/v1/models`, with conservative
  capability assumptions [source].
* A scripted OpenAI-compatible server was enough to drive the real runtime with no network
  model [experiment].
* There is no structured-output contract for decisions: the model's tool calls are the decision
  format.
* V1 has `LLMConfig` per agent (`orm/agent.py` L80) and many provider clients under `llm_api/`,
  plus `local_llm/` [source].
* E4 is blocked: there is no local endpoint in this environment.

## Tools

* letta-code has a large built-in coding toolset: Bash, Read, Edit, Write, Memory, Agent, Task*,
  Monitor, Skill, Workflow, and others [experiment: request log]. It also supports MCP and mods.
* Tool output is clipped (`overflow.ts`, `truncation`). Parallel and serial grouping is decided
  per resource.
* A `TURN_DID_NOT_COMPLETE` status and an `INTERRUPTED_BY_USER` tool return mark aborted
  calls [source].
* No idempotency key is passed to tools.

## Observability

* **Headless JSON** reports usage, `duration_ms`, `agent_id` and `conversation_id`. It reported
  `step_count: 0` and `num_turns: 0` for a 3-call run, so these counters are unreliable on the
  local backend [experiment].
* **Other surfaces:** `letta steps trace --agent --step` and a per-message run id (`otid`) [source].
* **Telemetry** is on by default and can be turned off with `LETTA_CODE_TELEM=0` or
  `DO_NOT_TRACK=1` ([`LC/src/telemetry/index.ts#L388-L402`](https://github.com/letta-ai/letta-code/blob/e8073d4e475ab7f56e76f9aaeea81a7618a92575/src/telemetry/index.ts#L388-L402)) [source].
* **V1:** `steps`, `runs` (statuses `created|running|completed|failed|cancelled`) and
  `run_metrics` tables, plus provider traces and OTel
  ([`LA/letta/orm/run.py#L22`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/orm/run.py#L22),
  [`LA/letta/schemas/enums.py#L152-L161`](https://github.com/letta-ai/letta/blob/56ba9c25552605eec89de8ed3dc6394b625c1993/letta/schemas/enums.py#L152-L161)) [source].

## Primitives worth porting

1. **Per-call effect boundary with three-way recovery** (from the letta-code listener).
   * **Guarantee:** before a capability runs, durably mark it `started/unknown` (fsync plus
     revision CAS). After it runs, durably record the exact result. On restart:
     * exact result present → replay the recorded result and do not re-execute;
     * provably unstarted → safe to execute;
     * started without a result → ambiguous.
   * **Rust boundary:** this matches FeltDB's existing "intent before execution, observation and
     outcome after, reconcile on restart". The parts worth taking are:
     * the **explicit `unstarted` set**, so batch recovery can prove a sibling never ran;
     * **rollback of the pre-effect mark** when the fence write itself fails or the claim is lost
       before the executor starts;
     * a revision token on every record update.
   * **Where stack-agent should differ:** Letta hands the ambiguous case to the model as text.
     Chip should keep `OutcomeUncertain` as a **blocking** state that only host reconciliation or
     a human can clear, never the model.
   * **Sketch:** `enum CallPhase { Unstarted, Started { intent_rev }, Settled { observation_id } }`
     per capability call inside the task record. Transitions go through FeltDB `fence`/`lease`
     CAS.
2. **Host-enforced memory/state write policy checked against the accepted prior state**
   (MemFS pre-commit: protected keys, read-only globs, size caps).
   * **Guarantee:** a write is rejected unless it satisfies the policy relative to the last
     accepted version.
   * **Rust boundary:** a `WritePolicy::validate(prev: &Accepted, next: &Proposed) -> Result<(),
     Violations>` for any agent-adjacent artifact, such as learning records or future notes.
     Lesson from V1: enforce at the write site, not only in the prompt.
3. **Tool-rule solver as a host-side loop policy** (V1 `ToolRulesSolver`: `required_before_exit`,
   `exit_loop`, `constrain_child_tools`, `max_count_per_step`, `requires_approval`).
   * **Guarantee:** the host, not the model, decides when the loop may end and which capability
     may follow which.
   * **Rust boundary:** these become preconditions on Chip's `complete` and `request_capability`.
     Strengthen `required_before_exit` to "a required *observation* exists", which is what Chip
     already does.
4. **Compaction cut-point invariants plus an append-only compaction marker.**
   * **Guarantee:** never split a call from its result, never compact a pending call, and keep
     the raw log with a `first_kept` pointer.
   * **Rust boundary:** used inside the bounded/deduplicated `EscalationContextPolicy`. The
     summary text itself is not portable.
5. **Approval as the universal tool handoff.** Every model tool call becomes a pending request
   that the host resolves.
   * This matches Chip's validate/authorize step already. It is worth copying because a pending
     request is a durable, listable object, recovered with `getResumeData` and pending approvals.

## Not worth porting

* **Agent-editable core memory, archival inserts, sleeptime reflection and self-rewriting
  prompts.** These are model-generated text placed in privileged context with no provenance,
  which contradicts "model output is never evidence".
* **LLM summaries replacing history in context.** Chip's host-checked condensed observations are
  the stricter design.
* **Letta's recovery message "re-issue if you still need it".** It caused the measured duplicate
  effect.
* **The local file store's persistence:** no fsync, a non-atomic agent JSON, and three separate
  orchestration paths with different recovery guarantees.
* **Shared mutable blocks across agents,** which are last-writer-wins.
* **The V1 server as a whole:** a Postgres-backed multi-tenant service, retired upstream.

## Open questions

* Does the websocket listener's three-way recovery behave as the source suggests under SIGKILL?
  Not exercised here; that needs a scripted App Server client.
* Torn trailing line in `messages.jsonl` after a crash mid-write: is it tolerated or a hard error?
  There is a corrupt-agent test (`local-store-corrupt-agent.test.ts`). The transcript case was not
  checked.
* Is the V1 block limit enforced anywhere I did not inspect, such as the REST layer or the DB?
  The paths named above show no check.
* A real model's behavior after the "stale/started" synthetic results (E4, blocked).
