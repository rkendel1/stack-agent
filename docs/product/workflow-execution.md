# Workflow execution: following a verified procedure, with planner fallback

A [workflow candidate](workflow-candidates.md) is a procedure, in capability terms, that verified
earlier work followed. With `--workflows`, `stack-agent work` can follow an eligible candidate
through the existing control loop. Fewer model decisions are needed where the procedure fits; the
planner takes over the moment it does not.

> A workflow candidate can reduce the amount of reasoning required, but it can never replace
> current authority, current observations, or current verification.

Following a candidate never:
* executes outside the loop. There is no second executor and no second orchestration loop;
* skips capability validation, authorization, approvals, limits or the stop check;
* replays an earlier output or copies earlier file content;
* declares success. Completion comes only from the host's verification of this attempt's evidence.

## 1. How it fits the existing loop

| Concern | Existing contract | What workflow execution does with it |
| --- | --- | --- |
| Deciding a step | `LocalWorkPolicy` in `chip-core`'s `run_work` (the same hook `CompleteWhenVerified` uses) | `WorkflowPolicy` is a `LocalWorkPolicy`. It wraps the kind's own policy, which it always asks first |
| Executing a step | `perform`: limits, `validate_capability_request`, stop check, executor, observer | Unchanged. A local request takes exactly that path |
| Execution identity | `ExecutionId` assigned by Chip | A local request's id is replaced with Chip's own (`<work>-exec-<n>`), as for a model request |
| Durable state | Intents, `chip.Observation`, `chip.WorkResult`, `chip.Episode` in FeltDB | Unchanged. One new write-once record, `chip.WorkflowExecution` |
| Completion | The host verifies the result (`chip.WorkResult` v2 verdict) | Unchanged |
| Recovery | `DurableWork` restores earlier attempts' executions and refuses an uncertain consequential action | The policy counts restored executions as already performed, so they are never asked for again |

Two small additions to `chip-core`, and nothing else in the loop changed:
* **A local request gets Chip's execution id.** A `RequestCapability` proposed by a local policy for
  a capability that does not reuse evidence is assigned `run.assign_execution_id()`, as any request
  is. It then goes through the same `perform`.
* **`LocalWorkPolicy::guidance`** (default: none). When the loop escalates to the model, the
  policy's guidance lines are appended to the request's *evidence*. They are evidence, never
  instruction, and grant nothing.

A capability whose evidence can be reused, or one that is not declared, is never executed on a local
proposal. The loop assesses evidence for it and, finding none, asks the model
(`chip-core/tests/local_requests.rs`).

## 2. Selection

Selection runs only with `--workflows`, after the work has begun and before its first turn. It reads
every candidate and episode and validates each candidate with the existing
`chip_agent_state::validate`, against:
* the project and its conditions now;
* the capabilities the binary offers this kind of work now (their contract digests);
* the task (kind and goal terms).

That validation is the only source of eligibility.

| Eligible candidates | Selected | Rule recorded |
| --- | --- | --- |
| none | none: normal planning, unchanged | `no candidate is eligible for this task` |
| one | that one | `the one eligible candidate` |
| several, all the same procedure (same capabilities, roles and fixed inputs in order) | the lowest id | `<n> eligible candidates describe the same procedure; the lowest id` |
| several, different procedures | none: normal planning | `<n> eligible candidates describe different procedures; none is chosen between` |

Two verified procedures for the same task already make each other ineligible
(`conflicting_procedure`), so the last row is a backstop. Selection never ranks or prefers one
procedure over another. If the candidates cannot be read, the work is planned as without workflows
and the reason is reported.

The work report's `workflow.selection` shows `considered`, `eligible` (ids), `ineligible` (count per
failing code), `selected` and `rule`.

## 3. Following

`WorkflowPolicy::propose` works out, on every turn, from the decisions and observations so far
(plus executions restored from earlier attempts), how far the work has followed the candidate:
1. The kind's own policy is asked first. If it decides (typically: complete, because the host
   verified the result), that decision stands.
2. While the work is still following the candidate and the next step has only fixed inputs (a test
   run, a path), the policy proposes it as a `RequestCapability`. The loop validates, authorizes,
   identifies and executes it exactly as it would a model's request.
