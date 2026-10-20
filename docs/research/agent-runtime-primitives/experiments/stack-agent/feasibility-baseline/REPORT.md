# Report: Stack Agent baseline feasibility (local Gemma, no pattern context)

Preregistration: `PREREGISTRATION.md` (committed e47b42a, harness fcb5457, both before any
trial). No deviations. All 12 trials ran once; none dropped, none rerun. Local model only
(`google/gemma-4-26b-a4b-qat` at localhost); no hosted inference, no external transmission,
no cost. No Stack Agent code, default, limit, guard or validation was changed.

## Result

| Cell | Task | Accepted | Final categories | Time per trial |
|---|---|---|---|---|
| A (2048 tokens, default) | F1 list `src` | 3/3 | accepted x3 | 2-10 s, 3 calls |
| A | F2 read-only question | **0/3** | length_limited x3 | 26-29 s |
| B (8192 tokens) | F1 | 3/3 | accepted x3 | 2-6 s |
| B | F2 | **0/3** | length_limited x3 | 154-191 s |

Stop rule 1 applied to both cells: F3-F5 were **not run**. Decision gate: F2 fails ->
investigate the model-to-completion interaction. No repeatable implementation task exists,
so the pattern-utility experiment must not proceed.

Traces: `results/<cell>-<task>-<n>.json` (12 files).

## What the evidence supports

- **Capability discovery is feasible through the interface** (F1 3/3 in both cells;
  F2's first two calls were a valid `project.list` then a valid `project.read`
  of `src/math.rs`, 6/6 trials). The capability list and input shapes are in the prompt
  (DESIGN §1, §3). Not "not feasible via interface" and not "failed to use an available capability".
- **F2 failed at the completion decision in 6/6 trials, always `length_limited`.** After the
  correct read, the reply that should carry the answer ran to the output limit and was
  rejected as a typed failure. In every F2 trial `goal_satisfied` was true (the observations
  held the answer) and terminal state was `failed`: the model's answer never arrived as a decision.
  Conclusion category: **guard or limit terminated the work** (output limit, not the stuck guard:
  0 stuck events; not invalid requests: 0; not timeouts: 0).
- The limit is not just a too-small default: quadrupling it (cell B) made each trial take
  ~6x longer (154-191 s vs 26-29 s) with the same outcome. In the earlier audit run the reply
  started correctly ("triple ... 3 * n") and continued to 8191 tokens with 0 reasoning tokens
  and 39,055 content characters in the `summary` string. That is consistent with unbounded
  generation inside the summary field, **not** with a thinking budget. I did not inspect the
  full text of the six trial replies, so the audit run is the only direct evidence of the shape.
- F1 passes while F2 fails; the difference is the completion `summary`: F1's completion
  was 62 tokens, F2's ran away. This is a single contrast between two tasks, not a
  controlled isolation of the cause. Why the F2 summary degenerates is **unresolved**.

## Implemented / tested / measured / unresolved

- Implemented: fixture crate, hidden acceptance tests and reference solutions for F3-F5,
  logging proxy, trial harness, ladder driver with stop rules, offline test of acceptance
  and classification.
- Tested (offline, no model): `validate_fixture.py` 6/6 (unmodified fails F4/F5, references pass);
  `test_harness.py` 12/12 (references accepted, unmodified and unrelated changes rejected, audit
  report classified `length_limited`).
- Measured (live, local model): F1 and F2 in two cells, n=3 each (12 trials).
- Not measured: F3, F4, F5 acceptance with a model (stopped by rule). Whether any change task is
  feasible is **unknown**, not negative.
- Unresolved: cause of runaway summary generation; whether other settings (e.g. canonical
  schema, a different model) change it. Those are outside this PR's allowed changes.

## Limitations (honest)

- One model, one fixture, n=3 per cell; temperature 0.0, so repetitions are near-deterministic
  and are weaker evidence than three independent samples.
- The harness did not capture the proxy record of the limiting third call (the proxy was
  terminated before it was flushed), so those trials have no per-call token usage for that
  call; length evidence there comes from Chip's own `model_failures` and elapsed time. The
  audit run did capture it.
- Cells A and B differ in one flag, but B is a documented bench value, not a product default.

## Recommendation (next experiment)

Do not start pattern utility. Next, with the same fixture and preregistration style, a small
experiment on the **completion interaction only**: characterize why an inspect completion's
`summary` does not terminate (e.g. capture full replies for F2 at several allowed settings
already exposed as flags: `--structured-output`, `--temperature`), without relaxing the guard, validation or
limits. If a repeatable F2 appears, resume this ladder at F3.

## Workspace checks

No Rust crate changed in this PR (only files under this experiment directory; the fixture has its own
`[workspace]` and is excluded from the repo workspace). See PR body for fmt/test results.
