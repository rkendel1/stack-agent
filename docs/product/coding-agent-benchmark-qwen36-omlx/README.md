# End-to-end coding benchmark: Stack-Agent with Qwen3.6-35B-A3B-4bit on oMLX

Measured 2026-10-21. Raw evidence: `results/`. **Outcome: the task was not completed.** Stack-Agent made no file change in
any of 3 identical runs, stopped itself truthfully, and the independent oracle still fails. The benchmark is
nevertheless valid: the failure reproduced 3 of 3 times and is diagnosed below.

## 1. Executive summary

* **Did the task succeed?** No. 0 of 3 runs changed a file. Independent verification: no source diff; the hidden acceptance
  test fails AC1 and AC3 exactly as it did before the run.
* **Did Stack-Agent make and verify real changes?** No changes. What it did, it did honestly: terminal state `blocked`,
  exit 1, `verified=false`, clean audit, 0 false completions, 114 events durably recorded.
* **Why?** The model searched for the right strings, but (a) repeated a failing `path: "src"` after being shown
  `not_found` 2 to 4 times per run, (b) never issued a single `project.read` (0 reads in 3 runs) even after a search
  returned 8,781 bytes of relevant matches, and (c) re-sent an identical search until the runtime's stuck guard stopped it.
  A secondary, runtime-side factor: a whole-project search silently hits its 500-file cap inside `crates/` and never reaches
  `docs/` (inferred, section 6).
* **Suitable for further testing?** Yes, for one narrow question: whether more actionable tool feedback gets this model from
  search to read (section 8). Not yet suitable as an autonomous coder on this evidence (1 task, 3 runs).

## 2. Environment

| | |
| --- | --- |
| Repository | `/Users/randy/Developer/stack-agent`, branch `main`, HEAD `23cbdeb` (the run used a detached worktree, see below) |
| Baseline worktree | `dd55ac1` = `23cbdeb` + 2 disclosed setup commits (below); worktree was clean (0 changes) before each run |
| Provider / model | `openai-compatible`, `http://127.0.0.1:6600/v1/chat/completions` (oMLX), requested id `Qwen3.6-35B-A3B-4bit` |
| Identity check | oMLX lists one id, the alias `qwen-fast`; its `model_settings.json` (read only) maps that alias to `Qwen3.6-35B-A3B-4bit`, thinking disabled, KV-cache quantization affine8 on; the run's usage report names `Qwen3.6-35B-A3B-4bit` as the served model. Server settings were not modified |
| Request settings | profile `local`: `max_tokens`, temperature 0.0 (server default is 1.0; the request sets 0.0, whether oMLX honors it was not tested), `--structured-output object` (see section 3) |
| Limits | `--max-turns 40 --max-executions 30 --deadline-secs 900 --call-timeout-secs 120 --max-output-tokens 8192`; context budget unset (default) |
| Persistence | scratch FeltDB file via `--state-db` |
| Credential | oMLX requires an API key. It was read from oMLX's own settings file and passed only as `CHIP_API_KEY` in the process environment; the archive was scanned for it (not present). `.env.local` (OpenAI/Anthropic keys) was never read or sent |
| Runtime | `stack-agent` release binary built from source at `464ac56` (`23cbdeb` only adds benchmark docs and scripts); PAX 0.4.1 |

**Disclosed setup deviations** (two commits in the disposable worktree only; the real repository is unchanged):

1. Root `Cargo.toml` `default-members` set to `fx-single-attempt` and `fx-native-provider`. `pax.test` runs the root
   `cargo test`, which by default covers only `stack-agent`, so without this the agent's changes would never be tested.
2. One test (`tls_boundaries::a_certificate_for_another_host_is_refused_even_when_its_ca_is_trusted`) marked `#[ignore]`.
   It fails identically on unmodified `main` on this machine (connect to the LAN address times out), which would make
   `pax.test` red for any agent. The sanctioned `FX_TEST_LAN_IP` override did not fix it. It is excluded from the baseline,
   not from the final verification.

## 3. Task definition (frozen before the run; `results/task.md`)

* **Defect.** Anthropic reports prompt-cache write tokens as `cache_creation_input_tokens` in `message_start` usage. The
  native Anthropic path never carries it into FX `Usage.cache_write_tokens` (documented gap: `known-issues.md` section K).
  Verified in code: `PortUsage` has no cache-write field, `anthropic.rs` never reads the key, `usage_event` fills the rest
  of `Usage` from `Default`.
