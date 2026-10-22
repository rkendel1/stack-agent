# Small local models: baseline, changes, and before/after evidence

**Result: no change in acceptance.** Every condition passed 3 of 12 trials. In each case the passes were task D, the
safety task, where the right outcome is to refuse the write. The 3B model completed none of the answer or change tasks,
before or after.

Some things did change, and they are measured here:

- **Speed.** Trials took about half as long (mean 205 s before, 118 s after). The deadline failures that had capped
  task A disappeared (3 before, 0 after). About 73-76% of input tokens were served from llama.cpp's prompt cache.
- **Schema-constrained output works.** Before, the schema mode could never request a capability (0 executions in 12
  trials). After, it behaves like the unconstrained mode (50 executions in 12 trials).
- **Rejected decisions are recoverable.** A rejected request or completion is now answered with a reason and the work
  goes on, within the existing bound. The model did use that second chance: it escalated honestly instead of failing,
  and in one schema trial it went on to make the correct source change. It never turned a refused claim into a
  completion.
- **What still blocks task A.** The model gives the right answer (`acme-inventory`, `serde "1.0.200"`, `4096 units`)
  but cites no file. Chip's answer rule requires a cited file, so it refuses the answer and says why. The model then
  repeats the same answer word for word ([replay](runs/task-a-replay-replies.txt)). That rule was not weakened.

## Setup (identical in every condition)

| | |
| --- | --- |
| Model | Qwen2.5-Coder-3B-Instruct, Q4_K_M GGUF (sha256 `724fb256…`, matches the Hugging Face file) |
| Server | llama.cpp `llama-server` b1-c7fc509, CPU build, 4 threads, `-c 16384 --jinja`, 4 slots, `127.0.0.1:1235` |
| Machine | 4 vCPU x86_64, no GPU. Prompt processing about 48-52 tok/s, generation about 7-11 tok/s |
| Harness | `stack-agent --agent-bench`, existing tasks A, B, D, E (fixtures and oracles unchanged), 3 trials each, legacy state arm |
| Fixed settings | `--max-output-tokens 8192 --call-timeout-secs 120`. Per-task turns, executions, deadline and context budget as the harness defines them (A, B, D: 12/12/300 s/24 KB; E: 14/10/600 s) |
| Cells | `local-object` (`--provider-profile local --structured-output object`) and `local` (the canonical `json_schema`) |
| Binaries | baseline `e73e74a` (main) and final `fde4c16` (this branch), both `--release` |
| Isolation | the four sets ran back to back on an otherwise idle machine, with nothing building or testing alongside them |

Task C (a 40-turn workspace inspection) was not run. At these speeds one trial would take 20 minutes or more.

Commands: `run-matrix2.sh BIN OUT PROVIDER` runs, for each task in A B D E and each trial 1-3:

```
stack-agent --agent-bench --providers PROVIDER --tasks TASK --trials 1 \
  --local-model qwen2.5-coder-3b-instruct-q4_k_m \
  --local-endpoint http://127.0.0.1:1235/v1/chat/completions --out OUT/TASK-N
```

The raw trial records are in [`runs/`](runs/): `clean-base`, `clean-final`, `clean-base-schema`, `clean-final-schema`.

## Results (n = 12 per row; failures stay in the denominator)

| binary | structured output | acceptance pass | correct completion | false completion | answer correct (A, B, E) | schema-valid replies | deadline / call timeout | stuck guard | other blocked or escalated | ended at the rejection bound | model calls | executions | latency mean (median), s | input tokens | of which served from cache | output tokens |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| baseline e73e74a | local-object | 3/12 | 0 | 0 | 0/9 | 51/51 | 3 | 9 | 0 | 0 | 54 | 51 | 205 (200) | 113,740 | not recorded¹ | 1,724 |
| final fde4c16 | local-object | 3/12 | 0 | 0 | 0/9 | 67/67 | 0 | 8 | 3 | 1 | 67 | 52 | 118 (116) | 160,712 | 117,556 (73%) | 2,519 |
| baseline e73e74a | local (json_schema) | 3/12 | 0 | 0 | 0/9 | 12/12 | 0 | 0 | 12 | 0 | 12 | **0** | 10 (10) | 22,248 | not recorded¹ | 801 |
| final fde4c16 | local (json_schema) | 3/12 | 0 | 0 | 1/9 | 65/65 | 0 | 7 | 3 | 2 | 65 | 50 | 112 (106) | 152,744 | 115,698 (76%) | 2,536 |

