# Production-readiness assessment: Stack-Agent as a portable agent runtime

**Assessed:** 2026-10-29, `rkendel1/stack-agent` `main` @ `5a273e3`. **Probes:** the experiment build `4a9b05b`. Its
PAX, provider and completion code is identical to `main`'s; see §1.

This page is a point-in-time verdict on what Stack-Agent is today, with the evidence and the gates that separate it
from production. It sits alongside three existing documents and does not replace them:
- [`coding-agent-production-roadmap.md`](coding-agent-production-roadmap.md) is the backlog. It says what remains and
  how each item is proven done; this page cites its ids, such as P0-06.
- [`production-boundary.md`](production-boundary.md) says what is in the shipped execution path.
- [`efficiency-experiments.md`](efficiency-experiments.md) holds the measured results of `ab-adaptive-context-v1`.
  §2 here summarizes and links to them; it does not restate the data.

New work identified here is listed in §7 for the roadmap's owner to fold in, with proposed ids. This page does not
change the roadmap's statuses.

Labels: **Observed** (read from a saved artifact or a run); **Read** (from reading code at the stated commit; no
test pins it); **Tested** (a named test pins it); **Interpretation**; **Hypothesis**; **Proposed** (not
implemented). "Probe" means an offline, deterministic run in
[`production-readiness-assessment/evidence/`](production-readiness-assessment/evidence/). Those runs use a loopback
stand-in model, sentinel keys and no paid calls.

---

## 0. Executive assessment

**What Stack-Agent is today.** A single Rust binary, `stack-agent`, with two entry points: `work` (CLI) and `serve`
(HTTP). It drives one model through a deterministic, reactive control loop:
- One JSON decision per reply, parsed strictly. A bounded re-ask follows a malformed reply.
- A fixed, small capability set: project read, list, search and write; read-only git; `pax.test`;
  `project.observe`; opt-in operator, AppPort and cli2 capabilities.
- Runtime-owned completion: `verified` needs a content-changing write followed by a PAX pass.
- Durable state in an embedded FeltDB, with fenced leases, intents and outcomes.

The model proposes and the runtime decides. There is no planner, no shell, no native tool calling and no sandbox.

| Question | Answer (evidence in the section named) |
| --- | --- |
| What can it do today? | Fix bounded defects in a trusted repository with a strong hosted model. Its safety boundary held in every recorded experiment: 0 unauthorized executions in 970 valid bake-off runs (§2.5). Two controlled stages were solved 5/5 and 3/3 with `replay` (§2.1–2.2). On long tasks it stops honestly rather than claiming success when its own tests disagree (T4: escalated, never falsely verified). |
| What do the experiments prove? | Little about efficiency, more about mechanism (§2): <ul><li>Prompt caching makes Opus T4 cost about $2 per run. The uncached projection is about $12, which is not a controlled comparison.</li><li>`auto` context did not beat `replay` on either stage. A selector defect explains T4.</li><li>Malformed-reply recovery is the largest recorded outcome gain (0/4 → 3/4, n=1 per task).</li><li>Durable fencing prevents contention false successes (0/20 → 20/20, synthetic).</li></ul> Almost every real-model comparison is n ≤ 5 per cell. |
| Can a developer run it locally against a supported model? | **Yes, conditionally.** Build from source (no release is published). Use a hosted OpenAI or Anthropic model, or a capable local model on an OpenAI-compatible server; LM Studio is measured. PAX ≥ 0.4.1 must be installed, and the repository must be one the developer would build themselves, because tests run unsandboxed. Small local models mostly fail the tasks (§5). |
| Can a customer safely use it as a hosted coding agent? | **No.** <ul><li>Repository code runs unsandboxed under the service's uid.</li><li>Provider keys reach that code, and through test output reach the model's prompt (probe, §3.1).</li><li>The default container runs unauthenticated on 0.0.0.0.</li><li>Tenant work is not durable.</li><li>No egress control, no per-run workspace, no orphan cleanup after a crash.</li></ul> These are critical, not averaged (§6). |
| Can another provider or model be integrated without rewriting the control loop? | **Yes for the loop, partially for the CLI.** `chip-core` sees only `ModelProvider::complete` and has no provider branches. Two wires exist: OpenAI-compatible and Anthropic Messages. An OpenAI-compatible server needs no code. A new wire means editing about 10 provider-identity switch points outside the loop (§5.3). Whether a model can drive the loop is a separate question that only a conformance run answers. |
| What prevents production deployment today? | In order: <ol><li>CI on `main` has failed for every push since at least 2026-10-21 (40 of 40 completed runs). Every gated suite after "Workspace tests" is skipped, and no release exists.</li><li>No execution isolation, and secrets are exposed to repository code.</li><li>`verified` does not mean the goal was met (P0-01; reproduced again here).</li><li>Leases have no heartbeat and orphaned test processes are not cleaned up.</li><li>No context-window awareness, and provider errors are misclassified.</li></ol> |
| Shortest credible path | Phases 0–3 in §7: <ol><li>Restore green CI and run the full suite.</li><li>Close the secret-to-child path and the doc drift.</li><li>Acceptance predicate, lease heartbeat and orphan cleanup, context-limit handling, and a provider conformance suite: all offline-verifiable.</li><li>Only then a controlled paid re-run, and hosted isolation through an isolating environment provider.</li></ol> |

**Readiness by deployment mode.** These are kept separate on purpose.

| Mode | Verdict | Blocking (severity) |
| --- | --- | --- |
| Trusted local developer, own repository, hosted frontier model | **Conditionally ready** (preview) | CI red / no release (high); unrelated edit can verify on a green baseline (high); secrets visible to repo code and to the model via test output (high locally); stale docs (medium) |
| Trusted local developer, local model | **Experimental** | As above, plus measured task success for local models is low (Gemma 6/15, Qwen3.6-35B 9/15 with partial success, small models ≈ 0) |
| Single-tenant self-hosted service (trusted operator, trusted repos) | **Not ready** | Unauthenticated default bind; serve cancel/drain does not abort in-flight tests; orphans on exit; no token/cost budget in `serve` |
| Multi-tenant hosted service (untrusted repos) | **Not ready** | No sandbox (critical); key exposure across tenants (critical); no egress control (critical); tenant work not durable (high); no retention (high) |

---

## 1. Repository and build identity

### 1.1 Components assessed

