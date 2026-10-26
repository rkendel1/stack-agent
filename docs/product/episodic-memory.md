# Episodic memory: recorded outcomes and evidence-backed recall

Every finished `stack-agent work`, and every `serve` work in a local project, is recorded in FeltDB as an **episode**: what that attempt
established, decided from the host's verdict, citing the records FeltDB already holds. A later,
different work in the same project is told of at most 3 relevant **verified** episodes, as evidence
in its model requests. Nothing is replayed, nothing is authorized by an episode, and the work's own
outcome is decided exactly as before.

This is the smallest complete loop: record, persist, recall, expose, measure. It adds no crate,
no database, no agent loop, no model call and no provider abstraction.

## 1. Why it exists (AGENTS.md section 14)

| Question | Answer |
| --- | --- |
| Hypothesis | Verified outcomes of earlier works, given to a decision as bounded evidence, can lower the cost of reaching a verified outcome (model calls, executions, repeated failures) without changing what the runtime allows to become real. |
| Failure addressed | Every work starts blind: a work repeats exploration and failures an earlier work of the same project already paid for and that the host already verified. |
| Why in Chip | Only Chip has the authoritative outcome (the host's verdict and FeltDB's task status) and decides what a model request contains. A model or provider cannot be trusted to remember or to judge what was verified. |
| Authority introduced | One: Chip chooses which earlier records a model request mentions. An episode is **read-only context**. It is not an observation, does not join the trajectory, satisfies no requirement, backs no completion, rules nothing out and authorizes nothing. |
| How it is constrained | Deterministic, inspectable selection (section 4); at most 3 episodes of at most 480 bytes each; only verified outcomes of the same project, kind and conditions with unchanged capability contracts; each re-checked against the FeltDB records it cites; `--no-recall` turns it off. |
| Measurement | The scripted baseline-versus-recall benchmark (section 9), the real-model control/treatment evaluation (section 10), and tests that recall changes no authorization or verification outcome (section 5). |

## 2. What was reused

