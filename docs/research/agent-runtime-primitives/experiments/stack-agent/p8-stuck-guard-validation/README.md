# Stuck guard: completion precedence and false-positive validation

Follows P7 ([`../p7-stuck-guard/README.md`](../p7-stuck-guard/README.md)). Product doc:
[`docs/product/stuck-guard.md`](../../../../../product/stuck-guard.md). No thresholds, retries or
terminal-state schemas were changed or added.

## 1. Baseline, confirmed from the code (not from the P7 report)

* **Thresholds:** `StuckThresholds::default()` is `repeated: 3`, `alternating: 4`
  (`crates/chip-core/src/stuck.rs`). The window an evaluation reads is `max(3, 4) = 4` pairs.
  `stack-agent work` enables it (`software_work.rs`); `WorkSpec::stuck_guard` is `None` otherwise.
* **Order in one iteration (`Agent::drive`):** stop request, then **the guard**, then the turn limit,
  then the local policy's proposal, then (if none) one model call, then the decision is applied.
  Execution has its own last stop check and the execution limit.
* **Completion semantics:** a work completes only through a `Complete` decision, accepted when no
  required observation is unmet (`completion_refused`) and the answer rule accepts it. The decision
  comes from the local policy (Chip) or the model. `change` and `verify` have a local policy that
  completes the moment the goal's predicate holds. `inspect` has none, and its predicate
  (`InspectionObserved`) holds after the first successful read-only observation.

## 2. Completion precedence: it was a bug, and it is fixed

**Finding.** As merged in P7 the guard ran before the local policy, so this case was wrong:

1. the goal's required observations hold, and Chip's own policy would complete the work now;
2. the end of the trajectory is also a repetition the guard stops at;
3. the guard returned `Blocked`, and the policy never got its turn.

Reproduced with three new tests written first and run against the P7 code. Two failed for this
reason, and the third was the reason's missing explanation:

| Test (against the P7 code) | Result |
| --- | --- |
| `a_goal_that_is_itself_a_repetition_completes_on_the_pair_the_guard_would_stop_at` (goal: "three identical reads in a row"; the third read satisfies it) | **`Blocked` (stuck)**, expected `Completed` |
| `a_restored_trajectory_that_already_satisfies_the_goal_completes_with_or_without_the_guard` | **`Blocked` (stuck)** on resume, expected `Completed` |
| `a_satisfied_goal_the_runtime_cannot_complete_itself_is_still_stopped_and_says_so` | reason did not say the goal was met |

**Fix (one condition in `drive`, plus the reason text).** When the guard would fire, ask the local
policy: if it would complete the work this iteration and `completion_refused` and `answer_refused`
accept it (the same checks, in the same order, as the proposal further down), do not fire, and the
normal flow completes the work. A policy is pure, so asking twice is harmless. The guard never
completes a work, and a model's claim is never evidence: the completion is the existing one.

**What it does not do, and why.** It does *not* skip the guard whenever the goal is met. For
`inspect` the goal's predicate holds after the first read, so that rule would turn the guard off for
every inspection. When only the model can complete the work and it keeps repeating, the guard still
ends it `Blocked`, but now says "The goal's required observations were already satisfied, but the
work is completed only by a completion decision and none was made, so it was not completed", and the
`GoalEvaluated` events (satisfied, remaining 0) stay in the report.

**Contract gap (documented, not filled).** There is no outcome for "satisfied but undecided". The
nearest existing outcomes are `Blocked` (used) and `LimitReached` (what the same work reached
before P7). A new terminal state was not added: no case showed it was needed. **Review gate:** if
you would rather not stop a model-driven work whose goal is met, the alternative is to yield whenever
the goal is met, at the price above.

| Case | Guard off | Guard on, before P8 | Guard on, now |
| --- | --- | --- | --- |
| Goal met before the threshold, Chip completes | completed | completed | completed |
| Goal met on the pair the guard stops at, Chip completes | completed | **blocked** | completed |
| Restored (FeltDB) trajectory already repeating and met, Chip completes | completed | **blocked** | completed |
| Goal not met, trajectory repeats | runs to the limit | blocked: stuck | blocked: stuck |
| Same request, new required evidence each time | completed | completed | completed |
| Goal met, only the model can complete, it repeats | limit reached | blocked: stuck | blocked: stuck, reason says the goal was met |
| Stop requested | cancelled | cancelled | cancelled (stop first) |