¹ The baseline binary does not record cached tokens. llama.cpp's own log for the baseline runs shows a common prefix of
3-7% between consecutive requests (slot similarity 0.11), and the full prompt was reprocessed on every call: 2,000 to
2,400 tokens, 42-51 s each.

How to read it:

- **Acceptance** is the benchmark's own oracle (`task_complete`). The three passes in every row are task D, where an
  inspection asked to overwrite README.md correctly writes nothing.
- **No run in any condition reached `completed`.** So there was no correct completion and no false completion. Every
  safety audit was clean, and no unauthorized execution happened.
- **"Answer correct" 1/9 in the final schema row** is E-1. The model wrote the correct change to the retry policy (the
  oracle confirms it), but never ran `pax.test`, so the work was not verified or completed.
- **Recovery** (final rows). Every rejection was followed either by a new valid decision or by the bound. In E (3/3
  object, 2/3 schema) the model requested `project.git.diff` with `inputs:{}`, was told why, and then escalated with an
  accurate reason. On the baseline, E looped on `git.status` until the stuck guard stopped it. In A, refused answers
  were followed by the same answer (see above); 3 trials ended at the bound.
- **Tokens.** The final rows send more input tokens in total because the trials do more (more calls, no deadline cut).
  73-76% of those tokens were not reprocessed.

Earlier, contended runs (built while other CPU work ran) are not used. They also showed 120 s call timeouts on the
baseline (B-1, D-1). The clean baseline hit the 300 s whole-task deadline instead.

## Baseline: failure modes and where they come from

| Failure (baseline, clean) | Trials | Source |
| --- | --- | --- |
| The whole-task deadline passed after 6-7 calls. Each call reprocessed the full prompt (about 45 s on CPU), because about 8 KB of unchanging text sat in the last message, after the growing observations | A ×3 (and call timeouts in contended runs) | `chip-core/src/lib.rs` `Agent::model_turn` (observations first, user message last); `chip-core/src/work.rs` `EscalationContext::render` (goal and question inside the user message) |
| The goal's required observations were met, but the model kept reading the same file or polling `git.status` until the stuck guard stopped it. Nothing told it the goal was met | B ×3, D ×3 (D counts as safe), A-1 in an earlier run | `work.rs` `progress_line` was only used when Chip's own local completion was refused, and was never sent otherwise |
| The model polled `project.git.status` three times and was stopped. A repeated *successful* identical result got no note before the stop (repeated failures did) | E ×3 | `work.rs` `note_failure` covers failures only |
| Schema mode: on the first turn the model claimed completion, escalated or blocked, and never requested a capability, even when it said one was needed | 12/12 in the schema cell | `fx-native-provider/src/openai.rs` `body()`. The schema was parsed into a `serde_json::Value` and serialized again, which sorts members by name. llama.cpp generates members in schema order, so the `request_capability` branch had to begin with `capability`; once a reply began `{"decision":` only complete, escalate or block were possible. Reproduced directly: asked to emit a `request_capability`, the sorted schema produced `{"decision":"complete","summary":"Project list retrieved successfully."}`; the same schema in its written order produced the request |
| A completion the observations do not support, a request with invalid inputs, and a request for a capability the work does not offer each ended the work at once, so the model had no chance to correct it. Seen in the intermediate runs: A ×3 refused answers, E ×3 `inputs:{}` on `git.diff`; earlier reports show the same for larger local models (`agent-bench.md`, run 4) | | `work.rs` `drive()` (refused `Complete` returns `Blocked`), `Run::perform` (validation errors return `Blocked`), `Run::escalate` (only `MalformedReply` was recoverable) |
| Latent: under a coverage contract the inspect context overwrote `relevant_evidence[0]` with its compacted progress summary. That slot holds whatever the loop supplies first (a rejection diagnostic), so the full summary was sent twice and the loop's note lost | found while measuring the context budget | `stack-agent/src/inspect_context.rs` `build` |

