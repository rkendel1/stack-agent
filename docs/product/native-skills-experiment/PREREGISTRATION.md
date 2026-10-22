# Pre-registration: does one reviewed workflow skill help a small local model on task E?

**Status: fixed for review. No trial has been run, and none may be run until this plan is approved
and inference is authorized.** Any change after approval is recorded in a dated amendment at the
end of this file, before the run it affects. Architecture and code:
[`../native-skills-experiment.md`](../native-skills-experiment.md).

This is a **diagnostic** experiment with one task, one model and one machine. Whatever it shows, it
does not establish general effectiveness.

## 1. Hypothesis and what would falsify it

- **H1 (workflow).** Given the skill's instructions, the model completes task E (independent oracle)
  more often than the same model given today's context.
- **H2 (not length).** Any such benefit exceeds the effect of adding reviewed, task-irrelevant text
  of the same length.
- **Falsified, or not supported:** the paired comparison of skill against baseline does not meet
  §8's criterion, or the skill does not beat the length control.
- **Boundary (must hold whatever the result):** zero unauthorized executions, zero attempted
  unauthorized actions, and zero completions or verifications that the oracle does not confirm, in
  every arm. Any violation stops the experiment (§9).

## 2. Task selection

The candidates were checked against the five criteria.

| Criterion | Task E (agent bench) | Coding benchmark T4 (AppPort resolver) | Coding benchmark T1 (Compute auth) |
| --- | --- | --- | --- |
| Documented, reproducible failure | **Yes**: 6 of 6 failed at the final small-model binary (below) | **No longer**: solved 8/8 by `claude-opus-5-5` in run 2 | Partial (3/4) for `claude-opus-5-5`; no small-model run |
| Runnable offline here | **Yes**: no dependencies, `cargo test --offline` | Needs pnpm, Node and the AppPort checkout | Needs the Compute checkout and a warm cache |
| Independent evaluator | **Yes**: `agent_bench::judge` plus the harness's own `cargo test` | Yes (hidden acceptance) | Yes (hidden acceptance) |
| The test runner supports the build system | **Yes**: PAX on Cargo | **No**: PAX reports pnpm/vitest `unsupported`, so `change` cannot complete | Partly: one workspace test fails at the base, so every run escalates |
| Exercises what the skill targets | **Yes** (below) | — | Rust, but the blocker is the base-failing test |

**Selected: task E.**

**The failure trace, from the artifacts.** The source is the records in
`../small-model-reliability/runs/clean-final*/*E*`: Qwen2.5-Coder-3B-Instruct, binary `fde4c16`.

| Trials | What the model did | End |
| --- | --- | --- |
| 5 of 6 (3 `local-object`, 2 `local`) | `project.git.status`, then `project.git.diff` with `inputs:{}` (rejected), then escalate | `escalated`; never read `src/lib.rs`, never ran `pax.test` |
| 1 of 6 (`local`, `1792650467-E-local-1.json`) | `project.git.status`, a **correct** `project.write` of `src/lib.rs` (without reading it first), then `project.git.status` ×3 | `blocked` by the stuck guard. `pax.test` never ran, so the work could not complete |

**Why the skill could plausibly change useful-work completion, not only add a test call.** A
`change` work completes only when `pax.test` passes after the last write that changed a file
(`GoalKind::Change`). Task E's oracle requires that completion.

- In the one trial that made the right edit, a single `pax.test` stood between the work and
  verified completion.
- In the other five, the model never located the code or established the failure.

The skill's steps 1–2 (run the tests first; read the failing test and the code) target the
localization failure, and steps 5–7 target the missing verification. Whether a 3B model acts on
2.4 KB of instructions is unknown. The same model ignored shorter corrections before
(small-model-reliability, "What still blocks task A"). That uncertainty is the point of the
experiment.

**Limitation of task E.** Its "acceptance test" is the project's one test, which the goal describes,
and the oracle reruns it independently. There is no hidden test, and the regression suite is that
same test.

## 3. Fixed configuration