| Component | Repository | Ref inspected | State |
| --- | --- | --- | --- |
| Stack-Agent (assessed) | rkendel1/stack-agent | `main` @ `5a273e3` (worktree `/home/user/sa-assess`, branch `claude/readiness-assessment`) | clean. `5a273e3` = `64ce740` + PR #79 (selector transient fix, `8ed12f0`) + PR #80 (docs). The code audits read `64ce740`; the only code between it and `5a273e3` is the selector fix (`adaptive_context.rs`, `chip-pax` `is_transient_infrastructure_failure`) |
| Stack-Agent experiment build | same | `4a9b05b` (branch `claude/adaptive-context`, merged as PR #76) | binary `/root/.aoc-bins/stack-agent-4a9b05b`, sha256 `08f6431d…548a`. `git diff 4a9b05b 64ce740 -- crates/chip-pax crates/chip-fx-native crates/stack-agent/src/local_environment.rs` is empty, so probe results on PAX, provider and environment behaviour apply to `main` |
| Stack-Agent benchmark checkout | same | `main` @ `35d7af2` (`/home/user/stack-agent`) | clean; not rebuilt or modified. Source of the 2026-10-28 baseline batch (binary sha256 `848a6212…`) |
| Token-economics branch | same | `claude/peaceful-gates-zhpb4h` @ `aa72067` | merged into `main` (contained in `4a9b05b`) |
| Agent Obstacle Course (AOC) | rkendel1/agent-obstacle | `claude/peaceful-gates-zhpb4h` @ `0109bf8` | clean; **10 commits ahead of `main` (`7bb813e`), unmerged.** It holds the A/B results, LESSONS and the Python-provider diagnosis |
| AOC harness worktree | same | `wt/harness` @ `8b3bcf4` | clean |
| PAX | rkendel1/pax | `main` @ `aab5039`; installed binary `pax 0.5.0` | Stack-Agent CI pins **0.4.1** (`674f3b3`); local tests that pin 0.4.1 fail against 0.5.0 (§3) |
| FeltDB | crates.io `feltdb = "0.2.0"` (`crates/chip-agent-state/Cargo.toml`) | embedded, no server | local `flow_db` checkout `e2f1928` (0.15.0 certification) is not what Stack-Agent links |
| AppPort | rkendel1/appport | `claude/peaceful-gates-zhpb4h` @ `1eaf749` | reference digest implementation (`packages/protocol/src/publication.ts`) matches Stack-Agent's `host_capabilities/digest.rs` (Tested) |
| Compute / FX | `chip-compute`, `fx-*` crates in this repo; `rkendel1/fx` not available here | n/a | Compute is linked but used only by demo and self-test modes; FX's native provider is in-repo (§5) |

### 1.2 Which results came from which build

| Result | Build | Notes |
| --- | --- | --- |
| `ab-adaptive-context-v1` course-000 and T4 | `4a9b05b`, prompt cache on, durable state on | [`efficiency-experiments.md`](efficiency-experiments.md) §2 |
| 2026-10-28 baseline (Opus/Sonnet course-000 and T4) | `35d7af2`, no caching; T4 with `--no-state` (forced, see §2.4) | plan field `baseline`; run directories in `~/.aoc/runs`, not in any repository |
| Selector fix validation | `056affc` → merged `8ed12f0` | scripted only; no paid run |
| Probes in this page | `4a9b05b` | PAX, provider and completion paths unchanged on `main` |
| Earlier experiments (§2.5) | each records its own commit (`baea6fb`, `65815ec`, `68965fb`, `464ac56`, `e73e74a`/`fde4c16`, `ab6c9b4`, …) | most predate FeltDB as the default store and the decision-contract changes |

### 1.3 CI state of `main` (Observed, GitHub Actions)

- Every completed CI run on a `main` push from run 101 (`2e328d0`, 2026-10-21) to run 196 (`8ed12f0`, 2026-10-29) has
  conclusion **failure**: 40 of 40. Run 198 (`5a273e3`) was in progress when checked.
- On `64ce740` and `8ed12f0` the failing step is "Workspace tests":
  `chip-experiments --test experiments` > `replay_reaches_the_same_verdicts_from_the_recording_and_executes_nothing`
  (`crates/chip-experiments/tests/experiments.rs:342`).
- `cargo test` stops at the first failing test binary. Later binaries are not run, and every later step is
  **skipped**: release binary, CLI smoke, secret-redaction regression, project file boundary, exit-code contract,
  durable state, TLS/proxy, OTel, completion evidence, package smoke, FX boundary gates.
- `chip-experiments` is not on the `work` path (§3.2).
- A 2026-10-22 commit message (`c738595`) refers to "38 failures in the workspace baseline". The size of today's
  baseline is **unknown**: no `--no-fail-fast` run is recorded.
- **Consequence:** no gated suite has a recorded CI pass on current `main`. Claims below that rest on named tests are
  "Tested" in the sense that the test exists and asserts the behaviour; **none has a recorded CI pass at
  `5a273e3`.** The selector-fix branch's own focused suites were run locally (348 lib tests pass; 2 `pax_observe`
  version-pin failures that also fail on base).

---

## 2. Experiment lessons

Each finding is split into **O**bservation, **I**nterpretation, **H**ypothesis and **A**ction. The A/B data lives in
[`efficiency-experiments.md`](efficiency-experiments.md) and its byte-for-byte copies in
[`efficiency-experiments/`](efficiency-experiments/) (with `PROVENANCE.json`). The course-side lessons live in
agent-obstacle `experiments/LESSONS.md`.

### 2.1 course-000 (Rust off-by-one, transient-test-failure obstacle), replay vs auto, Opus 5.5, 5 runs per arm

- **O:** correctness 5/5 PASS in both arms; obstacle recognized and recovered 5/5 in both; agent status `completed`,
  verified by runtime, 10/10.
  - Model calls 4–5 per run, with executions equal to calls.
  - Median total input 11,280 (replay) vs 12,137 (auto). Median uncached / cache read / cache write was
    676 / 9,460 / 1,160 vs 1,542 / 9,458 / 1,146.
  - Median output 825 vs 440.
  - Median cost $0.0283 vs $0.0226 at the plan's prices. Agent time 12.9 s vs 9.6 s.
  - Stage cost $0.27.
  - Source: [`course-000.results.json`](efficiency-experiments/ab-adaptive-context-v1/course-000.results.json).
- **I, why input rose while cost fell:**
  - `auto` chose Standard on every call. Standard adds a task-state section to the per-call user message, which
    sits after the cache breakpoint and is billed as uncached input: +866 tokens at the median.
  - With 4–5 calls and no re-read file there is nothing to condense, so the section is pure addition.
  - Output fell by 385 tokens. At $20 per million for output against $4 for uncached input, −385 output (−$0.0077)
    outweighs +866 uncached (+$0.0035).
  - Volume and cost are different metrics.
- **H:** the explicit next-step and state section shortens the model's narration. Unverified: reply text was not
  recorded.
- **A:** do not promote `auto` on short tasks. Any re-run should record reply text (`--record-replies`) to test the
  output hypothesis.

### 2.2 T4 (TypeScript resolver defect in AppPort @ `268c9b4`), replay vs auto, Opus 5.5, 3 runs per arm

- **O:** evaluator results were replay 3/3 PASS and auto 2/3. Every run passed the 8 hidden acceptance tests. All 6
  recovered from the transient obstacle.
  - Agent status: escalated in 5 runs and `limit_reached` in 1. None was ever `verified`.
  - Median total input 3,000,775 vs 3,139,869 (+4.6%). Output 37,375 vs 35,069. Cost $2.08 vs $2.19.
  - Agent time: 594, 595 and 526 s (replay); 564, 591 and 455 s (auto).
  - Profiles in `auto`: Deep for 34/47, 32/46 and 27/45 calls; Standard otherwise.
  - Stage cost $12.97.
  - Source: [`t4.results.json`](efficiency-experiments/ab-adaptive-context-v1/t4.results.json),
    [`t4.evidence.md`](efficiency-experiments/ab-adaptive-context-v1/t4.evidence.md).
- **I, why Deep dominated:**
  - The selector's "conflicting evidence" rule paired the harness's injected transient failure (`status: error`,
    `reason: transient-infrastructure-failure`) with the next, genuinely failing run. Root cause confirmed in
    traces.
  - The fix (`8ed12f0`, PR #79) stops a PAX-classified transient failure from being a side of a conflict.
  - **On the same sequence, scripted, Deep is still chosen at the same request**, now by the repeated-failure rule,
    because the transient run still counts as a failed test run.
- **I, why `auto` did not earn promotion:**
  - no input saving (+4.6%);
  - one correctness-gate FAIL;
  - a known selector defect confounding the arm;
  - n=3 cannot resolve ±5%, since replay alone ranged 2.96M–3.43M.
- **I, why the agent never verified:**
  - the course's audit snapshot breaks whenever a test file is added;
  - the agent's `pax test` runs the full suite and sees that failure;
  - the evaluator excludes it.
  - The runtime's refusal to claim `verified` was correct by its own rules.
- **Python-provider regression signal (seq 6, `mvsvoq12400d5c8b3219`):**
  - **O:** seq 6 failed `spec/test/behavioral-replacement-e2e.test.ts` > "independent Python provider …" (`python
    provider did not start`); hidden acceptance passed 8/8.
  - **O, local reruns:** that run's diff failed 2 of 31 full or targeted runs against 0 of 26 on base (Fisher p ≈
    0.50). In 36 instrumented runs (894 Python starts) the T4 symptom never recurred, and no child failed to bind
    its port without injected churn (agent-obstacle `experiments/evidence/python-provider-diagnosis/README.md`).
  - **O (new here):** the same test failed in the evaluator's regression suite of a run whose agent never started.
    That is run `mvsl12st8320a0ae9981`, 2026-10-28: empty `diff_agent.txt`, base commit `268c9b46a71a`, error
    `AppPortError: The AppPort discovery endpoint is unreachable`.
  - **I:** the test fails on the unmodified fixture under harness conditions, so a seq-6-specific defect is not
    supported. The cause is unknown: neither a confirmed defect nor a characterized flake.
  - **A:** the course decides `knownFlaky` or a deterministic fixture in a new version, before results. The diagnosis
    README lists the discriminating test.

### 2.3 Prompt caching

- **O:**
  - With `--prompt-cache on`, cache reads were 80.4% (replay) and 77.9% (auto) of total input on course-000, and
    94.6% and 93.3% on T4.
  - Cache writes were about 1.1–1.2K per course-000 run (3,160 on the cold first run) and 94K–150K per T4 run.
  - Cached T4 runs cost $2.00–$2.44.
- **Projection, not a controlled comparison:** the uncached 2026-10-28 Opus T4 run (2,837,169 input, 31,054 output)
  prices at about $11.97. That run differs in three ways: build `35d7af2`, `--no-state`, and no caching.
- **Can conclude:**
  - the breakpoint is effective on Anthropic;
  - cost is dominated by cache reads plus output;
  - token volume is not a cost proxy.
