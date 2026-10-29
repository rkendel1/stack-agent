# Efficiency experiments: token economics, prompt caching and adaptive context

What the token-economics work, prompt caching and adaptive context (`--context-profile`) have shown so far about
Stack-Agent's resource use, correctness and recovery. It interprets the records; it does not replace them. Every
number comes from a saved artifact in [`efficiency-experiments/`](efficiency-experiments/), byte-for-byte copies whose
origin and sha256 are in [`PROVENANCE.json`](efficiency-experiments/PROVENANCE.json).

The runs were made by the Agent Obstacle Course harness (rkendel1/agent-obstacle). Course- and harness-level
lessons are recorded there, in `experiments/LESSONS.md`: the T4 acceptance contract, and the course fixture's
Python-provider test. Registry entry: [`bench/registry/experiments/ab-adaptive-context-v1.json`](../../bench/registry/experiments/ab-adaptive-context-v1.json).

Labels:

- **Observed**: read directly from a saved artifact.
- **Interpretation**: an explanation the evidence supports, with the evidence named.
- **Hypothesis**: plausible, not established.
- **Proposed**: an action that has not been implemented or validated.

Recorded 2026-10-29 against `claude/adaptive-context` @ `4a9b05b`; no model was called to write it.

## 1. Findings

1. **Observed.** `--context-profile auto` did not reduce input tokens or cost in the only controlled comparison
   (`ab-adaptive-context-v1`, Opus 5.5, prompt cache on, build `4a9b05b`).
   - course-000: median total input 12,137 (auto) vs 11,280 (replay) per run.
   - T4: 3,139,869 vs 3,000,775.
   - Correctness: course-000 5/5 vs 5/5 PASS; T4 2/3 vs 3/3 PASS.
   - `replay` stays the default.
2. **Observed, root cause confirmed in the traces.** On T4, the selector moved Standard → Deep in every `auto` run,
   citing "conflicting evidence". The pair was always the first two `pax.test` executions: the harness's injected
   transient infrastructure failure, then a real run. Deep was then used for 27–34 of 45–47 calls (§3).
3. **Observed.** Prompt caching dominates input cost: 93–95% of T4 input tokens were cache reads, and cached T4 runs
   cost about $2.00–$2.44 each (§4).
4. **Observed.** Stack-Agent never reported `verified` on T4, in either arm, even when the independent evaluator
   passed the work.
   - **Interpretation:** the course's agent-visible test suite includes an audit snapshot the evaluator excludes, so
     the runtime's honest status cannot match the evaluator there (§5). That is a course contract issue, not a
     runtime defect.
5. **Observed in tests.** A durable-state defect let a resumed task continue with only part of its goal. It is fixed
   on this branch (§6).
6. **Observed in code and a probe.** The lease is renewed only before executions, never during model calls. A run
   of slow calls can lose it, and that is why interrupted runs wait about 6 minutes before takeover (§6).

## 2. Controlled comparison `ab-adaptive-context-v1`

**Setup.**
- **Plan:** [`ab-adaptive-context-v1/plan.json`](efficiency-experiments/ab-adaptive-context-v1/plan.json), with its
  amendments recorded in the file.
- **Build:** `4a9b05b` (sha256 `08f6431deec7f5e2021019e7707e07c266544d8c1d7ea84b4aac955acae6548a`).
- **Model and settings:** `claude-opus-5-5`, `--prompt-cache on`, durable state on, budgets 50/50/1200 s/300 s/16,384.
- **Evaluator:** the Agent Obstacle Course evaluator `@aoc/evaluator@1.1.0+dcdbd8767c7d`.
- **Arms:** `--context-profile replay` vs `auto`, launched in ABBA order, one run at a time.

**Metrics.**
- **Primary:** total input = uncached + cache read + cache write.
- **Cost:** computed from provider-reported token counts at the plan's prices (input $4, cache read $0.20, cache
  write $5, output $20 per million tokens). Anthropic does not report a dollar amount.

### course-000 (Rust off-by-one, obstacle transient-test-failure)

Source: [`course-000.results.json`](efficiency-experiments/ab-adaptive-context-v1/course-000.results.json).