| Item | Value |
| --- | --- |
| stack-agent | the commit that merges this change. `run-schedule` refuses a checkout with tracked changes and records `stack_agent_commit` in `plan.json`; a different commit invalidates the run |
| Build | `cargo build --release -p stack-agent --example skill_experiment` (the runner is the example binary; the work runs inside it as `work_with_experiment`) |
| Task fixture | `agent_bench::build_fixture(TaskId::E)`, committed as `baseline` by `run_trial`. File hashes (from `skill_experiment check`): `.gitignore` `f9b1ca6a…f79805`, `Cargo.toml` `1d5fa16d…cf364a`, `src/lib.rs` `a63e0080…15d1a1`, `tests/policy.rs` `b7dbb2fc…55caf6` (full values in `plan.json`) |
| Goal | `The project's tests expect the maximum number of retries to be 5. Change the retry policy so the tests pass, without editing the tests.` |
| Evaluator | `agent_bench::judge` (task E) and `run_project_tests` (`cargo test --offline --quiet` in the fixture), unchanged, at the same commit |
| Model | Qwen2.5-Coder-3B-Instruct, Q4_K_M GGUF. **The full SHA-256 of the file must be recorded before inference** (earlier runs give `724fb256…`, matching the Hugging Face file). A different file invalidates the run |
| Server | llama.cpp `llama-server`, build `b1-c7fc509` as in small-model-reliability, CPU, `-c 16384 --jinja`, 4 threads, `127.0.0.1:1235`, started once before the schedule with these flags and not restarted. Its exact command line is recorded with the results |
| Provider cell | `--provider local`: profile `local`, the canonical decision schema (`structured_output: schema`), `max_tokens`, temperature 0.0. This is the product's default for the local profile, and the cell in which the only correct edit occurred. One cell only; `local-object` is not run |
| Generation | `--max-output-tokens 8192`, `--call-timeout-secs 120` (`agent_bench` constants) |
| Limits | task E's own: `--max-turns 14 --max-executions 10 --deadline-secs 600`, no context budget |
| State | `--no-state` (the bench's legacy arm) in every arm: no FeltDB |
| Arms | **baseline**: nothing added. **skill**: `repo.diagnose-failing-test@1`, digest `sha256:1d43d17f…8caa7c`, 2,416 bytes (`instructions.md` + `refs/rust.md`). **length control**: `control.length-v1`, digest `sha256:ad57e213…432dcc1`, 2,413 bytes. Both text arms are preceded by the same 151-byte label |
| PAX | the version on `PATH`, at least 0.4.1, recorded in `plan.json`; it must not change during the run |

**Why not 50 turns, 50 executions and 1,200 s.** Those are the coding benchmark's limits for large
repositories with a frontier model. Task E's own limits are the ones its recorded failures ran
under, so keeping them preserves comparability with that baseline. They are not tuned for this
experiment.

## 4. Isolation and preparation

- **Repository state.** Every trial gets a fresh fixture directory, built, committed, run, judged
  and then deleted (`run_trial`). No trial sees another's files.
- **Runtime state.**
  - `--no-state` means no FeltDB.
  - Each trial has its own `STACK_AGENT_DATA_DIR` under its artifact directory, so no learning or
    default database carries over. Learning is never injected into prompts in any case.
  - The fixture's `.stack-agent/` is new per trial.
- **Build cache.** Each fixture has its own `target/` (`CARGO_TARGET_DIR` in `run_project_tests`;
  PAX builds in the fixture).
- **Model-server cache.** It persists across trials. This is recorded (cached-token counts), not
  controlled (see §11).
- **Machine.** Nothing else heavy runs during the schedule. Note the machine and its load in the
  results.

## 5. Trials and order

- **10 rounds × 3 arms = 30 trials,** run sequentially on one machine.
- In round *r* the arms run in rotation *(r − 1) mod 3* of (baseline, skill, length control), so
  every arm takes every position equally often over each 3 rounds (`skill_experiment::schedule`,
  tested). Round 10 is rotation 0.
- One command runs the whole schedule:
  `skill_experiment run-schedule --trials 10 --out <new dir> --provider local --local-model <id> --local-endpoint http://127.0.0.1:1235/v1/chat/completions`.

## 6. Outcomes

All outcomes are computed after the run from `trials/*.json` and `artifacts/*/`. Failures stay in
every denominator.

**Primary outcome:** `task_complete`, the independent oracle verdict. It requires all of:

- the work completed, with `verified` true;
- `src/lib.rs` changed;
- the harness's own `cargo test --offline` passes;
- no other file changed;
- a clean audit.

**Secondary outcomes:**

1. **Agreement.** Whether Stack-Agent reported completion (`terminal_state == completed`, `verified`)
   and whether that agrees with `task_complete`. Any disagreement is a stop (§9).
2. **Verification behavior.** Whether `pax.test` ran (`executions_by_capability["pax.test"] > 0`),
   and whether a passing run followed the last write (`verified`).
3. **Turns to first useful edit.** The 1-based index of the first model reply that is a valid
   `project.write` of `src/lib.rs` which, applied to the fixture's `src/lib.rs`, makes
   `max_retries()` return 5 (the reference change is `MAX_RETRIES: u32 = 5`). Computed from
   `stderr.log`, and confirmed by `paths_written` in `report.json`. **Elapsed time** to it is not
   available from existing records and is reported as unmeasured.
4. **Turns and time to verified completion.** `context.model_calls` and `latency_ms` for completed
   and verified trials.
5. Model calls, executions by capability, repeated identical requests (`context.observations.repeated_identical`),
   rejected decisions (`malformed_replies`), and stuck-guard stops (`outcome_reason` beginning
   `stuck:`).
6. Input and output tokens, cached input tokens (`reported_cache_read_tokens`), added text bytes
   (provenance), latency, and cost (local: none, reported as 0 USD with the machine time).
7. **Determinism.** The number of distinct trajectories per arm (the sequence of requested
   capabilities and the terminal state).

## 7. Infrastructure failures

A trial is an infrastructure failure when no model reply was received and the failure is not a
decision or a limit: for example a transport error, a call timeout before any reply, the launcher
failing to start, or no report. The runner marks it (`infrastructure_failure`) and keeps every
artifact.

- **No trial is retried or replaced.**
- The primary analysis counts an infrastructure failure as not complete (intention to treat).
- A sensitivity analysis repeats the paired tests without any round that contains one.
- **More than 3 infrastructure failures invalidate the run.** It is then reported as inconclusive.

## 8. Analysis (fixed now)

**Per arm:** solve rate *k/10*, with a two-sided 95% Clopper–Pearson interval.

**Paired, by round:**

- skill vs baseline: exact two-sided McNemar (binomial on discordant rounds), α = 0.05;
- skill vs length control: the same test.

With 10 rounds the test reaches α = 0.05 only with at least 6 discordant rounds all favoring one
arm (6–0: p = 0.031), or 8–1 (p = 0.039). Anything weaker is reported as **no detectable
difference**. It is not a trend.

**Decision rule:**

- **"Workflow benefit on task E for this model":** skill > baseline at α = 0.05, **and** skill >
  length control at α = 0.05.
- **"Text-length effect, not attributable to the workflow":** skill > baseline significant, but
  skill vs control not.
- Otherwise: **"not supported"**.

A nonsignificant primary result stays not supported, whatever the secondary outcomes show.

**Secondary outcomes** are descriptive: medians with ranges, and paired differences with bootstrap
95% intervals (10,000 resamples of rounds, seed 1). No secondary outcome can establish a benefit.

**Determinism.** If an arm produced one trajectory in all 10 rounds, the report says its 10 trials
are effectively one observation, and the comparison is between single trajectories.

## 9. Stopping rules

- **Safety stop (automatic).** `run-schedule` stops after any trial with an unauthorized execution,
  an attempted unauthorized action, or a completion or verification that the oracle does not
  confirm. It writes `STOPPED.json` and keeps the trace. The experiment is reported as stopped,
  with that trace.
- **No early stopping** for efficacy or futility. All 30 trials run unless a safety stop or §7's
  limit ends them.

## 10. Invalidation

The run is invalid (reported, not analyzed as a result) if:

- the commit, model file, server build or flags, PAX version, task fixture or evaluator differ from
  §3;
- the checkout had tracked changes (the runner refuses this);
- an artifact pin was changed (the runner refuses this);
- any trial ran outside the schedule or was re-run;
- trials were run in parallel or the machine was under other heavy load;
- there were more than 3 infrastructure failures;
- artifacts are missing for any trial.

## 11. Known limits

- **One task.** E is a tiny Rust project, so the result does not transfer to repository-scale work.
- **One model, one cell, one machine.**
- **Temperature 0.** Trials may be near-identical, so effective sample size may be far below 10.
- **The llama.cpp prefix cache persists across trials.** Its effect on outputs is not controlled.
- **Elapsed time to first useful edit is unmeasured** (§6.3).
- **The evaluator's test is visible to the agent** (§2).

## Amendments

### Amendment 1 (2026-10-22, before any trial): API model instead of a local model

**Why.** Local inference is out of scope for this run. The OpenAI account behind `OPENAI_API_KEY`
refuses all inference (`billing_not_active`, see [`OPENAI-RUN-STOPPED.md`](OPENAI-RUN-STOPPED.md)),
so `gpt-6-luna` cannot be used. As the operator directed, the run uses the Vercel AI Gateway key in
the session (`AI_GATEWAY_API_KEY`) with Mercury 3.

**What changes in §3 (everything else in §§1–11 stands):**

| Item | Was | Now |
| --- | --- | --- |
| Model | Qwen2.5-Coder-3B-Instruct GGUF on llama.cpp | `inception/mercury-3` (Inception Mercury 3) |
| Endpoint | `http://127.0.0.1:1235/v1/chat/completions` | `https://ai-gateway.vercel.sh/v1/chat/completions` (OpenAI-compatible) |
| Provider cell | `local` (`--provider-profile local`, full schema, `max_tokens`, temperature 0.0) | `openai` (`agent_bench::ProviderId::OpenAi`): `--provider-profile openai`, so `schema-flat`, `max_completion_tokens`, **temperature omitted (provider default, not 0)**. It is the existing supported path, and earlier gateway runs used the flat schema |
| Credential | none | `AI_GATEWAY_API_KEY`, handed by the runner to each trial child only, as `OPENAI_API_KEY`. It is redacted from every artifact and never printed or recorded (only `credential_supplied: true`) |
| Transport | loopback | `SSL_CERT_FILE=/root/.ccr/ca-bundle.crt`: this environment re-terminates TLS at its proxy (the documented setting, `native-provider.md`) |
| PAX | "on `PATH`, ≥ 0.4.1" | `pax 0.4.1`, built by the CI procedure from `rkendel1/pax` tag `v0.4.1` at `674f3b3143874d1a692aca103b33f89da31a82ac`, binary `sha256 05e3d861417bdb6c210ba317679adf118c0041270e55b27511b8d47ee0b4cbf7` |
| Source identity | a commit; tracked changes invalidate | the operator has not authorized a commit, so the source is HEAD plus the saved tracked diff plus every untracked file's hash, folded into one fingerprint. It is recorded in `plan.json` (`source.diff` is saved beside it) and **re-checked before every trial**, together with the runner binary's hash; any change stops the run as invalid. This replaces the "tracked changes" item of §10 |
| Generation, limits, task, evaluator, arms, label, schedule | unchanged | unchanged: `--max-output-tokens 8192 --call-timeout-secs 120`, `--max-turns 14 --max-executions 10 --deadline-secs 600`, `--no-state`, task E, `judge` plus `run_project_tests`, the pinned skill and control digests, 10 rounds × 3 arms in the §5 rotation |

**Trial count.** Kept at 10 rounds (30 trials). The default temperature makes trials genuinely
stochastic, so the §8 analysis applies as written: the exact paired McNemar test with the same
thresholds, and the same decision rule. §11's "temperature 0" limit no longer applies. The trial
count is not changed after seeing any result.

**Spend ceiling and rule.**
- **Prices**, per million tokens, from the gateway's own `GET /v1/models` entry for
  `inception/mercury-3`: input $0.10, output $0.35, cached input $0.01.
- **Cost per trial** is computed by the runner from the usage fields each call reported
  (`reported_input_tokens`, `reported_output_tokens`, `reported_cache_read_tokens`). It is
  reported two ways: conservative (all input at the full price) and with the cache discount.
- **Ceiling: $5.00.** Before each trial, the runner stops if the conservative spend so far plus
  one worst-case trial ($0.06: 14 calls, each with up to 8k input and the 8,192-token output cap)
  would exceed the ceiling. A spend stop is reported as such; it is not an invalidation.

**Infrastructure failures (clarifying §7).** These are infrastructure failures:
- a gateway or provider error before any reply (HTTP 429, 5xx, transport or timeout errors);
- a reply that is not a valid stream.

The runner classifies them, keeps them in the denominator, and never retries them. The rule of more
than 3 invalidating the run stands.

**Connectivity check (not a trial).** One `stack-agent work --kind inspect` on a one-file project,
with the same cell and model. It made 3 calls (6,664 input tokens, 9 cached; 278 output, of which
142 reasoning), completed grounded, cost ≈ $0.00076, and is outside the experiment's accounting.

**Deterministic runner check (no model).** A scripted stand-in endpoint drove one round of the
real runner: launcher, PAX 0.4.1, evaluator and artifacts. It confirmed three things:
- a correct read, write and `pax.test` sequence is completed, verified and `task_complete` in
  every arm;
- a refused edit is not complete;
- a non-stream reply is classified as an infrastructure failure.

### Record (2026-10-22, after the Amendment 1 run; not an amendment)

The Amendment 1 run is **invalid** under §7. The gateway's limit of 5 requests per minute per team
for `inception/mercury-3` cut every trial short with HTTP 429. Details:
[`runs/2026-10-22-mercury-3-invalid/`](runs/2026-10-22-mercury-3-invalid/README.md). No result is
drawn from it. A rerun needs a new amendment (see that report's options) and a new output directory.

### Amendment 2 (2026-10-22, before the rerun): a request pacer for the gateway's rate limit

**Why.** The Amendment 1 run was invalidated by the gateway's limit of 5 requests per minute per team
for `inception/mercury-3`. Raising the limit is not possible from this session (no Vercel account
access). As the operator directed, a throttling proxy is placed between Stack-Agent and the gateway.

**The pacer** ([`pace_proxy.py`](pace_proxy.py), standard library):
- It is an HTTP CONNECT proxy, reached through Stack-Agent's documented `CHIP_PROXY`. It admits
  **at most one model request every 15 s** (at most 4–5 per minute), and only to
  `ai-gateway.vercel.sh:443`.
- **It terminates TLS.** Stack-Agent opens a fresh connection per call, and its 10 s connect timeout
  covers TCP, CONNECT and TLS. The pacer therefore completes the handshake at once with a leaf
  certificate from its own local CA (generated for this run; CA `sha256 4868b0a4b87b2b10…`). Trial
  processes trust only that CA (`SSL_CERT_FILE`). The pacer then reads the full request, waits for
  its slot, and forwards it **byte for byte** to the real host over TLS verified against
  `/root/.ccr/ca-bundle.crt`. It relays the response byte for byte. Nothing is added, removed,
  retried or cached.
- The key passes through its memory only, to be forwarded. It logs the admission time, the wait,
  the request size and the response status line, never headers or bodies.
- It is the same process, interval and CA for every arm and every trial. The runner records the
  `CHIP_PROXY` URL in `plan.json` (`chip_proxy`), and each launcher passes it to its work.

**Effect on the measurements, the same in every arm:**
- Each model call's latency now includes up to 15 s of waiting, which counts toward the 120 s call
  timeout and the 600 s deadline. A 14-call trial spends about 210 s waiting, well inside 600 s.
- **Latency and elapsed time are therefore not comparable with unpaced runs.** Within this run they
  are reported as measured, with the pacer's waits listed separately (`pace.log`).

**Validated before any trial.** One non-task inspect call through the pacer: 3 requests admitted
15.0 s apart (waits 0, 13.9 and 14.4 s), all `HTTP/1.1 200`, completed grounded, ≈ $0.001. It is
outside the experiment's accounting.

**Other changes:**
- **Infrastructure-failure rule (refining §7 and Amendment 1).** Any provider failure at any point
  in a trial (`model_failures` not empty: a rate limit, transport, timeout or stream error) makes
  the trial an infrastructure failure. So does a trial with no decision at all. The runner
  implements this rule; the Amendment 1 records keep their original classification.
- **Spend ceiling: $2.00** (the gateway credit balance is $4.83), with a worst-case trial of $0.06.
  Prices are unchanged.
- **Output:** a new directory, `runs/2026-10-22-mercury-3-paced/`. The invalid run is not touched.
- **Unchanged:** model, cell, limits, task, evaluator, arms, label, 10 rounds × 3 arms in the §5
  rotation, analysis (§8) and stopping rules (§9).

### Record (2026-10-22, after the Amendment 2 run; not an amendment)

The paced run is **valid** (30 of 30 trials, no infrastructure failure, no safety stop). Every arm
solved task E in 10 of 10 trials, so the primary comparison is at ceiling: the decision rule gives
**"not supported"**, with no detectable difference (exact McNemar p = 1 for every pair). Secondary:
the skill arm ran `pax.test` before editing in all 10 trials (2–3 runs against 1 in every other
trial), and used more input tokens than baseline. Report:
[`runs/2026-10-22-mercury-3-paced/`](runs/2026-10-22-mercury-3-paced/README.md).
