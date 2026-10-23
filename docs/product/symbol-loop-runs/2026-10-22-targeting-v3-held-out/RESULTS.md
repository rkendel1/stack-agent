# Targeting v3 held-out evaluation: results

**Decision: FAIL. Targeting work remains.** The repair generators stay off. These 12 fixtures are
now development evidence. They must not be rerun as a held-out set.

## Integrity

- **Frozen hashes.** All 11 entries in `FROZEN.sha256` match: before the fixtures were ready, at
  the start of the evaluation, and again before scoring (`logs/heldout-reference-check.log`).
- **Fixtures.** 12 fixtures, 3 per category (deep, dispatch, multi, declaration), written by an
  independent agent that had neither the targeting source nor these criteria. Every fixture has
  `GROUND_TRUTH.json` and `solution.patch`, and the patches change `src/` only. Package names are
  all distinct.
- **Reference fixes, checked again by the evaluator.** In scratch copies, each with its own build
  directory, all 12 fail `cargo test --offline` before the fix (exit 101) and pass every test
  after `solution.patch` (exit 0). The failing tests match `logs/fixture-VERIFICATION.md`. f10 and
  f12 fail because the test file does not compile, as designed.
  - A first check that shared one build directory between the before and after copies of the same
    package wrongly showed every fix failing. It was rerun with separate build directories. Both
    runs are described in the log.
- **No model calls, and nothing edited.** Scoring took one run of the frozen binary per fixture.

## Scorer defect, and how it is reported

The designated scorer, `scorer/eval_v3.py`, has a defect. It splits a candidate id with
`rsplit(":", 1)`, which cuts inside the `::` of `Type::method`. As a result, **no method target
can match**; only free functions are counted.

- Its unchanged output is the designated result: `raw/results.json`, `logs/heldout-eval.log`.
- `scorer/rescore_v3_idfix.py` rescores the **same raw outputs**. It changes only the id parsing:
  ids are split at `.rs:`. Criteria and thresholds are unchanged, and no ranking is rerun. Output:
  `raw/results-idfix.json`.

**Both scorings FAIL.** The rescoring changes P1 and P4's value. It does not change the decision.

## Preregistered criteria

| Criterion | Designated scorer | ID-fix rescoring |
|---|---|---|
| P1 no silent miss | **FAIL** (f09 `Lexer::read_ident`, an artifact of the scorer defect: it is candidate 29) | pass |
| P2 no false uniqueness | **FAIL** (f03) | **FAIL** (f03) |
| P3 no false "not expressible" | pass | pass |
| P4 recall ≥ 80% (deep + dispatch) | **FAIL** 2/6 = 33% | **FAIL** 4/6 = 67% |
| P5 ranking < 10 s, budget exhaustion reported | pass (max 3 ms; every search complete) | pass |

## Per fixture (ID-fix rescoring; ranks are 1-based among all candidates)

| Fixture | Category | True target: rank | Candidates | Decision | Concealment flagged | Ranking ms (index ms) | Reference fix |
|---|---|---|---|---|---|---|---|
| f01 | deep | `tier_discount_pct`: **missed** | 11 | ambiguous | yes | 1 (22) | fails → passes |
| f02 | deep | `unescape`: 19 | 19 | ambiguous | yes | 1 (20) | fails → passes |
| f03 | deep | `page_bounds`: 29 | 29 | **single → `paginate` (wrong)** | yes | 2 (28) | fails → passes |
| f04 | dispatch | `SmsChannel::render`: **missed** | 8 | ambiguous | yes | 0 (19) | fails → passes |
| f05 | dispatch | `RunLength::encode`: 25 | 27 | ambiguous | yes (unresolved dispatch) | 1 (22) | fails → passes |
| f06 | dispatch | `TokenBucket::refilled`: 10 | 15 | ambiguous | yes (unresolved dispatch) | 1 (16) | fails → passes |
| f07 | multi | `parse_amount`: 1, `Ledger::statement`: 6 | 14 | ambiguous | yes | 1 (16) | fails → passes |
| f08 | multi | `Version::parse`: 1, `Version::cmp`: **missed** | 7 | ambiguous | yes | 1 (22) | fails → passes |
| f09 | multi | `Lexer::read_ident`: 29, `compare_values`: 36 | 36 | ambiguous | no | 3 (37) | fails → passes |
| f10 | declaration (compile) | `validate`: 7, `describe`: 11 | 15 | not_expressible ✓ | yes | 0 (15) | fails → passes |
| f11 | declaration (runtime) | `record`: 9, `total_bytes`: 3 | 11 | ambiguous (shape `possible`) | yes | 1 (24) | fails → passes |
| f12 | declaration (compile) | `parse_duration`: 1, `Job::from_line`: **missed** | 4 | not_expressible ✓ | yes | 0 (22) | fails → passes |