Context overhead, measured before reducing anything: a first request for task A is 9.9 KB. 3.7 KB of that is the
goal's own instructions and 4.3 KB is the decision question and capability list. Neither changes across turns. With the
fixed order nothing is shortened, and it is mostly served from cache instead. The only text added each turn is a
42-byte closing line, plus a one-line goal-progress note once every requirement is met. The inspect workspace test with
a 24 KB budget stays within its budget.

## What changed

1. **The order of each request (Chip).** The goal and the question (the reply contract and capabilities) go first, as
   one system message. The observations follow in order, then the turn (state, evidence, prior decisions, ruled out).
   Nothing is removed or summarized differently. Each request now starts with the whole of the previous one except its
   last message (test: `what_never_changes_is_sent_first_and_each_request_extends_the_one_before`). No cache was added
   to Chip; the provider's own prefix cache does the work.
2. **The schema reaches the provider exactly as given (FX).** Both native wires (OpenAI-compatible `response_format`
   and Anthropic `output_config`) pass the schema text through byte for byte, after checking that it is JSON. The
   contract and its schema are unchanged; they already put `decision` first. Validation of every reply is unchanged.
3. **Rejected decisions recover within the bound that already exists (Chip).** There is one counter,
   `WorkSpec::malformed_reply_limit` (default 2 in a row), now applied to any rejected decision:
   - malformed replies (as before);
   - schema-invalid decisions;
   - unknown or unavailable capabilities. The diagnostic lists the capability ids the work offers.
   - inputs the capability refuses;
   - completions the observations do not support. The diagnostic gives the goal's progress, or the description of each
     answer rule the answer fails.

   Nothing in a rejected decision is executed or completed. No limit, deadline or stuck guard is reset; the count
   resets only when a request passes validation. Past the bound the work ends with the outcome that decision alone used
   to produce, so failure classes are unchanged. Limit `0` restores the old one-strike behaviour exactly, and the tests
   that pin that behaviour now set it. Provider failures and timeouts are still never retried. There is no second
   retry system.
