# Targeting v4: development diagnostics (NOT a held-out evaluation)

**These results do not validate targeting.** f01–f12 were held out for v3. Their v3 failures were
then studied and used to write v4, so here they are development evidence, like T1 and T3. A pass
on them says only that v4 fixes what v3 showed. Whether v4 generalizes needs a new, independently
written held-out set (see "Next step").

- **Code:** the hashes are in `SOURCES.sha256` (targeting source, the `rank_target` binary,
  scorer v4).
- **Scorer:** `../scorer-v4/score_v4.py`, unchanged since its regression tests passed.
- **Earlier runs, kept as they are:**
  - `invalid-run-inside-repo-workspace/`: an infrastructure fault. The scorer copied the fixtures
    inside the repository, so cargo refused to build them. Every fixture shows a location-less
    compile error, and the results are void. Scorer v4 now works in a temporary directory and
    refuses to run below any `Cargo.toml` (tested).
  - `run1-before-uncompiled-file-note/`, `run2-before-recursion-bound/`: valid runs of earlier v4
    code. They are superseded by the final run for the two fixes described below.
- **Final run:** `raw/` (per-fixture `rank_target` output and `results-v4.json`), `logs/dev-run.log`,
  and the T1/T3 rankings in `raw-t1-t3/`.

## What changed from v3

**A. No certainty from a symptom location.**
- A compile error inside a body is still direct evidence. With declarations frozen, only that body
  can fix it.
