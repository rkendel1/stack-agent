# Stuck guard, measured

What it is and how to use it: [`docs/product/stuck-guard.md`](../../../../../product/stuck-guard.md).
The proposal it was built from, written before the code: [`DESIGN.md`](DESIGN.md).

**Scope of the claims.** Everything below uses a scripted model and a fake `pax`. It measures what the
runtime does when a model repeats itself. It says nothing about how often a real model does, or
whether a real model that is stopped would have gone on to succeed. Nothing here is local-model
evidence.

## Setup

* Host: macOS arm64, `pax` 0.4.1 on `PATH`.
* Baseline binary: release build of `origin/main` at `db6f0f7` (P1 to P4). Candidate: release build of
  this branch at the commit that contains this report.
* `stuck_experiment.py` runs identical scenarios against both: a fresh project, the E1/E2 fake `pax`,
  and `scenario_model.py`, which answers request n with script[n] (the last reply repeats forever). It
  reads nothing from the request. Counts come from the binary's `--json` report and the model's log.
  Raw results: `results.json`.
* **One run per cell.** The scenarios are deterministic by construction (no timing decides an
  outcome), so one run is the measurement, but it is a single run.

## Reliability over horizon

"Repeated requests" counts requests the scripted model made that were identical to an earlier one.
Limits are `--max-turns 20 --max-executions 20` unless the row says otherwise.

| Scenario | Intended | Binary | Terminal | Model calls | Executions | Repeated requests | Turns | Progress kept |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| identical loop (same read forever) | stops | main | `limit_reached` | 20 | 20 | 19 | 20 | n/a |
| | | guard | **`blocked`, `stuck:`** | **3** | **3** | 2 | 3 | n/a |
| alternating loop (read, list, ...) | stops | main | `limit_reached` | 20 | 20 | 18 | 20 | n/a |
| | | guard | **`blocked`, `stuck:`** | **4** | **4** | 2 | 4 | n/a |
| productive repeats (write, read, write, read, write, read, test) | completes | main | `completed` | 7 | 7 | 2 | 8 | file changed, 1 `pax` run |
| | | guard | `completed` | 7 | 7 | 2 | 8 | file changed, 1 `pax` run |
| ordinary task (write, test) | completes | main | `completed` | 2 | 2 | 0 | 3 | yes |
| | | guard | `completed` | 2 | 2 | 0 | 3 | yes |
| turn limit, no loop (6 different requests, `--max-turns 4`) | turn limit | main | `limit_reached` | 4 | 4 | 0 | 4 | n/a |
| | | guard | `limit_reached` | 4 | 4 | 0 | 4 | n/a |
| loop after progress (write, read, read forever) | stops, file kept | main | `limit_reached` | 20 | 20 | 18 | 20 | file written |
| | | guard | **`blocked`, `stuck:`** | **4** | **4** | 2 | 4 | **file written, 4 observations kept** |
| identical loop at the product defaults (12 turns, 8 executions) | stops | main | `limit_reached` | 9 | 8 | 8 | 9 | n/a |
| | | guard | **`blocked`, `stuck:`** | **3** | **3** | 2 | 3 | n/a |

* **The guard stopped no productive scenario.** The productive repeats, the ordinary task and the
  no-loop turn limit are identical before and after in every column.
* **Genuine loops end at the threshold**, with no further model call or execution, and say which
  pattern. The saving is the loop's length minus the threshold: 17 of 20 executions and model calls
  at limits of 20, 5 of 8 executions at the product defaults.
* **At the product defaults the loop was already bounded** (8 executions). The guard's gain there is
  smaller executions and a reason that says what happened, not a different safety property.
* **Progress is preserved.** In the loop-after-progress scenario the written file is still there and
  the report holds the four observations recorded before the stop.

### Restart consistency

The identical loop with `--state-db`, `--lease-secs 1`; a fresh process resumes after the lease
expires.

| Binary | First run | Second run (fresh process) |
| --- | --- | --- |
| main | `limit_reached`, 20 calls, 20 executions | `limit_reached`, **20 more calls, 20 more executions** (limits are per attempt) |
| guard | `blocked`, `stuck:`, 3 calls, 3 executions | `blocked`, **same reason, 0 calls, 0 executions** |

Without the guard a resume gives the loop a fresh budget. With it, the restored trajectory already
repeats, so the resume ends the same way at once. The same terminal state and the same reason hold
after the restart.

## Overhead

* **Guard evaluation:** about 30 ns, flat from 4 to 100,000 pairs (`stuck_guard_overhead`, release
  build, 200,000 evaluations per length, the worst-case tail that is one pair short of every
  pattern):

  | Trajectory (pairs) | ns per evaluation |
  | --- | --- |
  | 4 | 30.7 |
  | 8 | 30.6 |
  | 100 | 28.8 |
  | 1,000 | 29.3 |
  | 10,000 | 30.6 |
  | 100,000 | 28.6 |

* **Pairs examined per evaluation:** at most 4 at the defaults (the larger threshold), asserted by a
  test that always runs. The guard compares the last pairs only and copies nothing that grows.
* **Ordinary task, whole process, 40 alternating runs each:** median 51.5 ms on `main`, 52.3 ms with
  the guard; p10 to p90 50.1 to 55.2 ms against 49.9 to 55.4 ms. The difference is inside the noise.
* **Durability bench (20 repeats per cell):** every cell identical to `main` except wall time
  (`bench/base`, `bench/cand`). The bench's scripted models do not loop.

## Tests

