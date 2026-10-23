# Targeting v5 held-out evaluation: results

**Decision: FAIL. Targeting work remains.**

- **Failed:** P1 (a silent miss on g12) and P4 (deep + dispatch recall 5/7 = 71%).
- **Passed:** the safety criteria P2, P3 and P6, and P5.
- **Next stage:** repair generators stay off.
- **Fixtures:** g01–g12 are now development evidence.

## Integrity

- **Frozen state.** Revision `38f1b9d`. The `rank_target.v5frozen` binary (sha `9d45a273…`) is
  byte-identical to the v4 development binary. Scorer v4, `analyze_v5.py`, `AUTHOR_INSTRUCTIONS.md`
  and `PREREGISTRATION.md` are all in `FROZEN.sha256`. Every hash matched before verification,
  before the run and after it.
- **Fixtures.**
  - 12 fixtures, 3 per category, written by a fresh agent that had only `AUTHOR_INSTRUCTIONS.md`.
  - All six required situations appear: `producer_consumer` g01, `implicit_dispatch` g06/g07/g10,
    `factory_or_helper_type` g02/g10, `uncompiled_test` g04, `ambiguous` g09, `large_search` g05.
  - The archive is `heldout-v5-fixtures.tar.gz` (sha in `fixtures.sha256`), and the author's record
    is `fixture-author-VERIFICATION.md`.
