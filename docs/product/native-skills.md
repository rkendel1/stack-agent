# Native skills: design proposal

**Status: design only. Nothing here is implemented.** This change adds this document and one
example skill under [`native-skills/examples/`](native-skills/examples/). No code, configuration,
dependency, capability, decision field or benchmark artifact changes. `stack-agent work`, `verify`
and `serve` behave exactly as before. Implementation is a separate change, after review.

Labels: **read** means established from the code or documents cited. **proposed** means a design
choice of this document. **open** means a question it does not settle.

## 1. The question

Can a small, reviewed workflow description, given to the model only when it applies, help smaller
local models do recurring kinds of work? The model is told nothing new about authority, and Chip
validates, executes and evaluates exactly as before.

This is a model-capability question (AGENTS.md §3, §22). A skill must be able to change only what
the model is told. It must not change what Chip allows, executes, records or accepts. If an
implementation lets a skill change any of those, the boundary is wrong.

## 2. What exists today (read)

| Concern | Where it lives | What it means for skills |
| --- | --- | --- |
| What the model may say | `chip.work-decision.v1` ([`decision-contract.md`](decision-contract.md)); `ModelDecisionBoundary::interpret` in `chip-core/src/model_decision.rs` is the only reader of a reply. There are four decisions: `request_capability`, `complete`, `escalate`, `block` | The model has **no decision that selects or loads a skill**. Model-driven selection would need a fifth decision or a new capability. Either one is a contract change, and this design rules both out (§4) |
| How the model discovers operations | `ModelDecisionBoundary::question` lists every available capability's id, input names and one line of its Chip-declared description, plus the invocation forms. This is rebuilt from the work's `CapabilitySet` and sent at the start of every request | Capability discovery already exists and is authoritative. A skill must not restate inputs, forms or availability. It may only **name** capability ids |
| What one request carries | `EscalationContextPolicy::build` (`chip-core/src/work.rs`) is a pure function from `WorkState` and `WorkTrajectory` to `EscalationContext`. `render_instructions()` (goal + question) is sent first and does not change across a work's turns, which keeps the prefix cacheable. Production picks `DeduplicatedEscalationContext`, `InspectProgressContext` or `InspectCoveredContext` (`software_work.rs`, `run_software_work_kind`); autoconfig picks through the closed `ContextChoice` enum | **This is the smallest existing boundary for optional instructions.** A policy can add text, cannot execute, and is already measured by `ContextMetrics` |
| Reviewed instructions, already | `chip-experiments/src/plugin.rs`: `Guidance { path, sha256 }` (≤ 16 KiB, digest-pinned, read from the task project, inactive on any mismatch). `GuidedContext` (`guided-v1`) wraps the strategy's policy and appends each text to `context.goal` under a label saying "instructions, not observations". Experiment-only ([`capability-sources.md`](capability-sources.md)) | A native skill is **guidance without a capability source**. The mechanism, labelling, pinning and per-side activation already exist. Only the attachment point is missing: guidance is reachable today only through a `CapabilitySource` |
| Configuration selection | autoconfig catalog (`autoconfig/catalog.rs`): static, versioned `Configuration`s with a `Stage`. A configuration that names `sources` must be `Experimental`. `Policy::work()` provides none | Deterministic, evidence-ranked selection already exists. If skills ever get automatic selection, it belongs here, not in a new selector |
| What the model may read or write in a project | `chip-project/src/policy.rs`: `.git` and `.stack-agent` are **withheld** (unreadable). Agent instruction files (`AGENTS.md`, `CLAUDE.md`, `.claude/`, `.cursor/`, …) are **protected** (readable, never writable) | The model cannot rewrite the files that steer it. A new skill location has to keep that property (§6) |
| Durable task state | FeltDB through `chip-agent-state` ([`agent-state.md`](agent-state.md)) | Skills never write it. Recording which skill a work ran with is a later schema decision (§10) |
| Verification | `pax.test` through `chip-pax`. Completion is decided by `GoalKind` (`software_work.rs`): `change` needs a write followed by a passing `pax.test`; `inspect` needs a grounded answer | A skill's "verification" section is advice. The verdict stays with the `GoalKind`'s rule |
| MCP | `chip-appport-source/src/mcp.rs` (`McpSource`) builds descriptors from a recorded `tools/list` and runs in **replay only**, outside the product crates ([`appport-mcp-m1`](../research/agent-runtime-primitives/experiments/stack-agent/appport-mcp-m1/README.md)) | Consuming MCP tools through Chip's checks already has an experimental path. Skills need none of it (§9) |
| Skills today | [`release-limitations.md`](release-limitations.md): skills are a declared unsupported product surface (`subagents_mcp_skills_memory`), pinned by `tests/release_notices.rs` | Implementing skills later must update that notice and its test. This design does not |

