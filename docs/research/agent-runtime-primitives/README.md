# Agent-runtime primitives worth porting to native Rust

**Research only.** This directory changes no production code, tests, dependencies or behavior in
stack-agent. It recommends follow-on work; it implements none of it.

The question: which proven mechanisms from open-source agent runtimes would measurably improve
stack-agent? The candidates are judged against stack-agent's existing loop (chip-core) and its
native FeltDB agent-state contract. The answer is a ranked backlog of primitives, not a ranking of
frameworks.

How to read it:
* Method: [`protocol.md`](protocol.md). It sets the evidence tags `[source]`, `[experiment]`,
  `[claim]`, `[inferred]` and `[blocked]`; durability as four separate properties D1 to D4; crash
  windows W1 to W6; and a ledger as ground truth.
* What stack-agent is today: [`baseline.md`](baseline.md).
* One dossier per candidate: [`candidates/`](candidates/).
* Harnesses, exact commands and raw results: [`experiments/`](experiments/).

Every result was gathered on 2026-10-19/20 on one 4-vCPU Linux VM with Python 3.13.16. No network
model was called anywhere: every harness uses a scripted fake model.

## 1. Executive summary

**The main finding is a gap between two kinds of correctness, and stack-agent already has the
harder one.**

* **Every external framework tested repeats a side effect when the process dies inside it (W2).**
  That held for all eight with a durable store, each under a real `SIGKILL` and a fresh-process
  resume: LangGraph, the OpenAI Agents SDK, Agno, Letta, Microsoft Agent Framework, CrewAI,
  Pydantic AI and the OpenHands SDK. smolagents has nothing to resume and repeats it too.
  [experiment]
* **No framework records an intent before an effect and then refuses to resume when that intent's
  outcome is unknown.** Pydantic AI records exactly that ledger and resumes anyway (the charge ran
  twice). OpenHands' server answers the orphaned call with an error that says "did not complete"
  even when the effect happened. [experiment]
* **stack-agent's FeltDB contract is the only one that stops at W2.** It refuses to resume with
  `OutcomeUncertain`. [experiment]
* **But stack-agent repeats effects in windows where most frameworks do not.** A resumed attempt
  starts again from the goal, with no observations or decisions restored
  (`docs/product/agent-state.md:83` [source]). So a kill *after* the charge's outcome was durably
  recorded (W3), or during any later state write (W4), still charges twice. LangGraph, the OpenAI
  SDK, Agno and Letta resume W3 correctly. [experiment]
* **No idempotency key reaches any executor in any runtime tested.** stack-agent computes one
  (`intent_key`) and does not forward it (`crates/chip-agent-state/src/lib.rs:1141-1151` [source]);
  `ExecutionRequest` has no field for it (`crates/chip-core/src/lib.rs:309-315` [source]).

So the smallest set of primitives that measurably improves stack-agent is about **durable
execution**, and it builds entirely on FeltDB as the one authority:

1. **Resume the trajectory from FeltDB's recorded observations** instead of restarting from the
   goal (§9, P1).
2. **Forward an attempt-independent idempotency key** to executors (P2).
3. **An operator settle command**, and no lease held by a refused resume (P3).

Handoffs, delegation, agent-editable memory and code-as-action add no measurable value here, and
several are actively unsafe; they are rejected or deferred (§8). Effectiveness with small local
models could not be measured (E4 `[blocked]`), so no recommendation rests on it.

## 2. Baseline (summary of [`baseline.md`](baseline.md))

* **Loop:**
  * Turn-based, at most one decision and one execution per turn.
  * A local policy proposes; otherwise the model gets one escalation in the closed
    `chip.work-decision.v1` contract.
  * Every request is validated and allowlisted before it executes.
  * A completion is refused unless the required observations exist.
  * No retries or repair loops, by design (AGENTS.md §14).
* **Durable state:** FeltDB `feltdb.agent.transition` v1, holding a task with owner, fence and
  lease. An intent is recorded before every execution, and the executor is not called if that
  fails. An observation and an outcome are recorded after. On restart, intents are reconciled from
  observations, and an unresolved intent blocks resumption.
