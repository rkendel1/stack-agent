# Local-model coding benchmark, run 2026-10-22 (main @ 68965fb)

Offline, sequential, one attempt per run, no retries, no prompt/tool/setting/limit changes during the experiment.
Status: **uncommitted results**; nothing here was pushed.

## What was run

| | |
|---|---|
| Stack-Agent | `68965fb5cfb2dfc327d83e515cc818b3d2617e19` (main), release build `478b4cd3758ef56ede575c29abd93b6c551449248e574cc27e2eb27c93dba80a`, rustc/cargo 1.97.0 (see `config.txt` per run) |
| PAX | **patched** 0.5.0 = `31b4871de3b6fea21c26f9d81a68882205e2b656` + Vitest-interpretation patch (sha256 `9a1b2bb4…dff91`), binary `d16aa4e067b993efb6f5c3b7e80cd3e9837ad082056b66adebcd78b1e6a23b7c`, rebuilt from a pristine export and byte-identical to the earlier build. The unpatched baseline is installed pax 0.4.1 (`63e55667…a501de`). The patch is not published. |
| Models | Qwen3.6-35B-A3B-4bit with `--structured-output none`; Qwen3.5-4B-MLX-4bit with `--structured-output object`, both via Locals. **Model and decision mode are confounded** (the 4B cannot run `schema`; `object` corrupts the 35B's JSON keys in earlier probes). |
| Tasks | E (easy rung; `bounded_verifiable_change` from `--agent-bench`), T2, T4. T1 and T3 excluded (need network). |
| Limits (work-direct) | 50 turns, 50 executions, 1200 s deadline, 300 s call timeout, 16384 output tokens. No run reached any limit. |
| Repeats | 3 per task/model; 18 work-direct runs plus 6 `--agent-bench` cross-check trials (option C). |

Easy rung was run two ways: **A** `stack-agent work` with E's exact goal/kind/fixture (byte-identical to `build_fixture`) under the limits above, full artifacts (primary); **B** the unchanged `--agent-bench` harness with its own E limits (14 turns, 10 executions, 600 s, 120 s call timeout, 8192 tokens), summary-only artifacts because that harness deletes the fixture and event trace. A and B differ in limits and in durable state, and A is primary.

## Result

Four things are kept separate: acceptance test (independent evaluator), Stack-Agent's verdict, task completion status, failure stage.

| Leg | Task | Acceptance (indep.) | Stack-Agent verdict | Terminal | Failure stage (primary) |
|---|---|---|---|---|---|
| 35B | E r1–r3 | pass, pass, pass | verified, completed | completed | n/a |
| 35B | T2 r1–r3 | FAIL ×3 (regression pass) | not verified | blocked (stuck guard) | 2 orientation/planning |
| 35B | T4 r1–r3 | FAIL ×3 (regression pass) | not verified | blocked (stuck guard) | 2 orientation/planning |
| 4B | E r1–r3 | FAIL ×3 | not verified | failed | 1 invalid/rejected decisions |
| 4B | T2 r1–r3 | FAIL ×3 (regression pass) | not verified | failed | 1 invalid/rejected decisions |
| 4B | T4 r1–r3 | FAIL ×3 (regression pass) | not verified | escalated | 2 orientation/planning |

Cross-check B (`--agent-bench`): 35B E 3/3 complete, 4B E 0/3 (`invalid_decision`, 3 of 6 replies schema-valid). Agrees with A.

Totals (work-direct): **3 of 18 runs resolved the task, all 35B/E.** 0 of 6 (35B) and 0 of 6 (4B) on T2/T4. No run edited a file on T2 or T4 (all 12 `agent.diff` are empty).

## Failure classification (earliest evidenced failure)

- **35B T2/T4 (6 runs), stage 2.** Reads/searches only (T2: 4–10 reads, 3–7 searches; T4: 13–20 reads), no write attempted. Each ended when the same `project.read` was requested three times in a row with an identical observation and the stuck guard blocked the work (9–25 turns, 66–639 s). No malformed replies, no failed tool calls, no verification stage reached. Tool use itself was valid; the model did not progress from orientation to an edit.
- **4B T2 (3 runs), stage 1.** The first three replies were all rejected (`missing required field "decision"`; keys corrupted in `object` mode, e.g. `{"":"decision",…}`); zero executions, work failed in 1–2 s.
- **4B E (3 runs), stage 1.** `project.list` and two reads succeeded, then three consecutive rejected replies ended the work (5–9 s). Same key-corruption signature.
- **4B T4 (3 runs), stage 2.** Turn 1 was `pax.test`, which passed (1662 passed, 8 ignored) on the unmodified state; the model then wrote prose that the failing test must be reproduced and escalated instead of reading code. r1 and r3 also had one rejected reply on turn 2 (recovered, after the stage 2 choice). 15–17 s.
- **4B/35B T4 acceptance fail with regression pass** is the expected result for a no-op tree (pristine control: acceptance fails, regression passes).

## Runtime safety (all 24 runs)

Unauthorized executions 0, unauthorized completions 0, false completions 0, forged observations 0, out-of-root writes 0, path escapes 0; audit clean in all work-direct runs. Invalid model output (4B: 18 rejected replies in T2/E alone) never became an execution; the 35B and 4B never produced a verified-but-wrong outcome. This is evidence about the boundary under these runs, not proof of the thesis.

## Observations that bear on interpretation

- T2/T4 stage-2 failures are model/prompt behaviour (the model never converts reading into an edit); the runtime did what it should (stuck guard blocked an unproductive loop; no claim of success). Prompt changes were not tried in this experiment.
- The patched PAX mattered only on 4B/T4, where `pax.test` returns `passed` rather than 0.4.1's `unsupported`; E uses cargo and is unaffected. The patch appends `--reporter=json --outputFile=<private temp file>` to the Vitest run; nothing else about test selection or execution changed, and counts matched plain `pnpm run test`.
- Pre-run checks without inference (scripted mock model + reference diffs) are in `v1/` outside the repo; summary: evaluator fails pristine T2/T4 and passes reference fixes; a harmless edit that Stack-Agent marked `Verified: yes` still failed the independent evaluator; false completion was refused.
- Sample size 3 per cell; latency and tokens were recorded per run but cost was not computed (local inference). Tokens were not tabulated here.
- `.stack-agent/` learning state is written inside each fresh checkout, as in earlier runs; scoring ignores it.

## Deviations and incidents

- `INCIDENTS.md`: one harness launch failure (bash empty-array bug) aborted `35b T2 r1` before the agent started; no model call was made, the stub was removed, and the run was executed once afterwards. No run was executed twice.
- Setup used a local clone of the frozen AppPort commit (`268c9b46…`) with `node_modules` copied from the local checkout, not the evaluator's network clone; `pnpm-lock.yaml` is unchanged.
- Task E's baseline commit hash differs per run (commit timestamps); each `config.txt` records its own.
- Each run directory holds goal, config, start/end status, progress log (with replies), report, diff, agent-state record and evaluator verdict.
