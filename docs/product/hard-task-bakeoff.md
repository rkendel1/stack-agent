# Hard-task bake-off: does the runtime make a weak model reliably finish hard repository work?

The question:

> Can we make a small or weak model reliably complete difficult, real repository tasks by
> combining better context, deterministic control, durable state, memory, reusable workflows,
> execution, and verification?

Primary metric: **independently verified task completion within a fixed budget**. A model's claim
of completion never counts. Every number below is regenerated from the run records in
`docs/product/hard-task-bakeoff/runs/` by `python3 -m bakeoff report` (see §12); nothing was
edited by hand. The generated tables are in `docs/product/hard-task-bakeoff/report/`.

## 1. Short answer

**Not by combining components, on this evidence.** What the runtime contributes is *safety and
honest stopping*, not *more completed work* from a weak model:

* **No component raised verified success for either weak model.** `gpt-5.4-nano` verified 23 of
  59 development runs with the baseline and 17 of 59 with targeted context; Gemma 4 (4B active)
  verified 4 of 20 at baseline and no more with any context, prompt or output setting. The compact
  prompt broke Gemma outright (18 of 20 runs ended on malformed output). A strong model on the same
  tasks and budgets verified 26 of 28 (`gpt-5.5`), so the tasks are solvable: the gap is the model.
* **For a capable small model (`claude-haiku-5-5`) the suite is near its ceiling** (55 of 60
  development, 31 of 35 held out at baseline). There, components change *efficiency*, not
  success: the full stack and recall + workflow execution reached the baseline's held-out success
  with about 21% fewer input tokens per verified run, but on the development tasks they produced
  more false completions than the baseline (5 and 3 of 60 against 1), so by the rules fixed
  before stages 3 and 4 **no configuration is reliable or efficient**: the best-supported
  configuration remains **the baseline**.
* **The runtime never let a model manufacture success.** Across 970 valid real-model runs (and
  every scripted one) no run was verified on a model's say-so: 19 runs made a completion claim the
  host refused at the time. When the
  host *did* verify and the hidden acceptance tests disagreed (a "false completion"), the cause
  was always the same: the project's own tests did not cover what the task required. Host
  verification is only as strong as the project's tests.
* **Durable state closes the old crash gaps, with two exceptions.** At every one of five injected
  kill points, durable resume repeated no side effect; a restart without state repeated the change
  (deterministic model) or could not verify the work it found (real models). Resumed works reached
  verified success when the kill came before an action, after its evidence, or during a test run,
  but **never** when a test run's receipt or the final completion was lost: Stack-Agent then
  refuses to run the tests again, so the attempt stops safely with a correct, unverified change.
  That is the next thing to fix.

The rest of this document is the method (§2-3), what happened during the runs (§4), the results by
stage (§5-8), the verdicts (§9), what failed and what to do next (§10-11), and how to reproduce
everything (§12).

## 2. What was built

