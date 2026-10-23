# Targeting v3 held-out evaluation: pre-registration

Written 2026-10-22, after the targeting implementation was frozen and before any held-out fixture
existed. The frozen sources and the evaluation binary are listed with their SHA-256 in
`FROZEN.sha256`. No targeting rule, score, budget or threshold changes between this file and the
results; if one must, the evaluation is void and is reported as such.

## What is evaluated

Chip's deterministic target selection (`chip-symbol-loop/src/target.rs`, with the index in
`symbols.rs`): which functions enter the candidate set, the decision (`single`, `ambiguous`,
`not_expressible`, `none`), the repair-shape assessment, and the search frontier. No repair is
attempted, no model is called, nothing is edited.

## Development evidence (not held out)

T1 and T3 of coding-benchmark-v1, and the fixtures in `tests/targeting.rs` and
`tests/targeting_frontier.rs`, were seen while the implementation was written. They are regression
evidence only.

## Fixtures

Synthetic Cargo projects written by an independent author (a separate agent) who is given defect
categories and fixture-format rules, but not the targeting source, its rules, scores, budgets or
tests, and not this file's criteria. Categories:

- **deep**: the defect is at least three calls below the failing test, through ordinary
  request-handling layers; one function body is the fix.
- **dispatch**: the defect is in one implementation of a trait reached through a trait object or
  generic; one function body is the fix.
- **multi**: the fix needs body changes in two or more functions.
- **declaration**: the fix needs a change to a type definition or a function signature.

Each fixture carries its ground truth (the functions the reference fix changes and its kind), and
the author verifies that the acceptance tests fail before the reference fix and pass after it.
Fixtures whose ground truth does not verify (tests do not fail, or the reference fix does not make
them pass) are excluded and listed as excluded, before any ranking is read.

## Procedure

For each fixture: run `rank_target.frozen <fixture> cargo test --offline` in a scratch copy, with a
600-second acceptance timeout, and record the full output. Then compare with the ground truth.

## Measurements (all reported per fixture and in total)

1. Whether each true target enters the candidate set, and its rank.
2. The decision, and whether a `single` decision names a true target.
3. The repair shape (`possible`, `unknown`, `not_expressible`) against the ground-truth fix kind.
4. The frontier: stop reason, functions explored, edges, depth reached, cut-off count, excluded
   calls by kind, `could_conceal_target` and its reasons.
5. Candidate count, index and ranking time, acceptance timeouts.

## Pass criteria (all must hold)

- **P1, no silent miss.** For every true target that is not a candidate, the frontier reports
  `could_conceal_target = true`. A missed target with `could_conceal_target = false` fails.
- **P2, no false uniqueness.** Every `single` decision names a true target, and no `single`
  decision is made for a `multi` or `declaration` fixture.
- **P3, no false impossibility.** No `not_expressible` decision is made for a `deep` or `dispatch`
  fixture.
- **P4, recall.** At least 80% of the true targets of `deep` and `dispatch` fixtures enter the
  candidate set.
- **P5, bounded cost.** No ranking takes more than 10 seconds of ranking time (`rank_ms`), and every
  budget exhaustion is reported in the frontier.

Reported but not pass/fail: the rate at which `multi` and `declaration` fixtures are classified
`not_expressible` (some are undetectable from diagnostics when the tests compile), top-8 rate, and
mean rank.

If any criterion fails, the targeting change is not validated, the branch stays uncommitted, and
the result is reported as it is.