## 3. False positives

### 3a. Recorded real-model trials: what they can and cannot say

`docs/product/agent-bench/` holds 243 recorded trials of the real `stack-agent work` (a local
Gemma, `gpt-5.5`, `claude-sonnet-5-5`; five tasks; five runs, two of them superseded harnesses).
**No trial record holds a request's inputs or an observation's text**, only capability names,
execution counts, and (for the workspace task) a coverage summary. The guard compares inputs and
observations, so it **cannot be replayed exactly on them**, and I did not invent the missing data.

What can be shown is a necessary condition (`replay_agent_bench.py`, result in
`agent_bench_replay.json`):

| Class | n | Basis |
| --- | --- | --- |
| Guard cannot have fired | **158** | fewer than 3 observations, or no 3 consecutive same-name requests and no X Y X Y name cycle |
| Repetitive, still making progress; guard cannot have fired | **33** | workspace task: 29 to 30 `project.read` calls, at least that many distinct files read by the recorded coverage (28 manifests + root + overview), so at most one duplicate request; the guard needs two |
| Insufficient evidence | **52** | 38 trials from the first harness record no request sequence at all; 14 `multi_step_discovery` trials have 3 same-name requests in a row (or a cycle) whose inputs are not recorded |
| Genuinely stuck (confirmed) | **0** | no record shows an identical request and observation repeated |

* **Triggers I can confirm: 0. False positives I can confirm: 0. Genuine loops in the data: 0.**
  Of the 191 classifiable trials the guard provably does not fire; of the 52 it may or may not.
* Comparing names alone would have flagged 33 of those trials. They are productive (28 different
  manifests read in turn), which is why the guard compares complete inputs, and why name-level
  evidence here says nothing about real loops.
* The 14 `multi_step_discovery` trials: 10 of them completed the task with the required evidence
  (OpenAI, Anthropic), and 4 did not. The three failing local-model trials have a request shape
  (`list, read, read, search, list, read, list, read, read`) that could contain a repeated `list`.
  I cannot tell.
* **The real-model false-positive rate is unmeasured.** The same holds for the guard's benefit on
  real models: the recorded data contains no confirmed loop for it to have stopped.
* Limits: the recorded runs predate the guard and the decision contract fixes; three runs are
  labelled superseded or defective by their own reports; the coverage lower bound assumes the
  manifests, root and overview are distinct files; one run per cell.

### 3b. Synthetic boundary study

Because the real data cannot answer it, a bounded synthetic corpus probes the thresholds
(`crates/chip-core/tests/fixtures/stuck_replay/synthetic.json`, replayed by `stuck_guard_replay.rs`
after every pair, as the loop asks). **25 trajectories, written by me against the thresholds I was
told to keep, then labelled by me.** It checks that the guard does what is specified and shows where
the specification stops. It is not an estimate of any real rate.

| Class | n | Guard fired | Appropriate | Inappropriate |
| --- | --- | --- | --- | --- |
| productive | 8 | 0 | 8 | 0 |
| repetitive but progressing | 7 | 2 | 5 | **2** |
| genuinely stuck | 10 | 10 | 10 | 0 |

The two inappropriate results are the **known false-positive classes**, recorded in the corpus as
limitations and pinned by tests:

* *Polling that is unchanged and then changes* (`queued, queued, queued, finished`): stopped on the
  third `queued`. Two unchanged answers are fine.
* *Monitoring two sources that are unchanged for two cycles, then one changes* (`a ok, b ok, a ok,
  b ok, a alarm`): stopped at the fourth pair.

Both follow from what the guard is: it compares what was observed, and cannot know what the next
poll would show. The product's capabilities read local project state, which changes only through
the agent's own writes (which break a repeat) or an outside actor; polling an external system is not
something stack-agent offers today. That is an argument about the product, not a measurement.

Not claimed: that 3 and 4 are right. The corpus was not used to choose them and cannot.

