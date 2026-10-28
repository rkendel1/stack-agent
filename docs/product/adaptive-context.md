# Adaptive agent context: Light, Standard, Deep and Recovery

What one model request of a `work` carries is chosen by an *agent context policy*, not by the
model or the provider. This page describes the context assembler `adaptive-v1`, its four profiles,
how a profile is selected, how each request records the choice, how it interacts with prompt
caching, the related completion-status and objective-limit fixes, and the controlled comparison
that has to be run before anything is claimed about cost or quality. Nothing here claims a saving.

## The path a request takes

| Step | Where | What happens |
|---|---|---|
| Command line | `crates/stack-agent/src/software_work.rs` `run` (the `work` argument loop) | Flags and variables are resolved: generation (`work_generation.rs` `WorkGeneration::resolve`), the context profile (`adaptive_context.rs` `ContextMode::resolve`, into `Tuning::context`), durable state (`--state-db`, `--task-id`, `--no-state`). |
| Durable admission and start | `software_work.rs` `run` → `chip_agent_state::ensure_admitted` (`crates/chip-agent-state/src/admission.rs`), `chip_agent_state::begin` (`lib.rs`) | The task is admitted (`chip.Admission` holds the whole goal) and created `ready` with its objective (`work_state::objective_for`), then taken over (`running`). On a continuation, unresolved intents are reconciled; observation-only requests are performed again (`DurableWork::reobserve`). |
| Run | `software_work.rs` `WorkRuntime::run_durable_with_policy` → `run_software_work_durable_prompt` | Builds the agent (capabilities, the durable executor `DurableWork::executor`, the observer), the `WorkSpec` (limits, stuck guard, requirements, restored observations, settled actions, a restored response, recalled episodes) and picks the context policy. |
| Decision loop | `crates/chip-core/src/work.rs` `Agent::run_work_with_context_policy` → `drive` | Each turn: the local policy may decide (`CompleteWhenVerified` completes the moment `VerifiedChange` holds); otherwise one model escalation (`Run::escalate`). |
| Context assembly | `work.rs` `Run::call_model` → `Run::context` → `EscalationContextPolicy::assemble` | The policy builds an `EscalationContext` from `WorkState`, `WorkTrajectory` and `WorkSignals`. The loop checks every condensation (`Run::check_condensation`), measures the request (`ContextMetrics`), enforces the byte budget, and records `ModelEscalation` with the policy id and, for `adaptive-v1`, the `ProfileSelection`. |
| Messages | `crates/chip-core/src/lib.rs` `Agent::model_turn` | One system message with the instructions (`EscalationContext::render_instructions`: goal, authorization rules, capability list, reply contract), one system message per observation (`Observation::render`, `observation.rs`), one user message last (`render_turn`: state, evidence, prior decisions, ruled out, omitted, condensed, next step). |
| FX and wire | `crates/chip-fx/src/mapping.rs`; `crates/fx-native-provider/src/anthropic.rs`, `openai.rs` | Anthropic: every system message goes into the top-level `system` (one block per message with `cache_control` on the last when `--prompt-cache on`). OpenAI-compatible: consecutive system messages are joined into one. Usage comes back field for field. |
| Measurement | `crates/stack-agent/src/token_usage.rs` `Recording` | Wraps the provider outermost: one `CallRecord` per call (sizes by role, resent bytes, timing, usage). |
| Interpretation and execution | `work.rs` `Run::escalate` (boundary `ModelDecisionBoundary`), `Run::perform` | The reply is validated strictly; a rejected decision is answered with its diagnostic at the next escalation. A request is authorized, given a Chip-assigned execution id, executed once through the (durable) executor, observed, evaluated against the requirements (`Run::assess_goal`) and appended to the trajectory. |
| Durable record | `crates/chip-agent-state/src/lib.rs` `DurableWork::executor`, `events.rs` | An intent before, an outcome after each execution; every `WorkEvent` as a `chip.WorkEvent` record (the `ModelEscalation` payload now carries `context_profile`). |
| Completion | `work.rs` `drive` (a `Complete` the observations do not support is refused and rejected), `software_work.rs` `GoalKind::verified` (`VerifiedChange`, `VerifiedState`), `SoftwareWork::exit_status`, `work_state::verification` | Terminal state is the loop's outcome; `verified` is an independent predicate over the recorded observations; only it authorizes exit 0. |
| Reports | `software_work.rs` `render_json` (`context`, `completion`, `tuning`), `token_usage.rs` `work_section` (`token_usage`), `token_report.rs` `analyze` (`stack-agent token-report`) | Per call: the context the call was assembled as, and what the provider reported. |