### Mismatches between the requested model and the existing contracts

1. **"Make skill metadata discoverable" cannot mean discoverable *to the model* without a contract
   change.** No decision can act on a catalog. For the first version, the consumer of skill
   metadata is **Chip** (or the operator), not the model. A model-visible catalog is a separate
   future design (§5.3).
2. **Guidance is reachable only through `CapabilitySource`.** A skill uses no capability of its
   own. Wrapping one in a source with an empty fixture would misuse the seam. The implementation
   should attach guidance to the strategy or configuration directly (§10, step 1). It should not
   invent a parallel guidance type.
3. **`Guidance::MAX_BYTES` (16 KiB) is too large for the target models.** The small-model bench
   gives tasks A, B and D a 24,000-byte context budget (`agent_bench.rs`), and the 3B model ran
   with a 16,384-token window ([`small-model-reliability`](small-model-reliability/README.md)).
   16 KiB of guidance would use two thirds of that budget before the first observation. §5.4
   proposes a smaller bound for skills.
4. **AppWorks is not in this repository.** It appears only in research records
   (`rkendel1/appworks`). §8 states the distinction conceptually and depends on no AppWorks API.
5. **The suggested evaluation task (T4, AppPort resolver) is no longer a "previously failed" task
   for the model that was run.** Run 2 solved it 8/8
   ([`run2`](coding-benchmark-v1/runs/2026-10-22-run2-malformed-reply-recovery/README.md)). Also,
   PAX reports pnpm/vitest as `unsupported`, so `pax.test` cannot verify AppPort tasks at all.
   §11 handles this.

## 3. Definitions

| Term | What it is | Who owns it | What it can cause |
| --- | --- | --- | --- |
| **Skill** | Reviewed, versioned guidance for approaching a class of work: when it applies, steps, what to check | the repository's reviewers | text in one escalation context. Nothing else |
| **Capability** | An operation with a declared contract (`CapabilityDescriptor`: id, inputs, effect, idempotency, reuse) | the adapter that performs it, behind Chip's validation | an execution, an observation, a receipt, when Chip authorizes a valid request |
| **Recipe** (AppWorks, future) | A versioned, packaged process with declared inputs, outputs and evidence requirements | AppWorks | out of scope here (§8) |
| **Observation / evidence** | What an execution actually returned, recorded by Chip with its execution id | Chip and Compute/PAX | goal evaluation |

A skill is never an observation, never evidence and never a capability.

## 4. Decisions

| # | Decision | Why (evidence) |
| --- | --- | --- |
| D1 | Skills enter the model's request **only** through an `EscalationContextPolicy` wrapper, the same shape as `GuidedContext` | It is the existing pure boundary for "what the model is told". It cannot execute, it is already measured (`ContextMetrics`), and it already labels guidance as "instructions, not observations" (read, §2) |
| D2 | No new decision, no new capability, no change to `question()` | The decision contract is the one validator of model output (AGENTS.md §4, Invariant 1). A `skill.load` capability would put instructions into the observation channel, which is the evidence channel |
| D3 | **v1 selection is by the operator or a static configuration, never by the model.** Chip checks eligibility deterministically | The model has no decision to act on a catalog (mismatch 1). Non-goal: "automatically selecting skills based on unvalidated model-generated metadata". PR37 found model selection errors common even among capabilities |
| D4 | v1 skills are **built in**: files in this repository, compiled in, registered statically, pinned by digest | This matches every other extension here (`Configuration`, `Strategy`, `CapabilitySource`: "constructed explicitly by the host and registered by value; never loaded at runtime"). Skills read from target repositories are deferred (§6, open question Q2) |
| D5 | The manifest is JSON with unknown fields refused; instructions are a separate Markdown file | `serde_json` and the `deny_unknown_fields` pattern are already used (`Recording`). There is no YAML front-matter parser in the workspace, and none should be added. Keeping metadata and instructions in separate files lets discovery read the metadata without loading the instructions |
| D6 | Skill text is loaded **once per work** and appended to the instructions, so it is constant across turns | This keeps the request prefix stable, so provider prefix caches still apply (decision-contract.md, "What the model is sent": 55–90% cache reuse measured) |
| D7 | A skill that was asked for but cannot be used **refuses the work before any model call**. It is never silently dropped | Invariant 7 (no hidden fallback). The same rule as a configuration naming a source it cannot have |

