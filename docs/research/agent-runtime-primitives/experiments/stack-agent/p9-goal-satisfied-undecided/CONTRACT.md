# Goal satisfied, completion undecided: the contract, written before the change

Status: Phase 1 and 2 of P9. Superseded by `README.md` where they differ.
Base: `main` at `902cf52` (P1 to P4, P7, P8 merged).

## What the code represents today

| Fact | Where it lives | Durable? |
| --- | --- | --- |
| each observation (output, status, the request) | `chip.Observation` record, one per execution | **yes** (FeltDB) |
| the terminal outcome (`completed`, `blocked`, ...) and its reason | `chip.WorkResult.outcome`, `.detail`; the checkpoint summary | **yes**, as text |
| task status | `completed`, `blocked` or `cancelled` checkpoint | **yes** |
| whether an observation produced a required output | `WorkEvent::GoalEvaluated { satisfied, remaining }` | **no** (in the report only) |
| the same evaluation of an observation restored on resume | `WorkEvent::ObservationRestored { remaining }` (no `GoalEvaluated` is emitted for it) | **no** |
| which requirements are met now | `Run::satisfied`, rebuilt on resume by re-evaluating the restored observations with the same predicates (`restore` calls `assess_goal`) | derived, **not stored** |
| `goal_satisfied` in `work --json` | `goal_level(events)`: the last `GoalEvaluated.remaining == 0` | **no** |

So the goal's status is *derivable* from the durable observations, because a requirement is a
predicate over recorded observations and a resume re-evaluates it. It is *stored* nowhere, and
`WorkResult` is a closed record (`deny_unknown_fields`, schema v1).

## The outcomes a goal-satisfied work can end in

| Situation | Outcome | Distinction carried |
| --- | --- | --- |
| Chip's local policy completes (`change`, `verify`) | `Completed` | none needed |
| the model completes with an accepted answer | `Completed` | none needed |
| the model completes, the **answer** is not supported by the observations | `Blocked`, "completion refused: the proposed answer is not supported by the observations" | text; goal's observations are met |
| the model repeats, guard fires (P7, P8) | `Blocked`, "stuck: ... The goal's required observations were already satisfied, but ..." | text (durable in `WorkResult.detail`) |
| the model blocks or escalates itself | `Blocked` / `Escalated` with the model's reason | none about the goal |
| a limit is reached | `LimitReached` | none about the goal |
| stop requested | `Cancelled` | none about the goal |
| goal not met, model claims completion | `Blocked`, "completion refused: no authoritative observation satisfies the goal" | text |

## The gap found

Not "there is no new outcome". Measured on the real binary, before this change:

* An inspection whose goal is met and that the guard stopped reports `goal_satisfied: true` when it
  runs uninterrupted.
* The same task resumed by a fresh process (everything restored, the guard fires at once) reports
  **`goal_satisfied: null`**, in the same report whose `outcome_reason` says the goal's observations
  were already satisfied.

`goal_level` reads `GoalEvaluated` only, and a restored observation is evaluated without one. So the
distinction "goal satisfied" against "goal not evaluated" is lost across a restart even though the
state it describes is identical. That violates invariant 7 below. It affects every resumed work
whose goal was met by restored observations, not only stopped ones.

## Invariants

1. A satisfied goal predicate is not, by itself, proof of completed work.
2. Only the existing authorized completion path (a `Complete` decision accepted by the goal and
   answer rules) produces `Completed`.
3. Stuck detection does not override a valid completion decision (P8: it yields to one Chip's local
   policy would make now).
4. Stuck detection does not manufacture a completion decision.
5. A satisfied-but-undecided goal is distinguishable from an unsatisfied one.
6. Cancellation, deadline and the other stop conditions keep their precedence.
7. A resume reports the same semantics as uninterrupted execution.
8. Existing persisted records stay readable; no migration without a demonstrated need.

## Does `Blocked` plus the goal's evaluation already satisfy them?

* 1, 2, 3, 4, 6, 8: **yes**, and tests pin them (P8 and this PR).
* 5: **yes in memory and in the report for an uninterrupted run** (`GoalEvaluated`,
  `goal_satisfied`), **and durably as text** for the stuck case. Not durable as a field.
* 7: **no**, for the reason above.

## The minimum change

* **No new terminal state.** Every caller already treats a work that did not complete as not
  complete, and `Blocked` with the goal's status alongside answers "satisfied but undecided". A new
  state would change every consumer's match, the FeltDB `requested_status` mapping and the exit
  codes, to carry what an existing field can carry.
* **No new persisted field and no schema change.** The status is derivable from the durable
  observations, and the stuck reason already states it in `WorkResult.detail`. A new field in a
  closed schema would make records unreadable to older builds. It is not justified while the
  derivation is exact.
* **No new event.** `ObservationRestored.remaining` already carries the evaluation of a restored
  observation. Adding `GoalEvaluated` on resume would duplicate evaluations in the stream.
* **One change:** derive the goal's status from both events, in one place (`WorkSpec::goal_level`),
  and use it where `work --json` reports `goal_satisfied`. An `ObservationRestored` counts only when
  the spec has a requirement, because without one its `remaining` is vacuously 0 and would claim a
  goal that was never required.

## Unresolved by design

* For outcomes other than the stuck stop (`LimitReached`, `Cancelled`, a model's own `Blocked`),
  `WorkResult.detail` says nothing about the goal. The durable observations let anyone with the
  predicate re-derive it, and `work --json` reports it, but a reader of the FeltDB records alone
  sees only the text.
