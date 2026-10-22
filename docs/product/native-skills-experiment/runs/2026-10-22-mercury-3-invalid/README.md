# Run 2026-10-22: `inception/mercury-3` via the Vercel AI Gateway (INVALID)

**Conclusion: inconclusive. The run is invalid under pre-registration §7 and gives no evidence for or
against the skill.** The gateway enforces **5 requests per minute per team** for
`inception/mercury-3`, so every one of the 30 trials was cut short by HTTP 429:

- 29 failed on their first call;
- trial 1 failed on its 6th call, after 5 successful ones.

The model never got far enough in any trial for the arm to matter. No trial was retried or
replaced. The analyzer's mechanical output (`analysis.json`, `"decision": "not supported"`) is
**not** a result: every round contains an infrastructure failure, so there are no valid rounds.

## Provenance

| Item | Value |
| --- | --- |
| Source | HEAD `83b72e7cb7a664752802f0593bff9ca24fd3daa4` plus an uncommitted tracked diff (336 lines, `sha256 76b401ef…02ae58a`, saved as `source.diff`) plus the untracked files hashed in `plan.json` (`source.untracked`). Fingerprint `a66bb672dcff5ef9…`, re-checked before every trial |
| Runner | `target/release/examples/skill_experiment`, `sha256 f8cb10f3a8507452…`, re-checked before every trial |
| Model and endpoint | `inception/mercury-3`, `https://ai-gateway.vercel.sh/v1/chat/completions`, wire `openai-compatible` |
| Cell | `--provider-profile openai`: `schema-flat`, `max_completion_tokens 8192`, temperature omitted, `--call-timeout-secs 120` |
| Limits | `--max-turns 14 --max-executions 10 --deadline-secs 600`, `--no-state` (task E's own) |
| Credential | `AI_GATEWAY_API_KEY`, passed to each trial child as `OPENAI_API_KEY`; `credential_supplied: true`. A search of every file here for the key value: 0 matches |
| Transport | `SSL_CERT_FILE=/root/.ccr/ca-bundle.crt` |
| PAX | `pax 0.4.1`, `rkendel1/pax` at `674f3b3`, binary `sha256 05e3d861…b4cbf7` |
| Evaluator | `agent_bench::judge` (task E) plus `run_project_tests`, unchanged at HEAD |
| Arms | baseline (0 bytes added); skill `sha256:1d43d17f…8caa7c` (2,416 bytes); control `sha256:ad57e213…432dcc1` (2,413 bytes); 151-byte label |
| Schedule | 10 rounds × 3 arms in the pre-registered rotation (`plan.json`, `run.log`) |
| Prices | gateway `/v1/models`: $0.10 input, $0.35 output, $0.01 cached input per million tokens. Ceiling $5.00, worst-case trial $0.06 |

## Trial-level outcomes

| Trials | Calls | What happened | `task_complete` | Classified as |
| --- | --- | --- | --- | --- |
| 001 baseline, round 1 | 6 | `project.list`; `project.search "retry"`; `project.read tests/policy.rs`; `project.list src`; `project.read src/lib.rs`; then call 6 → **HTTP 429** (`rate_limited`, `confirmed_response`, not retried). 4.4 s | false | judgment failure ("no edit of src/lib.rs") by the runner's rule at run time. **Mis-classified:** the work ended on a provider failure, not a decision |
| 002–030 (all arms) | 1 | call 1 → **HTTP 429**, 0.09–0.23 s | false | infrastructure failure |

Per arm:

| Arm | `task_complete` | 95% CI | Provider-limited |
| --- | --- | --- | --- |
| baseline | 0/10 | [0, 0.31] | 10/10 (trial 1 included) |
| skill | 0/10 | [0, 0.31] | 10/10 |
| length control | 0/10 | [0, 0.31] | 10/10 |

The paired tests are all 0 discordant rounds (p = 1). That is meaningless here.

**Secondary outcomes:**

- No trial wrote a file, ran `pax.test`, reached verification or claimed completion.
- No stuck-guard stop occurred.
- No decision was rejected (trial 1's five decisions were all valid).
- Time to first useful edit: none occurred.

**Boundary:** 0 unauthorized executions, 0 completion claims contradicted by the evaluator, and no
safety stop. Every fixture was fresh. Every trial had its own `STACK_AGENT_DATA_DIR`. The source
fingerprint and runner hash never changed.

## Tokens, latency and cost (from the reported usage fields)

| | Input tokens | Cached | Output tokens | Cost |
| --- | --- | --- | --- | --- |
| Trial 001 (5 answered calls) | 9,323 | 0 | 599 | $0.00114 |
| Trials 002–030 | 0 (refused) | 0 | 0 | $0 |
| **Experiment total** | 9,323 | 0 | 599 | **$0.00114** |
| Connectivity check (not a trial) | 6,664 | 9 | 278 | ≈ $0.00076 |

## Incidents

1. **The provider rate limit invalidated the run** (§7: more than 3 infrastructure failures). The
   429 message: "Rate limit exceeded for inception/mercury-3 for provider inception: this team's
   limit of 5 requests per minute (per region) was reached." The same limit was recorded for the
   gateway's free tier in `performance.md` §8. Mercury answers in under a second, so a single task-E
   trial (up to 14 calls) exceeds 5 per minute on its own. Spacing trials apart would not have
   prevented it.
2. **Runner classification flaw (fixed for future runs, not applied to these records).** The rule
   counted a provider failure as infrastructure only when it came before any reply, so trial 1's
   mid-trial 429 was scored as a judgment failure. A provider failure ends a work without a decision
   (`decision-contract.md`), so it says nothing about the model's judgment. The rule now treats any
   provider failure as infrastructure. Under either rule the run is invalid.

## Deviations from the pre-registration

- **Amendment 1** (recorded before any trial): API model, gateway, `openai` cell, `SSL_CERT_FILE`,
  PAX build, source fingerprint in place of a commit, and spend ceiling.
- No deviation during the run. The schedule, limits, arms and evaluator were as registered, no
  trial was retried, and no setting was changed.

## What a valid rerun needs (for decision; not done)

Each option changes the conditions, so it needs a new amendment and a new output directory. It
must not overwrite this run.

1. **A higher gateway rate limit** for the team: the Vercel AI Gateway plan or a provider-key
   configuration. This changes nothing in Stack-Agent or the experiment, so it is preferred.
2. **A model whose gateway limit admits about 14 calls in a burst.** The model is part of the
   question, so this means a new pre-registration amendment naming it.
3. **A request pacer between Stack-Agent and the gateway.** For example, a forwarding proxy that
   spaces requests at least 12 s apart, set through the documented `CHIP_PROXY`. It would not touch
   request content or authority. But it adds waiting time inside the 600 s deadline (14 calls ×
   12 s = 168 s) and inside per-call latency, the same for every arm. It also adds a component
   outside the reviewed path. This is the least preferred option.

Pacing inside Stack-Agent itself would change production behavior, which this experiment must not
do.

## Files

- `plan.json`, `source.diff`, `run.log`
- `trials/*.json`: the bench record plus the arm, round, position, provenance, cost, fingerprint and
  classification
- `artifacts/<trial>/`: the complete `work --json` report, `stderr.log` with every model reply
  (redacted), `exit.txt`, `agent.diff`, `before.json` and `after.json`, and the per-trial data
  directory
- `analysis.json`: `analyze.py` output, mechanical, and not a result for this run (see above)