## 5. The skill contract (`chip.skill.v1`, proposed)

### 5.1 Files

```text
<skill-id>/
  skill.json        discovery metadata: read when choosing and validating
  instructions.md   workflow text: read only after selection
  refs/…            optional read-only reference text: read only if its predicate matches
```

Example: [`native-skills/examples/diagnose-failing-test/`](native-skills/examples/diagnose-failing-test/).

### 5.2 Manifest fields

| Field | Required | Type | Meaning | Used by |
| --- | --- | --- | --- | --- |
| `schema` | yes | `"chip.skill.v1"` | format version. Any other value is refused | validator |
| `id` | yes | string, `^[a-z0-9]+(\.[a-z0-9-]+)+$`, ≤ 64 bytes | stable identifier, never reused for different guidance | discovery, records |
| `version` | yes | integer ≥ 1 | bumped on **any** byte change to the manifest, instructions or references | records |
| `description` | yes | string ≤ 200 bytes | what class of work it is for, for humans choosing it and for reports | discovery |
| `applies_to` | yes | object: `kinds` (non-empty subset of `change`, `verify`, `inspect`), optional `build_systems` (values of autoconfig's `BuildSystem`) | **applicability, as predicates over facts Chip already establishes** (`GoalKind`, `ProjectProfile`). Prose applicability goes in `description`; selection never reads prose | eligibility |
| `capabilities` | yes | array of capability ids, may be empty | the existing capabilities the workflow mentions. **A precondition, not a grant**: the skill is eligible only if every one is offered to this work. Names only: no inputs, forms or effects | eligibility, validator |
| `instructions` | yes | relative path to a `.md` file | the workflow text | loading |
| `references` | no | array of `{ "path", "applies_to" }` | read-only reference text, each loaded only when its own `applies_to` matches (same vocabulary, narrower than the skill's) | loading |

That is the whole format. These were considered and left out:

- **`inputs` / `outputs`.** In v1 the input is the work's goal. The output and its verification are
  whatever the `GoalKind`'s existing completion rule decides. A workflow that needs other declared
  inputs, outputs or evidence requirements is a recipe (§8), not a skill. The fields are reserved,
  and a v1 manifest that carries them is refused.
- **`verification`.** Verification guidance is required, but as prose: `instructions.md` must
  contain a `## Verification` heading, which the validator checks. A structured field would
  duplicate `GoalKind` completion and could be mistaken for a rule Chip enforces.
- **Permissions, effects, scripts, commands, tools, model hints, priority.** These belong to
  capabilities, to Chip, or to nothing (§7).

### 5.3 The four layers the contract separates

| Layer | Source | Seen by the model? | Authority |
| --- | --- | --- | --- |
| Discovery metadata (`skill.json`) | reviewed file, digest-pinned | **no** in v1 | none. Eligibility is checked by Chip |
| Instructions and references | reviewed files, digest-pinned | yes, after selection, labelled "instructions, not observations" | none. They are text in the instructions message |
| Operations | `chip.work-decision.v1` requests, validated against the `CapabilitySet` | (the model writes them) | Chip's existing validation, authorization and execution only |
| Results and verification | observations, receipts, `pax.test`, `GoalKind` completion, the evaluator | observations only | Chip, Compute and PAX. A skill cannot add, remove or reinterpret any of them |

### 5.4 Bounds (proposed)

- `instructions.md` plus all loaded references: **≤ 4,096 bytes of UTF-8**, checked at
  registration. That is about 1,000 tokens and about 17% of the small-model bench's 24,000-byte
  budget. It is a starting bound to measure (§11), not a tuned value.
- At most **one skill per work** in v1. Composing skills makes it impossible to attribute an effect
  to one of them (AGENTS.md §12).

### 5.5 Versioning

- `schema` versions the format. A reader accepts exactly the schemas it knows. There is no
  forward-compatibility guessing.
- `version` versions the guidance. The registry pins `sha256` over the manifest, the instructions
  and every reference, in a fixed order. A test fails if the bytes change without a version bump:
  the digest is recorded next to the version, so the build compares them.
- Records name a skill as `id@version` plus the digest, exactly as configurations are named
  (`Configuration::key`).

### 5.6 Invalid or ambiguous definitions

Built-in skills are validated by a test at build time, so an invalid skill fails CI and never
ships. The run-time checks below apply when a work asks for a skill.

| Condition | When detected | Result |
| --- | --- | --- |
| Not JSON, wrong types, missing required field | registration (test) | refused; build fails |
| Unknown field, including the reserved `inputs`/`outputs` | registration | refused. It is never ignored, because an ignored field reads as a promise |
| `schema` not `chip.skill.v1` | registration | refused |
| Duplicate `id` (any version) | registration | refused. Only one version of an id is registered at a time |
| `instructions` or a reference missing, not UTF-8, outside the skill directory (`..`, absolute path, symlink) | registration | refused |
| Size over the bound | registration | refused |
| No `## Verification` heading | registration | refused |
| A capability id that no production capability declares | registration (checked against the declared set, like the name list `schema-closed` is pinned to) | refused |
| A capability id that is real but **not offered to this work** (narrowed by `--kind`, overrides or a configuration) | work start | work refused before any model call, with the missing ids (D7) |
| `applies_to` does not match the work's kind or profile | work start | work refused before any model call (D7). Under future automatic selection: not selected, and the reason recorded |
| Digest mismatch at load | work start | work refused. Only the pinned bytes ever reach a model |
| Instructions that claim success, contain commands, or tell the model to skip verification | review (human) | not detectable mechanically in general. Harmless by construction: claims are not evidence, commands are not capabilities, and the `GoalKind` rule decides completion (§7) |

## 6. Progressive disclosure (proposed)

| Step | v1 | Who decides | Context cost |
| --- | --- | --- | --- |
| 1. Eligible metadata is discoverable | the static registry; `stack-agent experiment` can list skills | Chip, operator | **0 bytes** to the model |
| 2. A skill is selected | named by the operator in an experiment strategy or an `Experimental` catalog configuration; eligibility checked by Chip (`applies_to`, `capabilities`, digest) | operator, Chip | 0 |
| 3. Its instructions and matching references are loaded | once, at work start, by the context wrapper | Chip | the skill's bytes, **once in the cached prefix**, then reused on every turn |
| 4. Operations run | through `chip.work-decision.v1`, as now | model proposes, Chip validates and executes | unchanged |
| 5. Results are evaluated | `GoalKind` completion, PAX, the evaluator, as now | Chip, PAX, evaluator | unchanged |

**Token implications.** Without skills nothing changes. With one skill, every request in the work
carries its bytes in the instructions message. On a provider with a prefix cache, those bytes are
processed once and reused, since they sit before the observations. On a provider without one, they
cost the same on every turn. The run-2 tasks used about 0.27 to 1.9 M input tokens. A 4 KiB skill
over 19 to 35 calls adds roughly 20 to 40 k tokens before caching. That has to be measured, not
assumed.

**What must be measured** (§11): instruction bytes (`ContextMetrics.bytes` already counts them),
cached versus uncached input tokens, and whether the skill changes the number of turns enough to
pay for itself. More instructions do not necessarily help. The 3B model in
[`small-model-reliability`](small-model-reliability/README.md) repeated a refused answer word for
word even after being told why.

**Later disclosure options, not in v1, in the order they should be considered:**

1. *Chip-deterministic selection.* Add `skills` to autoconfig `Configuration`, `Experimental` stage
   only, chosen by `resolve::decide` from `applies_to` and recorded verified outcomes. This needs no
   model involvement and no contract change.
2. *Model-visible catalog.* Listing skill ids and descriptions to the model needs a way for it to
   choose: a new decision or capability, a new validator and a §14 case. It costs catalog bytes on
   every request, and selection is exactly where PR37 measured frequent errors. It is a separate
   design.

**Where skill files live.** v1 skills are built in (D4). Project-local skills (in the target
repository) are deferred, for three reasons:

- target repositories are untrusted text that would steer the model;
- a host-side digest pin is needed so the repository cannot change the guidance the operator
  approved;
- the location must be withheld or protected from the model's writes. `.stack-agent/` is withheld
  but is Stack-Agent's record directory. `.claude/` is protected but belongs to another tool. A new
  directory would need a new `policy.rs` rule.

See Q2.

## 7. Security and package boundaries

| Rule | How it holds |
| --- | --- |
| Markdown is inert data | The loader reads bytes and appends them to a string. Nothing parses it for actions. The only structural check is the `## Verification` heading |
| References are read-only inputs | Read once from the registry's pinned bytes. No capability writes them. Built-in skills are not in any target project |
| No embedded commands are executed | There is no code path from skill text to `Executor`. A model that copies a command from a skill into a request still goes through `interpret` and the `CapabilitySet`; there is no shell capability, so it is refused (`release-limitations.md`: `shell_not_executed`) |
| No skill-provided scripts run | v1 has no executable asset type. Any file other than the manifest, the instructions and declared `.md` references is refused at registration |
| No implicit filesystem, network or process permissions | A skill adds no backend to the `CapabilitySet`. Its `capabilities` field can only make the skill **ineligible**; it can never make a capability available |
| No bypass of authority checks | Validation, confinement, protected paths, write-conflict detection, limits and the stuck guard are untouched |
| No change to durable task state | The wrapper is a pure `EscalationContextPolicy`. It has no handle to FeltDB, the journal or the evidence store |
| Skill completion claims are not outcomes | A skill that says "the task is done when…" is text. `complete` is still checked against observations, and `change`/`verify` still need `pax.test` (`GoalKind`) |
| Prompt-injection surface | Built-in, reviewed and pinned in v1. The label "instructions, not observations" separates skill text from evidence in the request |
| Telemetry | Skill text is never exported. Only `id@version`, the digest and the byte count, in keeping with the telemetry allowlist ([`telemetry.md`](telemetry.md)) |

**Executable skill assets** (scripts, binaries, WASM) are a separate future design. They would have
to be capabilities: a declared `CapabilityDescriptor` with `Effect`, confinement, Chip validation,
execution evidence and receipts. They would not be part of a skill.

## 8. Skills, recipes and capabilities

- A **capability** is what can be done, under authority.
- A **skill** is advice on how to approach a class of work with the capabilities that are offered.
  It has no inputs, outputs or evidence rules of its own.
- A **recipe** (AppWorks) is a packaged process whose inputs, outputs and evidence requirements are
  declared and checked.

A future recipe may reference a skill by `id@version` as guidance for its model-driven steps. The
recipe would still own its inputs, outputs and evidence. This design changes nothing in AppWorks
and defines no recipe format. If a proposed skill field starts to look like an input schema, an
output contract or an evidence requirement, it belongs to the recipe abstraction, and the v1
validator refuses it (§5.2).

## 9. MCP, a separate future boundary

MCP is not a prerequisite for native skills and is not added here.

| Direction | Today (read) | Future adapter (proposed boundary) |
| --- | --- | --- |
| Consume external MCP tools | `chip_appport_source::McpSource`: descriptors from `tools/list`, only tools whose authority metadata lies within the operator's grant, replay only, outside product crates | the same rule in general: each MCP tool maps to one canonical `CapabilityId` with a declared `Effect` and confinement, offered only within an operator grant, through the existing validation. A live mode is new authority and needs its own §14 case |
| Expose Stack-Agent capabilities to MCP clients | nothing | an adapter outside Chip that translates MCP `tools/call` into Chip-validated requests. The MCP client is then one more untrusted proposer |
| Skills over MCP | nothing | MCP prompts or resources could carry skill text to other clients. Build a dedicated MCP skill server only if a real client needs it |

No MCP dependency, server, transport or execution path is part of this change.

## 10. Implementation sketch (for the follow-up, not this change)

Each step is its own reviewed change:

1. Let a `Strategy` (or `Experimental` configuration) carry guidance without a `CapabilitySource`,
   reusing `GuidedContext` and its label. Add `skills()` beside `sources()`.
2. Add a `chip.skill.v1` loader and validator with a registry test covering every row of §5.6, and
   register the example skill.
3. Record `id@version`, the digest and the bytes in the experiment `RunRecord`, like
   `SourceActivation`. Whether FeltDB records it for `work` waits until skills reach `work`.
4. Only after §11 shows an effect: decide whether `work` gets an opt-in flag. That would also update
   `release-limitations.md` and `tests/release_notices.rs`.

## 11. Evaluation plan (follow-up, not run)

**Hypothesis.** With one skill's instructions given at work start, a fixed model solves a fixed
task more often, or with fewer turns to the first useful edit, than without it. Chip's boundary
measurements are unchanged.

**Falsified if** paired trials show no difference in solve rate or turns within their uncertainty,
or the extra context cost outweighs any gain. **Stop immediately** if either arm shows an
unauthorized execution, a false completion, or a completion claim the evaluator contradicts.

**Arms** (same binary, built with the skill code, so the runtime is constant):

| Arm | What the model is told |
| --- | --- |
| Baseline | exactly what it is told today |
| Treatment | the same, plus one skill's instructions |
| Length control (recommended) | the same, plus equally long, task-irrelevant reviewed text, labelled the same way. It separates "more instructions" from "this workflow" |

**Held constant:** model and every generation setting (`--provider-profile`, structured output,
temperature, output cap, call timeout); task brief (`tasks/Tn.goal.txt`); `prepare-checkout.sh` and
the frozen base commit; per-task data and cache directories; limits (`--max-turns 50
--max-executions 50 --deadline-secs 1200`, as in runs 1 and 2); `run-acceptance.sh` and the hidden
tests. Arms run interleaved (ABC, BCA, …) on the same machine to spread drift.

**Task choice (open, Q4).** The example skill targets Rust with `pax.test`.

- **T1** (Compute, Rust) is the one coding-benchmark task run 2 did not solve (3/4 acceptance), and
  it is PAX-verifiable.
- T4, the suggested resolver task, was solved 8/8 in run 2, and PAX cannot verify AppPort.
- For a small local model, no coding-benchmark run exists yet. A baseline must be recorded first.

The small-model bench's task E is a cheaper, closely matched secondary task: the 3B model wrote the
correct change but never ran `pax.test`, which is exactly the step this skill covers.

**Record per trial:**

- hidden-acceptance verdict and regression suite (the primary outcome, from the evaluator);
- Stack-Agent outcome and `Verified`;
- turns and seconds to the first write to a file the evaluator's reference diff touches (computed
  afterwards by the evaluator, never shown to the agent);
- model calls, executions by capability, repeated identical requests and stuck-guard stops;
- rejected decisions;
- whether a completion claim agrees with the evaluator;
- skill bytes, input tokens (cached and uncached) and output tokens;
- latency and cost.

Failures stay in the denominator.

**Analysis.** Pre-register before any run. Solve rate: paired by trial index, exact McNemar test on
discordant pairs, with Clopper–Pearson intervals per arm. Turns and tokens: paired differences with
bootstrap 95% intervals. With 10 pairs only a large effect is detectable. Report that limit rather
than claim a trend, and add pairs only under the pre-registered stopping rule. Model and
decision-mode effects are kept separate by changing nothing but the arm.

**Success criteria** (all required):

1. Zero unauthorized executions and zero false completions in every arm.
2. Treatment solve rate above baseline with the exact test's p < 0.05, **or** median turns to the
   first useful edit lower with the bootstrap interval excluding 0.
3. Treatment beats the length control on the same measure. Otherwise the effect is attributed to
   instruction length, not the workflow.
4. Added input tokens are reported and judged against the gain.

One successful run is never reported as a benefit.

## 12. Open questions

- **Q1.** Should v1 skills reach `stack-agent work` at all before §11 produces evidence?
  Proposed: no, experiments only (§10, step 4).
- **Q2.** Project-local skills. Which directory should they use? Should it be withheld (host-only)
  or protected (model-readable)? How does the operator pin their digest, for example
  `--skill path@sha256`?
- **Q3.** Is 4,096 bytes the right bound, and should it scale with `--context-budget-bytes`?
- **Q4.** Which evaluation task and model: T1 with run 2's model, a small local model on T1 or T3
  (both need a recorded baseline first), or task E as the cheap first test?
- **Q5.** Should a skill be able to target `inspect` (answer and citation rules), where the 3B model
  failed by not citing a file? This design allows it. Whether guidance helps there is untested.
- **Q6.** Does the experiment layer need real-model support first? Today the coding benchmark runs
  `stack-agent work` directly, which cannot select an experimental configuration. The treatment arm
  therefore needs either an experiment harness for real models or a narrowly scoped opt-in on
  `work`, and that choice is itself a reviewable boundary change.