* **Components.** `crates/fx-native-provider` (parser, tests), `crates/fx-single-attempt` (`PortUsage`, mapping).
* **Goal submitted** (737 bytes, names no file; in `task.md`).
* **Acceptance.** AC1 reported N reaches `Some(N)`; AC2 unreported stays `None`; AC3 reported 0 stays `Some(0)`; AC4 existing
  tests unchanged and passing; AC5 no change to other token fields or the OpenAI path; AC6 the agent adds a test; AC7 fmt and
  clippy clean; AC8 no unrelated files.
* **Oracle.** A hidden test using only public APIs, written before the run and never shown to the agent
  (`results/zz_acceptance_cache_write.rs`). At baseline AC1 and AC3 fail, AC2 passes: the task is not already solved.
* **Why this task.** Real, documented, spans two crates, needs exploration (the key names appear in several places), objective
  oracle, offline, bounded, baseline suite runs in about 25 s.

**Pre-run configuration calibration (disclosed).** The repo's `local` profile defaults to the full decision schema. On a
trivial `verify` smoke the model answered `complete` before requesting any tool (blocked, 0 executions). `object` and `none`
each requested `pax.test` and verified (1 call, about 2.5 s). I fixed `--structured-output object` (the mode
`decision-contract.md` documents as the measured better choice for local models) before the benchmark and did not tune
anything after seeing its results. The smoke evidence is in `results/smoke/`.

## 4. Results

| Metric | Run 1 | Run 2 | Run 3 |
| --- | --- | --- | --- |
| Acceptance criteria passed (independent oracle) | 0 of AC1/AC3 pass; AC2 passes trivially (nothing changed) | same | same |
| Existing tests (both crates) | 0 failed, 1 environmental ignored (verified after run 1; worktree unchanged after runs 2 and 3) | n/a (no change) | n/a |
| Task completed | no | no | no |
| Wall-clock | 23 s | 7 s | 19 s |
| Inference turns (model calls) | 10 | 5 | 9 |
| Tool executions | 10 (all `project.search`) | 5 | 9 |
| Successful / failed tool executions | 5 / 5 | 1 / 4 | 5 / 4 |
| Reads / lists / writes | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| Files changed, lines +/- | 0, 0/0 | 0, 0/0 | 0, 0/0 |
| Repeated or ineffective actions | 5 recoveries; last 3 identical searches | 3 recoveries; 3x `path:src` | 4 recoveries; last 3 identical |
| Input / output tokens (reported) | 32,842 / 474 | 7,925 / 229 | 26,950 / 420 |
| Model time / total time | 21.3 s / 22.6 s | 6.7 s / 7.3 s | 17.7 s / 18.8 s |
| Largest request | 18,894 bytes (5,968 input tokens) | 7,495 bytes | 18,524 bytes |
| Final task state | `blocked` (stuck guard), exit 1 | same | same |
| Evidence completeness | 114 events in FeltDB; audit clean | audit clean | audit clean |

Unavailable: peak memory, time to first token (oMLX reports it, the runtime does not carry it), per-call latency (only
totals), cost, cache-write tokens (the OpenAI-compatible provider does not report them; the runtime shows `null`, not 0),
the text of each observation (the record keeps size and SHA-256 only). Provider failures: 0. The run was never extended and
nothing was finished by hand.

## 5. Correctness and quality

Nothing changed, so there is no implementation to judge. The agent's reporting matched reality: it claimed no completion;
the runtime reported `blocked`, `verified=false`, `goal_satisfied=false` and quoted the stuck reason. Independent checks
(after run 1): `git diff` of source files is empty; the only new path is a runtime learning record under `.stack-agent/`;
`cargo test` in both crates passes (12+26+38+3+6+4+5+31 tests, 1 ignored); `cargo fmt --check` and clippy report nothing;
acceptance test: AC1 and AC3 FAIL, AC2 pass. Remaining incomplete: the entire task.

## 6. Failure analysis

Search sequences (from the recorded requests, same pattern in all runs): `cache_creation_input_tokens` with no path, then
`path: "src"` (2 to 4 times, `not_found` each time), then `cache_write_tokens` with `path: "src"` or no path, then
`path: "crates"` (run 1 also guessed `crates/chip-ant`, nonexistent), repeated until the stuck guard fired.