* **Measured** (`experiments/stack-agent/`, real `SIGKILL` of the release binary at `366f6c1`, 12
  FeltDB commits per run):
  * W2 is refused (`stuck`). Once an operator records `executed=true`, the next run charges again,
    because the loop starts from the goal.
  * W3, every W4 variant and the torn-record variants give `duplicate_effect`.
  * W1 gives `lost_progress`: the earlier read is redone.
  * A resume refused with `OutcomeUncertain` has already taken a new lease, so recovery waits for
    two lease expiries (2 × 900 s by default).
  * A `SIGKILL` can tear a FeltDB record, and FeltDB truncates the torn tail when it next opens the
    file. [experiment]
* **Missing entirely:** handoffs, delegation, approval or suspension, and an operator reconcile
  command. Model attempts are not persisted, and the evidence store is in memory only. [source]

**Must remain authoritative:**
* FeltDB for task, intent, outcome, checkpoint and observation records;
* Chip for decisions, validation, authorization and completion;
* FX for provider interaction;
* the session store (`chip-session`/`chip-turns`) for conversational turns.

No recommendation below adds a second store, a sidecar or a parallel orchestration layer.

## 3. Candidate matrix

The durability columns record what each candidate does at the protocol's crash windows with its
most durable built-in store; dossiers give the per-window evidence. Two terms recur below:
* a **pointer** is a separately saved "current position" that can lag the log;
* an **orphan** is a tool call with no recorded result.

| Candidate (commit) | License | Execution model | D1 save | D2 resume | D3 safe recovery | D4 effect safety | Handoffs / approval | Local models | Porting value |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **stack-agent** (`366f6c1`) | MIT | turn loop, closed decision contract | yes | partial: restarts from the goal | yes for records, but progress is not reused | **detects W2**; repeats W3/W4 | none | OpenAI-compatible, local profile | baseline |
| LangGraph (`1400d25`) | MIT | Pregel supersteps over typed state | yes | yes (not in `exit` mode) | mostly: whole-node redo, a W4 race, no lease | no; two resumers both charged | goto plus interrupt; approval survives restart | model-agnostic via LangChain | high (resume, interrupt) |
| OpenAI Agents SDK (`64cd5df`) | MIT | turn loop: final, handoff, run again, interrupted | yes (session is a log) | partial: run state only at interruptions | items intact; unrecorded effects invisible | no | handoffs, approval run state | Chat Completions, LiteLLM | medium (turn outcome, suspension) |
| Agno (`643f6e7`) | Apache-2.0 | tool loop inside the model layer | yes | yes (paused and tool-batch snapshots) | partial: two non-atomic commits | no | teams, approval across restart | many providers | medium (snapshot, fenced writes) |
| smolagents (`96f33fa`) | Apache-2.0 | minimal ReAct; code or tool calls | only by hand | no | no | no | managed agents as tools; no approval | broad local support | low to medium (loop details) |
| Letta Code (`e8073d4`) | Apache-2.0 | CLI/app server; MemFS memory | yes | yes | W2 resolved by asking the model | no (headless) | app-server approval (not run) | OpenAI-compatible | medium (three-way recovery, write policy) |
| Microsoft Agent Framework (`809cc04`) | MIT | superstep workflows with typed ports | yes (atomic rename, no fsync) | yes | partial: per-superstep redo | no | request/response ports | multiple | low (design reference) |
| Mastra (`8ea807f`) | Apache-2.0 outside `ee/` | workflows with suspend/resume | yes | yes [source] | partial: fenced ownership | no ([claim] in its docs) | atomic `suspended→running` claim | multiple | low (FeltDB already fences) |
| CrewAI (`c6e500a`) | MIT | crews and flows | yes | weak | weak | no | free-text delegation | multiple | none |
| Pydantic AI (`60c1d1a`) | MIT | typed agent, step persistence | yes | yes | ledger records ambiguity | **no: ambiguity recorded but not enforced** | deferred tool requests/results | multiple | medium (tool-declared key) |
| OpenHands SDK (`33d8aa2`) | MIT | fsynced event log, agent server | yes | yes | pointer lag drops effects from the conversation | no | confirmation mode; approve-then-crash re-runs | LiteLLM | medium (orphan rule, lease, stuck guard) |
| Rig (`98ed06d`) | MIT | serializable, I/O-free `AgentRun` | in memory, not fsynced | restore | started calls answered with an error | no | none | Ollama and OpenAI-compatible | medium (run-machine shape) |
| goose (`9920efa`) | Apache-2.0 | load → step → effects → apply | yes | yes [source] | not tested | not tested | — | multiple | low (small reference) |
| Google ADK (`1def14b`) | Apache-2.0 | runner, session and events | yes | resume decision | at-least-once ([claim]) | no ([claim]) | transfer vs agent-as-tool | LiteLLM | low |
| LlamaIndex Workflows (`5c55ee3`) | MIT | event-driven steps | yes | whole-step replay | — | no | human-in-the-loop events | multiple | low |
| swarms-rs, AutoAgents, langchain-rust | — | — | — | — | — | — | — | — | excluded ([`candidates/tier3.md`](candidates/tier3.md)) |