On this host, on the final code:

* `cargo fmt --all -- --check`: clean.
* `cargo test --workspace --locked --no-fail-fast`: **1873 passed, 5 failed. The workspace does not
  pass on this host.**
* **The 5 failures are the ones that fail on `main` here**, listed in the P4 report
  (`p4-capability-effects/four-failures-p3.txt` and the intermittent
  `a_fixture_is_executable_when_the_helper_returns_even_while_a_writer_holds_it`): they are
  `pax_boundary::a_file_held_open_for_writing_cannot_be_executed`,
  `pax_boundary::a_fixture_is_executable_…` (intermittent), `tls_boundaries::a_certificate_for_another_host_…`,
  `service::tests::malformed_requests_are_refused_and_start_nothing` and
  `verify::the_standalone_path_needs_no_compute_…`. All five were run one at a time on `main` at `db6f0f7` for this PR and each exits 101 there too. I did
  not compare their failure messages for this PR (the P4 report did, for four of them), and I did not
  investigate their cause.
* **New failures introduced by the guard, found and fixed in this PR:** three existing tests whose
  scripted models asked for one identical request three times (`each_trajectory_stays_sequential_while_works_overlap`,
  `an_inspection_that_hits_a_limit_did_no_useful_work`, `a_single_read_is_real_and_the_goal_is_still_unmet`)
  now stopped as `stuck`. That is the intended behavior. I changed their scripts to ask for different
  requests (and the last one to 2 turns) rather than loosen the guard. Anyone with a script that
  repeats one request three times will see the same.

### New tests

| # | Required | Where |
| --- | --- | --- |
| 1 | pair repeated below the threshold | `chip-core/tests/stuck_guard.rs` `an_identical_pair_below_the_threshold_goes_on` |
| 2 | beyond the threshold | `the_same_pair_beyond_the_threshold_ends_the_work_without_another_call_or_execution`; CLI `an_identical_loop_ends_as_blocked_…` |
| 3 | alternating | `an_alternating_loop_ends_the_work`; CLI `an_alternating_loop_ends_as_blocked` |
| 4 | context-window failures | **not implemented** (see below). `one_context_limit_is_terminal_and_is_not_a_loop` and `a_failed_model_call_is_not_a_stuck_work` pin the absence |
| 5 | same capability, different inputs | `the_same_capability_with_different_inputs_goes_on_even_when_the_answer_is_the_same`, `a_pair_is_the_request_with_its_inputs_…` |
| 6 | same action, different observations | `an_identical_request_with_materially_different_observations_goes_on`, `status_and_kind_are_part_of_the_observation` |
| 7 | productive then stuck | `a_productive_trajectory_followed_by_a_loop_keeps_its_progress` |
| 8 | max_turns without the guard firing | `reaching_the_turn_limit_without_a_loop_is_not_stuck`; CLI `the_turn_limit_without_a_loop_is_still_the_turn_limit` |
| 9 | restored trajectory already repeating | `a_restored_trajectory_that_already_repeats_ends_before_anything_runs` |
| 10 | resumed after a restart | `chip-agent-state/tests/stuck_guard.rs` (real FeltDB), CLI `a_stuck_work_resumed_by_a_fresh_process_…` |
| 11 | cancellation and deadline | `a_stop_is_reported_as_a_stop_even_when_the_trajectory_is_stuck` (both causes), `a_stop_requested_during_a_loop_…` |
| 12 | no further call or execution after the trigger | asserted in 2, 3, 9 and 10 (model calls, executions, `DecisionStarted` count, last event) |
| | P4 interaction | `a_crash_inside_the_third_identical_read_is_re_observed_and_the_guard_then_ends_the_work` |

The tests that a loop terminates are matched by tests that near-misses continue (below the
threshold, different inputs, different observations, alternation with progress, a stop in the
middle).

## Findings and limits

* **Pattern C (repeated context-window failures) is not implemented.** FX's `ErrorKind` has no
  context-window variant, so a provider's context-length error is text and recognising it would be
  vague text matching. The one structured signal, Chip's own `ContextLimit`, ends the work at its
  first occurrence and is never retried, so it cannot repeat within a run. The tests pin that
  absence. A structured `ContextWindow` error kind in FX would be a separate change with its own
  justification.
* **Thresholds (3 and 4) are untuned hypotheses.** They are not derived from real trajectories. The
  false-positive rate on real model work is **unmeasured**. The guard fires on the *content* of
  pairs, so legitimate polling that returns the same answer three times will be stopped. A run
  that wants that has no flag to disable the guard.
* **Ordering.** The guard is evaluated before the turn limit, so a loop that reaches the limit on
  the same iteration is reported `stuck`, not `limit_reached`.
* **A stuck task stays stuck on resume.** The restored trajectory repeats, so every resume ends the
  same way. Getting out of it needs a different trajectory, which resuming does not provide.
* **Terminal state.** There is no distinct terminal state; `Blocked` with a `stuck: ` reason prefix
  is the contract. A caller that counts stuck stops matches the prefix. No event was added.
* **Goal satisfaction is not consulted.** If every requirement was already satisfied and the model
  then repeats, the work still ends `blocked` (its `GoalEvaluated` events are kept). With the
  product's local policies completion is proposed before such a repeat, and no measured scenario
  hit this; it is untested.
* **Not run:** a Linux run, torn-write or crash windows with real `SIGKILL` for this change (the
  crash-recovery coverage is the unit-level FeltDB test and the fresh-process CLI test), real-model
  trajectories.