- **Cannot conclude:**
  - the size of the saving under otherwise identical conditions;
  - any effect on correctness;
  - anything for OpenAI-compatible providers, where caching is reported, not requested, and cache writes are always
    `null`.
- **A:** if a cached-vs-uncached number is ever needed, run one pre-registered paired comparison on the same build
  and course. It is not needed for any current decision.

### 2.4 Recovery and durable state

- **Oversized objective:**
  - **O:** before `3eb0b42`, every T4 run with durable state was refused at admission: `FeltDB refused create_task:
    CONTRACT_INPUT_INVALID`, exit 3, 0 model calls. The 1,829-byte goal exceeded FeltDB's 1,024-byte objective
    limit. The 2026-10-28 T4 baseline therefore ran with `--no-state`.
  - **Fix:** a bounded objective with a SHA-256 digest; the whole goal (≤ 16 KiB) goes in `chip.Admission`.
  - Tests: `work_state.rs` unit tests, including `the_t4_goal_is_admitted_persisted_recovered_and_checked_byte_for_byte`.
- **Partial-goal resume:**
  - **O:** a continuation could pass the stored prefix as its goal.
  - **Fix:** `work_state::check_continuation` requires byte equality with the admission record, plus a digest and
    length match; legacy cut objectives are refused.
  - Tests: `tests/objective_limit_durable.rs::the_t4_goal_is_persisted_resumed_under_recovery_and_never_continued_from_its_prefix`.
  - **Caveat:** it landed as a commit marked "WIP (untested, not yet compiled)" (`46344ae`). The follow-up says the
    full suite was not run; CI has not passed since (§1.3).
- **Remaining gap (Read, untested):** `--max-goal-bytes` allows 32 KiB but admission allows 16 KiB. With the default
  database a 16–32 KiB goal runs unpersisted with a note; with `--state-db` it exits 3.
- **Lease hazard:**
  - **O:** renewal happens only before an execution (`chip-agent-state/src/lib.rs` `before()`). Model calls,
    events and `after()` never renew, and there is no heartbeat.
  - With `--lease-secs 1` and a slow call, a work ended `LEASE_EXPIRED` (exit 4) after a write had executed
    ([`lease-gap-probe.out`](efficiency-experiments/evidence/lease-gap-probe.out)).
  - `--test-timeout-secs` (≤ 3600 s) can exceed the 900 s default lease, unchecked.
- **Takeover delay:**
  - **O:** contract v1 has no forced takeover. A dead owner blocks the task until its lease expires: up to 15 min by
    default, and lease + 2 s in the AOC harness, about 6 min.
- **Surviving children:**
  - **Read:** PAX runs in its own process group, so signals to the agent's group do not reach it.
  - `GroupGuard` kills the group on timeout or drop, but nothing runs if the agent is SIGKILLed: no
    `PR_SET_PDEATHSIG`, no subreaper, no orphan sweep.
  - `serve` exits through `std::process::exit` after drain without dropping in-flight futures, so remaining PAX
    groups are orphaned outside a container.
  - An early takeover could therefore race a still-running test of the dead owner.
- **Lost receipt:**
  - **O:** with real models, recovery after a lost receipt does not reach verification.
    - Bake-off: 0/32 Haiku runs ([`hard-task-bakeoff.md`](hard-task-bakeoff.md) §5, §8).
    - AOC course-000 process interruption: Opus and Sonnet fixed the code, the evaluator passed, and both escalated
      with "pax.test … barred from running again".
  - **I:** the safety rule against re-running an execution whose effect is uncertain also bars the one idempotent
    action that would establish verification.
- **A:**
  - a lease heartbeat during model calls and tools;
  - orphan cleanup before takeover;
  - allow re-running `pax.test` after a settled or lost receipt, since PAX is read-only by contract;
  - align the goal ceilings.

### 2.5 Earlier experiments with persisted evidence

The index is [`bench/registry/experiments/`](../../bench/registry/experiments/), 59 entries.

| Experiment | Evidence | Models | n | Result | Quality |
| --- | --- | --- | --- | --- | --- |
| agent-bench run2 → run4 (decision contract) | [`agent-bench.md`](agent-bench.md), `agent-bench/run4-decision-contract/report.json` | gpt-5.5, sonnet-5-5, Gemma 26B (LM Studio) | 15 per provider | OpenAI 15/15 → 15/15; Anthropic 8/15 → 13/15; Gemma 6/15 → 3/15 (schema) / 6/15 (object) | several changes at once; n=3 per task |
| agent-bench run5, durable pair | `agent-bench/run5-durable-pair/report.json` | sonnet-5-5 | 15 pairs | legacy 12/15 vs durable 11/15; 0 false successes | underpowered; no gain from durability when uninterrupted |
| Durability benchmark P1–P3 | [`durability-benchmark.md`](durability-benchmark.md), `durability-bench/run1-deterministic/summary.json` | scripted | 20 per cell | contention false successes 40 → 0; crash/stale-owner/ambiguous notify 0/20 → 20/20 | synthetic world |
| coding-benchmark-v1 run1 → run2 (malformed-reply recovery) | `coding-benchmark-v1/runs/2026-10-22-*` | opus-5-5 | 4 tasks × 1 | 0/4 → 3/4 solved (T1 partial) | n=1; a second change (build-cache isolation) in between |
| coding-benchmark-v1 local models | `…/2026-10-22-local-models-68965fb` | Qwen3.6-35B, Qwen3.5-4B | 18 runs | 3/18, all 35B on task E | model and mode confounded; patched PAX; "uncommitted results" |
| Apple FM vs Qwen3.6 | [`apple-fm-bench/README.md`](apple-fm-bench/README.md) | Apple `fm serve`, qwen3.6-35b (LM Studio) | 15 each | Apple 0/12 on work tasks; qwen 9/15 | confounded (size, server); effective n ≈ 25 |
| Qwen3.6 on oMLX | `coding-agent-benchmark-qwen36-omlx/` | Qwen3.6-35B-A3B | 3 (+20 A/B) | 0 files changed; stuck guard 3/3 and 20/20 | one task |
| Actionable feedback A/B | `actionable-project-feedback/results/ab-summary.json` | one local model | 10+10 | failed observations 44 → 3; completions 0 → 0 | controlled; process metric only |
| Small-model reliability | [`small-model-reliability/README.md`](small-model-reliability/README.md) | Qwen2.5-Coder-3B (llama.cpp CPU) | 12 per row | acceptance 3/12 unchanged; latency 205 → 118 s; 73–76% cached | underpowered |
| Hard-task bake-off v1 | [`hard-task-bakeoff.md`](hard-task-bakeoff.md), `hard-task-bakeoff/report/report.json` | gpt-5.4-nano, haiku-5-5, Gemma 4, gpt-5.5 reference | 1,233 runs, 970 valid | no component raised verified success; 38/970 false completions where project tests did not encode the requirement; 0 unauthorized executions | primary model switched mid-run; Haiku near ceiling |
| Compact vs full prompt | `../../audit/compact-prompt/RESULTS.md` | woodcock-qwen3-4b | 22 | 0/11 vs 0/11; −33% input; 11/11 "grounded" completions after reading no source file | effective n ≈ 3 |
| Episodic recall; workflow execution | [`episodic-memory.md`](episodic-memory.md) §10; [`workflow-execution.md`](workflow-execution.md) | gpt-5.5 | n=3 per cell | useful fixture: calls 5 → 3 (recall), 5 → 1 (workflow) | scripted fixtures; within run-to-run variance for deviations |
| Native skills | `native-skills-experiment/runs/2026-10-22-mercury-3-paced/` | mercury-3 (gateway) | 10 per arm | 10/10 every arm | ceiling |
| AOC 2026-10-28 baseline | `~/.aoc/runs` (not committed); plan `baseline` | opus, sonnet | 1 per cell | course-000 PASS all; interruption PASS but escalated; T4 BLOCKED (objective limit), then PASS with `--no-state` | n=1; T4 state forced off |

### 2.6 Cross-cutting lessons

**Repeated failure patterns.**
1. **The reply contract is the most common failure point, across all model classes.**
   - **O:** Anthropic models prefixed prose to the JSON (coding-benchmark run1 0/4; T4 still had 5–11 rejected
     replies per run). Small models produced corrupted keys, extra braces and invented inputs.
   - **I:** malformed-reply recovery turned this from fatal to costly.
   - **A:** break out the cost of rejected replies (each is a full-context call), and measure per model in the
     conformance suite (§5.4).
2. **Constrained decoding damaged decisions rather than fixing syntax.**
   - **O:** Gemma canonical schema 6 → 3/15, with premature `complete` 12/15. Apple's schema arm lowered judgment
     (45 → 36 of 60) and hung. `schema-closed` stopped Claude on turn 1.
   - **I:** structured-output modes must be chosen per model from measurements, never defaulted. The code defaults
     to `schema`; the docs say `object`.
