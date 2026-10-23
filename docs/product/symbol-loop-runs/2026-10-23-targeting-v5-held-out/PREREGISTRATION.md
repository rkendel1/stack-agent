# Targeting v5 held-out evaluation: preregistration

Written 2026-10-23, before the fixture author was started and before any fixture existed. Nothing
in this directory's frozen files changes after the author starts. If anything frozen must change,
the evaluation is void and is reported as such.

## What is frozen

- **Targeting implementation:** git revision `38f1b9d063f5a8508b117d04d558849c992c085b` on
  `claude/symbol-loop-targeting` ("targeting v4"), with a clean worktree. Its sources are hashed
  in `FROZEN.sha256`.
- **Evaluation binary:** `rank_target.v5frozen`, built from that revision (debug profile).
  - SHA-256 `9d45a273db70caed81be59cad86b5507d6e0965ccebb8afe1e0a62c2ce7e5a4d`.
  - It is byte-identical to the v4 development binary.
- **Scorer:** `../scorer-v4/score_v4.py` and its tests, unchanged.
- **Supplementary analyzer:** `analyze_v5.py` and its tests (`test_analyze_v5.py`, 7 known-answer
  tests, passing). It computes P1 (material omissions), the ambiguous-case part of P2, P6, P7, P8
  and the decision from scorer v4's outputs. It never reruns ranking.
- **Fixture-author instructions:** `AUTHOR_INSTRUCTIONS.md`.

All of these are in `FROZEN.sha256`.

## Development evidence (never held out)

T1, T3, the v3 fixtures (`f01`–`f12`) and the tests in the repository. Their artifacts in
`../2026-10-22-*` are not modified.

## Fixtures

- **Author.** 12 new fixtures (`g01`–`g12`), 3 per category: deep, dispatch, multi, declaration.
  They are written by a fresh agent that receives only `AUTHOR_INSTRUCTIONS.md`: behavioral
  requirements, the ground-truth format, and isolation rules forbidding it to read the
  implementation, the earlier fixtures or the repository.
- **Required situations.** The instructions require each of these at least once:
  `producer_consumer`, `implicit_dispatch`, `factory_or_helper_type`, `uncompiled_test`,
  `ambiguous` and `large_search`.
- **Ground truth for each fixture:** true targets, an `any_of` flag for ambiguous fixtures, tags,
  the panic site when it differs from the targets, and at least one documented call path from a
  failing test to each target.

## Validity checks, before any ranking is run

1. There are 12 fixtures, 3 per category, every required tag appears at least once, and every
   `GROUND_TRUTH.json` has the required fields, including a documented path for every target.
2. **Reference fixes, re-verified by the evaluator.** In fresh copies, each with its own build
   directory, `cargo test --offline` fails before the reference fix (for `uncompiled_test`, the
   test file does not compile) and passes every test after it. The author's `VERIFICATION.md` must
   record the same.
3. All hashes in `FROZEN.sha256` match.

A fixture that fails check 1 or 2 is excluded, and listed with the reason before any ranking. It is
not repaired or replaced. If the remaining set lacks 3 fixtures in some category, or lacks a
required tag, the evaluation is reported as **incomplete**, and no PASS can be claimed.

## Procedure: one run

```
TMPDIR=<scratchpad>/v5-work python3 -I ../scorer-v4/score_v4.py run \
    <scratchpad>/rank_target.v5frozen <scratchpad>/heldout-v5-fixtures raw
python3 -I analyze_v5.py <scratchpad>/heldout-v5-fixtures raw
```

Run from this directory. Raw outputs, stdout and stderr, and both result files are kept.

- An infrastructure failure voids the run. Examples: a fixture that cannot be built for reasons
  unrelated to its defect, a tool crash, or a scorer error.
- A void run is documented, then fixed and run once more. The fix may touch only infrastructure
  (the harness, never targeting, scorer rules or fixtures), and is described.
- A crash of the frozen binary on a valid fixture is **not** infrastructure. It is a result: that
  fixture fails.

## Criteria

Scorer v4 computes P2 (single decisions), P3, P4 and P5 exactly as in the v3 preregistration. It
also computes a flag-only P1, which is reported for reference and is **not** the deciding P1.

- **P1, material omissions are reported (deciding).**
  - **Material omission.** A true target that is not in the candidate set, although the author
    documented a real call path from a failing test to it. Its denominator is the missed targets
    of all fixtures. An unresolved call that is not on a documented path to a missed target (for
    example a standard-library call, or code unrelated to the target) is benign and does not count
    either way.
  - **Judgment, independent of the implementation's explanation.** The *gap* is the first
    production step of the documented path that is not a candidate; the *predecessor* is the step
    before it. The omission is reported when `could_conceal_target` is true **and** at least one of
    these holds:
    - (a) the walk stopped on a budget;
    - (b) a concealment line names the path's test file as not compiling;
    - (c) the target or the gap function is listed as hidden, or as a plausible target of an
      excluded call;
    - (d) an excluded call, or a concealment example, comes from the predecessor.
  - A flag with no such report fails P1. The rule is mechanical (`analyze_v5.material_report`).
- **P2, no false uniqueness.** No `single` decision may name a function that is not a true target,
  and none may be made for a `multi` or `declaration` fixture (scorer v4). Also, no `single`
  decision on a fixture tagged `ambiguous` (or with `any_of`), even when it names one of the valid
  targets: the evidence does not determine one.
- **P3, no false impossibility.** No `not_expressible` decision for a `deep` or `dispatch` fixture.
- **P4, recall.** At least 80% of the true targets of `deep` and `dispatch` fixtures enter the
  candidate set. Every listed target counts, including each alternative of an `any_of` fixture.
- **P5, bounded cost.** Ranking takes at most 10 seconds (`rank_ms`), and every budget stop is
  reported in the frontier.
- **P6, producer/consumer safety.** For a fixture with a `panic_site` that is not a true target, the
  decision must not be `single` naming the panic site.
- **P7, implicit dispatch (reported, no threshold).** Target recall over fixtures tagged
  `implicit_dispatch`.
- **P8, ranking usefulness (reported, no threshold).** Per category and overall: top-1, top-5 and
  top-10 recall, mean reciprocal rank (a miss counts 0), and mean rank of the targets found. No
  pass threshold is set, now or after the results.

Also reported: per-fixture decisions, every miss with its gap and predecessor, every budget stop,
candidate counts, runtimes, and the rate at which `could_conceal_target` is set.

## Decision

- **FAIL — targeting work remains** if any of these holds:
  - P1 (material), P2, P3 or P6 fails;
  - P4 is below 80%;
  - P5 fails;
  - a fixture errored;
  - the set is incomplete.
- **PASS — targeting eligible for the next stage** otherwise.

A PASS permits only a separately scoped exact-replay generator experiment. It does not authorize
broad autonomous repair.