| Part | Where | What it does |
| --- | --- | --- |
| Experiment registry | `bench/registry/experiments/*.json` (59 entries) | Every earlier experiment, as its own documents report it: setup, tasks, models, configuration, results (quoted), artifacts, limitations, how to reproduce, and how far it is comparable. Requested evidence the repository does not hold has an explicit `not-found` entry (XERJ retrieval, speculative decoding, tool selection as a named metric). `registry validate` checks every cited path; `registry export` writes JSONL. |
| Hard-task suite v1 | `bench/hard-tasks/v1` | 32 tasks (20 development, 12 held out) over six multi-module Rust fixtures. Each task fixes, before any run: the starting repository (fixture + defect overlay, committed with a fixed author and date: a deterministic revision) and its digest, the goal, hidden acceptance tests, a reference fix, budgets (turns, executions, deadline), the allowed capabilities and paths, protected paths, expected failure modes, safety constraints and a machine-checkable verdict (`change`, `refusal`, `safe_stop`). `validation.json` freezes that every acceptance suite fails at the start and passes with the reference fix, with no visible regression. |
| Harness | `bench/bakeoff` (Python, standard library only) | Runs `stack-agent work` itself (the real CLI and lifecycle) in a fresh checkout outside this repository, so the agent can never read the hidden tests. Owns setup, history seeding, timeouts, cancellation, process groups, fault injection, resumption, operator settlement and independent verification. One JSONL record per run. |
| Model proxy | `bakeoff/proxy.py` | The only endpoint `stack-agent` talks to. Holds the provider key (the agent gets a placeholder), paces requests, retries rate limits (counted, and classified as infrastructure when exhausted), and records per call: timing, status, provider usage, a summary of the decision (type, capability, input names, a project-relative path, a content digest) and whether recalled episodes or workflow guidance were in the request. No prompt or reply content is stored. |
| PAX wrapper | `bakeoff/paxwrap.py` (through `PAX_BIN`) | Runs the real PAX and logs every invocation; for faults, holds a test run before it starts, holds it after it finished with the result unreported, or signals the moment it exited. Its log is the ground truth the operator settlement uses. |
| Independent verifier | `bakeoff/verify.py` | Copies the final tree, adds the hidden acceptance tests to the copy, runs every test target, and compares with the frozen validation: acceptance, regressions (visible tests that passed at the start and no longer do), the task's own failing tests, and scope (allowed and protected paths). |
| Classifier and report | `bakeoff/classify.py`, `bakeoff/report.py` | One outcome and one primary failure class per run, from the harness's evidence only; tables, paired differences, lifecycle and verdicts, regenerated from the records (`report`, `export`). |
| Controls | `bakeoff/controls.py` | Scripted decisions with a known correct verdict: the reference fix (must succeed), a completion claim with no work, malformed output, a spinning loop, a wrong fix, vandalism of a protected file, weakening a visible test. 140 of 140 controls scored as built on the full suite. |

## 3. Method

**Primary metric: verified success.** A change task succeeds only if the work ended verified by the
host (exit 0, `verified: true`) **and** the hidden acceptance tests pass **and** no visible test that
passed at the start fails **and** the task's own failing tests pass **and** nothing outside the
allowed paths or inside the protected paths changed **and** no capability outside the task's
allowance executed **and** the runtime's audit is clean **and** no change was repeated by a later
attempt or process. A refusal task succeeds if the work changed nothing, claimed nothing and broke no
safety rule; a safe-stop task (the verifier is missing) if it claimed no verification and broke no
safety rule. A model's `complete` is recorded and never counted.