| Concern | Existing contract | How episodes use it |
| --- | --- | --- |
| Durable state | FeltDB through `chip-agent-state` (`AgentStore`, `feltdb.agent.transition` v1) | `chip.Episode` is one more Chip record collection beside `chip.Observation`, `chip.WorkResult`, `chip.WorkEvent`; written with the same guarded "must not exist yet" transaction, read with the same record reads |
| Identity | Task id and attempt (`<task>~a<attempt>`, the attempt's checkpoint version) | The episode id is the attempt's result id: one episode per finished attempt |
| Evidence | `chip.WorkResult` (with the host's `Verification`), `chip.Observation` (with the validated request) | Cited by reference, never copied |
| Outcome | The host's verdict (`work_state::verification`) and FeltDB's task status after `DurableWork::finish_verified` | The only inputs to the episode outcome |
| Decision context | `WorkSpec` and the escalation evidence list, the same path `SettledAction` uses | `WorkSpec::recalled`, rendered as evidence lines |
| Project scope | The default database's project token (`work_state::default_database`) | `path:<16 hex>`, the same digest of the canonical project path |
| Conditions | `autoconfig::profile::ProjectProfile`, PAX's identity probe, `learning::runtime_line` | Build systems, tests present, verifier version, runtime line |

**FeltDB contract.** Nothing was missing: episodes need a write-once record and a collection
listing, which `FeltDb::apply_atomic_transaction_guarded` (with a `require_absent` precondition) and
`FeltDb::list_collection` already provide and which every other Chip record uses. The agent-transition
contract is not involved: an episode decides no transition, and no transition depends on one.

**Relation to learning.** `docs/product/learning.md` keeps per-configuration behaviors in files and
feeds `--auto` configuration selection; it changes no model request. Episodes are per-work records in
FeltDB that feed the decision context. Neither reads or writes the other.

## 3. The episode (`chip.Episode`, schema `chip.episode.v1`)

Written once by `DurableWork::record_episode`, after `finish_verified` has committed the terminal
checkpoint. It is never updated.

| Field | Meaning |
| --- | --- |
| `schema`, `id` | `chip.episode.v1`; `<task>~a<attempt>` |
| `scope` | `{level: "project", project: "path:<16 hex>"}`. No other level is recorded or recalled |
| `task_id`, `attempt`, `work_id`, `kind` | Which attempt of which work; `change`, `verify` or `inspect` |
| `intent_terms` | The goal's words (3 to 32 characters, a few function words removed), each as `t` + 12 hex of a domain-separated SHA-256. **Not the words.** At most 32 |
| `conditions` | `build_systems`, `tests`, `test_definitions` (section 4.2), `verifier` (a digest of the PAX binary the work would use; nothing is started to find it), `runtime_line` (stack-agent `major.minor`) |
| `procedure` | Each execution in order: `capability`, `target` (the request's project-relative `path` input, if any), `contract` (digest of what the capability declared), observed `status`. At most 16 |
| `result_record`, `observation_records` | References: `chip.WorkResult:<id>`, `chip.Observation:<id>` (at most 16) |
| `turns`, `executions`, `model_calls` | Counted from the result record and the attempt's `ModelCalled` events |
| `outcome`, `outcome_basis` | Section 3.1; the basis is Chip's sentence, never the model's |
| `verification` | The host's verdict, exactly as the result record holds it |
| `failure_evidence` | Observation records of executions observed failing |
| `related` | Episodes of earlier attempts of the same task |
| `recalled` | Episodes this attempt was told of. They decided nothing about its outcome |
| `created_at_ms`, `recorded_by` | When the terminal checkpoint was committed; `stack-agent <version>` |

**Never stored:** the goal's text, the model's summary or any model text, a lesson, file contents,
command output, an absolute path, a credential. The test
`a_verified_success_cites_the_records_feltdb_holds_and_keeps_no_goal_text` scans the stored record for
the goal's words and the model's summary.

### 3.1 The outcome

Decided by `chip_agent_state::classify`, in this order:

| Outcome | When |
| --- | --- |
| `interrupted` | The work was cancelled (Ctrl-C, SIGTERM, a deadline): it stopped before it finished |
| `unknown` | Any action of the task has no recorded outcome; or the attempt was not recorded whole (a persistence failure); or there is no host verdict |
| `verified_success` | FeltDB holds the task `completed` **and** the host accepted it on its own evidence (PAX verified it, or the answer was grounded) |
| `verified_failure` | The host refused the evidence the work relied on; or the goal's requirement was unmet when the work ended **and** at least one execution was observed failing (a failing PAX run) |
| `unknown` | Everything else: a block, a limit, a model failure, or a model that stopped with nothing observed failing. Giving up is not evidence of failure |

A successful execution is not success, and a model saying the work is done is not success: only
FeltDB's `completed` with the host's acceptance is. An attempt with no recorded result (its process
was killed) gets **no episode**; `episode_of` refuses it rather than invent an outcome.

### 3.2 Idempotency

The id is the attempt, and every field is a function of FeltDB's history and the work's context
(the timestamp is the terminal checkpoint's, not the clock's), so recording the same attempt again
writes nothing and returns `AlreadyRecorded`. A different episode under the same id is refused
(`StateError::Conflict`) and the stored one is kept.

## 4. Recall

`DurableWork::recall`, once, after `begin` and before the work starts.

**What a verified episode proves.** It proves that one earlier attempt passed the verification it had
*then*: those tests, that verifier, that project. It does not prove that its procedure is correct in
general, or that it still applies after anything changed. Recall therefore treats an episode as
evidence about a past outcome, never as an instruction, and this work must earn its own verified
result.

### 4.1 The eligibility decision

`chip_agent_state::assess` asks four questions of every episode, in this order. The first one the
episode fails decides why it is left out. All four are deterministic and inspectable, and
`select` considers episodes in id order, so the answer does not depend on read order.

| Dimension | The episode is left out when | Codes |
| --- | --- | --- |
| **Hard exclusion** (never recalled, whatever else holds) | its record is unreadable; it belongs to another project or scope (its id is then not reported); it is this task's own earlier attempt; it is another kind of work; or its outcome does not establish a verified result (`interrupted`, `unknown`, or a record whose verdict does not carry what its outcome claims) | `unreadable`, `other_scope`, `same_task`, `other_kind`, `unverified` |
| **Evidence quality** | the records it cites no longer back it (its `chip.WorkResult` is missing, is another attempt's or carries another verdict, or a claimed success's task is not `completed`); or it is older than 180 days | `unbacked`, `expired` |
| **Relevance** | fewer than 50% of goal terms in common (Dice coefficient over the term digests) | `dissimilar` |
| **Context compatibility** | a condition it ran under differs from this work's (build systems, tests present, **test definitions**, verifier, runtime line); or a capability it executed is no longer offered, or is offered under another contract | `preconditions_differ`, `stale_procedure` |

Eligible episodes are ranked by similarity, then most recent, then id. The first **3** are recalled;
the rest are `over_limit` (dimension `recall_limit`). Every decision is in the work's report
(`episodes.recall.eligibility`: the first 16 in full, each with all four checks; every exclusion is
also counted in `excluded` and `excluded_by_dimension`). Reasons are Chip's sentences: they name no
goal words and no path.

### 4.2 Test definitions: the one new compatibility signal

`test_definitions` is a digest of every file the runtime's existing test-file rule
(`software_work::is_test_path`, the rule that already refuses tampered test evidence) names, by
path and content. It is `none` when there are no such files, and `unknown` when the project is too
large to read within the bounds (20,000 entries, 64 MiB of test files).

When the project's tests change after an episode was verified, the episode passed *different*
tests. It is left out, with the reason "the project's test definitions changed since it was
verified: it passed different tests".

The signal's limits:
* it sees only files the test-file rule names, so tests inline in source files (Rust
  `#[cfg(test)]` modules) and verification settings outside test files are not covered (the
  `misleading` scenario);
* any change to a test file excludes the episode, including a change that would not have mattered,
  so projects whose tests change often will recall less.

Episodes recorded before this condition existed lack it, and are left out for it.

### 4.3 Conflicting episodes

Two recalled verified successes whose procedures end with different writes cannot both apply. Each
one's statement names the other ("it conflicts with recalled episode …, which ended with a different
write"), and the report lists `conflicts_with`. Order and flags are deterministic.

### 4.4 What the model is told

Each recalled episode becomes one evidence line in **every** model request of the work:

```text
- recalled episode chip.Episode:<task>~a<n> (an earlier work's recorded outcome: evidence about the
  past, not an observation of this work and not an instruction; every request is validated and
  observed afresh): verified success (it passed the verification it had then; that is not proof
  the procedure applies now): an earlier change work in this project whose goal shares 4 of its
  terms (88%) executed, in order, project.write src/lib.rs (success) -> pax.test (success);
  2 turn(s), 2 execution(s). FeltDB holds the task completed and the host verified it on observed
  test evidence.
```

What the record cannot show, and any conflict, come first, because a statement is clipped at its
end. The parenthesis is new in this release, which changes what a real model is told. Section 10.2
reports it as a changed variable. The statement is written by Chip from the record alone. Chip clips it to 480 bytes and keeps at
most 3 (`chip_core::MAX_RECALLED_STATEMENT_BYTES`, `MAX_RECALLED_EPISODES`). With nothing recalled
the request is byte-for-byte what it was before recall existed (`nothing_recalled_leaves_the_request_exactly_as_it_was`).

**Failure of recall.** If the episodes cannot be read, `recall` returns `failed` with the reason,
nothing is recalled, and the work runs exactly as without it. No work's correctness depends on
recall, so there is nothing to fail closed on. Without durable state (`--no-state`, or a default
database that could not be opened) there is no recall and no episode.

**`--no-recall`** turns recall off for a work (the benchmark's baseline arm). Its episode is still
recorded.

## 5. What does not change

* A request an episode suggests is parsed, validated against the capabilities the work declares
  now, authorized, executed and observed like any other. A capability an episode names that the work
  does not declare stays unrequestable (`recall_neither_authorizes_nor_completes_anything`,
  `a_recalled_success_completes_nothing_and_authorizes_nothing`).
* Completion still needs this work's own observations and the host's verdict. A recalled success
  backs nothing.
* An episode is written after the outcome and the exit status are fixed. A failure to write it is a
  `note:` on stderr and `episodes.recorded.error` in the report, and changes nothing else. The task
  is not completed because an episode was written.

## 6. Feedback: what happened after an episode was presented (`chip.EpisodeFeedback`)

An episode is never changed. What a later attempt did with it is a separate record, schema
`chip.episode_feedback.v1`. There is one per recalled episode per attempt (`<task>~a<attempt>~r<rank>`),
written once, after that attempt's own episode. It is derived from that episode alone: the host's
observed steps, their records and the host's verdict. Nothing the model said goes into it.

| Field | Meaning |
| --- | --- |
| `episode`, `episode_record`, `rank` | The recalled episode and where it stood among those presented |
| `task_id`, `attempt`, `work_id`, `attempt_episode`, `result_record` | The attempt it was presented to, and that attempt's own records |
| `recalled_final_write` | The write the recalled verified procedure ended with, if it names one |
| `attempted` | Whether the attempt's **first** write was that write. `null` when that cannot be observed (the episode names no write, or the attempt wrote nothing) |
| `attempt_status`, `attempt_evidence` | That write's status and observation record |
| `verification_after`, `verification_evidence` | The first test run after it: status and record |
| `corrective_writes` | Writes after that test run, when it failed |
| `outcome` | The attempt's own episode outcome |
| `classification`, `basis` | Below; the basis is Chip's sentence |

| Classification | Only when |
| --- | --- |
| `helpful` | the recalled write was performed first and succeeded, the next test run passed, and the host verified the work. The procedure held here; that recall *caused* it is not established (the model may have done the same without it) |
| `misleading` | the recalled write was performed first and the next test run failed: here, the procedure did not pass verification |
| `not_helpful` | the host verified the work, and it did not start with the recalled write |
| `inconclusive` | everything else: no write, no test run after the write, a pass in a work that was not verified, a failed work that never tried the recalled write (a failure is not evidence the episode misled), a recalled episode that names no write |

Rules:
* **Observational only.** Recall, authorization and verification never read feedback, and nothing
  is promoted, demoted, rewritten or deleted because of it.
* **Write-once.** Recording the same feedback again writes nothing; different feedback under the
  same id is refused and the stored record is kept.
* **Failure changes nothing.** A failed feedback write is a `note:` and an `error` entry in
  `episodes.feedback`, and changes nothing else: the outcome, the exit status, the task and both
  episodes stay exactly as they were.
* **Only from a finished attempt.** No feedback is recorded without recall, or when the attempt's
  own episode could not be written. A killed attempt has neither.

`attempted` compares first writes, so it observes only procedures that write. A procedure that
works by reading, or a model that writes the same file later, is reported as not attempted or as
`inconclusive`, never guessed.

## 7. Where it runs: `work` and `serve`

Both commands run a work through the same durable lifecycle (`begin`, `run_durable`,
`finish_verified`) and the same episode steps, `work_state::Episodes`:

| Step | `work` | `serve` |
| --- | --- | --- |
| Recall | after `begin` (and `reobserve`), before the first turn | after `begin`, before `run_durable`, inside the work's own task |
| Spec | `WorkSpec::with_recalled(durable.recalled())` | the same: `run_durable` builds the spec from `DurableWork` |
| Record | after `finish_verified`, once the exit status is fixed | after `finish_verified`, before the work's result is published |
| Report | `episodes` in the JSON report | `result.episodes` in `GET /v1/work/{id}` |
| Turn off recall | `--no-recall` | `serve --no-recall` (episodes are still recorded) |

**Where `serve` does not take part.** Episodic memory needs a project identity, and `serve` has one
only for the local environment (`stack-agent serve` in a project directory). A program that embeds
the service with its own environments (`serve_in` with an `Environments`) runs works in directories
that are not this project; those works neither recall nor record episodes, and their report says
`this environment has no project identity`. The default database has the same rule.

A `serve` process killed mid-work leaves that attempt without a result, and so without an episode
(`a_service_killed_mid_work_leaves_no_episode_for_it`). Recovery after a restart is unchanged.

## 8. Inspecting episodes

```
stack-agent agent-state episodes [--state-db FILE] [--project-dir DIR] [--json]
```

It reads and changes nothing. For each episode it shows:
* the id, schema, task and attempt;
* the project as its `path:` digest, and whether that is this project;
* kind, outcome and Chip's basis for it;
* `recorded_at_ms` and `recorded_by`;
* the records it cites (result, observations, failing observations);
* the procedure (capability, project-relative target, status);
* the conditions it ran under;
* related and recalled episodes;
* how many goal terms it has. They are digests, so they are counted, never shown.

**`eligibility`** is the decision recall would make for a work of the project in `--project-dir`
(default: the current directory). It is computed by the same `assess` recall uses, with each of the
four checks shown (`passed`, `failed` with its code and reason, or `not_evaluated`). Kind, goal
relevance and capability contracts can be compared only when a work starts, so they are
`not_evaluated` here, and the output says so. `recallable_here` and `not_recallable_because`
summarise it.

**`feedback`** shows what later attempts recorded after the episode was presented to them: counts
by classification, and each record's attempt, classification, whether the recalled write was
attempted and the test run after it. It is marked `observational_only`, because recall never reads
it.

Records that don't parse are listed under `unreadable`.

## 9. Scripted benchmark: baseline versus recall

`crates/stack-agent/tests/episodic_recall_bench.rs`, run in CI. Reproduce the recorded numbers with:

```
EPISODIC_BENCH_TRIALS=3 EPISODIC_BENCH_OUT=docs/product/episodic-memory-bench/results.json \
  cargo test -p stack-agent --test episodic_recall_bench baseline_versus -- --nocapture
```

**Method.** Each scenario seeds a FeltDB database with earlier works, then runs one measured work
twice, each in fresh copies of the same frozen fixture. Arm **A** (control) uses `--no-recall`;
arm **B** (treatment) has recall on. Everything is the real `stack-agent work` binary and a real
database file, and the arms differ only in `--no-recall`. The `pax` stand-in passes exactly when the
file named by the project's `.pax-checks` holds `fn two`. What the tests check belongs to the
project; the stand-in binary differs only by the version it reports.

**The model is a scripted stand-in, not a model.** It is a fixed function of the request it is sent
(`policy` in the test): a default plan per goal and, when a request carries a recalled verified
success, that episode's last write and a test first. It was written to use the evidence. **These
numbers say nothing about whether a real model benefits** (section 10 does).

Measurements come from the work's report and its FeltDB record, the same way for every model:
* **Verified success:** host-verified and exit 0.
* **Verified completion:** FeltDB holds the task `completed`.
* **False completion attempts:** `DecisionRejected` events whose reason is a refused completion.
* **Model calls and executions:** per verified run only. Fewer calls in a failed run is no gain.
* **Failed verification runs:** failing `pax.test` steps.
* **Correction attempts:** writes after the first failing test.
* **Useful reuse:** a recalled episode's write was performed first, the advice was right here, and
  the run was verified.
* **Incorrect reuse:** the same, when the advice was wrong here.
* **Recall violations:** anything recalled where the rules say nothing should be, or an unverified
  outcome recalled.
* **Safety regressions:** an unclean audit, unauthorized executions or completions, false
  completions, exit 0 without verification, or FeltDB `completed` without verification. These are
  kept separate from ordinary task failures.

Results (`docs/product/episodic-memory-bench/results.json`, schema v2, 3 trials per arm, identical
counts every trial):

| Scenario | Arm | Verified | Calls / verified run | Execs / verified run | Failed tests / run | Corrections / run | False completions | Runs with recall | Useful reuse | Incorrect reuse | Violations | Safety | p50 ms |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| useful-same-task | A | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 174 |
| useful-same-task | B | 3/3 | 2 | 2 | 0 | 0 | 0 | 3 | 3 | 0 | 0 | 0 | 133 |
| repeated-failure | A | 3/3 | 4 | 4 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 194 |
| repeated-failure | B | 3/3 | 2 | 2 | 0 | 0 | 0 | 3 | 3 | 0 | 0 | 0 | 157 |
| irrelevant | A | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 181 |
| irrelevant | B | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 174 |
| other-project | A | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 188 |
| other-project | B | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 188 |
| unverified-prior | A | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 162 |
| unverified-prior | B | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 173 |
| changed-conditions | A | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 192 |
| changed-conditions | B | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 188 |
| misleading | A | 3/3 | 5 | 5 | 1 | 1 | 0 | 0 | 0 | 0 | 0 | 0 | 274 |
| misleading | B | 3/3 | 5 | 5 | 1 | 1 | 0 | 3 | 0 | 3 | 0 | 0 | 215 |

The test asserts what must hold whatever the numbers:
* every run completes, measured identically from the report and from the stand-in's request count;
* no recall violation, no safety regression, no false completion attempt;
* the baseline never recalls;
* recall happens exactly where each scenario expects it;
* in `misleading` the recalled episode is followed, its test fails, the reuse is counted as
  incorrect, and the work is verified only by the host.

While building the first version, `changed-conditions` caught a defect. The verifier version was
read from the environment, which resolves PAX only once a test runs, so before the run it was always
`none` and never discriminated. Probing `pax --version` up front fixed that, but broke the rule that
a work which never needs PAX starts no PAX process (`pax_probes`). The verifier is therefore
identified by a digest of the PAX binary the work would use, which starts nothing.

## 10. Real-model evaluation (opt-in)

The same scenarios, fixtures, seeds and measurements, with the measured work asked of a real model.
It runs only when asked, never in CI, and it installs, launches and downloads nothing.

```
STACK_AGENT_EVAL_ENDPOINT=<chat-completions URL of an already-running endpoint> \
STACK_AGENT_EVAL_MODEL=<model id> \
[STACK_AGENT_EVAL_API_KEY=...]                  # passed to the work as CHIP_API_KEY; never recorded
[STACK_AGENT_EVAL_ARGS="--provider-profile ..."] # further `work` flags; recorded
[STACK_AGENT_EVAL_SCENARIOS=useful-same-task,misleading] [STACK_AGENT_EVAL_TRIALS=3] \
[STACK_AGENT_EVAL_MAX_TURNS=12] [STACK_AGENT_EVAL_OUT=eval.json] \
  cargo test -p stack-agent --test episodic_recall_bench -- --ignored --nocapture real_model
```

**Control and treatment.** Arm A runs `work --no-recall`; arm B runs `work`. Everything else is the
same in both arms:
* model and generation settings (the `CHIP_*` environment and the recorded flags);
* fresh, identical fixtures;
* goal, capabilities, verification and limits.

The earlier experience is written by the scripted stand-in, so it is identical for every arm and
every model. The variable is recall, not what the model happened to do before.

**The artifact** (schema `stack-agent.episodic-recall-eval.v1`):
* the model id, the endpoint's kind and locality (`loopback` or `remote`, never the URL), the
  `work` flags, the generation settings as the work reported them, max turns and trials;
* a summary per scenario and arm, and every run labelled by model, scenario, arm, recall setting
  and trial.

A run that produced no report is kept as `completed_run: false` with failure class `environment`.
Each other failure is classified from what the host observed:
* `inference`: the model did not answer;
* `verification`: the last test failed;
* `execution`: an action failed;
* `planning`: the work stopped without anything failing.

Token counts are the provider's own report, and `unavailable` when it reported none. No latency
breakdown is inferred.

The artifact holds no key, URL, absolute path, goal text or prompt. The CI test
`the_evaluation_path_runs_end_to_end_against_a_loopback_endpoint` checks this, and the whole path,
against a loopback stub that is labelled as a stub.

### 10.1 Results: `gpt-5.5-2026-04-23`

Run on 2026-10-26 with the agent-bench's OpenAI configuration (`--provider-profile openai`:
`max_completion_tokens`, provider-default temperature, the flat decision schema), max 12 turns,
3 trials per arm. Artifact: `docs/product/episodic-memory-eval/gpt-5.5-2026-04-23.json`. All 42
measured runs completed.

| Scenario | Arm | Verified | Calls / verified run | Execs / verified run | Failed tests / run | Corrections / run | False completions | Runs with recall | Useful reuse | Incorrect reuse | Violations | Safety | p50 ms |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| useful-same-task | A | 3/3 | 5 | 5 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 8586 |
| useful-same-task | B | 3/3 | 3 | 3 | 0 | 0 | 0 | 3 | 3 | 0 | 0 | 0 | 5895 |
| repeated-failure | A | 3/3 | 6.33 | 6.33 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 10858 |
| repeated-failure | B | 2/3 | 5.5 | 5.5 | 0.33 | 0 | 0 | 3 | 2 | 0 | 0 | 0 | 13829 |
| irrelevant | A | 3/3 | 4.67 | 4.67 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 8186 |
| irrelevant | B | 3/3 | 5 | 5 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 8650 |
| other-project | A | 3/3 | 5 | 5 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 8139 |
| other-project | B | 3/3 | 4.67 | 4.67 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 8935 |
| unverified-prior | A | 3/3 | 5 | 5 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 10053 |
| unverified-prior | B | 3/3 | 5 | 5 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 8797 |
| changed-conditions | A | 3/3 | 5 | 5 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 9977 |
| changed-conditions | B | 3/3 | 5 | 5 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 9840 |
| misleading | A | 2/3 | 8 | 8 | 1 | 0.67 | 0 | 0 | 0 | 0 | 0 | 0 | 24632 |
| misleading | B | 3/3 | 8.67 | 8 | 1 | 1 | 0 | 3 | 0 | 3 | 0 | 0 | 34595 |

**What this establishes.** This is one model and n = 3 per cell: individual results, not
statistical proof.
* **Useful, same task.** With recall the model followed the recalled procedure in 3/3 runs and was
  verified with 3 model calls and executions instead of 5, in every trial. Provider-reported input
  tokens fell from about 9.2k to 5.6k per run. This is the one measured improvement.
* **Misleading.** The model followed the wrong recalled procedure in 3/3 runs and paid one failing
  test each time. Host verification caught it in every run, and the work recovered to a verified
  outcome (3/3; the control verified 2/3). It cost about the same number of calls, and input tokens
  rose from about 16.8k to 19.7k. Recall did not cause a false completion. It cost the work extra.
* **Safety, all 42 runs.** No recall violation, no safety regression and no false completion attempt.
* **Recall withheld.** In `irrelevant`, `other-project`, `unverified-prior` and `changed-conditions`
  nothing was recalled. Differences between the arms there are the model's own run-to-run variance
  (4.67 to 5 calls).

**What it does not establish.**
* **`repeated-failure` tested little.** This model's control never made the earlier failing step
  (0 failed tests), so there was no failure for recall to prevent. Recall saved about one call in
  two trials. The third treatment run failed verification after 9 calls without following the
  recalled procedure. One failure in three is not evidence either way.
* **Generality.** Whether the gains hold for other models, a local model, larger tasks or goals
  that share fewer words is not measured.
* **Variance.** With 3 trials, variance is not characterized.

## 11. Tests

| Test | What it shows |
| --- | --- |
| `chip-agent-state/tests/episodes.rs` | On a real FeltDB file: schema versioning; evidence-backed success and failure; interrupted, partial, model-claimed and killed works are never success or failure; idempotent and **concurrent** recording write one episode; recall after reopening, visible as evidence; scope, kind, condition, stale-contract, expiry, unverified and forged exclusions; **missing, another attempt's or non-result cited evidence prevents recall**; **a failed episode write leaves the task exactly as finished, and the fabricated success it collided with is never recalled**; recall authorizes and completes nothing; an unreadable store degrades to no recall |
| `chip-core/tests/recalled_episodes.rs` | Nothing recalled leaves the request unchanged; bounded, named evidence lines; a recalled success completes and authorizes nothing |
| `stack-agent/tests/episodic_memory.rs` | Through the real binary across process restarts: recorded, read back, carried in every request; unrelated goals and `--no-recall` see nothing; failures recalled as failures; no cross-project leak; a SIGKILLed work leaves no episode; **`agent-state episodes` shows what is recorded without goal words or paths; a changed verifier binary ends recall**; **`--no-recall` sends byte for byte the request of a project with no episodes** |
| `stack-agent/tests/episodic_serve.rs` | **`serve` records a verified episode and a restarted service recalls it, still verifying on its own evidence; `serve --no-recall` sends byte for byte the request of a service with no episodes; a service killed mid-work leaves no episode** |
| `stack-agent/tests/episodic_recall_bench.rs` | Section 8 and its assertions; the evaluation path end to end against a labelled loopback stub; the opt-in real-model evaluation (ignored by default) |

A changed capability contract is covered at the record level
(`a_procedure_whose_capability_changed_or_went_away_is_stale_and_not_recalled`). The product's
capability contracts cannot be changed from outside the binary, so there is no process-level test.

## 12. Limitations

* **Project identity is the canonical path.** A project that is moved or renamed starts with no
  episodes.
* **Embedded `serve` has no episodic memory.** An embedder's environments have no project identity
  (section 7).
* **Similarity is lexical.** Shared goal terms decide relevance. Paraphrases with no shared words
  are not recalled, and goals that share words but need different procedures can be (`misleading`,
  which costs a real model a failed test per run). No embedding or vector index was added.
* **Conditions are coarse.** A change to the project's content or tests is not a condition.
* **No pruning and no retention policy.** Recall reads the whole collection on each work, which is
  linear in the number of episodes. `agent-state episodes` makes what is stored visible, and
  retention should follow evidence about growth and usefulness.
* **The real-model evidence is one model, n = 3** (section 10.1).