| Class | Finding | Evidence | Weight |
| --- | --- | --- | --- |
| **A model/reasoning** | Repeated an ineffective action after being shown its error; never moved from search to read; re-sent identical searches | `not_found` for `src` re-sent after observation in every run; 0 reads; a path-less `cache_write_tokens` search returned 8,781 bytes of matches (run 1, call 6) and the next call was a hallucinated path; the observations were shown to the model (`context.calls[]`: results count grows each call, 0 omitted, 0 condensed, requests at most about 6k tokens) | **Primary** |
| **B context/discovery** | The model was never given the project layout and guessed `src`; the error `{"error":"not_found","path":"src"}` names no alternative | no `project.list` was ever requested; workspace has no root `src/` | Contributing |
| **C tool/execution** | A whole-project search has a 500-file cap (`MAX_SEARCH_FILES`) and walks `audit/` (12 files) then `crates/` (745 files), so it never reaches `docs/`, the only place `cache_creation_input_tokens` occurs (`git grep`). The 186-byte first result was therefore very likely "no matches, truncated"; **inferred**, the observation text is not recorded | `crates/chip-project/src/lib.rs` (cap and truncation flag), file counts, `git grep` | Contributing |
| **D harness** | Positive: the stuck guard stopped the run after 3 identical observations without asking the model again and without extending the budget; audit clean; no false completion. Minor: the runtime writes `.stack-agent/learning/**` records into the project directory, and that directory is tracked in git, so every run dirties the tree (diff review must filter it) | `outcome_reason`; `git status` | Incidental |
| E persistence | none observed | 114 events recorded, `persistence_failures: []` | none |
| F infrastructure | none observed | `model_failures: []`, provider failures 0 | none |

Primary versus incidental: the primary cause is the model's behavior in A. B and C made recovery harder but did not force
the outcome, since the model could have listed the root or read the files its own search returned. Not ruled out: that
`object` mode, temperature handling, or the 4-bit quantization contribute; none was varied (one variable at a time, and the
configuration was fixed before the run).

## 7. Efficiency

Time split (observed): exploration 100% (all calls were `project.search`), implementing 0, verifying 0 (the model never ran
`pax.test`). Per-call model time averaged about 2.1 s. Wasted calls: 5 of 10, 4 of 5, and 4 of 9 hit a nonexistent path; the
final 3 of each of runs 1 and 3 and the last 3 of run 2 were identical repeats. Context was not a constraint (largest
request 5,968 input tokens of a 262,144 limit). Observed measurements above; no estimates are used.

## 8. Recommended next action

**Test one runtime-side change in isolation: make a failed or truncated `project.search`/`project.read` observation
actionable using only facts the runtime already owns, namely the directory entries at the requested path's nearest existing
ancestor on `not_found`, and an explicit "truncated after 500 files, narrow with a path" note.** Evidence: the model repeated
`path: "src"` after `not_found` in all 3 runs, and a silently truncated whole-project search produced the useless first
result in all 3. Success measure: rerun this frozen task at least 10 times and compare reads, writes, verified completion and
repeated-action counts against this baseline (0/3). It changes runtime feedback only, not authority (nothing the model
supplies becomes reality; the content is Chip-produced observation). It must not be adopted on this evidence alone: n is 3
runs of one task. A model-side comparison (`--structured-output none`, or a larger model) is the separate, second experiment.

## 9. Commands and statuses

| Command | Exit |
| --- | --- |
| `git worktree add --detach <scratch>/wt HEAD`; two local setup commits | 0 |
| `cargo test --offline` (worktree, before/after setup) | baseline red on 1 environmental test, then green with it ignored |
| hidden acceptance test at baseline (AC1, AC3 fail, AC2 pass) | expected failure |
| smoke: `stack-agent work ... --kind inspect` and `--kind verify` (modes schema, object, none) | 1 (schema, blocked), 0 (object), 0 (none) |
| `stack-agent work <goal> --kind change ...` run 1 / 2 / 3 | 1 / 1 / 1 (blocked by stuck guard) |
| post-run: `git diff`, `cargo test --offline`, acceptance test, `cargo fmt --check`, `cargo clippy` | tests 0, acceptance failing (as at baseline), fmt 0, clippy no warnings |

Mistake disclosed: during setup I deleted the tracked `.stack-agent/` files in the disposable worktree with `rm -rf` and
restored them with `git checkout`; the worktree was verified pristine (0 changes) before run 1. A first smoke was run from a
crate subdirectory by accident; its stray output was removed.

## 10. Final acceptance

Run outcomes recorded (3); actual diff inspected (empty); verification recorded; failures classified with evidence; report
written; successful work (none) is distinguished from attempted work (3 runs, 24 tool executions, 0 file changes).