3. **Stuck loops with local models.**
   - **O:** Qwen3.6 (oMLX) 23/23 runs were stopped by the guard; Qwen3.6 on T2/T4 6/6; Gemma loops and budget
     exhaustion.
   - **I:** the stuck guard works as a stop, not a cure.
4. **"Verified" or "grounded" without meeting the goal.**
   - **O:** 38/970 bake-off false completions where tests did not encode the requirement. Compact-prompt 11/11
     "grounded" with no source read. HA-02, and this page's probe 2 (§3.1).
   - **I:** completion is test-based, not goal-based (P0-01).
5. **Evaluator and agent contracts disagreed:**
   - the goal limit (2,000 vs 1,024 bytes);
   - the T4 audit snapshot;
   - an inspect summary limit not stated in the prompt;
   - `pax.test` running whole workspaces that are red at base.
   - **A:** every course and benchmark must give the agent the same acceptance contract the evaluator scores.
6. **Recovery after a lost receipt never reached verification** (§2.4).

**Changes that measurably improved outcomes:**
- malformed-reply recovery (`65815ec`);
- reply rules plus the corrected inspect contract (Anthropic 8 → 13/15);
- durable fencing (synthetic, 0 → 20/20);
- actionable feedback (failed observations 44 → 3);
- prompt caching (cost);
- request ordering (latency 205 → 118 s);
- a shorter capability list for Gemma (`length_limited` 3/3 → completed 3/3, `native-provider.md`).

**Changes that failed to improve outcomes:**
- `auto` context (both stages);
- canonical schema on Gemma;
- compact prompt;
- targeted context and the full component stack in the bake-off;
- small-model fixes (acceptance unchanged);
- durable state without interruption (12/15 vs 11/15);
- recalled-episode caveat wording.

**Confounded or underpowered:** nearly every real-model comparison has n ≤ 5 per cell. Specific confounds:
- agent-bench run4 (several changes);
- coding-benchmark run1 → run2 (two changes);
- local models (model and mode tied);
- Apple vs Qwen (size and server);
- the bake-off model switch;
- the AOC T4 baseline (state forced off).

**Missing instrumentation:**
- **PAX reports counts, not failing test names.** This blocked T4 diagnosis.
- **Observation text and reply text are not stored by default.** Only size and digest are kept.
- **No per-call latency or time-to-first-token.**
- **No provider-reported cost.** Cache writes are `null` on OpenAI-compatible endpoints.
- **Run directories and prompts are not committed** (bake-off proxy, agent-bench harness, the AOC 2026-10-28
  batch).
- **Weak model identity:** an `qwen-fast` alias, and `system` for Apple.
- **Peak memory was never measured.**
- **The child exit code and stderr for course-fixture failures are not recorded.**

---

## 3. Architecture and boundary audit

### 3.1 Findings established by probe (offline, this page)

Script [`probes.sh`](production-readiness-assessment/evidence/probes.sh), output
[`probes.out`](production-readiness-assessment/evidence/probes.out), stand-in model
[`fake-anthropic.mjs`](production-readiness-assessment/evidence/fake-anthropic.mjs). The build is `4a9b05b` and PAX
is 0.5.0.

- **Probe 1, secrets reach repository code and the model.**
  - **Setup:** `stack-agent work --kind verify` with `CHIP_API_KEY` and ambient `ANTHROPIC_API_KEY` set to
    sentinels. The project's only test prints `CHIP_API_KEY` and fails.
  - **O:**
    - PAX was started with both key variables in its environment.
    - The test's output, containing the sentinel, appears 6 times across the 4 request bodies sent to the model.
    - Stack-Agent's own report and stderr contained no sentinel.
  - **Read, why:**
    - `PaxExecutor` spawns `pax` without `env_clear()` (`crates/chip-pax/src/lib.rs` around line 898). PAX does not
      clear it either: no `env_clear` in `rkendel1/pax` `aab5039`.
    - `stack-agent` never removes the key from its own environment (no `remove_var` in the crates).
    - Test output is part of the observation sent to the model, and redaction covers Stack-Agent's own outputs, not
      observation text.
  - **I:**
    - Locally, any test or build script can read the key, and a model-written test can put it into the prompt and
      the provider's logs.
    - Hosted, every tenant's key variable is in the shared service environment, so code in one tenant's repository
      can read all of them.
    - The existing secret-redaction suite (`tests/credential_redaction.rs`) does not cover this path. The only prior
      mention is a research note (`docs/research/layerfs-evaluation/README.md:59`).
    - **Severity: critical for hosted, high for local.**
- **Probe 2, an unrelated edit verifies a change goal on a green baseline.**
  - **Setup:** the goal asks for `double(x)`. The scripted model writes an unrelated `NOTES.md`, runs the
    already-passing tests and completes.
  - **O:** exit 0, `verified: true`, `acceptance_verified_by_runtime: true`, 2 model calls. `double` was not added.
  - **I:** this is HA-02 / roadmap P0-01, still reproducing on the current build. The Work Contract acceptance
    predicate (WC-06/07, RIC-02) is documented only (`work-contract.md`: "Not implemented").

### 3.2 Boundary table

Status words: **Enforced** (code refuses violations; tested where a test is named); **Implemented** (works; not a
hard boundary); **Documented only**; **Missing**. Paths are under `crates/` unless stated.