4. **Evidence-based notes (Chip), compact and only when true.**
   - Once Chip's own check finds every requirement met, the turn says `goal progress: every required output has been
     observed; an answer the observations support can complete the work`.
   - An observation that arrives after the goal was met is no longer marked "did not satisfy the goal".
   - A successful request returning an identical result is noted, with a count, before the existing stuck guard would
     stop the work. It is the same mechanism as the repeated-failure note; no second detector was added.
5. **The inspect coverage context** compacts its own progress summary in place and keeps the loop's evidence intact
   (regression test added).
6. **`agent-bench`** records `reported_cache_read_tokens` per trial.

Not done, and why:

- **JSON repair:** it conflicts with the never-repair contract.
- **A second loop detector:** the stuck guard already exists, and the repeat note feeds it.
- **Batching reads:** not justified by these failures, and the contract has one decision per reply.
- **Runtime negotiation of structured output:** the provider profile already chooses it, and the defect was in how
  the schema was sent, not in which mode was chosen.
- **Shrinking prompts:** caching made it unnecessary. The bytes are processed once instead of every turn.

## References (source-verified; nothing installed or copied)

| Project | Commit read | What was transferable | Verified in source |
| --- | --- | --- | --- |
| little-coder (itayinbarr/little-coder) | `89d4fa0` | correction text per failure reason; a repeat check that exempts intervening state changes; at most 2 consecutive corrections | `quality-monitor/index.ts:14,84-99`, `quality.ts:17-24,51-66,78-109`. Its JSON repair (`output-parser/parser.ts:100-137`) was not adopted |
| smolagents (huggingface) | `96f33fa` | a parse error that states the expected form; an unknown-tool error that lists the valid names | `utils.py:198-245`, `agents.py:1453-1502`. It has no consecutive-error cap (only `max_steps` 20) |
| nanobot (HKUDS/nanobot) | `421bd6b` | at most 2 empty-reply retries; "did you mean" for unknown tools; budget wording that forbids claiming completion without evidence | `agent/runner.py:1064-1167`, `registry.py:122-157`, `utils/runtime.py:25-33` |
| Atomic Agents (BrainBlend-AI/atomic-agents) | `2a26bbc` | schema-constrained output delegated to Instructor | default system prompt 246 chars; progressive disclosure only in an example |
| agent-mini | not verified | none | no upstream could be confirmed; nothing is claimed from it |
| Locals (Generated-Computers/Locals) | README, `install.sh`, `pyproject.toml`, `examples/` at main | the idea that structured generation is an engine concern | `install.sh:15-19` refuses anything but Apple-silicon macOS. It is an MLX server, so it could not run on this Linux CPU host and was not benchmarked. Its "always parses" and loop-stopping claims are README-level only |

Stack-Agent's completion check (an authoritative observation plus answer rules) was already stricter than every
reference. None of them verifies a completion claim against observations.

## Tests

- **New:** `chip-core/tests/small_model_reliability.rs` (9 tests), two FX schema-order tests (`conformance.rs`,
  `anthropic.rs`), and `inspect_context.rs::the_loops_own_evidence_is_kept_ahead_of_the_inspection_progress`.
- **Updated:** the existing recovery suites (`chip-core` and `stack-agent` `malformed_reply_recovery.rs`).
- **Coverage map for the requested cases:**
  - native and fallback structured output: FX conformance tests and the schema-order tests;
  - malformed JSON and missing arguments: `malformed_reply_recovery`;
  - unknown and unauthorized tools: `an_unknown_capability_is_rejected_...`, `capability_scenarios` scenario 4,
    `inspect_is_read_only_...`;
  - provider error, empty and non-JSON replies: `provider_failures_and_timeouts_keep_their_class_...`,
    `incomplete_response`;
  - repeated actions and failures: `a_repeated_identical_result_is_noted_before_the_stuck_guard_...`, `stuck_guard`;
  - budget exhaustion: `recovery_spends_the_turn_budget_and_never_resets_it`,
    `a_completion_claim_that_keeps_being_refused_ends_at_the_bound_...`;
  - early completion claims: `an_early_completion_claim_is_refused_and_the_work_goes_on_to_the_evidence`;
  - read-only completion: `inspect_coverage`, `capability_scenarios`;
  - writes verified against bytes: `bounded_edit`, `stale_write`;
  - recovery keeps identity and budget: `malformed_reply_recovery`, `small_model_reliability`;
  - invariants: `audit_safety` asserted in every new test.
- **Tests that pinned one-strike behaviour** set limit 0, or now script the model insisting on the refused decision.
  Every assertion that nothing executed, was written or was completed is kept.
- **Workspace** (`cargo test --workspace`, `-j 2`): 2,183 passed and 9 failed before the last three fixes. All three
  are fixed and pass. The remaining 6 are the known failures on main in this environment: `pax_observe` ×2 (PAX 0.5.0
  installed, 0.4.1 pinned), `journal` keyed write (inode reuse), `chip-fx` architecture (`ureq` in
  `chip-appport-source`), and `chip-experiments` recordings ×2. `cargo fmt --check` is clean. `cargo clippy` reports
  no warning on any line this branch changed.

## Limitations

- **Small sample.** n = 3 per task, one model, one machine. With temperature 0 the replies are close to deterministic,
  so repeats add little variance. A difference of one trial is not meaningful.
- **No task C**, for time.
- **The latency gain depends on the provider.** It comes from the provider reusing its prefix cache. A provider
  without one gains nothing, and nothing gets slower.
- **Recovery has a cost.** A model that keeps repeating a refused decision now spends up to two more calls before the
  work ends, as with malformed replies.
- **The dominant remaining failure is the model's.** The 3B model does not act on the correction (a missing citation,
  a goal it has already met). The model choice, or how answer rules are phrased, is the next lever. Weakening the rules
  is not.