- A panic inside a function is now **symptom** evidence (+60, "where a bad value was used, not
  necessarily where it was made"). Two other groups of functions become candidates beside it:
  - the functions whose results it uses (+40, producers);
  - its callers on the failing path (+20, which may pass it a bad value).
- A panic never yields `single` by itself. `single` needs one of:
  - a compile error in exactly one body;
  - exactly one candidate, with nothing in the frontier that could hide another.
- A panic site whose callers were never followed (no failing test reached it) counts as possibly
  hiding the target.

**B. Reachability.**
- **Typing values from what the source writes.** Each function records its parameters, `let`s,
  `for` loops and iterator closures. A value's type comes from:
  - its written type;
  - the declared return type of the call it is initialized from, seen through `?`, `.await`,
    `.unwrap()` and similar (`-> Self` maps to the impl's type);
  - or the field chain it iterates or aliases.
- **Implicit calls.** Comparison, equality and arithmetic operators between non-literal operands,
  and `.max()`, `.min()`, `.sort()`, `.binary_search()`, `.contains()` and similar, may call a
  user's `PartialOrd`/`Ord`/`PartialEq`/`Add`… implementation. They are followed when the caller
  names the implementing type, and reported otherwise.
- **Unresolved edges carry their cause.** Each kind is reported separately, with examples:
  - *unknown type*: the receiver has no visible type;
  - *missing return-type propagation*: the call a value comes from could not be followed;
  - *unresolved trait implementation*: a possible trait object of a named trait;
  - *implicit operator dispatch*;
  - *search budget*.
- **Uncompiled test files.** When a test file does not compile, none of its tests ran. The frontier
  now says so, and counts the functions that were never observed.
- **Passing tests.** `test … ok` lines are observed (never shown to a model). A candidate that no
  passing test reaches gets +15. At most 32 passing tests are walked, and a note says when the cap
  applies.
- **Fixed after the first valid run:** a binding defined from its own field chain
  (`let node = node.next`) recursed without bound and overflowed the stack on T1/T3. Binding
  resolution now follows at most three links on every path (tested).

**C. Scorer v4.** See `../scorer-v4/README.md`. 17 regression tests pass. Rescoring the frozen v3
raw outputs with v4 reproduces the labeled v3 id-fix rescoring exactly.

## Results on f01–f12 (development)

P1–P5 are computed as preregistered for v3. **They are not a held-out claim.**

| Criterion | v3 frozen (id-fix rescoring) | v4 (development) |
|---|---|---|
| P1 no silent miss | pass | pass |
| P2 no false uniqueness | **FAIL** (f03) | pass (no `single` decision at all) |
| P3 no false "not expressible" | pass | pass |
| P4 recall, deep + dispatch | **FAIL** 4/6 | pass 6/6 |
| P5 bounded cost | pass | pass |

| Fixture | Category | v3: decision, target ranks (candidates) | v4: decision, target ranks (candidates) | v4 ranking ms (index ms) |
|---|---|---|---|---|
| f01 | deep | ambiguous, miss (11) | ambiguous, 24 (24) | 2 (18) |
| f02 | deep | ambiguous, 19 (19) | ambiguous, 23 (23) | 3 (19) |
| f03 | deep | **single → wrong**, 29 (29) | ambiguous, 7 (29) | 5 (28) |
| f04 | dispatch | ambiguous, miss (8) | ambiguous, 15 (22) | 3 (22) |
| f05 | dispatch | ambiguous, 25 (27) | ambiguous, 6 (27) | 3 (17) |
| f06 | dispatch | ambiguous, 10 (15) | ambiguous, 12 (14) | 1 (13) |
| f07 | multi | ambiguous, 1 + 6 (14) | ambiguous, 4 + 8 (14) | 3 (19) |
| f08 | multi | ambiguous, 1 + miss (7) | ambiguous, 1 + 2 (13) | 3 (21) |
| f09 | multi | ambiguous, 29 + 36 (36) | ambiguous, 28 + 35 (35) | 4 (35) |
| f10 | declaration | not expressible, 7 + 11 (15) | not expressible, 9 + 13 (17) | 1 (17) |
| f11 | declaration | ambiguous, 9 + 3 (11) | ambiguous, 5 + 7 (11) | 1 (17) |
| f12 | declaration | not expressible, 1 + miss (4) | not expressible, 1 + miss (4) | 1 (19) |

| Category | v3 found | v4 found | v3 top 8 | v4 top 8 | v3 mean rank | v4 mean rank |
|---|---|---|---|---|---|---|
| deep | 2/3 | 3/3 | 0/3 | 1/3 | 24 | 18 |
| dispatch | 2/3 | 3/3 | 0/3 | 1/3 | 17.5 | 11 |
| multi | 5/6 | 6/6 | 3/6 | 4/6 | 14.6 | 13 |
| declaration | 5/6 | 5/6 | 3/6 | 3/6 | 6.2 | 7 |
| **all** | **14/18** | **17/18** | **6/18** | **9/18** | 13.4 | 11.8 |

- **False single targets:** v3 1 (f03), v4 0. **False "not expressible":** 0 in both.
  Multi/declaration fixtures classified not expressible: 2/6 in both.
- **Runtime:** ranking 1–5 ms (v3: 0–3 ms), indexing 13–35 ms, no timeouts.
- **Candidate counts:** grew where new edges were found: f01 11→24, f04 8→22, f08 7→13.
  Elsewhere they were about unchanged.
- **Rank regressions (reported, not tuned):**
  - f02: 19→23;
  - f07: `parse_amount` 1→4 and `statement` 6→8;
  - f10: 7/11→9/13;
  - f11: `total_bytes` 3→7.

  New candidates and the producer, caller and passing-test signals moved some targets down.

## Development evidence on T1 and T3 (larger codebases)

| Task | v3 | v4 |
|---|---|---|
| T3 `Policy::intersect` | 7th of 47 | 7th of 44, complete walk, ranking 39 ms |
| T1 `TargetAuthorizer::credentials` / `RemoteService::respond` | 66th / 124th of 400 | 71st / 129th of 400, candidate budget reached (reported), ranking 179 ms |

## Are the unresolved-call explanations meaningful?

- **Better:**
  - f12's remaining miss is now attributed to its cause: the test file does not compile, so 3 of its
    functions were never observed. v3 blamed an unrelated untyped call.
  - f08's `Version::cmp` is now reached through the comparison operators. v3 missed it and flagged
    it for an unrelated reason.
  - f01's and f04's helper-returned receivers now resolve.
- **Still weak:**
  - `could_conceal_target` is true on 11 of 12 fixtures. Most of these flags come from method names
    shared between standard-library receivers (`str::len`, `.is_empty()`, `.trim()`) and indexed
    methods.
  - Each cause is accurate ("the receiver `sku` of `.is_empty` has no visible type"), but the flag
    carries little information. Any decision that needs "nothing hidden" will rarely get it.

## Remaining safety and quality issues

1. **Generalization is untested.** Every improvement here was made after seeing f01–f12. Only a new
   held-out set can show whether it generalizes.
2. **Deep-chain ranking is still poor.**
   - Deep targets rank 23–24 in f01 and f02.
   - Only 9 of 18 targets are in the top 8 a classification is offered.
   - The search is not too narrow; the ranking can't tell which reached function is wrong.
3. **The concealment flag is noisy** (above). Making it informative needs a way to rule out
   standard-library receivers (for example, the declared types of more expressions).
4. **T1 still hits the candidate budget,** and its targets rank 71st and 129th.

## Next step

Decision policy and scorer are ready for a **new, independently authored held-out evaluation**,
preregistered before it runs. The plan:

- **Fixtures:** a new author and new fixtures (f01–f12, T1 and T3 are development evidence only),
  in the same four categories. Add two more:
  - a symptom/producer split, where the panic is in one function and the defect in another;
  - implicit operator dispatch.
- **Frozen before authoring:** the code and `rank_target`, with hashes recorded. Scorer v4 and its
  tests, unchanged.
- **Same safety criteria (P1–P3, P5), plus one:** no `single` decision when the defect and the
  panic are in different functions.
- **P4 and ranking:** keep P4 at 80% recall. Also report, separately and without thresholds unless
  the preregistration sets them: top-8 rate and mean rank per category, and the concealment-flag
  rate.

Repair generators stay off until that evaluation passes.