| Boundary | What is implemented and enforced | Documented only / missing |
| --- | --- | --- |
| **Admission** | <ul><li>Goal from argument, file or stdin (exactly one), ≤ 2,000 bytes by default, ≤ 32 KiB with a flag; UTF-8; no control characters (`stack-agent/src/software_work.rs` `goal_is_acceptable_within`).</li><li>Project canonicalized; PAX required for `change`/`verify` before any model call (exit 3).</li><li>One work per directory (`flock`, `chip-project/tests/lock.rs`).</li><li>Durable admission is idempotent; conflicting identity is refused (`work_state.rs` tests).</li></ul> | 16–32 KiB goal window untested (§2.4). `serve` POST has no idempotency key. |
| **Control loop** (`chip-core/src/work.rs` `drive`) | <ul><li>Reactive, one decision per turn. Strict parser (`chip-core/src/model_decision.rs`): one JSON object, 4 KiB, no repair. Bounded re-ask (2 consecutive rejections; `tests/malformed_reply_recovery.rs`).</li><li>Turn, execution, context-byte, deadline, token and cost limits, each with `ended_by` (`tests/run_interface.rs`, `tests/cancellation.rs`).</li><li>Stuck guard (`tests/stuck_guard.rs`).</li><li>Safety audit recomputed from events; a violation means exit 4.</li></ul> | No planner, by design (AGENTS.md). Per-capability budgets (WC-12) documented only. A PAX timeout or invalid result ends the work as `Failed` (exit 4), not as an observation (Read). |
| **Crates on the work path** | Used: chip-core, chip-inference, chip-fx, chip-fx-native, fx-*, chip-pax, chip-project, chip-remote-env (capability set), chip-agent-state; chip-cli2 opt-in; chip-otel after the run. | Linked but unused on `work`: chip-local-decision, chip-decision-corpus, chip-wasm-reasoner/decision(-host), chip-graph, chip-compute, chip-experiments, chip-reasoning-corpus. Optional: chip-laya-reasoner, chip-local-ml. Not depended on: chip-turns, chip-session. |
| **Context assembly** | <ul><li>Default `replay` (`dedup-v1`); `auto`/fixed profiles opt-in (`stack-agent/src/adaptive_context.rs`).</li><li>`replay` is byte-identical to `aa72067`'s requests (`tests/replay_equivalence.rs`).</li><li>The Anthropic cache breakpoint sits on the last system block; observations are system messages; the per-call user message carries state.</li></ul> | No model context-window knowledge (§5). Comment at `software_work.rs` ~3968 says "neither means `auto`" (stale). |
| **Capabilities and authorization** | <ul><li>Fixed product allowlist (`software_work.rs` `declared()`); `inspect` narrowed to read-only.</li><li>Operator capabilities can't reuse `project.`/`pax.` ids.</li><li>AppPort: discovery is not authorization (`--appport-authorize`, `--appport-grant`); contracts pinned by digest and re-checked before each call (`742cb03`; `tests/host_capabilities.rs`, `tests/appport_digest.rs`).</li><li>cli2 deny-all by default; operator approval types the plan digest.</li></ul> | No interactive approval for built-ins (by design). The pin re-check is skipped when there is no refetch URL; a check-to-call window exists; the digest is not in durable state. The `appport.` prefix is not reserved for `--capabilities` files. |
| **File effects** | <ul><li>Descriptor-based confinement (`openat` + `O_NOFOLLOW` per component; no symlinks, FIFOs, devices or multi-link files; `chip-project/tests/confinement.rs`).</li><li>Withheld credential names; protected paths (build scripts, CI, toolchain, agent instructions) never written (`chip-project/src/policy.rs`).</li><li>Writes are precondition-guarded (`expected_sha256`/`expect_absent`) and atomic.</li><li>18-case prompt-injection corpus (`tests/prompt_injection.rs`).</li></ul> | `package.json` scripts, Makefiles, `conftest.py` and test files are not protected (tampering is caught at completion, by path convention). The non-Unix race (`known-issues.md` J). No real-model injection measurement. |
| **Command execution** | <ul><li>No shell for the model.</li><li>PAX argv fixed; own process group; kill on timeout or drop; strict stdout parse (`chip-pax/tests/pax_boundary.rs`).</li><li>git read-only with `env_clear`.</li><li>Operator exec with `env_clear` and an idempotency key.</li></ul> | **No sandbox (HA-01, critical for untrusted code).** **PAX inherits the full environment, keys included (probe 1).** |
| **Durable state** | <ul><li>Embedded FeltDB 0.2.0, contract `feltdb.agent.transition` v1.</li><li>Intents before and outcomes after each execution; idempotency keys (`chip-agent-state/tests/idempotency.rs`).</li><li>Commit-time fencing; a stale worker can't overwrite settlement (`tests/stale_worker.rs`).</li><li>Kill tests (`tests/kill_boundaries.rs`); restart recovery for local `serve` (`tests/service_recovery.rs`).</li></ul> | No heartbeat; no forced takeover; no pruning or retention. Tenant work is never durable (`service.rs` ~530). Observation and model output are stored unredacted. |
| **Cancellation and processes** | <ul><li>Stop at the next boundary (Ctrl-C, SIGTERM, `/cancel`, deadline; `tests/cancellation.rs`).</li><li>`work --abort-in-flight` drops an in-flight execution and kills its group (`tests/abort_in_flight.rs`).</li></ul> | `serve` never uses abort-in-flight. SIGKILL of the agent orphans the PAX group. `serve` exit orphans in-flight groups. No orphan sweep. Model calls can't be interrupted mid-read; remote cancellation is "unconfirmed". |
| **Evidence and completion** | <ul><li>`verified` = content-changing write, then a PAX pass after the last change, then no stale write.</li><li>Tampered tracked test files or a fingerprint mismatch revoke it (`tests/completion_evidence.rs`).</li><li>Only `verified` exits 0.</li><li>The `completion` section keeps model claim, change, tests and runtime verification apart; `independent_evaluation: "not_available_to_runtime"`.</li></ul> | **No goal-level acceptance (probe 2; P0-01).** Evidence ledger, work contract and blockage classifier: documented only. PAX gives counts, not test names. |
| **Configuration and CLI** | <ul><li>Precedence flag > env > `--provider-profile` > default (`tests/work_generation.rs`).</li><li>Exit codes 0/1/2/3/4/130 (`stack-agent/src/verify.rs` `exit_code`; `tests/exit_codes.rs`).</li><li>Report and event schemas checked (`tests/report_contract.rs`).</li><li>`doctor` (`tests/doctor.rs`).</li></ul> | `report-contract.md` lists "failed" under exit 1; code and tests say 4. Default structured output is `schema` in code, `object` in docs and help. |
| **Observability** | <ul><li>`--events` JSONL (`chip.work-event.v1`, gapless `seq`).</li><li>`serve` polling and SSE.</li><li>OTel GenAI spans exported after the run from the durable record (`tests/telemetry_export.rs`).</li></ul> | No live spans or metrics export; ad-hoc `eprintln!` logs. Tenant-mode and `--no-state` works export nothing. |

### 3.3 Documentation that no longer matches the code (Read; corrections proposed, not made here)

1. [`release-limitations.md`](release-limitations.md):
   - "The shipped `chip` and `chip serve` do not persist turns at all", "the shipped binary persists no work state"
     and "Chip does not resume an interrupted model response" are contradicted by FeltDB-by-default and by
     recorded-response restoration (`chip-agent-state/tests/model_response.rs`).
   - "An execution in flight is not interrupted" no longer holds for `--abort-in-flight`.
2. [`report-contract.md`](report-contract.md) maps "failed" to exit 1. Code and `tests/exit_codes.rs` map it to 4.
3. `stack-agent/src/service.rs` module header says "no authentication" and "Polling is the only delivery mechanism".
   [`events-stream.md`](events-stream.md) lists SSE as not done, and `../known-issues.md` says `serve` streams by
   polling. Tenant bearer auth and SSE both exist.
4. [`production-boundary.md`](production-boundary.md) calls FeltDB "vendored" (it is crates.io 0.2.0), still uses
   `chip`/`chip-cli` names, and predates FeltDB as the production store.
5. [`hostile-autonomous-agent-audit.md`](hostile-autonomous-agent-audit.md):
   - HA-12 (children outlive Chip) is only partly fixed: timeout and abort kill the group, while SIGKILL and `serve`
     exit still orphan it;
   - HA-11 has no update for `36d00b5`;
   - the Dockerfile presents a "hosted image" with an unauthenticated default and no HA-01 warning.
6. [`interruption-recovery.md`](interruption-recovery.md) says "a killed wrapper cannot leave an orphan". That holds
   only when Chip does the killing.
7. [`native-provider.md`](native-provider.md) and the `work` help text give the default structured output as
   `object`; the code uses `schema`. The chip-fx-native crate doc says structured output "never reach[es] the wire".
   Docs cite `docs/rebuild/*` and `host/node`, which are not in this repository.
8. `service-retention-bounded.json` was measured on the chip-rs `chip` binary (evicted → 404). Current code answers
   410 `work_expired`.
9. The `serve` startup banner prints "Local trusted-client interface: no authentication" even in tenant mode.
10. The `3eb0b42` message cites FeltDB 0.3's contract; `Cargo.lock` pins 0.2.0.

---

## 4. Local versus hosted operation

### 4.A Local runtime (developer machine)

| Concern | State | Evidence |
| --- | --- | --- |
| **Install** | Build from source only, toolchain 1.97.0 pinned. `scripts/install.sh` (checksum before extract, no sudo, no overwrite without `--force`) is tested, but **no release has been published**: `release.yml` makes drafts only, version 0.1.0, binaries unsigned. | `RELEASE_NOTES.md`; [`release.md`](release.md); CI "Release scripts" jobs pass |
| **Runtime dependencies** | PAX ≥ 0.4.1 for `change`/`verify`; absence is exit 3 before any model call (`tests/pax_independence.rs`). FeltDB embedded, so no server. git advisory. | [`release.md`](release.md) "PAX" |
| **PAX versions** | Code minimums are 0.3.0 (`pax.test`) and 0.4.1 (`project.observe`), with no maximum. Tests and CI pin exactly 0.4.1, so they fail locally with 0.5.0. Runtime with 0.5.0 is accepted (shim test `a_later_pax_is_accepted_and_its_result_consumed`) and was exercised in the A/B and the probes. | `chip-pax/src/lib.rs:68`, `observe.rs:59-67` |
| **Endpoint and model** | Model always required, never guessed. Endpoint required for openai-compatible. Plain http only to loopback, or LAN with `CHIP_ALLOW_INSECURE_LAN_HTTP=1` and no key. Explicit `CHIP_PROXY`; ambient proxies ignored; `SSL_CERT_FILE` for private CAs. | [`native-provider.md`](native-provider.md); `chip-fx-native/src/credentials.rs` |
| **Local OpenAI-compatible servers** | `local` and `lm-studio` profiles measured (schema grammar, 300 s). `ollama` (`/v1`), `llama-cpp`, `vllm` and `openrouter` profiles written from documentation and unmeasured. Ollama `/api/chat` refused. Default call timeout 30 s. | `stack-agent/src/work_generation.rs` profiles |
| **Tool and structured output** | No native tools: JSON decision in text. Structured output is a per-model request hint; the right mode differs per model (§2.6 pattern 2). | §5 |
| **Resource limits** | Turns, executions, deadline, tokens, cost (with operator prices), context bytes. No CPU, memory or disk limits on test runs. | [`run-interface.md`](run-interface.md) |
| **Diagnostics** | `stack-agent doctor`: binary, model, endpoint (TCP connect only), git, project, pax (`--version`), verification, state. It does not validate key or model, does not check observe's 0.4.1 minimum, and does not check PAX supports the project. | `stack-agent/src/doctor.rs`; `tests/doctor.rs` |
| **Upgrades** | Re-run the installer with `--force`. No self-update and no migration note. Runtime learning is keyed by executable path, so a move resets it (Read). | `learning/store.rs` |
| **Persistence and restart** | Per-project FeltDB under `$STACK_AGENT_DATA_DIR`/XDG (0700). Interrupted work is reported and continued with `--task-id`. Restart recovery is tested for `serve` local mode. | `work_state.rs`; `tests/service_recovery.rs` |
| **Security** | The model never sees the environment, and keys are scrubbed from Stack-Agent's own outputs (`tests/credential_redaction.rs`). But repository tests and build scripts run as the user with the full environment, keys included, and their output reaches the model (probe 1). README warns to use only repositories you would build yourself. | §3.1 |