## Per category (ID-fix rescoring)

| Category | Targets in candidate set | In top 8 | Mean rank (found) | False single | False "not expressible" |
|---|---|---|---|---|---|
| deep | 2/3 | 0/3 | 24 | **1** (f03) | 0 |
| dispatch | 2/3 | 0/3 | 17.5 | 0 | 0 |
| multi | 5/6 | 3/6 | 14.6 | 0 | 0 |
| declaration | 5/6 | 3/6 | 6.2 | 0 | 0 (2/3 correctly not expressible; f11 undetected) |
| **all** | **14/18** | **6/18** | 13.4 | **1** | **0** |

- **Runtime.** Ranking takes 0–3 ms per fixture (worst case 3 ms, f09). Indexing takes 15–37 ms.
  Every walk was complete (no budget stopped one). There were no acceptance timeouts.
- **Multi/declaration classified not expressible: 2/6.** Both are the compile-error declaration
  fixtures. f11 (a runtime failure that needs a field-type change) and all three multi fixtures
  were assessed `possible`. That is not a false claim (`possible` asserts nothing), but the shape
  check did not detect them.

## Failures, by cause

1. **False confidence (safety): a panic location is treated as the defect location.** In f03 the
   slice panic is inside `paginate`. The defect is in `page_bounds`, a callee that computes the
   bounds `paginate` slices with. `decide` returns `single` whenever exactly one non-test function
   holds a compile error or panic. A panic shows where a bad value was *used*, not where it was
   *made*. This rule picked a wrong single target and would have sent the loop to edit the wrong
   function. It is the decisive failure.
2. **Honest miss: the receiver's type comes from a helper's return type.**
   - In f01, `let mut shop = shop(); shop.place_order(..)`: the test helper returns `Shop`, but the
     test function never names it.
   - In f04, `let mut d = dispatcher(); d.dispatch(..)`: the same pattern.
   - The method call gets no edge, so the whole layer below it is never walked, including the
     `Vec<Box<dyn Channel>>` dispatch that leads to `SmsChannel::render`.
   - The frontier listed these exact calls (`.place_order`, `.dispatch`) and their plausible
     targets, with `could_conceal_target = true`. The misses were reported, not concealed.
3. **Miss flagged for the wrong reason: operator and trait-implicit calls.** f08's `Version::cmp`
   is reached only through `>`, `<` and `.max()` on `Version` (`Ord`/`PartialOrd`). The call graph
   has no expression call to it, and the frontier does not record operator dispatch. Concealment
   was flagged only because of an unrelated untyped call. Under the rule, P1 holds for f08, but the
   reported reason does not name the real gap.
4. **Weak ranking for deep targets.** Depth decay puts true targets at ranks 19–29 in deep chains.
   Only 6 of 18 targets fall within the top 8 a classification is offered.
5. **Measurement defect.** The scorer's id parsing (above) undercounted method targets.

## Smallest next targeting change (for a future evaluation, not done)

1. **Remove "a panic location alone yields a single target."** A function holding a panic becomes
   a candidate together with the callees whose results it uses. `single` would then require
   direct evidence that is not a run-time use site: a compile error inside a body, or one
   candidate with nothing concealed. This is the safety fix.
2. **Type a `let` binding from the declared return type of the function it calls.** Use only the
   type named in the callee's signature (`fn shop() -> Shop`), and give no edge when the callee is
   unresolved or generic.
3. **Record operator and trait-implicit dispatch in the frontier.** Comparison and arithmetic
   operators, and `sort`/`max`/`min`/`==` on types with in-scope `Ord`, `PartialOrd`, `PartialEq`
   or `Add` implementations. Either follow them as dispatch edges or list them as unresolved.
4. **Fix the scorer's id parsing, and freeze it with a self-test** on known ids before the next
   pre-registration.

Validate on a **new, independently written held-out set**. Keep T1, T3 and f01–f12 as development
evidence. Keep the same criteria (P1–P5).