### Why observations are system messages

This is intentional, not a defect. Chip has no native tool-calling: the model replies with one JSON
decision object, and Chip, not the provider, executes the request. The FX message contract has three
roles (`chip_inference::MessageRole`: system, user, assistant) and no tool role. An observation is
the runtime's authoritative record of an execution Chip performed, rendered deterministically and
quoted so its output cannot alter its kind, status or identifiers (`Observation::render`). It is not
something the user said and not something the model said, so it is neither a user nor an assistant
message; the system role is the one that carries runtime-owned context. The order (instructions,
then observations, then the per-turn user message) also makes each request a byte-for-byte
extension of the previous one, which is what a provider prefix cache matches
(`docs/product/small-model-reliability/README.md`, "What changed", 1). Roles were not changed.

## The profiles

`adaptive-v1` (`crates/stack-agent/src/adaptive_context.rs`) is used for change, verify and act work.
An inspection keeps its own bounded policy (`inspect-bounded-v1`); an automatic configuration
(`--auto`) keeps the policy it selects.

**In every profile, unchanged:** the instructions system message (goal, authorization rules,
capability list, structured-output reply contract), the work's state line and bounds exactly as
the legacy request has them (no per-run id is added: it would make two works' first requests differ
where nothing about the work does, which `--no-recall` promises not to; the execution ids carry the
work id, and Recovery names it), the loop's own evidence first (a rejection diagnostic, recalled
episodes, the goal-progress note), every failed observation in full, every test run in full, the
prior decisions, everything ruled out (failures, repeats, settled actions), and every observation not
sent in full, listed under `Condensed` with its execution id, the bytes withheld and what it
establishes. A condensed observation stays recorded in full in the work report, FeltDB and the
audit. Requesting the same capability again returns its *current* answer: for a read superseded by a
later, different answer that is not the withheld text, and the request says so. The loop refuses to send a request whose condensation
names an observation that is not one recorded, successful execution, or that is also sent in full
(`Run::check_condensation`). No profile truncates history to reach a size target.