## 4. Primitive dossiers

Source-backed detail is in each dossier. The strongest implementation of each mechanism:

| Mechanism | Strongest reference | Why it is the reference | Dossier |
| --- | --- | --- | --- |
| Per-task results reapplied on resume | LangGraph | Deterministic task ids; completed tasks' writes are restored and never re-run [source, experiment] | [langgraph.md](candidates/langgraph.md) |
| Serializable run state with no I/O | Rig `AgentRun` | Every position is data; re-issuing a step is idempotent; restore answers started calls with an error [source] | [rig.md](candidates/rig.md) |
| Intent before effect, with three-way recovery | Letta app server; Pydantic AI harness | Mark "unknown" before the tool; on restart, replay the recorded result, run a provably unstarted call, flag a started one as ambiguous [source]. Pydantic AI's tool-declared idempotency key [source] | [letta.md](candidates/letta.md), [tier3.md](candidates/tier3.md) |
| Orphan rule and owner lease | OpenHands agent server | A stale run becomes an error; an orphaned call gets a synthetic error; the lease has a generation counter and refuses an old owner's writes [source, experiment] | [openhands.md](candidates/openhands.md) |
| Durable suspension and approval | OpenAI SDK run state; LangGraph `interrupt`; Mastra atomic claim | A versioned snapshot resumes once; an approval is consumed atomically [source, experiment] | [openai-agents.md](candidates/openai-agents.md), [tier2.md](candidates/tier2.md) |
| Errors fed back to the model | smolagents, OpenAI SDK, Agno, LangGraph | A rejected call becomes an observation the next turn sees [experiment] | [smolagents.md](candidates/smolagents.md) |
| Host-side tool rules | Letta V1 `ToolRulesSolver` | Must-call-before-exit and allowed-next-tool rules as host predicates [source] | [letta.md](candidates/letta.md) |
| Stuck guard | OpenHands | Five repetition patterns detected without asking the model [source] | [openhands.md](candidates/openhands.md) |
| Provider wire/transport split | Rig | A pure encoder plus a transport seam. **Mostly already in FX**, which is stricter [source] | [rig.md](candidates/rig.md) |

## 5. Experiment report

Every harness, its exact commands and raw results are in `experiments/<candidate>/`. Shared
definitions are in [`protocol.md`](protocol.md).

### E1: minimal tool use

Every candidate that ran E1 completed the task with exactly one charge in the ledger:
* stack-agent;
* LangGraph, the OpenAI SDK, Agno, smolagents (tool-calling), Letta, OpenHands and Pydantic AI.

For a call missing a required argument:
* stack-agent rejects it at validation, before any intent, and ends the work `blocked` without
  asking the model again.
* Every framework that rejects it before the tool runs feeds the error back to the model and
  recovers on the next call: LangGraph, Agno, the OpenAI SDK, smolagents (tool-calling), OpenHands
  and Pydantic AI.
* smolagents' code-as-action did not validate types: `amount="twelve"` was charged.

[experiment]

### E2: crash recovery (real `SIGKILL`, fresh-process resume)

Charges in the ledger after resume (1 is correct):