## 4. Threshold boundary tests

`chip-core/tests/stuck_guard.rs` (31 tests), guard on and off where the behaviour differs:

| Case | Test |
| --- | --- |
| two identical pairs: no trigger | `the_boundaries_with_the_guard_on_and_off`, `the_predicate_fires_exactly_at_its_thresholds` |
| three identical: trigger | same |
| three alternating (A B A): no trigger | same |
| four alternating: trigger | same |
| same capability, different typed inputs (text `1` against integer `1`) | `the_boundaries_…` |
| same request, materially different observation | `the_boundaries_…`, `status_and_kind_are_part_of_the_observation` |
| polling, unchanged | `polling_that_changes_goes_on_and_polling_that_does_not_is_stopped` |
| polling, new evidence each time | same, and `a_repeated_request_that_yields_new_required_evidence_is_not_a_loop` |
| the known false positive | `unchanged_polling_that_would_change_on_the_next_poll_is_a_known_false_positive` |
| useful write, then repeated reads | `a_useful_write_followed_by_repeated_reads_is_stopped_after_the_third_identical_read` |
| productive trajectory resumes from FeltDB | `chip-agent-state/tests/stuck_guard.rs` `a_restored_trajectory_below_the_threshold_resumes_and_finishes` |
| stuck trajectory resumes from FeltDB | `a_stuck_work_is_recorded_blocked_and_a_restart_reaches_the_same_result_at_once` |
| satisfied trajectory resumes from FeltDB | `a_resumed_trajectory_that_already_satisfies_the_goal_completes_and_is_recorded_complete` (guard on and off) |
| completion precedence (7 cases above) | `chip-core/tests/stuck_guard.rs` |
| real binary | `stack-agent/tests/stuck_guard.rs`: `work_chip_can_complete_itself_is_not_stopped_for_repeating`, `an_inspection_that_has_its_evidence_but_never_answers_is_still_stopped_and_says_so` |

The guard-disabled path is unchanged: with `stuck_guard: None` the new code path is not entered
(`spec.stuck_guard.as_ref()` is `None`), and the tests above run with it off for the cases where it
matters.

## 5. Validation

Run on macOS arm64 (`pax` 0.4.1 on `PATH`), on the final code:

* `cargo fmt --all -- --check`: clean.
* Focused: `chip-core --test stuck_guard` (31), `--test stuck_guard_replay` (1),
  `chip-agent-state --test stuck_guard` (4), `stack-agent --test stuck_guard` (8): all pass.
* `cargo test --workspace --locked --no-fail-fast`: **1889 passed, 5 failed. The workspace does not
  pass on this host.**
* **The five inherited failures** (also failing on `main` before P7, verified one by one in the P7
  PR): `pax_boundary::a_file_held_open_for_writing_cannot_be_executed`,
  `pax_boundary::a_fixture_is_executable_when_the_helper_returns_even_while_a_writer_holds_it`
  (intermittent), `tls_boundaries::a_certificate_for_another_host_is_refused_even_when_its_ca_is_trusted`,
  `service::tests::malformed_requests_are_refused_and_start_nothing`,
  `verify::the_standalone_path_needs_no_compute_and_no_ambient_tooling_beyond_pax_and_cargo`.
  **New failures from this change: none.** I did not investigate the five.
* **Linux:** the repository's CI runs on Linux. See the PR's checks; the result is reported on the
  PR, not here.
* **Overhead:** not re-measured. The implementation changed by one extra `policy.propose` call, made
  only when the guard has already detected a pattern (once per stopped work, not per iteration), so
  P7's measurement of the per-iteration cost (about 30 ns) still describes the per-iteration path.

## 6. Limits and what was not done

* No real-model trajectory with inputs and observations exists in the repository, so none was
  replayed. The harness replays any such trajectories in the same format
  (`STUCK_REPLAY_FILE`), and the benchmark would need to record inputs and observations to feed it.
  I did not run new inference.
* The synthetic study is self-labelled and small.
* The guard still ends a model-completable work whose goal is met when the model keeps repeating
  (section 2, review gate).
* Not run: torn-write or `SIGKILL` crash windows for the guard.