| Per run | replay (n=5) | auto (n=5) |
| --- | --- | --- |
| Evaluator outcome | 5 PASS | 5 PASS |
| Obstacle injected / recognized / recovered | 5/5/5 | 5/5/5 |
| Agent status (`completion`) | completed, verified by runtime: 5/5 | completed, verified by runtime: 5/5 |
| Profiles | replay (recorded as `none`) on every call | Standard on every call |
| Model calls (= executions) | 4, 4, 5, 4, 4 | 4 in all |
| Total input: median (range) | 11,280 (11,273–15,713) | 12,137 (12,121–12,167) |
| Uncached / cache read / cache write: medians | 676 / 9,460 / 1,160 | 1,542 / 9,458 / 1,146 |
| Output: median (range) | 825 (736–1,200) | 440 (413–604) |
| Cost per run: median (sum) | $0.0283 ($0.1553) | $0.0226 ($0.1157) |
| Agent time: median | 12.9 s | 9.6 s |
| Rejected replies | 0 | 0 |

### T4 (TypeScript resolver defect, appport @ 268c9b4, default obstacle: transient-test-failure)

Sources: [`t4.results.json`](efficiency-experiments/ab-adaptive-context-v1/t4.results.json) and
[`t4.evidence.md`](efficiency-experiments/ab-adaptive-context-v1/t4.evidence.md). The regression results come from the
evidence file; the results JSON has no regression field.

| Seq · arm | Outcome | Regression | Calls / execs | Rejected | Total input | Uncached / cache read / cache write | Output | Cost | Agent time | Profiles | Agent status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 1 · replay | PASS | pass | 47 / 38 | 8 | 2,961,993 | 62,544 / 2,804,951 / 94,498 | 39,845 | $2.08 | 594 s | replay | escalated |
| 2 · auto | PASS | pass | 47 / 35 | 11 | 3,198,100 | 99,681 / 2,948,861 / 149,558 | 35,069 | $2.44 | 564 s | Deep 34, Standard 13 | escalated |
| 3 · auto | PASS | pass | 46 / 39 | 6 | 3,139,869 | 97,552 / 2,941,936 / 100,381 | 35,494 | $2.19 | 591 s | Deep 32, Standard 14 | escalated |
| 4 · replay | PASS | pass | 50 / 45 | 5 | 3,429,085 | 74,636 / 3,250,189 / 104,260 | 37,375 | $2.22 | 595 s | replay | limit_reached |
| 5 · replay | PASS | pass | 46 / 37 | 8 | 3,000,775 | 60,026 / 2,830,573 / 110,176 | 34,770 | $2.05 | 526 s | replay | escalated |
| 6 · auto | **FAIL** | 1 failed | 45 / 39 | 5 | 3,133,869 | 82,309 / 2,949,366 / 102,194 | 28,297 | $2.00 | 455 s | Deep 27, Standard 18 | escalated |

- **Medians:**
  - total input 3,000,775 (replay) vs 3,139,869 (auto, +4.6%);
  - output 37,375 vs 35,069;
  - cost $2.08 vs $2.19;
  - calls 47 vs 46;
  - agent time 594 s vs 564 s.
- **Observed:** all 6 runs passed the 8 hidden acceptance tests and recovered from the obstacle.
- **Observed:** no run's agent status was `verified`.
- **Observed:** seq 6's FAIL is one regression test in the course fixture
  (`spec/test/behavioral-replacement-e2e.test.ts`, a Python-provider test). That investigation belongs to the course
  and is recorded in agent-obstacle `experiments/evidence/python-provider-diagnosis/`. It is unresolved, and not
  attributed to the context profile.
- **Observed:** stage costs were $0.27 (course-000) and $12.97 (T4).
- **Scope:** 5+5 and 3+3 runs are a screen, not proof.

**Separate, earlier baseline** (plan field `baseline`). The 2026-10-28 batch used build `35d7af2`, no prompt caching
and `--no-state`, so it is not comparable with the A/B:
- Opus T4: 42 calls, 2,837,169 input and 31,054 output tokens.
- Sonnet T4: 48 calls, 3,437,273 and 44,890.

## 3. Context selection