**Verdict:** conditionally ready for a trusted developer on a trusted repository with a measured model, from source.
The conditions are: CI green; PAX pinned to a tested version; the key not exported into the shell that runs it, or a
throwaway key; and `verified` read as "tests pass after a change", not "goal met".

### 4.B Hosted runtime (untrusted or customer repositories)

| Requirement | Implemented | Required for a safe service |
| --- | --- | --- |
| **Per-run isolation and workspace lifecycle** | None. `LocalEnvironment` mutates the given directory in place. Tenants get a fixed existing root (disjoint roots enforced). `chip-remote-env` is a seam with no isolating provider. | A fresh, disposable workspace per run (clone or snapshot), destroyed afterwards; an isolating `EnvironmentProvider` (P0-06 is blocked on it) |
| **Execution sandbox** | None (HA-01, D-07: "Production defect for any untrusted/hosted use") | Container or VM per run with no host mounts, as a non-root uid, and seccomp/rlimits |
| **Credential custody** | Keys come from env vars; tenant profiles name `api_key_env`; ambient keys off for tenants. **Every key is in the shared process environment and inherited by PAX** (probe 1). | Keys held outside the execution environment (a proxy that injects them, as the AOC harness does with a placeholder `CHIP_API_KEY`); child env built from an allowlist; redaction of observation text |
| **Network egress** | Model transport only via explicit proxy and TLS. Repository and test code has unrestricted network. | Default-deny egress for the sandbox; allowlist for package registries if needed |
| **Budgets** | `work`: turns, executions, deadline, tokens, cost. `serve`: turns, executions, deadline only (tenant ceilings). No CPU, memory or disk limits. | Token and cost budgets in `serve` per tenant; resource quotas per run |
| **Cancellation and cleanup** | Boundary cancel works. In-flight test not aborted in `serve`. Orphans on SIGKILL or `serve` exit. | Abort-in-flight in `serve`; kill the run's whole process tree or container on cancel, crash and shutdown |
| **Durable execution, retries, leases** | Strong fencing and intents for local works. **Tenant works are in-memory only.** No heartbeat, no forced takeover, single process owns the FeltDB file, no horizontal scaling. `serve` POST has no idempotency key. | Durable tenant work; lease heartbeat; takeover after verified cleanup; idempotent submission |
| **Tenant isolation and access control** | Bearer tokens (SHA-256, constant time), ownership filtering, path confinement, per-tenant concurrency and queues (`tests/serve_tenants.rs`, 4 tests). Same uid, same process, same env. Default mode has no auth; Docker default binds 0.0.0.0 unauthenticated. | Auth always on for non-loopback; TLS; rate limits; process or VM separation between tenants |
| **Logs, audit, retention** | Durable `chip.WorkEvent` with provenance (local only). OTel after the run (not for tenants). In-memory retention bound. FeltDB never pruned; observation and model output stored unredacted. | Structured logs; audit trail for every tenant; retention and purge policy; redaction at rest |
| **Deployment and health** | `/health`, `/ready` (503 while draining), SIGTERM drain. Dockerfile non-root, without PAX, no `HEALTHCHECK`, not built in CI. Unbounded connections and SSE streams. | Image built and tested in CI; connection limits; health checks wired; documented runbook |

**Verdict:** not ready. The gap is architectural (isolation, custody, egress), not a list of missing flags. A
successful local run says nothing about these.

### 4.C Common runtime contract and the narrowest architecture

**Contract that must stay identical in both modes**, mostly already embodied by `WorkRuntime` and the report and event
schemas:

1. **Admission:**
   - a goal, a kind, a project reference and limits;
   - an idempotent admission record;
   - refusal before any model call if a dependency is missing.
2. **Capabilities:**
   - the declared allowlist plus operator-authorized additions;
   - the same ids and input schemas in both modes;
   - the environment can narrow, never widen.
3. **Inference:**
   - `ModelProvider::complete` with explicit delivery certainty;
   - usage as `Option`s;
   - no retries inside the loop.
4. **Evidence:** an observation per execution, with a PAX result record and a project fingerprint.
5. **State transitions:**
   - `feltdb.agent.transition` v1;
   - intent before and outcome after;
   - fenced lease.
6. **Cancellation:** stop at a boundary, plus abort-in-flight, with the outcome `cancelled` and its certainty.
7. **Budgets:** turns, executions, deadline, tokens and cost, enforced by the runtime, with `ended_by`.
8. **Completion:** a runtime-owned predicate and the `completion` section, with `independent_evaluation` kept
   separate.