| Candidate | W1 before | W2 inside effect | W3 after record | W4 during state write | W6 awaiting approval |
| --- | --- | --- | --- | --- | --- |
| **stack-agent** | 1 (progress lost) | **stuck** (OutcomeUncertain); 2 after the operator records `executed=true` | **2** | **2** (all variants) | n/a |
| LangGraph (sqlite, sync) | 1 | **2** (5/5) | 1 | 1 or **2** (race) | 1 |
| OpenAI Agents SDK | 1 (manual rerun) | **2** | 1 | **2** | 1 |
| Agno (tool-batch) | 1 | **2** | 1 | **2** | 1; **2** if killed inside the approved charge |
| smolagents | 0 (lost) | **2** | **2** | n/a | n/a |
| Letta Code (headless) | 1 | **2** | 1 | n/a | not run |
| Microsoft Agent Framework | 1 | **2** | 1 | **2** | 1 |
| CrewAI `@persist` | — | **2** | **2** | — | 1 |
| Pydantic AI | 1 | **2** (ledger flagged it) | 1 | — | 1 |
| OpenHands SDK / server | 0–1 / 0 | **2** / 1, but "did not complete" | 1 / 1 | **2** / 1–2 | 1; approve-then-crash gives **2** |

(— not measured; n/a not applicable. The dossiers give exact per-window definitions, repetitions
and variants.)

### E3: handoff and approval

* Approval survived a process restart in LangGraph, the OpenAI SDK, Agno, Microsoft Agent
  Framework, CrewAI and OpenHands.
* Three ran approve-then-crash, and all three re-executed the effect: the OpenAI SDK (one saved
  run state resumed twice), Agno and OpenHands (the saved status still says "waiting").
* Handoff context is the full shared history by default in LangGraph and the OpenAI SDK. It is a
  task string in Agno, smolagents and CrewAI.
* Two report a failed specialist's effect wrongly. In the OpenAI SDK the run completes and the model
  can claim success with 0 charges. In Agno the team run completes. [experiment]

### E4: small-model effectiveness

**[blocked]**: no local inference endpoint is reachable here. stack-agent's own recorded local
runs (gemma via LM Studio) are summarized in [`baseline.md`](baseline.md) §8 as `[claim]`. They
are not compared with anything here.

### E5: runtime overhead

Median milliseconds per run of the E1 task. Each figure includes the fake tools' two `fsync`s.

| Candidate | No store | Durable store | Process model |
| --- | --- | --- | --- |
| Agno | 2.6 | 9.1 (`runs`) / 17.0 (`tool-batch`) | in-process |
| LangGraph | 9.1 | 25.3 (sqlite sync) | in-process |
| OpenAI Agents SDK | 11.9 | 20.1 (file sqlite) | in-process |
| smolagents | 16.2 | — | in-process |
| OpenHands SDK | 18 | 42 | in-process |
| stack-agent | 83.3 | 101.7 | **whole process** plus an HTTP fake model and a fake pax process |
| Letta Code | — | 2980 | whole CLI process (about 1.7 s cold start) |

Only rows with the same process model are comparable. **stack-agent's figures are not comparable
with the in-process Python figures.** They include process start, HTTP and a child process per
effect. The like-for-like statement is that FeltDB durability adds about 18 ms (22%) to a whole
stack-agent run. Python import times (0.5 to 2.5 s) are in the dossiers. Agno's published
construction figures are recorded as `[claim]` only. [experiment]

## 6. Failure semantics

| Failure | What frameworks do | What stack-agent does | What it should do |
| --- | --- | --- | --- |
| Killed before an effect (W1) | Resume and run it once | Re-run from the goal; one charge, progress redone | Resume from recorded observations (P1) |
| Killed inside an effect (W2) | Re-run it (duplicate). Or record the ambiguity and ignore it (Pydantic AI). Or call it "did not complete" (OpenHands server) | Refuse: `OutcomeUncertain` | **Keep refusing.** Settle by an operator (P3); then resume from the settled outcome without re-deciding (P1) and pass the key (P2) |
| Killed after the effect's record (W3) | Resume correctly (most) | **Duplicate**: restarts from the goal | Resume from recorded observations (P1) |
| Torn or partial state write (W4) | Background-write races (LangGraph, CrewAI); non-atomic commits (Agno); a lagging pointer (OpenHands) | FeltDB truncates the torn tail; then duplicates, because it restarts from the goal | P1. Derive every position from FeltDB records, never from a separately saved pointer |
| Two resumers | LangGraph: both charge (no lease). OpenHands and Mastra: generation-fenced lease | Fence and lease refuse the second resumer; a refused resume holds the lease | Release the lease on refusal (P3) |
| Killed after approval, before the effect | Re-executes (OpenAI SDK, Agno, OpenHands) | n/a (no approval) | If approvals are ever added, the approval is a durable record consumed atomically with the intent (P6) |