**Secondary metrics**, per run: false completions (the host verified, the hidden acceptance did not),
completion claims the host did not verify, regressions, safety violations (scope, protected paths,
unauthorized capabilities, audit, duplicate side effects), budget exhaustion (from the runtime's
structured outcome: a limit, the deadline, the stuck guard, or the harness's wall clock; never from
a model's words), malformed replies, interruptions, resumptions, uncertain outcomes and operator
settlements, repeats of a known-failed change, redundant writes, model calls, provider-reported
tokens, executions, test runs, model latency, pacing and wall time. Cost: neither provider reports it
in usage; tokens are reported instead.

**Infrastructure failures** (the provider unreachable, rate limits that outlasted the retries, a
harness error) are counted and excluded from every rate.

**Models.** `gpt-5.4-nano` (OpenAI's smallest current model; the primary weak model), `gemma-4-26b-a4b`
(open weights, about 4B active parameters, through the existing AI Gateway, paced to its 5 requests
per minute), `gpt-5.5` (a strong reference, to separate what the runtime enables from what the model
contributes), and `scripted` (deterministic decisions; controls, history seeds and the lifecycle
experiment). Every model uses the existing `--provider-profile openai`, `--max-output-tokens 8192`.
No local GPU server or new paid service was used: the scripted runs and CI are fully local; the real
models are opt-in through providers already configured for this repository.

**Configurations** (`bench/bakeoff/configs/configurations.json`), each an existing supported setting
or a harness context mode:

| Component | Values | How |
| --- | --- | --- |
| context | brief / targeted / diagnosis | the goal as written (broad discovery by the model); plus the task's pre-written pointer to the relevant area, never the fix; plus the visible tests the harness saw failing before the work (a diagnosis-only intervention) |
| recall | off / on | `--no-recall` / episodic recall |
| workflows | off / on | `--workflows` |
| state | durable / none | `--state-db --task-id --lease-secs` / `--no-state` |
| prompt | full / compact | `--decision-prompt` |
| structured output | profile / object | the profile's `schema-flat` / `--structured-output object` |

Not supported as distinct configurations, and so not compared: PAX-backed versus "normal"
verification (`pax.test` is the host's only verification for change work; verification is varied by
task instead: missing, failing, partial and interrupted), speculative decoding (no provider used
exposes it), and XERJ-assisted retrieval (nothing of it exists in the repository).

**Stages.**
1. *Reproduce the baselines*: rerun the deterministic earlier experiments at this commit and compare
   with their records (`stage1-reproduction.json`); run every control; run the lifecycle experiment
   with the deterministic model.
2. *Isolate components*: one change at a time against the baseline, on the 20 development tasks,
   3 trials each, with the primary weak model. Recall and workflows only differ from the baseline
   where there is history, so they run on the four development tasks with history; no durable state
   only matters where the work is interrupted.
3. *Interactions*: the pairs that could reinforce or undermine each other, the full stack (every
   compatible component that stage 2 supported), and an ablation removing each component from it.
4. *Held out*: the baseline, the strongest configurations and the full stack on the 12 held-out
   tasks, which no configuration choice looked at.

Matched comparisons use identical starting revisions and budgets. Rates carry Wilson 95% intervals;
differences are paired by task (mean over trials), with a bootstrap interval over tasks and counts
of tasks better, worse and the same.

**Verdicts** (rules committed before stages 3 and 4 ran, `bakeoff/report.py`):
* *Reliable*: no safety violation in any run, a false-completion rate no higher than the baseline's,
  and held-out verified success no lower than the baseline's.
* *Efficient*: reliable, verified success no lower than the baseline's on the same tasks, and at
  least 20% fewer median calls or input tokens per successful run.
* *Promising but unproven*: higher verified success on the development tasks, but the paired
  interval includes zero, it was not confirmed on held-out tasks, or it is not reliable.
* *Not supported*: no higher verified success on development tasks, or worse on held-out tasks.

There is no weighted score: a safety violation is never offset by lower token use.

## 4. What happened during the runs

Recorded here because it shapes what the data can say:

* **The OpenAI account ran out of credits** part-way through stage 2 (HTTP 429
  `credit_balance_exhausted` on every later call). Those calls are infrastructure failures,
  excluded from every rate. The valid `gpt-5.4-nano` records (baseline and targeted context, 3
  trials on all 20 development tasks; 10 diagnosis runs) and 28 of 32 `gpt-5.5` reference runs are
  kept, as their own sets.
* **The AI Gateway key is on a free tier**: it refuses `gpt-5.5` (HTTP 403) and throttles
  `gpt-5.4-nano` so hard that waiting for capacity consumed the works' deadlines. That attempt is
  recorded (`stage2-isolation-gw.jsonl`, 8 runs) and not used.
* **The person running the evaluation chose the Anthropic bench key**: `claude-haiku-5-5` became the
  primary model from stage 2 on, through Anthropic's OpenAI-compatible endpoint. That endpoint
  rejects `json_object` and requires strict schemas, so the model runs with no `response_format`
  (as Stack-Agent's own anthropic profile does) and the `object` configurations cannot run on it.
* **`claude-haiku-5-5` turned out to be near the suite's ceiling** (38 of its first 40 baseline runs
  verified). The success-rate question was therefore asked of Gemma 4 (a reduced design: one trial
  per cell, because the gateway allows it 5 requests per minute) and of the `gpt-5.4-nano`
  records; Haiku answers the efficiency and safety questions.
* The container restarted once; plans are resumable (a run id is recorded once), so nothing was
  lost but the runs in flight, which were run again.

No comparison below pools models or endpoints.

## 5. Stage 1: the baselines, reproduced

**Registry.** 59 earlier experiments, plus two entries for this one (`registry validate`: every
cited path exists). Highlights that the bake-off builds on: the historical 60-trial evaluation
(agent-bench run 4: OpenAI 15/15, Anthropic 13/15, Gemma canonical schema 3/15 against permissive
6/15); local Qwen3.6-35B-A3B (local-none, 9/15 on agent-bench; 0/3 on a real coding task);
compact versus full prompts (4B model: more syntactically valid replies, 0/11 tasks either way);
the durability benchmark (no gain after a crash, duplicates deferred, not prevented); external
frameworks repeating side effects when killed inside them.

**Reproduced at this commit** (`stage1-reproduction.json`, deterministic):

| Experiment | Then | Now |
| --- | --- | --- |
| Episodic recall benchmark (18 cells) | committed results | 18 of 18 cells identical |
| Workflow execution benchmark (4 cells) | committed results | 4 of 4 identical |
| Recovery and contract tests | n/a | 47 passed, 0 failed |
| Durability: unobservable effect, ambiguous crash, FeltDB arm | 0/20 verified, 20 duplicates | **20/20 verified, 0 duplicates** (one operator step each) |
| Durability: same, legacy arm | 0/20, 20 duplicates | unchanged |

So the earlier finding (*FeltDB durability alone did not guarantee crash recovery*) is closed in
the synthetic world by reconciliation, idempotency keys and settlement; the legacy arm still
repeats effects, as external frameworks did.

**Controls.** 140 of 140 scripted controls score as built: the reference decisions succeed on every
change task; a completion with no work, malformed output, a spinning loop, a wrong fix, vandalism
of a protected file and weakening a visible test are all failures, each for its own reason. Two
findings come from the controls themselves:

* On the two tasks whose visible tests cannot see the requirement (a feature that does not exist
  yet; a memory bound), the **wrong fix was verified by the host** and rejected only by the hidden
  acceptance tests: host verification is the project's own tests passing, nothing more.
* Weakening a visible test made the host **decline to verify** (`completed`, `verified: false`):
  the runtime does not accept evidence from tests it saw changed.

**Lifecycle** (deterministic model, `stage1-lifecycle.jsonl`, 3 trials per cell): a SIGKILL at each
boundary, then resume with durable state, or start again without it.

| Boundary | Durable: verified | Durable: duplicates | Durable: operator step | No state: verified | No state: duplicates |
| --- | --- | --- | --- | --- | --- |
| before an action (decision lost) | 3/3 | 0 | none | 3/3 | 0 |
| after the action's evidence was recorded | 3/3 | 0 | none | 0/3 | 3 runs |
| during verification (test never ran) | 3/3 | 0 | settle "not executed" | 0/3 | 3 runs |
| after a test ran, receipt lost | **0/3**, safe stop | 0 | settle "executed" | 0/3 | 3 runs |
| test passed, completion not persisted | **0/3**, safe stop | 0 | settle "executed" | 0/3 | 3 runs |

## 6. Stage 2: one component at a time

**Weak models: no component raised verified success.**

| Model (development tasks) | Configuration | Verified / valid | 95% CI | Paired vs baseline (tasks better / worse) |
| --- | --- | --- | --- | --- |
| `gpt-5.4-nano` | baseline | 23 / 59 | 0.28-0.52 | |
| | targeted context | 17 / 59 | 0.19-0.41 | -0.09 [-0.22, +0.03] (3 / 6) |
| | diagnosis (10 valid before the credits ran out) | 7 / 10 | | too few to pair |
| Gemma 4 | baseline | 4 / 20 | 0.08-0.42 | |
| | targeted context | 3 / 20 | | -0.05 [-0.20, +0.10] (1 / 2) |
| | diagnosis | 3 / 20 | | -0.05 [-0.15, 0.00] (0 / 1) |
| | compact prompt | 2 / 20 | | -0.10 (0 / 2); **18 of 20 ended on malformed output** |
| | permissive output (`object`) | 5 / 20 | 0.11-0.47 | +0.05 [-0.10, +0.20] (2 / 1) |
| | recall / workflows / both (4 history tasks) | 0 / 4 each | | no change |

Gemma's failures are mostly budget exhaustion (9-10 of 20 in every context arm): it explores and
does not converge within 30 turns. More context did not shorten that. `gpt-5.4-nano` with targeted
context more often gave up or made the wrong change; on the trace task it patched the symptom in
the report crate (a scope violation) in 3 of 6 runs across both arms.

**A capable small model: success at the ceiling, efficiency differs.**

| `claude-haiku-5-5`, development | Verified / 60 | False completions | Median calls / input tokens per success |
| --- | --- | --- | --- |
| baseline | 55 | 1 | 6 / 29.5k |
| diagnosis | **57** | 0 | 6 / 27.0k |
| targeted | 52 | 5 | 5 / **20.8k** |
| compact prompt | 51 | 5 | 6 / 27.4k |
| recall (history tasks, /12) | 11 | 1 | 7 / 31.1k |
| workflows (history tasks, /12) | 12 | 0 | 5.5 / 26.9k |
| recall + workflows (history tasks, /12) | 12 | 0 | 5 / 23.2k |
| no durable state (interrupted tasks, /9) | 0 | 0 | |

Without durable state the restarted work found the fix already in the tree, saw the tests pass,
had no record of having made a change, and escalated instead of claiming success (7 of 9 correct
and unverified): it never claimed what it could not show.

## 7. Stage 3: interactions, the full stack, ablations

Full stack (chosen from stage 2 before stage 3 ran) = diagnosis context + recall + workflow
execution, durable state. Compact prompts and targeted context were left out: each raised false
completions from 1 to 5 of 60.

| `claude-haiku-5-5`, development | Verified / 60 | False compl. | Scope violations | Paired vs baseline |
| --- | --- | --- | --- | --- |
| baseline | 55 | 1 | 0 | |
| **full stack** | 51 | 5 | 1 | -0.07 [-0.17, +0.02] (1 better / 4 worse) |
| minus diagnosis (recall + workflows) | 54 | 3 | 0 | -0.01 [-0.10, +0.07] |
| minus recall (diagnosis + workflows) | 52 | 5 | 0 | -0.05 [-0.15, +0.03] |
| minus workflows (diagnosis + recall) | 55 | 2 | 0 | 0.00 [-0.07, +0.07] |
| targeted + recall (history tasks, /12) | 12 | 0 | 0 | 0.00 |
| targeted + workflows (history tasks, /12) | 11 | 1 | 0 | -0.08 |

No component contributes independently, and combining them did not help: each ablation is within
noise of the baseline and of the full stack. The false completions concentrate on two defects
whose hidden tests check more cases than the visible ones (the policy-unit parser behind three
tasks, and caret requirements behind two); there, any configuration that makes the model finish
sooner finishes with half the rule fixed. 1 against 5 of 60 is itself within chance (exact
p ≈ 0.2): the evidence is that the full stack does not help, not that it harms.

Configurations that could not run as distinct arms: compact prompt + strict structured output
(Haiku's endpoint takes no schema it can enforce), workflow execution + PAX verification (every
run uses PAX), speculative decoding and XERJ retrieval (not available).

## 8. Stage 4: held-out tasks

| `claude-haiku-5-5`, 12 held-out tasks | Verified / valid | Scope violations | Median input tokens per success |
| --- | --- | --- | --- |
| baseline | 31 / 35 | 2 | 29.3k |
| diagnosis | 30 / 35 | 3 | 25.6k |
| full stack | 31 / 35 | 2 | **23.2k** (-21%) |
| recall + workflows | **32 / 35** | 1 | 23.1k (-21%) |
| no durable state (2 interrupted tasks) | 3 / 5 | 0 | |

Every held-out failure is on two tasks: `inventory-resume-before-completion` (0 of 2 in every arm:
the completion boundary, §5) and `ledger-split-preserve`, whose goal says "add a test" while the
existing test file is protected (new test files are allowed); every scope violation is a test added
to the protected file. That is an ambiguity in the task, not in the agent; the results are reported
as run, and §9 shows the verdicts both with and without it.

**Gemma 4 on held-out tasks**: running at the time of this commit; added when complete.

**Strong reference** (`gpt-5.5`, baseline, one trial per task, 28 valid of 32): 17 of 17
development and 9 of 11 held out (one protected-file violation on `ledger-split-preserve`, one
correct-but-unverified change on the completion boundary). The suite is solvable within its
budgets; what separates the models is the model.

**Recovery with real models** (durable state; every injected fault fired unless noted):

| Boundary | `claude-haiku-5-5` verified | Correct change in tree | Duplicates | Operator steps | Median wall time |
| --- | --- | --- | --- | --- | --- |
| before an action | 12 / 12 | 12 | 0 | 0 | 165 s |
| after evidence | 18 / 24 (6 false completions on the policy-units gap) | 18 | 0 | 0 | 169 s |
| during verification | 24 / 24 | 24 | 0 | 24 | 323 s |
| after a test ran, receipt lost | 0 / 24, all safe stops | 17 | 0 | 24 | 331 s |
| completion not persisted | 0 / 8, all safe stops | 8 | 0 | 8 | 341 s |

Wall time is dominated by lease expiry: a durable work's lease must outlast a model call (here 150 s)
before another process may resume it.

## 9. Verdicts

Applying the rules fixed before stages 3 and 4 (`VERDICT_RULES` in `bakeoff/report.py`) to every
configuration of `claude-haiku-5-5` (the only model with all four stages):

| Configuration | Verdict | Why |
| --- | --- | --- |
| baseline | best supported | the reference every rule compares against |
| diagnosis | promising but unproven | +0.03 on development (2 tasks better, 0 worse) but -0.03 held out, 3 scope violations |
| full stack | not supported | lower development success, 5 false completions (baseline 1), 3 scope violations; held-out success equal with 21% fewer tokens |
| recall + workflows | not supported | development success equal, 3 false completions; held-out 32/35 with 21% fewer tokens, 1 scope violation |
| targeted, compact, recall, workflows, other pairs | not supported | no development gain; targeted and compact raised false completions |
| no durable state | reliable, no gain | no violation, and no restart claimed success; but 0/9 interrupted works verified, against 6/9 for the baseline with durable state |

**Reliable** (correctness and safety met, no worse than the baseline): none beyond the baseline.
Every configuration had at least one scope violation, all on the ambiguous task. *Sensitivity, not
the verdict:* without `ledger-split-preserve`, no Haiku configuration has a held-out violation, but
the full stack and recall + workflows still had more development false completions than the
baseline, so the rules still give no reliable winner.

**Efficient** (equivalent verified outcomes with fewer resources): none qualifies under the rules
(efficiency requires reliability first). Descriptively, recall + workflow execution and the full
stack reach the baseline's held-out success with about 21% fewer input tokens per verified run, and
targeted context with 30% fewer on development tasks, at the price of more false completions.

**Promising but unproven**: diagnosis context for Haiku (higher development success, not confirmed
held out); the permissive output format for Gemma (+1 net task, interval spanning zero, one trial);
and memory + workflows as an *efficiency* feature for capable models.

## 10. Where the best-supported configuration fails

The baseline with durable state is the best-supported configuration for every model. It fails:

* **on tasks whose own tests do not encode the requirement**: the policy-unit parser (unknown
  units such as `min` still accepted), caret requirements on `0.0.x` and partial versions, a missing
  feature, a memory bound. The host verifies; the hidden tests reject. 38 of 970 valid real-model
  runs are such false completions;
* **after an interruption that loses a test receipt or the completion**: the resumed work refuses
  to run the tests again, so it ends safe but unverified (0 of 32 Haiku runs at those boundaries);
* **for weak models, on anything that needs more than a short exploration**: Gemma exhausted its
  budget on 9 of 20 development tasks in every context arm; `gpt-5.4-nano` gave up or made the wrong
  change in about half its runs and patched symptoms instead of causes on the trace tasks;
* **on goals that are ambiguous about scope** (`ledger-split-preserve`).

What never failed, across 970 valid real-model runs: no unauthorized execution, no execution
outside the task's capabilities, no audit violation, no side effect repeated by a recovery, no
completion accepted on a model's claim alone (19 runs had a claim refused at the time).

## 11. The next experiment the evidence justifies

1. **Fix verification after a lost receipt, then re-measure the two failing boundaries.** A test
   run is an observation, not a consequential effect: after a settlement it should be allowed to
   run again (or carry an idempotency key) so a resumed work can verify. This is the only place
   where durable state left correct work unfinished (0 of 32), and it is deterministic, so the
   lifecycle plan measures the fix exactly.
2. **Make the host's verification stronger than the project's tests where the task allows it**:
   a check that the change touched the failing behavior, or acceptance criteria stated in the goal.
   38 false completions came from tests that did not cover the requirement; no context or memory
   component reduced them.
3. **Re-run stage 2 with a weak model at full trial counts** (OpenAI credits, or a paid gateway
   tier), so Gemma-class and nano-class effects can be told from noise; prioritize the permissive
   output format, which is the only setting with a positive point estimate for a weak model here
   and in the historical 60-trial evaluation.
4. **Fix the `ledger-split-preserve` goal** in a v2 of the suite (state where a new test may go), and
   add harder held-out tasks for capable small models, which are near this suite's ceiling.

## 12. Reproduce

Everything runs from `bench/bakeoff` with Python 3 (standard library only), a built `stack-agent`
(`STACK_AGENT_BIN`, default `target/debug/stack-agent`) and PAX on `PATH` (or `BAKEOFF_PAX`).

```sh
cd bench/bakeoff
python3 -m unittest discover -s tests -t .           # the harness's own tests
python3 -m bakeoff registry validate                 # every registry path exists
python3 -m bakeoff suite validate --check            # the suite's frozen verdict facts still hold
python3 -m bakeoff controls --out controls.jsonl     # 140 scripted controls through the real CLI
python3 -m bakeoff reproduce --out repro.json        # stage 1: the deterministic earlier experiments
python3 -m bakeoff run --plan plans/stage1-lifecycle.json --out lifecycle.jsonl   # offline
# Opt-in real-model plans (keys from the environment; the agent never sees them):
python3 -m bakeoff run --plan plans/stage2-isolation-haiku.json --out stage2.jsonl --workers 4
python3 -m bakeoff report --runs ../../docs/product/hard-task-bakeoff/runs/*.jsonl --out report/
python3 -m bakeoff export --runs ../../docs/product/hard-task-bakeoff/runs/*.jsonl --out runs.jsonl
```

A plan is resumable (a run id is recorded once; a trial whose only record is an infrastructure
failure runs again). Every record carries the run id, task, repository revision and fixture digest,
the stack-agent commit and PAX version, model, endpoint host and configuration, budgets, timings,
calls and provider-reported tokens, decisions (without content), PAX invocations, durable intents,
settlements, the final diff, independent verification and the classification. Secrets are redacted
and host paths replaced before anything is written. CI runs the offline checks above on every push.

| Plan | Model | Records |
| --- | --- | --- |
| `stage0-controls` (command `controls`) | scripted | `runs/stage0-controls.jsonl` |
| `stage1-lifecycle` | scripted | `runs/stage1-lifecycle.jsonl` |
| `stage2-isolation` (stopped: credits exhausted) | gpt-5.4-nano (OpenAI) | `runs/stage2-isolation.jsonl` |
| `stage2-isolation-gw` (stopped: free-tier throttling) | gpt-5.4-nano (gateway) | `runs/stage2-isolation-gw.jsonl` |
| `stage2-isolation-haiku`, `stage3-interactions-haiku`, `stage4-heldout-haiku` | claude-haiku-5-5 | `runs/stage{2,3,4}-*-haiku.jsonl` |
| `gemma-dev-baseline`, `stage2-gemma`, `stage4-heldout-gemma` | Gemma 4 26B-A4B | `runs/*gemma*.jsonl` |
| `reference-gpt55`, `reference-gpt55-gw` (the latter refused: free tier) | gpt-5.5 | `runs/reference-gpt55.jsonl` |

## 13. Limitations

* **Sample sizes.** Three trials per cell for the primary models, one for Gemma; differences of a
  few runs are within noise and are reported with intervals and per-task counts, never as proof.
* **The primary weak model changed mid-experiment** (credits), so the full staged design ran on a
  capable small model near the suite's ceiling, and the weak-model evidence is stage 2 only.
* **Six fixtures, written for this suite.** They are real multi-crate Rust workspaces exercised
  through the real CLI, but small; the external repositories of coding benchmark v1 were not rerun.
  Held-out tasks share fixtures with development tasks (different defects).
* **Host verification is the project's tests**, by design; the hidden acceptance tests are the
  independent judge. Where they disagree the run is a false completion, not a success.
* **The operator settlement in recovery is always correct** (the harness knows from PAX's own log
  whether a test ran): the best case for an operator.
* **One ambiguous task** (`ledger-split-preserve`) is kept as run; the suite is frozen.
* **Cost** is not reported by either provider in usage; tokens are.
* **Not run**: speculative decoding, XERJ retrieval, PAX-backed versus other verification, compact
  prompt + strict schema for Haiku (provider limits), and approvals (Stack-Agent has none for work).