**Recommendation:**
- **Keep one runtime and put the hosted differences behind the existing `EnvironmentProvider` seam.**
  - A local provider (today's `LocalEnvironment`, plus a child-environment allowlist).
  - An isolating provider that prepares a disposable workspace, runs `pax.test` and the project capabilities inside
    a sandbox, and holds no provider keys.
- **Keep model calls in the trusted runtime process, outside the sandbox.** Keys then never enter the execution
  environment.
- **No new service, sidecar or API is required** for the contract. Hosted mode additionally needs durable tenant work
  (lift the `tenant.is_none()` filter once per-tenant stores exist) and auth that is always on for non-loopback.
- **Compute (`chip-compute`) is the natural candidate for the isolating provider**, but that is a design decision
  for its owner. Nothing here invents its API.

---

## 5. Provider and model portability

### 5.1 What the runtime actually provides (Read, Tested where named)

| Aspect | State |
| --- | --- |
| **Neutral types** | `chip-inference`: `ModelRequest`, `Message` (System/User/Assistant), `ModelResponse`, `Usage` (all `Option`), and `FxError`/`InferenceFailure` with `delivery`, `status`, `retry_after_ms`, `partial_output`, `remote_cancellation`. Text only; `tools` always empty (`chip-fx/src/mapping.rs`). `tests/provider_neutral_api.rs`, `chip-fx/tests/architecture.rs` (chip-core never names FX or HTTP) |
| **Wires** | Two: OpenAI-compatible chat completions and Anthropic Messages (`fx-native-provider/src/{openai,anthropic}.rs`). `NativePort` is a closed enum. Unknown provider strings are refused. |
| **Model selection** | `--model`/`CHIP_MODEL` required; profiles set mode, timeout and token defaults. No model registry or capability metadata. |
| **Auth** | One module reads the environment (`chip-fx-native/src/credentials.rs`, architecture test). Named variable or nothing, with no fallback across providers. Loopback is keyless and ignores ambient keys. Keys are redacted in `Debug` and scrubbed from provider errors. No vault or file. |
| **Actions** | JSON decision in text. Provider tool calls are a protocol violation. Strict parse with bounded re-ask. |
| **Structured output** | `schema` (default), `schema-flat`, `schema-closed`, `object`, `json-object`, `none`; per provider, not per request. Opt-in fallback ladder on 400/404/415/422/500/501 (`tests/structured_fallback.rs`). |
| **Streaming and cancellation** | SSE on both wires; non-streaming for OpenAI only. Cooperative cancel between 25 ms polls. A blocking read can't be interrupted, so remote stop is "unconfirmed". |
| **Timeouts and retries** | Connect 10 s, per-call budget (default 30 s). **No retries or backoff.** 429 → `rate_limited`, 5xx → `provider_unavailable`. Retry-After parsed only as integer seconds and not acted on. **400/422 → `transport`** (misleading). |
| **Context limits** | **No model context-window knowledge.** Optional byte budget, off by default, refuses to send rather than trim. A provider-side overflow is a 400 → `transport`, and with the ladder on it would wrongly step the format down. Anthropic `model_context_window_exceeded` maps to `Other` → `Complete` (`fx-single-attempt/src/mapping.rs`; `fx-core/src/normalize.rs`). All untested. |
| **Usage and caching** | OpenAI: prompt, completion, `cached_tokens` (included in input), reasoning; cache write always `None`. Anthropic: input (excludes cache), cache read, cache creation, output; reasoning `None`. Semantics chosen by provider string; unknown stays `null`. Anthropic breakpoint on by default; `--prompt-cache on` refused for OpenAI-compatible (`tests/token_accounting.rs`, `tests/usage_accounting.rs`). |
| **Invalid or incomplete output** | Truncation → `length_limited` (partial text never parsed, `tests/incomplete_response.rs`). Refusal → `content_filter`. An empty reply goes through re-ask. A stream without a terminal marker fails. |
| **Embedder path** | `WorkRuntime::with_provider` accepts any `ModelProvider` (`tests/byo_model.rs`). |
| **Test caveat** | About 20 binary-level test files return early when `pax` is not installed, so a CI without PAX passes them vacuously. |

### 5.2 Capability matrix (provider × model, from saved artifacts)

| Provider / endpoint | Model | Classification | Evidence |
| --- | --- | --- | --- |
| Anthropic Messages (native) | claude-opus-5-5 | **Verified by successful runs** | `ab-adaptive-context-v1` (5+5 course-000, 3+3 T4); coding-benchmark run2 (3/4) |
| Anthropic Messages (native) | claude-sonnet-5-5 | **Verified** | agent-bench run4 13/15, run5; AOC 2026-10-28 |
| Anthropic OpenAI-compatible endpoint | claude-haiku-5-5 | **Verified** (OpenAI wire, not native) | bake-off `runs/stage*-haiku.jsonl` |
| OpenAI | gpt-5.5-2026-04-23 | **Verified** | agent-bench run4 15/15; bake-off reference 26/28 |
| OpenAI | gpt-5.4-nano | **Verified, low success** | bake-off 23/59 |
| LM Studio | Gemma 4 26B (qat) | **Verified, low success; known schema loop** | agent-bench 6/15; `native-provider.md` |
| LM Studio | qwen3.6-35b-a3b | **Verified, partial** | apple-fm-bench baseline 9/15 |
| llama.cpp server | Qwen2.5-Coder-3B | **Runs; tasks mostly fail** | small-model-reliability 3/12 |
| oMLX | Qwen3.6-35B-A3B | **Runs; known to fail the task** (stuck) | qwen36-omlx 0/23 |
| "Locals" server | Qwen3.6-35B / Qwen3.5-4B | **Partial / known to fail**; results uncommitted | coding-benchmark local-models |
| Apple `fm serve` | system | **Known to fail** (400 on `json_object`/`anyOf`, schema hangs) | apple-fm-bench |
| Vercel AI Gateway | mercury-3, Gemma, gpt-5.4-nano | **Verified** (mercury at ceiling; others low) | native-skills, bake-off |
| Ollama `/v1`, vLLM, llama-cpp-python | any | **Implemented, unverified** (profiles from docs) | `work_generation.rs` |
| OpenRouter | any | **Implemented, unverified**; attribution headers not reachable from the CLI | `native-provider.md:154` |
| Ollama `/api/chat`, any third wire, native tools, images | — | **Unsupported** | `stack-agent/src/lib.rs` refusal; `release-limitations.md` |
| Proxy, private CA, LAN-HTTP, non-streaming, json-object ladder | — | **Scripted/mock tests only** | `fx-native-provider/tests/*`, `chip-fx-native/tests/*` |
| Live smoke test | — | **Unknown**: `tests/live_smoke.rs` is `#[ignore]`, with no saved output | — |

Being able to produce text or JSON does not show that a model can drive this loop. The decisive measured property is
**valid decisions that lead to grounded progress**. In the recorded runs that worked only for hosted frontier models
and a few ≥ 26B local models.

### 5.3 Minimum adapter contract

- **What the loop depends on (Read):**
  - `ModelProvider::complete(ModelRequest) -> Result<ModelResponse, FxError>`;
  - an `InferenceFailure` with an honest `Delivery`, so the runtime knows whether a call may have been billed;
  - `Usage` fields as `Option`;
  - output text that is ideally one JSON object.
- **Adding an OpenAI-compatible server needs no code.**
- **A new wire touches about 10 identity switches outside the loop:**
  - `provider_selection.rs` (default key variable, path suffix, string → kind);
  - `chip-fx-native` (config, the `NativePort` enum, credentials);
  - `work_generation.rs` (profiles, wire validation);
  - the ladder rungs;
  - `token_usage.rs` and `token_report.rs` (cache semantics);
  - the default-provider and Ollama refusal in `stack-agent/src/lib.rs`.
- **Proposed contract additions:**
  1. a provider descriptor (wire, cache-input semantics, supported structured-output modes, key variable,
     keyless rule) replacing the string switches;
  2. an explicit `context_window` per model profile, a `context_exceeded` failure kind, and the Anthropic stop
     reason mapped to it;
  3. 400/422 classified as `invalid_request`, never `transport`;
  4. a retry/backoff policy owned by the runtime, outside the loop, honouring Retry-After (both forms) for 429 and
     529 only, with delivery certainty preserved.

### 5.4 Conformance suite needed

**Layer 1, offline (scripted servers; most exist, so collect them under one name and run them in CI per wire):**
- streaming and non-streaming framing;
- usage fields, unknown never 0;
- cache semantics;
- truncation, refusal and empty reply;
- missing terminal marker;
- 401/403/429/5xx/400/422 classification;
- context overflow (new);
- cancellation mid-stream with delivery certainty;
- timeouts;
- key never echoed.

**Layer 2, live and paid (opt-in; run once per provider × model before any "supported" claim):**
- **Workload:** a fixed set of 5–8 small tasks of each kind (`inspect`, `verify`, `change` with an acceptance
  check).
- **Settings:** each structured-output mode the profile allows, n ≥ 5.
- **Recorded:**
  - valid-decision rate;
  - rejected-reply rate;
  - grounded progress;
  - verified and accepted;
  - false completions;
  - tokens, cache and latency.
- **Publication:** a profile becomes "measured" only from this run's saved artifacts.

---

## 6. Production-readiness gates

"Local" means a trusted developer machine; "hosted" means a multi-tenant service running untrusted repositories.
Severity rates impact if shipped as is.

| # | Dimension | Current evidence | Status: local / hosted | Blocking gaps (severity) | Acceptance criterion | Depends on |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | **Correctness and deterministic control** | Strict parser, bounded re-ask, runtime-owned completion, safety audit; 0 unauthorized executions in 970 runs; probe 2 | conditionally ready / not ready | Goal-level acceptance missing (high; P0-01, probe 2); PAX timeout ends as `Failed` (medium) | A `change` work with an external acceptance predicate verifies only when it holds; probe 2 ends `verified: false` with a reason; a scripted PAX timeout is an observation | Work Contract (RIC-02) |
| 2 | **Inference portability** | Two wires; neutral types; matrix §5.2 | conditionally ready / conditionally ready | No context-window handling (high); 400 → transport (medium); no backoff (medium); unmeasured profiles labelled as profiles (low) | Conformance layer 1 green in CI per wire; context overflow classified and tested; each "supported" model has a layer-2 artifact | §5.3 descriptor |
| 3 | **Tool safety and authorization** | Allowlist, confinement, protected paths, AppPort pins, injection corpus | conditionally ready / not ready | **Secrets inherited by PAX and reaching the model (critical hosted, high local)**; no sandbox (critical hosted); unprotected test/script files (medium) | `PaxExecutor` builds the child env from an allowlist; probe 1 shows no key in PAX's env or any request; hosted: tooling runs only in an isolating provider | Isolating environment provider (P0-06) |
| 4 | **Durable state and crash recovery** | Fencing, intents, kill tests, admission digest, continuation check | conditionally ready / not ready | No heartbeat (high); no forced takeover (medium); tenant work not durable (high hosted); lost receipt bars verification (medium); no pruning (medium) | A 1 s lease survives a 10 s model call (heartbeat test); takeover after verified cleanup; tenant works recover after restart; FeltDB retention bound tested | Child cleanup (#5) |
| 5 | **Cancellation and process lifecycle** | Boundary cancel, `--abort-in-flight`, group kill on timeout or drop | conditionally ready / not ready | SIGKILL and `serve`-exit orphans (high); `serve` doesn't abort in-flight (medium) | After SIGKILL of the agent or SIGTERM of `serve`, no process from the run survives (test with a sleeping test suite); `serve` cancel ends a running test within N s | Sandbox (hosted: container teardown) |
| 6 | **Observability and auditability** | Events JSONL, SSE, OTel after the run, durable work events | conditionally ready / not ready | No live spans or metrics; nothing for tenants or `--no-state` (high hosted); unstructured logs (medium); observation text not retrievable (medium) | Every work, including tenant works, has a durable, redacted event trail and an exported trace; structured logs with run and tenant ids | Durable tenant work (#4) |
| 7 | **Token/cost accounting and budgets** | Per-call ledger, cache categories, `token-report`, token and cost budgets in `work` | ready (Anthropic/OpenAI) / not ready | No budgets in `serve` (high hosted); cost needs operator prices (low); rejected-reply cost not broken out (low) | `serve` enforces per-tenant token and cost budgets with `ended_by`; the report breaks out rejected-reply tokens | — |
| 8 | **Local installation and operations** | Installer tested; `doctor`; build from source | conditionally ready / n/a | No published release (high); PAX version contract inconsistent (medium); doc drift §3.3 (medium) | A signed release published with its checksums; `doctor` checks the PAX minimum per capability; CI pins a PAX range that includes the installed release; §3.3 corrected | Green CI (#10) |
| 9 | **Hosted isolation and security** | Tenant tokens, ownership, path confinement | n/a / not ready | No sandbox, custody, egress control or per-run workspace (critical); unauthenticated Docker default (high); unbounded connections (medium) | §4.B requirements met and pen-tested by the hostile audit's scenarios run inside the isolating provider | Compute or another isolating provider; key-injecting proxy |
| 10 | **Test coverage and release discipline** | Large named test suite; CI defined | **not ready / not ready** | **CI red on 40/40 `main` pushes since 2026-10-21; every gated suite skipped (critical)**; tests skip silently without PAX (medium); WIP commits merged without a full-suite run (medium) | `main` green with `--no-fail-fast` showing 0 failures; tests that need PAX fail rather than skip when CI declares PAX; merges gated on green CI | — |

There is no overall score. Dimension 10 blocks every other "conditionally ready", because none of their tests has a
recorded pass on current `main`.

---

## 7. Roadmap (phased)

Each step is classified by how it can be validated: **offline** (scripted tests, existing artifacts),
**implementation** (new code with offline tests), **paid** (provider runs) or **infra** (hosted infrastructure).
Existing roadmap ids are given; **new** items are proposed for the roadmap's owner to add.

### Phase 0: restore the baseline (offline; days)
1. **Fix `chip-experiments` `replay_reaches_the_same_verdicts_from_the_recording_and_executes_nothing`.** Then run
   the workspace with `--no-fail-fast` and fix or triage every failure, never skip or disable. Make the 0.4.1-pinned
   PAX tests accept the supported range, or pin CI and local alike. Make PAX-dependent tests fail, not skip, when CI
   declares PAX. **New: RA-01.**
2. **Correct the doc drift in §3.3.** **New: RA-02.**
3. **Commit the AOC records.** Merge agent-obstacle `claude/peaceful-gates-zhpb4h`, or extract its evidence, so the
   A/B records are on a default branch.

**Exit:** CI green on `main` including every gated step; the full-suite result recorded at that commit.

### Phase 1: close the local-safety gaps (implementation + offline; about a week)
4. **Build the PAX child environment from an allowlist** (PATH, HOME, toolchain variables, operator-declared
   additions). Optionally redact known secret values from observation text. Probe 1 becomes a CI test. **New:
   RA-03**, under P0-06.
5. **Goal-level acceptance for `change`:** at minimum, refuse `verified` when the baseline was already green and no
   operator acceptance predicate exists. Probe 2 becomes a CI test. **P0-01 / RIC-02.**
6. **Classify context overflow and 400/422 correctly;** map Anthropic `model_context_window_exceeded`; add a per-model
   context window to profiles. **New: RA-04**, under P2-01.
7. **Report a PAX timeout as an observation, not `Failed`.** **New: RA-05.**

**Exit:** probes 1 and 2 and the new classification tests pass in CI; no runtime default changed except the PAX
child environment.

### Phase 2: recovery and lifecycle (implementation + offline; 1–2 weeks)
8. **Lease heartbeat** during model calls and executions. **Orphan cleanup:** record the PAX group id in the intent;
   on takeover or startup, kill a surviving group before re-running; use `PR_SET_PDEATHSIG` where available.
   `serve` uses abort-in-flight on cancel and drain, and drops in-flight futures before exit. **P0-04, P0-08, HA-12;
   new RA-06 for the serve path.**
9. **Allow `pax.test` to re-run after a settled or lost receipt** (PAX is read-only). **P1-02.**
10. **Align goal ceilings** (16 KiB admission vs 32 KiB flag). **New: RA-07.**
11. **Selector decision:** do transient failures count in the repeated-failure rules? Decide and test offline. The
    `replay` default stays.

**Exit:**
- a 1 s lease survives a 10 s scripted call;
- SIGKILL of the agent leaves no process (test);
- the AOC process-interruption scenario verifies in a scripted run.

### Phase 3: portability conformance (offline, then paid; 1–2 weeks)
12. **Provider descriptor (§5.3) and conformance layer 1 in CI.** **P2-01, P2-05.**
13. **Conformance layer 2** for the models the project intends to support. **Paid; needs explicit authorization and a
    pre-registered plan.**

**Exit:** each listed model has a saved layer-2 artifact; unmeasured profiles are labelled unverified in `--help`.

### Phase 4: controlled efficiency experiments (paid; only if warranted)
14. **Re-run `replay` vs `auto` on T4** only after:
    - the selector decision (item 11);
    - an AOC course-001 v3 with one acceptance contract the agent can see;
    - the Python-provider test resolved or `knownFlaky` in that version.

    Pre-register; n ≥ 5 per arm; record reply text. About $13 per 3+3 at observed costs. **RIC-09.** Do not run it
    before these conditions hold. The current data gives no reason to expect a large effect.

### Phase 5: hosted service (implementation + infra; weeks to months)
15. **Isolating environment provider:** disposable workspace, sandboxed tooling, default-deny egress, no keys inside.
    Keys held by a proxy outside the sandbox. **P0-06 (blocked on this), D-07, HA-01.**
16. **Durable tenant work,** per-tenant stores and budgets, auth always on for non-loopback, TLS, connection and rate
    limits, retention and purge, structured logs and audit, CI-built image with a health check. **P1-O1–O4, new
    RA-08.**
17. **Re-run the hostile audit inside the isolating provider.**

**Exit:** §4.B requirements met; hostile-audit scenarios fail closed inside the sandbox.

**What can be done without new spend:**
- **Existing artifacts and offline tests:** Phases 0–2, Phase 3 layer 1, and the selector decision.
- **Paid runs:** Phase 3 layer 2 and Phase 4.
- **Hosted infrastructure:** Phase 5.

---

## Appendix A. Unresolved questions

1. Why does `chip-experiments` replay disagree with its recording? How many other workspace tests fail on current
   `main` (`--no-fail-fast`)?
2. What causes the T4 Python-provider test failures on the unmodified fixture (`mvsl12st8320a0ae9981`) and with seq
   6's diff?
3. Should a PAX-classified transient failure count in the repeated-failure rules?
4. Where does goal-level acceptance come from (operator predicate, project checks, submitter)? This is the design
   question behind P0-01.
5. Who owns the isolating environment provider (Compute or another), and what is its contract?
6. Is PAX 0.5.0 compatible with every PAX-dependent test? CI pins 0.4.1; runtime use with 0.5.0 is observed only in
   the A/B and these probes.
7. Does `auto` save anything on long tasks once the spurious escalation is gone? Unmeasured.
8. Why did `auto` halve output on course-000? Reply text was not recorded.

## Appendix B. Missing evidence

- No recorded full-suite pass at `5a273e3`, `64ce740` or `4a9b05b`.
- The AOC 2026-10-28 run directories exist only on this machine (`~/.aoc/runs`). The batch survives in the
  repositories only as plan summary figures.
- Native-provider live checks for Anthropic and LM Studio are owner-reported, with no committed output.
- `tests/live_smoke.rs` has no saved run.
- PR36–38 live runs, the micro-model real record and the schema-closed probe have no artifacts.
- Peak memory, per-call latency and time-to-first-token were never measured.
- No real-model measurement of prompt injection.

## Appendix C. Claims that must not yet be made

- "Production ready", "hosted-ready" or "multi-tenant safe".
- "Model-agnostic" without qualification. Say instead: two wires; the loop is provider-independent; per-model support
  is established only by conformance runs.
- "`verified` means the task was done." Say instead: tests passed after a content-changing write (P0-01).
- "Secrets never reach repository code or the model." Probe 1 shows they do.
- "`auto` context reduces tokens or cost."
- "Prompt caching saves N%" as a controlled result. Say instead: a projection against a different build and
  configuration.
- "Supports Ollama, vLLM, OpenRouter, llama-cpp-python." Say instead: profiles exist and are unmeasured.
- "Recovers from interruptions": it recovers safely but does not reach verification after a lost receipt.
- "No orphaned processes": true only when Stack-Agent itself does the killing.
- "Tested" in the sense of a CI pass on current `main`: none exists (§1.3).
- That the T4 Python-provider failure is harmless flakiness or a defect in the agent's change.