The underlying lesson: **persisting state (D1) is common; protecting effects (D4) needs an intent
before the effect, enforcement of unknown outcomes, and a key the effect can de-duplicate on.**
stack-agent has the first two and lacks the third, and it throws away everything recorded when it
resumes.

## 7. Rust portability

Each primitive maps onto existing types. None needs a new crate, a new store or a new dependency.

* **P1, trajectory resume.**
  * `chip_agent_state::begin` already reads every observation record back (`History`).
  * The port gives the resumed `WorkSpec` a **seeded trajectory**: the recorded
    `(Observation, ObservationOrigin)` pairs in order, inserted by chip-core as already-observed
    and never re-executed.
  * FeltDB stays the authority, because the observations are its records.
  * Complexity M, touching chip-core's work loop, chip-agent-state and stack-agent's durable path.
  * Rig's `AgentRun` shows the end state: positions derived from data. Its unconditional re-issue
    of unanswered calls must not be copied.
* **P2, idempotency key.**
  * Add `ExecutionRequest.idempotency_key: Option<String>`, or a sealed `ExecutionContext` passed
    alongside.
  * The key is derived from task id plus the position of the decision in the restored trajectory,
    **not** the attempt number. Today's `intent_key(attempt, execution_id)` changes across attempts
    and so de-duplicates nothing.
  * Executors that ignore the key are unchanged.
  * Complexity S to M (chip-core type, chip-agent-state, executors opt in).
* **P3, settle command and lease release.**
  * A `stack-agent agent-state settle --task-id --intent --executed true|false --evidence …`
    command writing the same `chip.Observation` and outcome records the durability bench writes.
  * `begin` releases its fresh lease when it refuses with `OutcomeUncertain`.
  * Complexity S.
