# Run 2026-10-22: Stack-Agent with claude-opus-5-5

**Result: 0 of 4 tasks solved. In 1 of 4 the real defect was fixed, and the hidden acceptance test
passes, but the task was not completed.** Every attempt ended within 2.5 minutes, far inside the
20-minute deadline. None of them ran out of time, budget or ideas. Each one ended when a single
model reply did not start with JSON.

## Configuration

| | |
| --- | --- |
| stack-agent | `baea6fb` (release build), PAX 0.5.0 (`31b4871`) |
| Provider / model | `anthropic`, `claude-opus-5-5` (served as `claude-opus-5-5`, per each report) |
| Profile | `--provider-profile anthropic` (structured output `none`, no temperature, `max_tokens`) |
| Limits | `--max-turns 50 --max-executions 50 --deadline-secs 1200 --max-output-tokens 32000 --call-timeout-secs 300` |
| Goal | `tasks/Tn.goal.txt` (the brief, restated within the 2,000-byte goal limit) |
| Isolation | fresh checkout per task from `prepare-checkout.sh`; `STACK_AGENT_DATA_DIR` per task, so no learning carried between tasks |
| Exact command | `run-task.sh` in this directory |

`--state-db` was not used. With it, Stack-Agent refuses any objective over 1,024 bytes (see
"Product findings"). The default state store accepted the work without recording it, so the
evidence here is the per-operation progress log, every model reply (`--print-reply`) and the final
report.

## Results

| Task | Stack-Agent outcome | Wall time / model calls | What it did | Hidden acceptance | Regression | Score |
| --- | --- | --- | --- | --- | --- | --- |
| T1 Compute target auth | Work failed: reply 12 not valid JSON | 31 s / 12 | read `credentials.rs` and `lib.rs` (both defect sites); no write | 0/4 (as at base) | not run (no change) | **Failed** |
| T2 AppPort event observation | Work failed: reply 20 not valid JSON | 102 s / 20 | found the defect; wrote a per-event permission check in `connection.ts`; no tests written, tests never run | **8/8 pass** | pass (1660 passed) | **Partial**: defect fixed, task incomplete (no regression tests; not verified by Stack-Agent) |
| T3 Compute policy composition | Work failed: reply 8 not valid JSON | 141 s / 8 | wrote a correct reproduction test (fails at base on exactly the defective cases, passes its controls); ran `pax.test`; fix not yet written | 2/5 (as at base) | not run (test file only) | **Failed** |
| T4 AppPort resolver versions | Work failed: reply 11 not valid JSON | 68 s / 11 | read `resolver.ts`, `manifest.ts` and the specs; was writing a reproduction test | fails (as at base) | pass | **Failed** |

Stack-Agent made no false claims: every run reported `Verified: no`, and its audit was clean (no
unauthorized execution, path escape, forged observation or false completion).

**What the model did well.** In every task it went straight to the files that hold the defect.
Briefs named symptoms only, never causes. T2's fix closely matches the reference fix. T3 followed
the brief's order, writing a reproduction test before the fix, and its test independently matches
the hidden one. When `pax.test` failed in T3, it correctly identified the failure as unrelated to
its change.

**Why every attempt ended.** At some turn the model began its reply with one sentence of narration
before the JSON decision:

- T1: "I'm reading the job manager's ownership checks…"
- T2: "I've written the fix, so next I'll write the regression tests…"
- T3: "The new test file is written, but the fix is not applied yet…"
- T4: "I've read the specs, resolver and registry. Next I'm writing the reproduction test…"

The decision contract requires the reply to be exactly one JSON object and never repaired. So
Stack-Agent rejected the reply as `invalid decision: not valid JSON: unexpected character at byte
0`, and ended the whole work on that one reply. It did not refuse the reply and ask again.

## Product findings

1. **A single malformed reply is fatal to the whole work.** Refusing to repair an invalid decision
   is the documented contract, and it held. But ending the work, rather than returning the
   refusal to the model as an observation and asking again, turned four promising runs into four
   failures. It is the dominant cause in this run.
2. **Neither Anthropic structured-output mode works for a long task.**
   - `none` (the profile default) leaves the model free to prefix prose.
   - `schema-closed` makes Claude models stop on turn 1. This is already measured in
     `docs/product/decision-contract.md`, and reproduced here once (see void attempt 2 below).
3. **The default per-call timeout (30 s) cannot accommodate a real file write.** A write reply
   carries the complete new file. T2 attempt 1 under the defaults timed out on turn 15, the turn
   after it had read every file it needed. `connection.ts` is 9.2 KB.
4. **The two goal-length limits disagree.** `stack-agent work` accepts a goal of up to 2,000 bytes,
   but `--state-db` refuses an objective over 1,024 bytes. The default state database silently
   records nothing for such a work.
5. **`pax.test` always runs the whole workspace, with a fixed 300 s timeout.** On Compute, 3 tests
   fail at the frozen base however the code is changed, and on AppPort, PAX reports pnpm/vitest
   results as `unsupported`. So on none of these four repositories could Stack-Agent reach
   `Verified: yes`, even with a correct fix.

## Attempts that are not the scored attempt

| Attempt | What happened | Status |
| --- | --- | --- |
| T2 void 0 | goal over 2,000 bytes: usage error, no model call | void (evaluator error) |
| T2 void 1 | objective over 1,024 bytes refused by `--state-db`: nothing ran | void (evaluator error) |
| T2 void 2 | evaluator set `--structured-output schema-closed`, which the product documents as causing turn-1 stops for Claude; the model answered `complete` on turn 1, which was refused; 1 call, 6 s | void (evaluator misconfiguration) |
| T2 attempt 1 | product defaults (30 s call timeout, 16,000 output tokens): 14 read-only operations, then call 15 timed out; 67 s | **valid, Failed**; kept in `T2/attempt-1-default-timeout/` |
| T2 attempt 2 | the scored T2 row above, with the 300 s call timeout used for all four tasks | scored |

So T2 had two valid attempts, which departs from the one-attempt rule. Attempt 1 is reported
because it was valid. The configuration was then changed once, before T1, T3 and T4 ran, and that
configuration was used for all four tasks.

## Evaluator artifacts

- **A shared build cache leaked one checkout's path into another.** T1 and T3 shared one pre-built
  Cargo target directory, warmed in the T1 checkout. During T3's `pax.test`, one `compute-cli`
  test looked for a path in the T1 checkout and failed. The model correctly called it unrelated,
  and it did not change T3's outcome. Future runs should give each checkout its own target
  directory, warmed in that checkout.
- **Regression was run only where there was a change.** For T1 (no change) and T3 (a new test
  file only), only the acceptance test was run. A tree with no source change cannot pass it, and
  the T1/T3 regression suites take about 15 minutes each. T2 and T4 were scored with the full
  `run-acceptance.sh`.

## Files

Per task: `goal.txt`, `progress.log` (each model call and operation, and every model reply),
`report.txt`, `exit.txt`, `agent.diff` (excluding Stack-Agent's own `.stack-agent/` record), and
`verdict.txt` or `acceptance-only.log`.
