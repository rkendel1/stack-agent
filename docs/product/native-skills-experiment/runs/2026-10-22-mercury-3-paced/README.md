# Run 2026-10-22: `inception/mercury-3` via the Vercel AI Gateway, paced (Amendment 2)

**Conclusion: no evidence of benefit on the primary outcome. The test could not show one, because
every arm reached the ceiling.**

- Baseline, skill and length control each solved task E in **10 of 10** trials. The independent
  oracle confirmed every solve, and every trial was verified by `pax.test`.
- With 0 discordant rounds in every comparison, the pre-registered decision rule gives
  **"not supported"** (exact McNemar p = 1). Mercury 3 already solves this task without guidance,
  so there was nothing for the skill to improve.
- The skill did measurably change *how* the model worked (secondary outcomes, below). That is not a
  benefit claim.

The run is **valid**:
- 30 of 30 trials ran;
- no infrastructure failures, and all 185 gateway requests were `HTTP 200` (`pace.log`);
- no safety stop;
- the source fingerprint and runner hash never changed;
- no trial was retried.

## Provenance

| Item | Value |
| --- | --- |
| Pre-registration | `../../PREREGISTRATION.md` with Amendments 1 and 2 (recorded before the first trial) |
| Source | HEAD `83b72e7` plus the saved diff (`source.diff`) and the untracked-file hashes in `plan.json`. Fingerprint `cec6a4fdd2521a6b…` |
| Runner | `skill_experiment`, `sha256 c21e1a1e28f04435…` |
| Model and endpoint | `inception/mercury-3`, `https://ai-gateway.vercel.sh/v1/chat/completions`, profile `openai` (`schema-flat`, `max_completion_tokens 8192`, temperature omitted) |
| Pacer | `../../pace_proxy.py`, one request per 15 s, `CHIP_PROXY=http://127.0.0.1:18081`, local CA `sha256 4868b0a4…` |
| Task, limits, evaluator | task E fixture; `--max-turns 14 --max-executions 10 --deadline-secs 600 --no-state`; `agent_bench::judge` plus `run_project_tests` |
| PAX | 0.4.1 at `674f3b3` |
| Arms | baseline (0 bytes added); skill `sha256:1d43d17f…8caa7c` (2,416 bytes); control `sha256:ad57e213…432dcc1` (2,413 bytes); 151-byte label |
| Key exposure | a search of every file here for the key value: 0 matches |

## Primary outcome

| Arm | `task_complete` | 95% Clopper–Pearson | Completion claimed | Claim agrees with the oracle |
| --- | --- | --- | --- | --- |
| baseline | 10/10 | [0.69, 1.00] | 10 | 10 |
| skill | 10/10 | [0.69, 1.00] | 10 | 10 |
| length control | 10/10 | [0.69, 1.00] | 10 | 10 |

| Paired by round | Only the first arm solved | Only the second | Both | Neither | Exact McNemar p |
| --- | --- | --- | --- | --- | --- |
| skill vs baseline | 0 | 0 | 10 | 0 | 1.0 |
| skill vs length control | 0 | 0 | 10 | 0 | 1.0 |
| length control vs baseline | 0 | 0 | 10 | 0 | 1.0 |

## Secondary outcomes (descriptive; none can establish a benefit)

| | baseline | skill | length control |
| --- | --- | --- | --- |
| `pax.test` runs per trial | 1 in all 10 | **2 in 9, 3 in 1** | 1 in all 10 |
| First useful edit, turn (median, range) | 5 (4–6) | 4.5 (4–5) | 5 (4–7) |
| Model calls (median, range) | 6.5 (6–8) | 5.5 (5–9) | 6 (5–8) |
| Rejected decisions, stuck-guard stops, repeated identical requests | 0, 0, 0 | 0, 0, 0 | 0, 0, 0 |
| Distinct trajectories | 6 | 5 | 6 |
| Input tokens, total (cached) | 131,552 (19,327) | 166,409 (33,535) | 148,005 (31,918) |
| Output tokens, total | 16,824 | 10,142 | 12,131 |
| Latency per trial, median | 97.2 s | 81.8 s | 89.8 s |
| Cost (cache-discounted / conservative) | $0.0173 / $0.0190 | $0.0172 / $0.0202 | $0.0162 / $0.0191 |

Paired bootstrap of per-round differences (95% intervals, 10,000 resamples, seed 1):

| Measure | skill − baseline | skill − length control |
| --- | --- | --- |
| Model calls | −0.7 [−1.4, +0.1] | −0.1 [−1.0, +1.0] |
| Input tokens | **+3,486 [+984, +6,703]** | +1,840 [−1,388, +6,027] |
| Latency | −9.9 s [−17.9, +0.4] | −1.8 s [−15.4, +14.8] |

How to read it:

- **The workflow text changed behavior, and the control did not.** In every skill trial the model
  ran `pax.test` before editing (the skill's step 1), as well as after. In every baseline and
  control trial it ran `pax.test` only once. This is the one difference attributable to the
  workflow's content rather than its length.
- **It did not change the outcome.** On this task, reproducing the failure first was not needed to
  succeed.
- **Cost.** The skill used more input tokens than baseline (the added text plus the extra test
  observation). Against the length control the difference is not significant. Fewer output tokens
  and slightly fewer calls kept total cost about level.
- **Latency is dominated by the pacer** (up to 15 s before each call). It is comparable across arms
  in this run only, not with unpaced runs.
- **"Time to first useful edit"** is measured in turns (from the replies and `paths_written`).
  Elapsed time to it is unmeasured, as pre-registered.

## Spend

| | Cost |
| --- | --- |
| Experiment, 30 trials (cache-discounted) | **$0.0506** |
| Experiment, conservative (all input at full price) | $0.0583 |
| Ceiling | $2.00, never approached |
| Outside the experiment | connectivity check $0.00076, pacer validation ≈ $0.001, invalid Amendment 1 run $0.00114 |

## Deviations

None during the run. Amendments 1 and 2 were recorded before any trial of the runs they govern. The
pacer's waits are inside every arm equally.

## Limits

- **One task, one model, n = 10 rounds per arm.**
- **A ceiling effect makes the primary comparison uninformative.** A benefit could only show on a
  task the model fails without help.
- **The task is very small.** Its single test is visible to the agent (pre-registration §2).
- **The result says nothing about general effectiveness.**

## Files

`plan.json`, `source.diff`, `chip-proxy.url`, `run.log`, `pace.log` (the pacer's per-request admission
log), `trials/*.json`, `artifacts/<trial>/` (complete reports, redacted replies, diffs, file hashes),
`analysis.json` (`analyze.py` output).