* **P4, read-only effect declaration.**
  * `CapabilityDescriptor` gains an effect declaration: observation, consequential or unknown.
  * An open intent on an observation-only capability may be re-observed instead of blocking
    (Rig's idea, made safe by declaration).
  * Complexity S.
* **P5, typed rejection observation.**
  * A rejected decision becomes a host-authored observation ("field `path` missing") seen next
    turn, bounded by the turn budget.
  * Complexity S. This is a policy change against AGENTS.md §14's "no repair loops", so it is
    gated on E4 evidence.
* **P6, durable suspension and approval.**
  * A FeltDB intent state `awaiting_approval`, with an approval record consumed atomically with the
    lease before the effect runs.
  * Complexity L. It needs a contract revision, which belongs to FeltDB's owners, and the §14
    justification for Attn-like behavior.

## 8. Recommendation matrix

| Primitive | Verdict | Evidence |
| --- | --- | --- |
| Trajectory resume from FeltDB observations (P1) | **Adopt (adapt)** | stack-agent W3/W4 duplicates [experiment]; LangGraph/OpenAI/Agno/Letta W3 correct [experiment] |
| Attempt-independent idempotency key to executors (P2) | **Adopt** | No runtime forwards one [source]; stack-agent computes one and drops it [source] |
| Settle command and lease release on refusal (P3) | **Adopt** | Two lease expiries; no operator command [experiment] |
| Effect declaration on capabilities (P4) | **Adapt** | Rig restore, Letta three-way recovery [source]; extends the experiment seam's `Effect` (merged in rkendel1/stack-agent#11) to production descriptors |
| Keep `OutcomeUncertain` blocking | **Keep** | Every framework that resolves ambiguity automatically duplicated or misreported [experiment] |
| Typed rejection observation (P5) | **Investigate further** | Frameworks recover from malformed calls [experiment]; effect on small models unmeasured (E4 blocked); conflicts with §14 |
| Durable suspension and approval (P6) | **Investigate further** | Survives restart in six candidates; approve-then-crash duplicates in all three that ran it [experiment]; needs a FeltDB contract revision |
| Stuck guard (repetition patterns, terminal, never prompts) | **Investigate further** | OpenHands [source]; no stack-agent measurement of loops yet |
| Host-side tool rules as completion preconditions | **Investigate further** | Letta V1 [source]; overlaps Chip's required observations |
| Provider wire/transport split | **Already covered** | FX's pure wires and decoder are stricter than Rig's [source] |
| Strict JSON-schema output for local models | **Already covered, measured worse** | stack-agent run 4: full schema 3/15 vs plain object 6/15 on gemma [claim, baseline §8] |
| Session/conversation log as durability | **Reject** | OpenAI SDK, OpenHands pointer lag [experiment] |
| Background or async checkpoint writes | **Reject** | LangGraph `async`, CrewAI [experiment] |
| Whole-node or whole-run re-execution on resume | **Reject** | LangGraph, Agno, Mastra [experiment, claim] |
| Handoffs with full shared history; free-text delegation | **Reject (for now)** | No measured benefit; context and authority leak [experiment, source]; §14 |
| Agent-editable memory as evidence | **Reject** | Letta: every tier but the transcript is model-written, with no provenance [source] |
| Code-as-action executed in-process | **Reject** | smolagents: "not a security sandbox", no type validation, partial effects [source, experiment] |
| Argument salvage or coercion | **Reject** | Rig partial-object salvage, Agno string coercion [source] |
| Built-in retries | **Reject** | AGENTS.md §14; duplicates under ambiguity [experiment] |

## 9. Ranked primitive backlog and follow-on proposals

Each item is a separate implementation PR, **not done here**. Each states its measurable
acceptance criteria against the experiments in this directory, which become its regression
harness.

**P1: resume the trajectory from FeltDB's recorded observations.**
* *Value:* removes stack-agent's duplicate effects at W3 and W4 and its lost progress at W1.
* *Boundary:* `begin` → `History` → a seeded `WorkSpec` trajectory in chip-core. No new store.
* *Acceptance:*
  * `experiments/stack-agent` W1a, W3, W4a, W4b and the torn-outcome and torn-final windows give
    exactly 1 ledger line, with `progress_lost=false`;
  * W2 still gives exit 3 `OutcomeUncertain`;
  * E1 is unchanged;
  * the durable E5 median rises by no more than 10%;
  * the durability bench's restart scenarios show the same outcome as an uninterrupted run;
  * no record outside contract v1 plus the existing Chip records.

**P2: attempt-independent idempotency key.**
* *Value:* an idempotent executor can de-duplicate the one case FeltDB must leave to the operator
  (W2 settled as `executed=true`).
* *Boundary:* chip-core `ExecutionRequest`, chip-agent-state key derivation.
* *Acceptance:*
  * the key is identical across attempts for the same restored decision position;
  * a test executor that de-duplicates on it gives 1 charge for W2 → settle → resume;
  * executors that ignore it are byte-for-byte unchanged.

**P3: settle command and lease release.**
* *Value:* recovery without hand-written FeltDB records and without a second lease wait.
* *Acceptance:*
  * `experiments/stack-agent` W2 recovers with the shipped command instead of `sa-state reconcile`;
  * a refused resume leaves the task acquirable immediately;
  * the operator's settle is recorded with who decided (Rig's `decided_by`).

**P4: capability effect declaration.**
* *Value:* fewer operator interventions, since an open intent on a read-only capability is
  re-observed.
* *Acceptance:* a W2-style kill inside `project.read` resumes without exit 3; a kill inside
  `pax.test` or `project.write` still exits 3.

**P5: typed rejection observation** (gated).
* *Prerequisite:* a local inference endpoint for E4.
* *Acceptance:* on the agent-bench tasks with the local model, the invalid-decision rate and task
  completion improve over the current baseline at the same turn budget, with no safety
  regressions.
* If not, reject.

**P6: durable approval** (gated).
* *Prerequisite:* a FeltDB contract revision proposed to its owners, and a §14 justification.
* *Acceptance:* W6 and approve-then-crash give 1 charge; one approval authorizes one execution.

Explicitly **not** proposed: a handoff or delegation primitive, a memory hierarchy, a graph or
workflow engine, a session-based durability layer, or any framework dependency.

## 10. Limitations

* **E4 is blocked.** Nothing here measures effectiveness with small local models.
* **Single runs.** Most windows were run once; LangGraph ran 5 per window. Runs are deterministic
  by construction (exact kill points, scripted models), but a race like LangGraph's W4 shows up
  only across repetitions.
* **Not run:** Rig (source only; building it would not fit on this disk), Mastra (source only),
  Letta's app-server recovery path, Agno's server-side job queue, and ADK and LlamaIndex
  experiments.
* **Version skew.** The Letta experiment ran `letta==0.34.12` against source at 0.34.18. Pydantic
  AI ran PyPI 2.57.0 against source at the pinned commit. Each dossier records both.
