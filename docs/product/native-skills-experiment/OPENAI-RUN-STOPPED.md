# OpenAI run: stopped before experiment inference (2026-10-22)

**Result: no trial was run, and no experiment inference was spent.** The OpenAI account behind
`OPENAI_API_KEY` refuses every inference request with `HTTP 429 billing_not_active` ("Your account is
not active, please check your billing details"). This meets the follow-up's stop conditions "the
OpenAI API key is unavailable" and "no suitable model is available through the supported provider
path". There is no benefit result, positive or negative.

## What was checked, and the outcome

| Check | Outcome |
| --- | --- |
| Branch and working tree | `claude/gracious-bohr-xnz7y3`, HEAD `83b72e7`; the experiment implementation is uncommitted (as directed) and preserved. Nothing was discarded, switched or overwritten |
| Local-model report and incident log | **None exist.** The local-model experiment was never run (no PAX and no model server in the earlier session) |
| `OPENAI_API_KEY` | Present (checked by length only; never printed). `grep` for the key in every file written here: 0 matches |
| Model availability | `GET /v1/models` → 200. The list includes `gpt-6-luna`, the preferred model |
| Supported provider path | `agent_bench::ProviderId::OpenAi`: `openai-compatible` wire, `https://api.openai.com/v1/chat/completions`, key from `OPENAI_API_KEY`, `--provider-profile openai` (`max_completion_tokens`, no temperature, the flat decision schema). No new provider or dependency is needed |
| Transport | The first call failed in TLS (`invalid peer certificate: UnknownIssuer`), because this environment re-terminates TLS at its proxy. Fixed with the documented setting `SSL_CERT_FILE=/root/.ccr/ca-bundle.crt` (`native-provider.md`). The bench already passes `SSL_CERT_FILE` to every trial |
| Connectivity call (non-task, through `stack-agent work --kind inspect` on a one-file project) | `HTTP 429 billing_not_active`, `delivery confirmed_response`, not retried. No usage was returned. Cost: $0 |
| Cross-check (one direct request, `gpt-4.1-nano`, 5 tokens max) | The same refusal: `billing_not_active`, no usage. So the refusal is account-wide, not model-specific. Cost: $0 |
| PAX | Not installed at first. Built by the CI-documented procedure (`.github/workflows/ci.yml`): `rkendel1/pax` tag `v0.4.1`, verified at commit `674f3b3143874d1a692aca103b33f89da31a82ac`, `cargo build --release --locked`. Binary `sha256 05e3d861…b4cbf7`, `pax 0.4.1`. Placed on `PATH` only for this session (scratchpad); nothing in the repository changed |
| Task E fixture and evaluator (deterministic, no inference) | A fixture built by hand matches `build_fixture(E)` file for file (hashes `a63e0080…`, `b7dbb2fc…`, `1d5fa16d…`, `f9b1ca6a…`). At the base, `pax test` → `failed / tests-failed` (0 passed, 1 failed). After the reference change (`MAX_RETRIES = 5`), `pax test` → `passed`, and the harness's own `cargo test --offline` exits 0 |
| Suites that need PAX, now with PAX | `work` (20), `micro_shadow` (6), `capability_scenarios` (34), `completion_evidence` (5), `product_path` (5), `skill_experiment` (18): all pass, none skipped. The six tests that had failed only for lack of PAX now pass. `agent_bench` evaluator unit tests: 14 pass |
| Arms | `skill_experiment check`: baseline adds 0 bytes; skill `sha256:1d43d17f…8caa7c` adds 2,416 bytes; control `sha256:ad57e213…432dcc1` adds 2,413 bytes, behind the same 151-byte label. The baseline and equivalence tests pass |

## Pricing found (for when the account is active)

`gpt-6-luna` was not run, so no cost was incurred. Published rates are $0.10 per million input
tokens, $0.50 per million output, and about $0.01 per million cached input. These come from
secondary sources: VentureBeat, Yahoo Finance, Kingy AI and BenchLM. One listing shows $0.20 /
$1.00, and the pre-cut rate was $0.20 / $1.20.

A spend ceiling should be computed from the trials' own `usage` fields at the conservative
$0.20 / $1.20.

A rough worst case is about $0.15 per trial (14 calls, about 4k input and up to 8k output tokens
each), or about $4.50 for 30 trials. A $10 ceiling is proposed. The rule: before each trial, stop if
the spend so far plus one worst-case trial would exceed the ceiling.

## Provenance at the stop

| Item | Value |
| --- | --- |
| HEAD | `83b72e7cb7a664752802f0593bff9ca24fd3daa4` |
| Tracked diff (`git diff HEAD`) | 336 lines, `sha256 76b401ef3f81332adf70b18329bbb6a4abf35ade06f3a99c9bf1402dc02ae58a` |
| Untracked files | `examples/skill_experiment.rs` `19cc32cf…`, `src/skill_experiment.rs` `0108b7c5…`, `tests/skill_experiment.rs` `07d6919c…`, `native-skills-experiment.md` `f22ba255…`, `PREREGISTRATION.md` `9210024d…`, `length-control-v1.md` `90b94b12…` (and this file) |
| `target/release/stack-agent` | `sha256 843018cc5fa5cfcc2e65a9d16725445d1de8ae3eea75d87a6389c5c8b5aac14d` |
| `target/release/examples/skill_experiment` | `sha256 f9533d80799de788ecd40f1384925879319cf38f941790f6e0c909af244fce5a` |
| Evaluator | `agent_bench::judge` and `run_project_tests`, unchanged at HEAD |

## What must change before an OpenAI run (not done here; each is small)

1. **An active OpenAI account,** or a key for one. Nothing in the repository can fix this.
2. **The runner accepts only local providers today** (`run-schedule --provider local|local-object|local-none`).
   It needs `--provider openai --model <id>`. It must pass the key to `run_trial` (which hands it only
   to the child, through `OPENAI_API_KEY`) and redact the key from every artifact the launcher writes.
3. **The runner refuses a checkout with tracked changes** (pre-registration §10). The experiment
   code is uncommitted, by direction, so the run needs either a commit or an amendment that
   identifies the source by HEAD plus the saved diff and untracked-file hashes. That fingerprint
   would be checked again before every trial.
4. **A pre-registration amendment, before any trial:**
   - model `gpt-6-luna`, cell `openai` (flat schema, `max_completion_tokens 8192`, default
     temperature, which is not 0, so trials are not deterministic);
   - the transport (`SSL_CERT_FILE`);
   - PAX 0.4.1 at `674f3b3`;
   - the spend ceiling and stop rule above;
   - the trial count re-justified. Because sampling is not deterministic here, 10 rounds give real
     repetitions; the exact McNemar thresholds in §8 are unchanged.

## Sources for pricing

- [VentureBeat: OpenAI releases GPT-6 Sol and Luna](https://venturebeat.com/technology/openai-releases-gpt-6-sol-and-luna-models-slashing-api-costs-50-or-more)
- [Yahoo Finance: OpenAI cuts GPT-6 Sol and Luna API prices](https://finance.yahoo.com/technology/ai/articles/openai-cuts-gpt-6-sol-111351382.html)
- [Kingy AI: GPT-6 Sol and Luna specs and pricing](https://kingy.ai/blog/gpt-6-sol-luna-specs-benchmarks-pricing-comparison/)
- [BenchLM: OpenAI API pricing](https://benchlm.ai/openai/api-pricing)
- [pricepertoken.com: GPT-6 Luna](https://pricepertoken.com/pricing-page/model/openai-gpt-6-luna)