| Profile | Observations in full | Added to the user message |
|---|---|---|
| Light | the latest two, every test run, every failure | profile line; task state (files changed as a count, last test run and when, requirements met, failures, what is outstanding) |
| Standard | all, except a read-only observation (`project.read`, `project.search`, `project.list`, `project.git.*`, `project.observe`) whose same request was answered again later with a different result: the earlier answer no longer describes the project, and it is condensed with "superseded by execution X" | profile line; task state: files changed with the execution that last changed each, the last test run and whether it came after the last change, the last test run before any change, requirements met, failed executions, each outstanding requirement as `open:` |
| Deep | as Standard | Standard, plus `attempts` (every change and the test run that followed it), `conflicts` (the same request answered differently with no change by this work between), `hypotheses` (the model's own refused completion claims, labelled as claims, never evidence; each over 400 bytes is cut with the number of bytes left in the recorded decision), `unresolved` (what verification still requires, as Chip evaluates it) |
| Recovery | as Standard | Standard, plus `recovery` (the work was resumed: restored and re-observed executions, settled actions, a recorded response, last verified progress, the uncertainty rule), `attempts`, `unresolved` |

Verified facts (`verified:` lines, Chip's evaluation of observations), open items (`open:`) and
hypotheses (the model's words) are written apart. A fact Chip has not established is never written
as verified: a work with no test run says `no test run yet`; a test run before the last change says
it does not describe the project now.

## Selection

`adaptive_context::select` is a pure function of the trajectory, the work's kind and `WorkSignals`
(`chip-core`: the work id; restored, re-observed and settled counts and whether a recorded response
was offered; rejected decisions in a row; refused completions; requirements and how many are met;
each unmet requirement's description; the previous profile). No model is asked and nothing is
learned. The first rule that holds wins:

1. **Recovery**: the work was resumed from durable state (restored observations, settled actions or
   a recorded response) and has recorded no observation of its own yet.
2. **Deep**, in this order: conflicting evidence that is still current (the later answer is the
   latest to that request and no change followed; for two test runs, only a different PAX result,
   line one of the observation, is a conflict: their diagnostics, timings and fingerprint differ
   between runs that established the same thing); the last two or more test runs after a change did
   not pass; the model's last decision was a completion Chip refused; the last two or more
   executions failed; late in the turn budget (two thirds of the turns) with requirements
   outstanding after a change.
3. **Light**: Chip's evaluation finds every requirement met (the next decision is completing), or
   verify work that has observed no failure and has no rejection pending.
4. **Standard** otherwise; when the previous request was another profile, the reason says it
   de-escalated and from which.

Escalation and de-escalation follow from the signals: a passing test run after a further change
ends "repeated failed attempts"; a change after a conflicting pair makes it no longer current; the
first observation of a resumed run ends Recovery. A malformed reply does not change the profile
(more context does not fix a reply format). Every request records the profile, the rule's reason,
the previous profile and the sections added (`WorkEvent::ModelEscalation.profile`).

## Switch

```
stack-agent work ... --context-profile auto|light|standard|deep|recovery|replay
CHIP_CONTEXT_PROFILE=auto|light|standard|deep|recovery|replay   (the flag wins)
```

`auto` is the default. A profile name fixes every request to that profile (for ablation).
`replay` sends the legacy request, `dedup-v1`, byte for byte as before this change. The report's
`tuning.context_profile` says which was in effect.

## Prompt caching

`--prompt-cache` (Anthropic, on by default) puts a cache breakpoint on the last system block.
Standard, Deep and Recovery send the same observations, so switching between them changes only the
user message, which is after the breakpoint: the cached prefix survives. Two things change the
prefix and make the next request write the cache again from that point:

* Standard condensing a superseded read: the earlier observation leaves the system blocks once,
  when the later read arrives, and stays out (the rule is monotone).
* Light: the observations it leaves out depend on recency, so entering, staying in and leaving Light
  move the prefix. Light is selected only for routine decisions, which are few.

Cache writes are billed above ordinary input and reads below it, so the net effect is only what the
provider's `cache_read_tokens` and `cache_write_tokens` show in a controlled run (below). The prompt
cache setting and the context profile are separate settings and are recorded separately
(`generation.prompt_cache`, `tuning.context_profile`).

## What each call records

* `WorkEvent::ModelEscalation`: `context_policy` (`adaptive-v1` or the legacy id), `context`
  (`ContextMetrics`: bytes, messages by role, observations in full, condensed, omitted, evidence
  items) and `profile` (`ProfileSelection`: profile, reason, previous, sections).
* FeltDB `chip.WorkEvent`: the `ModelEscalation` payload's `context_profile`.
* `WorkReport::context_assembly`: assembly time per request; `WorkLatency::context_assembly`, total.
* `token_usage.calls[].context` (`work --json`): policy, profile, reason, previous profile,
  changed, sections, observations in full / condensed / omitted as repeats, evidence items, request
  bytes, assembly time, beside the call's provider-reported input, output and cache tokens, sizes
  by role and resent bytes. `token_usage.context_profiles`: calls per profile and every change.
* `stack-agent token-report`: `context_profiles.by_profile` (calls and reported tokens per profile)
  and a per-call `context_profile` in the series.
* `completion` (`work --json`): see below.

## Completion, verification and escalation

The work report keeps apart: what the model last decided (`model_terminal_decision`), how many of
its completions Chip refused, whether a candidate change exists, the tests (runs, the last run and
whether it came after the last change, the last run before any change), whether Chip verified the
acceptance criteria (`acceptance_verified_by_runtime`, the same `verified`), whether test evidence
was invalidated, and `independent_evaluation: not_available_to_runtime`. The human report has the
same as one `Completion:` line. Nothing about `terminal_state`, `verified` or the exit status
changed: these are additive fields of `stack-agent.work-report.v1`.

What the benchmark showed, from the T4 Opus run's report: the work changed
`packages/core/src/resolver.ts` and added regression tests; the last whole-suite `pax.test` after the
last change reported `failed` with 1 failed and 1,666 passed (6 failed before the change); the model
escalated, saying it could not tell whether the remaining failure predates its change because the
PAX result names no failing test. `verified: false` and `escalated` were therefore correct under the
runtime's contract (a change is verified only by a passing run after it), and the evaluator's PASS
measured something the runtime cannot see (hidden tests and a regression suite that excludes the
pre-existing failure). The canonical PAX observation carries counts only (`chip_pax`
`canonical_json`), by design. The fix is the `completion` section and the task-state lines, which
keep the change and the before/after test evidence visible instead of erasing it under
`escalated`; giving the runtime a way to establish pre-existing failures (a baseline test run on the
starting tree) would be a feature and is a follow-up. The kill-and-relaunch case was not analysed:
its run data was not available to this change.

## Goals over FeltDB's objective limit

See `agent-state.md`, "Goals longer than the objective limit": `--state-db` and `--task-id` runs
used to send the whole goal as the task objective, which FeltDB 0.3's agent-transition contract
refuses over 1,024 bytes (`CONTRACT_INPUT_INVALID`, the T4 failure). The objective is now the goal
when it fits, and otherwise its beginning plus `[goal sha256:<digest>, <n> bytes; the whole goal is
in chip.Admission]`; the whole goal is in the admission record.

## Controlled comparison (A/B plan)

Nothing above is a measured saving. The comparison that would establish one:

**Held fixed.** Agent Obstacle Course version; course and obstacle; starting repository commit;
model and provider; generation settings (temperature, output cap, structured-output mode); turn,
execution, deadline and token budgets; evaluator and hidden tests; the Stack-Agent revision (this
branch's head) and its `pax` binary; durable-state flags. The runs differ in the context setting
only.

**Arms.**

1. Baseline: `--context-profile replay` (the legacy request, `dedup-v1`).
2. Adaptive: `--context-profile auto`.
3. Optional ablations: `--context-profile standard`, `deep`.

**Prompt cache.** Hold `--prompt-cache` fixed across arms (run the main comparison with `on`, the
default for Anthropic, which is what a user gets). To separate the two effects, cross them: replay/off,
replay/on, auto/off, auto/on. `generation.prompt_cache` and `tuning.context_profile` in each report
identify the cell; label runs with `CHIP_RUN_LABELS=arm=...,cache=...,course=...,obstacle=...,rep=N`.

**Order.** Course-000 first (calibration, transient test failure, hard kill and relaunch), then
T4. The existing benchmark results remain the historical baseline; the replay
arm is re-run on the same revision so that runtime changes other than the context policy are held
fixed. Repeat each cell (at least 5 runs per cell for course-000, 3 for T4) and report every run.

**Measured per run** (from `work --json` and `stack-agent token-report --json`): hidden-test and
regression results from the evaluator; terminal state, `verified`, and `completion` against the
evaluator's verdict (completion-status accuracy); recovery behaviour on the kill/relaunch obstacle
(resumed under Recovery, no repeated effects, outcome); provider-reported input, output, cache read
and cache write tokens (totals and per call, by profile); model calls, executions, rejected replies;
wall-clock time, with model time, tool time and lease wait reported separately (lease wait from the
time between the kill and the relaunch's `begin`); provider cost where the provider reports one. No
token or cost figure is estimated from bytes.

**Reading it.** An arm is better only where the evaluator's correctness is not worse; differences
in tokens are reported with their spread across repetitions, per cache setting.

## Known limitations and follow-ups

* Light is selected only in narrow, explicitly routine cases; most change work runs Standard and
  Deep. Standard condenses only superseded read-only observations, so on a run that never re-reads a
  file it sends what `dedup-v1` sends plus the task-state section.
* Standard decides that a read is superseded only by the same request (capability and complete
  inputs, so another range or path never counts) answered later, successfully, with a different
  result. It does not ask why the answer changed. Almost always the reason is this work's own write,
  so the read condensed is the file *before* the change, and nothing else in the request carries
  that text (a write's observation names the path, not the content). Replay sends both. The current
  state is never misrepresented, but the earlier content is gone from the model's view; whether that
  costs quality is a question for the controlled comparison, not something this design settles.
* Deep and Recovery add text to the user message; their benefit, if any, is a quality question for
  the controlled comparison.
* The selection thresholds (two failed attempts, two failed executions, two thirds of the turns)
  are fixed constants, not tuned.
* `CHIP_CONTEXT_PROFILE` is read by `work`; `serve` uses the default (`auto`).
* Inspections are not under `adaptive-v1`.
* Lease expiry: course-000 interruption runs waited about six minutes for the previous attempt's
  lease to expire before the relaunch could take the task over. Out of scope here; a follow-up for
  the lease and scheduling design.
* A baseline test run (failures that predate the work) is not available to the runtime; see above.