* **Letta telemetry.** It was left at its default (on) during its runs. Only synthetic fixture data
  existed.
* **Not comparable across process models.** stack-agent's E5 numbers include process start and
  HTTP (§5).
* **Different fake models.** Each harness uses a fake model adapted to its framework's interface
  (each dossier says which). CrewAI's had to key replies by step rather than by count; its README
  records why.
* **Baseline benchmark figures** in [`baseline.md`](baseline.md) §8 are recorded results, not
  reproduced here.

## 11. Source index

All links are immutable. Per-file references are in each dossier.

| Project | Inspected tree | License |
| --- | --- | --- |
| stack-agent | [366f6c1](https://github.com/rkendel1/stack-agent/tree/366f6c13c243f8bd419e593e0bfbe67d071c9d3e) (feltdb at rkendel1/flow_db `b244813`) | MIT |
| LangGraph | [1400d25](https://github.com/langchain-ai/langgraph/tree/1400d25b7b88cfa055c55b8a82bd95b7c25caef1) | MIT |
| OpenAI Agents SDK | [64cd5df](https://github.com/openai/openai-agents-python/tree/64cd5dfc5a1579870dcbed0ea42b27aa84033fee) | MIT |
| Letta Code | [e8073d4](https://github.com/letta-ai/letta-code/tree/e8073d4e475ab7f56e76f9aaeea81a7618a92575); retired V1 server at letta [56ba9c2](https://github.com/letta-ai/letta/tree/56ba9c25552605eec89de8ed3dc6394b625c1993); landing [8816d3a](https://github.com/letta-ai/letta/tree/8816d3a8516893bfdc0fa14bfb1235226012e295) | Apache-2.0 |
| Agno | [643f6e7](https://github.com/agno-agi/agno/tree/643f6e797312bb00a80a7bc2e445449b8106970d) | Apache-2.0 |
| smolagents | [96f33fa](https://github.com/huggingface/smolagents/tree/96f33faaf028479119ec8d34507b47694cf14e34) | Apache-2.0 |
| Microsoft Agent Framework | [809cc04](https://github.com/microsoft/agent-framework/tree/809cc04f2d610bb48fa4274b6e7f0e96773d576d) | MIT |
| Mastra | [8ea807f](https://github.com/mastra-ai/mastra/tree/8ea807fc7cc79c9e05d72093098396a95face0cc) | Apache-2.0 (outside `ee/`) |
| CrewAI | [c6e500a](https://github.com/crewAIInc/crewAI/tree/c6e500ab3730ccf3badb640c6e0170e5c6aa3882) | MIT |
| Pydantic AI | [60c1d1a](https://github.com/pydantic/pydantic-ai/tree/60c1d1a6cdbe51f7b80150f9e237488b51ade335) | MIT |
| OpenHands software-agent-sdk | [33d8aa2](https://github.com/OpenHands/software-agent-sdk/tree/33d8aa29c3078d074799e2fbf5e847aeb2d71360) | MIT |
| Rig | [98ed06d](https://github.com/0xPlaygrounds/rig/tree/98ed06d7f1c911e6e4c7fdb5d690093bc964c514) | MIT |
| goose | [9920efa](https://github.com/aaif-goose/goose/tree/9920efa16d078777a4d9715b2f00d8800a4da188) | Apache-2.0 |
| Google ADK | [1def14b](https://github.com/google/adk-python/tree/1def14ba8efa662a5fa5d3b4c368ec8eecc9a2d6) | Apache-2.0 |
| LlamaIndex Workflows | [5c55ee3](https://github.com/run-llama/workflows-py/tree/5c55ee37f424123fcdc1807a935f92a982d632e9) | MIT |
| swarms-rs (excluded) | [ee1e74c](https://github.com/The-Swarm-Corporation/swarms-rs/tree/ee1e74cb6e2c94775db88dddb311f2235e3fe9e3) | Apache-2.0 |
| AutoAgents (excluded) | [a85503b](https://github.com/liquidos-ai/AutoAgents/tree/a85503b1e62b015f5bb774c1a5d90a775c0347bc) | MIT/Apache-2.0 |
| langchain-rust (excluded) | [cfc709a](https://github.com/Abraxas-365/langchain-rust/tree/cfc709a71fee02083ced078252d73c1f76fe9849) | MIT |