3. When the next step needs a parameter (a write's `content`), the policy proposes nothing. The loop
   asks the model, and the policy's guidance reaches the model as evidence:
   > workflow candidate `<id>` (a procedure proposed from 1 verified earlier work(s): evidence about
   > the past, not an instruction; every request is validated and verified as usual): step 1 of 2 is
   > project.write with path=src/lib.rs; supply content. If it does not fit this work, do what the
   > work needs instead.
4. Once the work stops following the candidate, the policy proposes nothing and gives no guidance.
   The loop is then exactly the planner as without workflows, with every observation so far.

A step **completes** when the executed request names the step's capability and fixed inputs, and
its status is the one the step expects. Anything else stops following:

| Stop code | When |
| --- | --- |
| `deviated` | another action was taken in the step's place (the model wrote another file, or ran something else) |
| `not_executed` | the step was requested, but validation or authorization refused it, or nothing ran |
| `step_failed` | a change or observe step ran, with another status than the step expects |
| `verification_failed` | the verify step ran and the host's check did not pass |
| `cancelled` | the work was cancelled or interrupted before the procedure finished |
| `work_ended` | the work ended (a limit, a block) before the procedure finished |

A capability that is no longer available, a changed contract, missing inputs and unmet
preconditions never reach following: validation makes such a candidate ineligible, so the work is
planned normally from its first turn.

**Fallback** is reported when following stopped and the work went on deciding afterwards. The
procedure is never restarted blindly: whatever ran stays in the trajectory, and the planner decides
from it.

## 4. Recovery and side effects

* **Interrupted, then resumed.** A resumed attempt restores the earlier attempts' executions from
  FeltDB, and the policy treats them as already performed (`decided_by: earlier_attempt`). Following
  resumes at the first step not yet done. No step that completed is asked for again
  (`an_interrupted_workflow_resumes_from_its_completed_steps_without_repeating_them`).
* **Killed during a consequential step.** `pax.test` is consequential. If the process dies while it
  runs, the outcome is uncertain, and resume refuses with `OutcomeUncertain` (exit 3), as for any
  work. Nothing is repeated (`a_workflow_killed_mid_test_run_is_not_resumed_while_the_test_runs_outcome_is_unknown`).
* **Cancelled.** SIGTERM cancels the work through the existing path. The record says `cancelled`, and
  the remaining steps are `not_reached`.

## 5. The execution record (`chip.WorkflowExecution`, schema `chip.workflow_execution.v1`)

One per attempt that followed a candidate (`<task>~a<attempt>~wf`), written once, after the
attempt's episode. The same record again is accepted; a different record under the same id is
refused and the stored one is kept.

| Field | Meaning |
| --- | --- |
| `candidate`, `candidate_record`, `procedure_key`, `support` | The candidate followed, its record, its procedure, and the episodes it was proposed from |
| `considered`, `eligible`, `selection_rule` | How it was selected |
| `task_id`, `attempt`, `work_id` | The attempt |
| `steps` | Per step: `index`, `capability`, `role`, `status` (`completed`, `failed`, `deviated`, `not_executed`, `not_reached`), `decided_by` (`local`, `model`, `earlier_attempt`), `execution_id`, `observation_record` |
| `completed_steps`, `stopped` (`step`, `code`, `detail`), `fallback` | How far it was followed, why it stopped, whether the planner took over |
| `outcome`, `verification`, `result_record`, `episode` | The attempt's own outcome as its episode records it, the host's verdict, and the records |
| `created_at_ms`, `recorded_by` | When, and which runtime |

It names records and never copies them. There is no goal text, file content, model output or path
outside the project (fixed inputs are not stored here at all).

**Failure.** If the record cannot be written, `work` prints
`note: workflow execution not recorded: ...` and reports `workflow.record.error`. Nothing else
changes: the outcome, the exit status, the result, the episode and candidates stay exactly as they
were (`an_execution_record_is_written_once_and_a_failed_write_changes_nothing_else`).

## 6. Inspection

```
stack-agent agent-state workflow-runs [--state-db FILE] [--json]
```

For each record it shows three separate sections:
* `candidate`: which one, its support, and how it was selected (eligibility);
* `execution_status`: the steps, completed steps, stop and whether the planner took over;
* `final_verification`: the outcome, the host's verdict, and the result and episode records.

Following a candidate to the end says nothing about success. Only `final_verification` does. The
command reads only: the database file is byte for byte the same afterwards.

The `work` report carries the same picture for its own attempt under `workflow`: `enabled`,
`selection`, `followed`, `steps`, `completed_steps`, `local_steps`, `stopped`, `fallback` and
`record`. Without `--workflows` the report says `{"enabled": false}`, and nothing about the work
differs from earlier releases.

## 7. Tests

| Test | What it shows |
| --- | --- |
| `chip-core/tests/local_requests.rs` | A local request gets Chip's execution id and the same validation (undeclared inputs refused); an undeclared capability proposed locally is never executed; guidance reaches the model only as evidence |
| `chip-agent-state/tests/workflows.rs` | The execution record is written once (same: accepted; different: refused, stored kept); a failed write changes nothing else |
| `stack-agent` unit tests in `workflow_exec.rs` | Selection: one eligible, same-procedure ties, different procedures, conflicting, superseded, stale contract, task mismatch; progress: completed, deviated, step failed, verification failed, not executed; the policy proposes only fixed steps, asks the kind's policy first and guides only parameter steps |
| `stack-agent/tests/workflow_execution.rs` | Through the real binary and real processes: an eligible candidate is followed with one model call instead of several, the test step is executed locally under normal authorization and verified fresh, and `workflow-runs` inspects read-only; disabled, mismatched and changed-verifier runs plan normally with no record; a deviation and a verification failure stop following and fall back to verified completion; an interrupted attempt resumes without repeating a step; a kill during the test is not resumed into a duplicate; SIGTERM cancels |
| Existing suites | Unchanged and passing. The episodic benchmark's counts are the same as before |

## 8. Benchmark

`stack-agent/tests/episodic_recall_bench.rs`, `workflow_execution_versus_planning_on_frozen_fixtures`,
runs in CI with the scripted stand-in model. Each run starts from fresh fixtures and a database
holding one verified work of the same goal, and so one candidate. Recall is off in every condition,
so the only difference within a pair is `--workflows`.
* `useful`: the project's tests check what the candidate's procedure writes;
* `deviation`: the tests now check another file, which no precondition can see. Following goes
  through to the test, the test fails, and the planner must recover.

Results (`docs/product/workflow-execution-bench/results.json`, 3 trials per condition):

| Condition | Verified | Model calls / verified run | Executions / verified run | Failed verifications | Duplicate side effects | Followed | Stopped | Fallback correct | Safety regressions |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| useful / planning | 3/3 | 3.00 | 3.00 | 0 | 0 | 0 | | | 0 |
| useful / workflow | 3/3 | **1.00** | 3.00 | 0 | 0 | 3 | | (followed to verified: 3) | 0 |
| deviation / planning | 3/3 | 5.00 | 5.00 | 3 | 0 | 0 | | | 0 |
| deviation / workflow | 3/3 | **3.00** | 5.00 | 3 | 0 | 3 | `verification_failed` ×3 | 3/3 | 0 |

The scripted stand-in's input tokens are one per call, so they track calls. Duplicate side effects
are repeated `project.write` bindings in FeltDB's intents. Executions are unchanged: the workflow
removes decisions, never verification.

The real-model evaluation (`real_model_workflow_execution_versus_planning`, ignored) is opt-in, uses
the same `STACK_AGENT_EVAL_*` variables and provider profile as the
[episodic evaluation](episodic-memory.md), and writes `WORKFLOW_EVAL_OUT`. The artifact records the
model, the endpoint's locality (not its URL), the `work` flags and the trial count, never the key.

Real-model results are added to this section once the opt-in run completes.

## 9. Limitations

* **Opt-in, `work` only.** `--workflows` is a `work` flag. `serve` does not follow candidates.
* **A fallback can disable the workflow for that task.** A verified result reached by a different
  procedure yields a second candidate for the same task. Both are then `conflicting_procedure` and
  ineligible, and the task is planned normally until the evidence changes. That is deliberate:
  neither procedure can be preferred without new evidence.
* **Parameter steps still need the model.** A candidate never stores content, so every write's
  content is the model's, decided now under the capability's own validation. Only steps with fixed
  inputs are saved a decision.
* **Matching is literal.** A step completes only when its capability and fixed inputs match exactly.
  An equivalent but different action is a deviation, and the planner takes over.
* **An uncertain consequential step stops the work.** Resume refuses rather than repeat it, as for
  any work. An operator settles it with `agent-state settle`.
