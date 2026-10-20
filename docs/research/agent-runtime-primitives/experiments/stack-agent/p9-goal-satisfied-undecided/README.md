# Goal satisfied, completion undecided

The contract as it was and the gap found: [`CONTRACT.md`](CONTRACT.md), written before the change.
Product doc: [`docs/product/stuck-guard.md`](../../../../../product/stuck-guard.md).

## Outcome

* **No new terminal state, no new persisted field, no schema change, no migration, no new event, no
  CLI change.** The contract needs none of them: `Blocked` (or any other non-completed outcome)
  together with the goal's evaluation already says "satisfied but undecided".
* **One production change, and it is a bug fix:** after a restart, `work --json` reported
  `goal_satisfied: null` for a work whose goal was met by restored observations. `goal_level` read
  only `GoalEvaluated`, and a resume evaluates restored observations without emitting one (it carries
  the result in `ObservationRestored.remaining`). The fix is `WorkSpec::goal_level`, which reads both,
  and `work --json`, `verify` and the PAX demo use it.
  * `ObservationRestored` counts only for a spec that has a requirement, because without one its
    count is vacuously 0 and would claim a goal nobody required (tested).
  * Effect: `goal_satisfied` for a resumed work goes from `null` to `true` or `false`. Nothing else
    in a report or a record changes. A work that was never resumed is unchanged.
* The goal's status is **derived, not stored**: the durable observations and the same predicates
  re-derive it on every attempt. The one durable statement of it is text in `WorkResult.detail`,
  for the stuck stop.

## The gap, measured

Before the change, on the real binary (`the_goal_status_of_a_stopped_inspection_is_the_same_after_a_restart`):
an inspection whose goal was met and that the guard stopped reported `goal_satisfied: true`
uninterrupted, and **`null`** when a fresh process resumed it, in a report whose `outcome_reason`
said the goal's observations were already satisfied. The test was written first and failed on that.

## Invariants and where each is pinned

| # | Invariant | Pinned by |
| --- | --- | --- |
| 1 | a satisfied goal is not completed work | `goal_satisfied_and_no_completion_decision_while_repeating_…`, `goal_satisfied_is_distinguishable_from_unsatisfied_in_every_other_ending` |
| 2 | only the authorized path completes | `goal_satisfied_and_the_model_completes_with_an_accepted_answer_completes`, `…_the_models_answer_is_refused_is_blocked_…`, `a_model_claim_without_qualifying_evidence_is_never_completed` |
| 3 | the guard does not override a valid completion | `goal_satisfied_and_the_local_policy_authorizes_completion_completes` (guard on and off); durable `goal_satisfied_with_an_authorized_completion_is_completed_on_resume_…` |
| 4 | the guard does not manufacture a completion | `…no_completion_decision_while_repeating…`: no completion event, no model call after the third pair |
| 5 | satisfied-but-undecided is distinguishable from unsatisfied | `goal_level` in every ending (block, limit, stop, stuck), both ways, before and after a restart |
| 6 | stops keep their precedence | `a_stop_beats_a_satisfied_goal_and_the_guard_and_keeps_the_goals_status` (both causes); durable `goal_satisfied_and_a_stop_on_resume_is_cancelled_with_the_goal_met` |
| 7 | a resume says what an uninterrupted run says | durable `goal_satisfied_and_undecided_is_blocked_…_and_a_restart_says_the_same`, `goal_unsatisfied_and_repeating_…`; CLI `the_goal_status_of_a_stopped_inspection_is_the_same_after_a_restart` |
| 8 | existing records stay readable | no record, schema or event changed; the existing durable suites pass |

The goal events are neither duplicated nor dropped: one `GoalEvaluated` per observation the loop made
(3 for 3), none added for what a resume restored (0 for 3 restored, which are in `ObservationRestored`),
and the stop discards none (`each_observation_is_evaluated_once_and_the_stop_discards_none`,
`a_resume_evaluates_what_it_restores_without_adding_goal_events`,
`a_productive_trajectory_that_repeats_a_capability_resumes_and_completes`).

## New tests

| File | Tests | Result |
| --- | --- | --- |
| `chip-core/tests/goal_completion_contract.rs` | 12: the decision matrix (unsatisfied+repeat; satisfied+policy; satisfied+model accepted; satisfied+answer refused; satisfied+undecided+repeat; block, limit and stop with the goal's status; stop precedence over restored satisfied; claim without evidence, before and after an observation; productive repeat; event counts; restored events; a spec with no requirement) | pass |
| `chip-agent-state/tests/goal_completion_contract.rs` | 6, real FeltDB: undecided+stuck and restart (outcome, task status, `WorkResult.detail`, goal status, no model call, no execution); unsatisfied+stuck and restart; satisfied+authorized completes on resume and is recorded `completed`; satisfied+stop on resume is `cancelled`; productive repeat resumes and completes; a claim without evidence is never recorded `completed` | pass |
| `stack-agent/tests/stuck_guard.rs` | +1, real binary: the goal status of a stopped inspection is the same after a restart | pass (failed before the fix) |

Restart and recovery were exercised: 7 of the 19 tests stop a work, expire its lease and resume it in
a fresh `begin` (6 through FeltDB in-process, 1 through two real `stack-agent` processes).

## Validation

Run on macOS arm64 (`pax` 0.4.1 on `PATH`).

* `cargo fmt --all -- --check`: clean.
* Workspace and Linux: see the PR; the results are in its description.

## Unresolved

* `WorkResult.detail` carries the goal's status only for the stuck stop. For `LimitReached`,
  `Cancelled` and a model's own `Blocked` a reader of the FeltDB records alone sees only the outcome
  text. The status is re-derivable from the durable observations by anyone with the predicate, and
  `work --json` reports it, but it is not stored. Storing it needs a field in a closed record schema
  (`chip.work_result.v1`, `deny_unknown_fields`), which older builds would reject. That is a real
  cost for a status that is derivable, so I did not make it. If a consumer needs it without the
  predicate, that is the case for a schema change.
* Whether a model-completable, goal-met, repeating work should end `Blocked` was a P8 review gate and
  is unchanged.
* No real-model false-positive rate is claimed: the benchmark records lack the inputs and
  observations needed for exact replay (P8).