- **Evaluator's validity checks** (`verify_fixtures.py`; `verification.json`, `verify-logs/`):
  - structure, balance and documented paths are complete;
  - in fresh copies, each with its own build directory, all 12 fail before the reference fix and
    pass every test after it (g04's test file fails to compile, as designed);
  - no fixture was excluded.
- **One run, no infrastructure fault.** Logs: `run-scorer.log`/`.stderr`, `run-analyze.log`/`.stderr`.
  Raw outputs and results: `raw/`. No model calls were made, and nothing was edited.

## Criteria

| Criterion | Result |
|---|---|
| P1 material omissions reported (deciding) | **FAIL**: g12 `OrderStatus::fmt` was missed with `could_conceal_target = false` |
| P1 flag only (scorer v4, for reference) | FAIL (the same miss) |
| P2 no false uniqueness, ambiguous cases included | pass (no `single` decision on any fixture) |
| P3 no false "not expressible" | pass |
| P4 recall, deep + dispatch | **FAIL**: 5/7 = 71.4% |
| P5 ranking ≤ 10 s, cut-offs reported | pass (max 20 ms; g05's budget stop is reported) |
| P6 producer/consumer safety | pass (g01 is ambiguous, not `single` on `paginate`) |
| P7 implicit-dispatch recall (reported) | 2/4 targets; top-10 0.25, MRR 0.048 |

P2 and P6 hold partly because v4 made **no** `single` decision anywhere. That is safe, but it
means P6 was never exercised against a confident wrong pick.

## Per fixture

| Fixture | Category | Tags | Decision | Target ranks (candidates) | Concealment flag | Ranking ms (index ms) |
|---|---|---|---|---|---|---|
| g01 | deep | producer_consumer | ambiguous | `page_bounds` 15 (28) | yes | 3 (24) |
| g02 | dispatch | factory_or_helper_type | ambiguous | `CsvEncoder::encode_field` 20 (23) | no | 12 (31) |
| g03 | multi | — | ambiguous | 18, 3 (20) | no | 3 (27) |
| g04 | declaration | uncompiled_test | **not expressible** ✓ | `Backoff::delay_for` 4, `parse_backoff` **miss** (5) | yes | 0 (29) |
| g05 | deep | large_search | ambiguous | `stage_21::r14` **miss** (400, candidate budget) | yes | 20 (365) |
| g06 | dispatch | implicit_dispatch | ambiguous | `Job::cmp` 11 (16) | yes | 3 (30) |
| g07 | multi | implicit_dispatch | ambiguous | `Sku::eq` 10, `StockLine::fmt` **miss** (14) | yes | 3 (24) |
| g08 | declaration | — | ambiguous | `Parcel::billable_grams` 11 (15) | no | 3 (20) |
| g09 | deep | ambiguous (any_of) | ambiguous ✓ | 15, 11 (22) | yes | 6 (31) |
| g10 | dispatch | factory, implicit_dispatch | ambiguous | `RotatedSource::next` **miss** (5) | yes | 1 (17) |
| g11 | multi | — | ambiguous (shape `unknown`) | 1, 6 (16) | yes | 5 (33) |
| g12 | declaration | — | ambiguous | `Order::status` 6, `OrderStatus::fmt` **miss**, `customer_message` 9 (19) | **no** | 4 (26) |

## Per category (P8; reported, no threshold)

| Category | Found | Top-1 | Top-5 | Top-10 | MRR | Mean rank found |
|---|---|---|---|---|---|---|
| deep | 3/4 | 0 | 0 | 0 | 0.056 | 13.7 |
| dispatch | 2/3 | 0 | 0 | 0 | 0.047 | 15.5 |
| multi | 5/6 | 0.167 | 0.333 | 0.667 | 0.276 | 7.6 |
| declaration | 4/6 | 0 | 0.167 | 0.5 | 0.103 | 7.5 |
| **all** | **14/19** | 0.053 | 0.158 | 0.368 | 0.139 | 10.0 |

- **Multi/declaration fixtures classified not expressible:** 1/6 (g04).
- **`could_conceal_target` set:** 8/12.
- **Runtime:** ranking 0–20 ms; indexing 17–365 ms (g05's 7,400 lines).

## Every miss, judged under the preregistered P1 rule

| Fixture | Missed target | Gap (via) | Preregistered P1 judgment | What the report actually says |
|---|---|---|---|---|
| g04 | `parse_backoff` | `parse_policy`, called from a test that never ran | reported (uncompiled test file) | Correct: the frontier names `tests/retry.rs` as not compiling, with its unobserved functions. |
| g05 | `stage_21::r14` | the chain beyond the 400-candidate budget | reported (budget stop) | Correct: the candidate budget stopped the walk, and it says so. |
| g07 | `StockLine::fmt` | reached through `Display` (`format!`/`{}`) | reported (excluded call from the predecessor) | **Not really.** The cited excluded calls are unrelated (`.as_str`, `.shortfall`, `.needs_reorder` on untyped receivers). Nothing names the implicit `Display::fmt` call. |
| g10 | `RotatedSource::next` | `Iterator::next` from a `for` loop over `Box<dyn Iterator>` | reported (excluded call from the predecessor) | **Not really.** The cited calls are `Summary::default` and `Vec::new`, non-concealing external paths. The flag is set only because of an unrelated `.parse`. Nothing names the implicit `next` call. |
| g12 | `OrderStatus::fmt` | reached through `Display` (`.to_string()`) | **not reported: P1 fails** | `could_conceal_target = false`, with an empty concealment list. A silent miss. |

**The preregistered P1 rule was too lenient.** Rule (d) accepts any excluded call from the
predecessor, including non-concealing kinds and unrelated calls. Under a strict reading, g07 and g10
are also undiagnosed misses: three of the five misses would lack a meaningful explanation, not one.
The rule is not changed after the fact, and the decision is FAIL either way. A future
preregistration should require the reported exclusion to be of a concealing kind and to match the
gap's call (see below).

## Causes

1. **Implicit trait calls beyond operators are not modeled.** This is the main cause: a silent miss
   (g12) and two misses whose explanations are wrong (g07, g10).
   - v4 follows comparison, equality and arithmetic operators and `.max()`/`.sort()`-style
     methods.
   - It does not follow `Display::fmt` through `.to_string()`, `format!`, `write!` or `{}`, nor
     `Iterator::next` through `for` loops and iterator adapters on user iterators.
   - It does not even record these as unresolved, so the frontier cannot point at them.
2. **Large search.** g05's target lies beyond the 400-candidate budget, about 20 calls deep in
   877 generated functions. This is reported correctly, but it is still a recall miss.
3. **Uncompiled tests.** g04's second target is unobservable, and this is reported correctly. Not a
   defect.
4. **Ranking is weak.**
   - Overall top-1 is 5% and top-10 37%, with MRR 0.139.
   - Deep and dispatch have no target in the top 10.
   - Targets that are found mostly sit below the 8 candidates a classification is offered.

## Proposed next step (not implemented)

1. **Follow or record implicit `Display`/`Debug` and `Iterator` calls.**
   - Treat `.to_string()`, `format!`/`write!`/`println!` arguments, and `{}` formatting of a value
     whose type has an in-scope `Display` impl as calls to that `fmt`.
   - Treat `for x in expr` and iterator adapters over a value whose type (or `dyn` trait) has an
     in-scope `Iterator` impl as calls to its `next`.
   - When the operand's type is unknown, record an `implicit_operator` exclusion naming the
     candidate impls, so the miss cannot be silent.
2. **Tighten the P1 rule in the next preregistration.** A material omission counts as reported only
   through a concealing-kind exclusion, or a hidden listing, that names the gap function or its
   call, or a budget stop, or an uncompiled test file. Fix the rule before the next fixtures exist.
3. **Large-search budgets.** Keep the budget explicit. Report the frontier depth reached against
   the deepest documented path. Decide separately whether a larger default budget is justified,
   since cost is only 20 ms here.
4. **Re-evaluate on another fresh, independently written held-out set.** g01–g12, f01–f12, T1 and
   T3 are development evidence only.

Repair generators stay off.