- **Observed:** each `auto` T4 run switched Standard → Deep at call 14, 15 or 19. The reason was "conflicting
  evidence: executions … answered the same request differently with no change by this work between or since"
  ([`t4.evidence.md`](efficiency-experiments/ab-adaptive-context-v1/t4.evidence.md)).
  - The cited executions are each run's first two `pax.test` intents.
  - In the harness's PAX shim log, #1 is the injected obstacle (`status: error`, `reason:
    transient-infrastructure-failure`) and #2 is a real run (`status: failed`, `reason: tests-failed`). So the
    transient error was followed by a genuine failing run, not a success.
  - The runs then stayed in Deep for 34/47, 32/46 and 27/45 calls.
- **Interpretation:** `adaptive_context.rs` treats an execution PAX explicitly classifies as a transient
  infrastructure failure as conflicting evidence about the code. Deep adds sections to every later user message,
  after the cache breakpoint, which is consistent with the extra uncached input: `auto` median 97,552 vs 62,544 for
  replay, with similar cache-read volume.
- **Proposed:** an execution PAX classifies as a transient infrastructure failure is not one side of "conflicting
  evidence", whatever follows it (a pass or a real failure). Genuine contradictions between real results, unresolved
  failures and regressions still escalate; missing or unknown classifications keep today's behavior. Validation
  required:
  - deterministic selector tests;
  - an end-to-end scripted run of the transient-then-real sequence;
  - `replay` byte-identical;
  - then a controlled re-run.
  - In progress on branch `claude/selector-transient-fix`; not yet validated.
- **Why course-000 showed no saving (interpretation):** 4–5 calls and no re-read file give Standard nothing to
  condense, while its task-state section is billed as uncached input (+866 median per run). The "Known limitations"
  in [`adaptive-context.md`](adaptive-context.md) anticipated this.
- **Why T4 established no improvement (interpretation):**
  - the spurious Deep use dominated the `auto` runs;
  - one `auto` run failed the correctness gate;
  - 3 runs per arm cannot resolve ±5% (replay total input ranged 2.96M–3.43M).
- **Open:** `auto` on T4 without the spurious escalation is unmeasured.

## 4. Prompt caching

- **Observed:** with `--prompt-cache on`, cache reads were 80.4% / 77.9% of total input on course-000 (replay / auto,
  pooled) and 94.6% / 93.3% on T4. Cache writes were ~1.1–1.2K per course-000 run (3,160 on the first, cold run) and
  94K–150K per T4 run.
- **Projection, not a controlled comparison:** the uncached 2026-10-28 Opus T4 run (2,837,169 input, 31,054 output)
  would cost about $11.97 at the same prices, against $2.00–$2.44 for cached runs. That run used a different build,
  `--no-state`, and no caching.
- **Lesson:** token volume and cost are different metrics. Cached tokens are input tokens (the primary metric counts
  them) but priced differently, here $0.20 against $4 per million. A fall in uncached input alone is not an
  input-token reduction.
- **Provenance:** the breakpoint and the Anthropic cache-write reading came with `d86ae35` and `361ac17`
  ([`token-usage.md`](token-usage.md)). Earlier reports have unknown cache writes, not zero.

## 5. Agent status versus independent evaluation

- **Observed:** on course-000 they agreed (10/10 completed and verified, 10/10 PASS).
- **Observed:** on T4 the agent's last own test run had exactly 1 failure in every run, and every run's evaluator
  audit-snapshot check (`spec/test/audit.test.ts`, recorded but not scored) failed
  ([`t4.evidence.md`](efficiency-experiments/ab-adaptive-context-v1/t4.evidence.md)).
- **Interpretation:** adding a test file breaks the course's audit snapshot; the agent's `pax test` runs the full
  suite and sees it, while the evaluator excludes it. The runtime's `escalated` / not verified is correct by its own
  rules, as `ed06459`'s `completion` section shows (tests run, last run after the change, failures).
  - **Caveat:** PAX reports counts, not test names, so the single failure is inferred to be the snapshot.
- **Lesson:** keep the runtime's completion status and the external evaluator's verdict as separate signals. The
  fix belongs in the course (a v3 with one consistent acceptance contract), not in Stack-Agent.

## 6. Durable state and leases

- **Observed, fixed on this branch:**
  - Goals over FeltDB's 1,024-byte objective limit are stored as a bounded objective with a SHA-256 digest
    (`3eb0b42`).
  - A continuation could then pass that stored objective as its goal and continue with part of it.
  - `work_state::check_continuation` (`46344ae`) requires the goal to equal the admission record's goal byte for
    byte, with matching length and digest. A legacy cut objective with no readable admission record is refused.
  - Tests: `crates/stack-agent/tests/objective_limit_durable.rs` (the real 1,829-byte T4 goal, fixture
    `tests/fixtures/objective/t4-goal.txt`: kill, refused bad resume, correct resume). Unit tests in `work_state.rs`
    cover boundaries, multi-byte UTF-8, 16 KiB, missing or corrupt records and legacy tasks. See
    [`agent-state.md`](agent-state.md).
  - In the A/B, all 6 T4 reports show `agent_state.recorded: true`. Before `3eb0b42`, every T4 run with durable
    state was refused (`FeltDB refused create_task: CONTRACT_INPUT_INVALID`).
- **Observed, lease hazard:**
  - The lease is renewed before each execution, never during or between model calls
    ([`agent-state.md`](agent-state.md)).
  - With `--lease-secs 1` and a slow call, a work ended with `FeltDB refused checkpoint: LEASE_EXPIRED` (exit 4)
    after a write had executed ([`lease-gap-probe.out`](efficiency-experiments/evidence/lease-gap-probe.out), source
    [`lease-gap-probe.rs`](efficiency-experiments/evidence/lease-gap-probe.rs)).
  - **Hypothesis:** enough slow or rejected calls between executions could do the same at a 360 s lease.
  - **Observed:** `pax test` runs in its own process group, so it can outlive a killed agent. That is why an early
    takeover is not simply safe.
  - The Obstacle Course harness waits `lease + 2 s` after a kill: about 6 minutes for a 360 s lease.
- **Proposed, separate change:** renew the lease on a heartbeat while a call or tool is in flight, and clean up
  orphaned test process groups on takeover; only then shorten leases.

## 7. Token instrumentation

From [`token-usage.md`](token-usage.md) and the `token-report` analysis of the historical Opus T4 run
([`token-report.opus-t4-historical.json`](efficiency-experiments/evidence/token-report.opus-t4-historical.json); input
fixture `crates/stack-agent/tests/fixtures/token_report/opus-t4-historical.json`):

- **Observed:**
  - Input grew from 2,482 tokens (call 1) to 101,180 (call 42), and requests were append-only.
  - Up to 2,735,989 tokens (96.4%) repeated the previous request. This is an upper bound from counts, not a measured
    byte comparison.
  - The largest step added 13,586 tokens, carried by 40 later calls.
- **Lessons:**
  - Measure per call.
  - Keep cache read, cache write and uncached input separate under each provider's semantics. Unknown is `null`,
    never 0.
  - Record the profile and selection reason on every call. That is how the spurious Deep escalation was found.
  - Byte sizes and estimates are proxies.
- **Not yet mined:** rejected replies were 5–11 per T4 run in both arms, each a full-context call. Their cost has not
  been broken out.

## 8. Method lessons

- **Keep an exact legacy request as the default.** A first default of `auto` broke the exact-request tests. `replay`
  is now checked byte for byte against requests from `aa72067` (`tests/replay_equivalence.rs`).
- **Compare every failing test with the exact base.** The adaptive branch's apparent `pax_work`/`verify` regressions
  came from an exported `CARGO_TARGET_DIR`; five other failures already failed on base
  ([`adaptive-context.md`](adaptive-context.md), Validation status).
- **Separate the correctness gate from efficiency metrics.**
- **Isolate builds and record identity:** named build, commit, sha256, and effective settings read from the
  agent's own report.
- **Validate offline first** with scripted or fake providers (one conversation per fake server).
- **Pre-register metrics and stop rules.** Treat small samples as screens.

## 9. Next steps (ranked)

| # | Step | Evidence | Impact | Risk | Validation | Paid calls |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Selector: PAX-classified transient failures are not "conflicting evidence" (in progress: `claude/selector-transient-fix`) | §3 | Removes most Deep use on T4 | Low | Deterministic selector tests, scripted end-to-end run, replay byte-identical | No |
| 2 | Mine the saved per-call ledgers (rejected replies, growth per profile) | §7 | Targets the next optimization | None | `stack-agent token-report --json` on saved reports | No |
| 3 | Controlled replay vs auto re-run after 1 (and after the course's v3 contract fix) | §2–§5 | First fair reading of auto on T4 | Spend; small n | Same build except the fix, ABBA, pre-registered | **Yes** (T4 ≈ $13 per 3+3 at observed costs) |
| 4 | Lease heartbeat and orphaned test-process cleanup | §6 | Removes ~6 min per interruption; closes a lease-loss hazard | Medium (concurrency) | Offline slow-call probe; prompt, duplicate-free takeover | No |

## Missing evidence

- The adaptive-context validation's per-test logs are not durable; [`adaptive-context.md`](adaptive-context.md) holds
  the summary.
- The 2026-10-28 batch exists here only as the plan's summary figures.
- The single T4 test failure seen by the agent is inferred to be the audit snapshot.
- `auto` without the spurious escalation, and the quality cost of Standard condensing pre-edit reads, are
  unmeasured.
- No dollar figure is reported by the provider; all costs are computed from token counts.
