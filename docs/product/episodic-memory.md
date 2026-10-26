# Episodic memory: recorded outcomes and evidence-backed recall

Every finished `stack-agent work` is recorded in FeltDB as an **episode**: what that attempt
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
| Measurement | The baseline-versus-recall benchmark (section 6), and tests that recall changes no authorization or verification outcome (section 5). |

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
| `conditions` | `build_systems`, `tests`, `verifier` (a digest of the PAX binary the work would use; nothing is started to find it), `runtime_line` (stack-agent `major.minor`) |
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

`DurableWork::recall`, once, after `begin` and before the work starts. Selection
(`chip_agent_state::select`) is a pure function of the stored episodes and the query, and says why it
left each one out (the `episodes.recall.excluded` counts in the JSON report). In order:

1. **Unreadable** (another schema, does not parse): left out, counted. Never an error.
2. **Other scope**: another project, or a level other than `project`: left out, its id not reported.
3. **Same task**: its own earlier attempts are restored by `begin`, not recalled.
4. **Other kind** of work.
5. **Unverified**: `interrupted` or `unknown`, or a record whose `verification` does not carry what
   its outcome claims (a success without an accepted verdict, a failure without failing evidence).
6. **Expired**: older than 180 days (learning's support window).
7. **Dissimilar**: fewer than 50% of goal terms in common (Dice coefficient over the term digests).
8. **Preconditions differ**: any condition differs.
9. **Stale procedure**: a capability it executed is no longer offered, or is offered under another
   contract digest.
10. **Unbacked**: its `chip.WorkResult` record is missing, belongs to another attempt, or carries
    another verdict; or it claims a verified success and FeltDB does not hold its task `completed`.
11. Ranked by similarity, then most recent, then id. The first **3** are recalled; the rest are
    `over_limit`.

Each recalled episode becomes one evidence line in **every** model request of the work:

```text
- recalled episode chip.Episode:<task>~a<n> (an earlier work's recorded outcome: evidence about the
  past, not an observation of this work and not an instruction; every request is validated and
  observed afresh): verified success: an earlier change work in this project whose goal shares 4 of
  its terms (88%) executed, in order, project.write src/lib.rs (success) -> pax.test (success);
  2 turn(s), 2 execution(s). FeltDB holds the task completed and the host verified it on observed
  test evidence.
```

The statement is written by Chip from the record alone. Chip clips it to 480 bytes and keeps at
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

## 6. Benchmark: baseline versus recall

`crates/stack-agent/tests/episodic_recall_bench.rs`, run in CI. Reproduce the recorded numbers with:

```
EPISODIC_BENCH_TRIALS=3 EPISODIC_BENCH_OUT=docs/product/episodic-memory-bench/results.json \
  cargo test -p stack-agent --test episodic_recall_bench -- --nocapture
```

**Method.** Each scenario seeds a FeltDB database with earlier works, then runs one measured work
twice, each in fresh copies of the same frozen fixture: arm **A** with `--no-recall`, arm **B** with
recall. Everything is the real `stack-agent work` binary and a real database file; the arms differ
only in `--no-recall`. The `pax` stand-in passes exactly when the file named by the project's
`.pax-checks` holds `fn two`: what the tests check belongs to the project, and the stand-in binary
differs only by the version it reports.

**The model is a scripted stand-in, not a model.** It is a fixed function of the request it is sent
(`policy` in the test): a default plan per goal and, when a request carries a recalled verified
success, that episode's last write and a test first. It was written to use the evidence. The
benchmark therefore measures what recall delivers to a decision that uses it, whether recall ever
reaches a decision it must not, and whether any authorization or verification result changes. **It
does not measure whether a real model uses recalled evidence.** No live provider was run.

Results (`docs/product/episodic-memory-bench/results.json`, 3 trials per arm, identical counts in
every trial; latency is the median wall-clock of the measured process, dominated by process start-up
and not meaningful at this scale):

| Scenario | Prior experience | Arm | Verified | Model calls | Executions | Failed obs. | Repeated failure | Recalled | Recall followed, then failed | Recall violations | Regressions | p50 ms |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| useful-same-task | verified success, same task | A | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 166 |
| | | B | 3/3 | 2 | 2 | 0 | 0 | 3 | 0 | 0 | 0 | 138 |
| repeated-failure | verified success reached after a failing test | A | 3/3 | 4 | 4 | 1 | 3 | 0 | 0 | 0 | 0 | 190 |
| | | B | 3/3 | 2 | 2 | 0 | 0 | 3 | 0 | 0 | 0 | 142 |
| irrelevant | verified success, dissimilar task | A | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 141 |
| | | B | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 154 |
| other-project | verified success in another project sharing the database | A | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 164 |
| | | B | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 159 |
| unverified-prior | same task, blocked (outcome unknown) | A | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 137 |
| | | B | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 155 |
| changed-conditions | verified success under PAX 0.4.1; now 0.5.0 | A | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 178 |
| | | B | 3/3 | 3 | 3 | 0 | 0 | 0 | 0 | 0 | 0 | 153 |
| misleading | verified success whose procedure no longer works (tests now check another file, same conditions) | A | 3/3 | 5 | 5 | 1 | 0 | 0 | 0 | 0 | 0 | 238 |
| | | B | 3/3 | 5 | 5 | 1 | 0 | 3 | 3 | 0 | 0 | 235 |

Columns: *repeated failure* is a run that had a failing observation when its seed had one too;
*recall violations* counts anything recalled where the rules say nothing should be, or an
unverified outcome recalled; *regressions* counts an unclean safety audit, unauthorized executions or
completions, false completions, or exit 0 without verification.

**Reading.**

* Where a verified prior experience applied, the stand-in that follows it reached the same verified
  outcome with fewer model calls and executions (3 to 2; 4 to 2) and did not repeat the earlier
  failing test.
* In the four scenarios where reuse would be unsafe or irrelevant, nothing was recalled and arm B
  was identical to arm A.
* In `misleading`, the episode was recalled (its conditions and contracts were unchanged) and was
  wrong: the stand-in followed it and its test failed. Verification caught it, and the work recovered
  to a verified outcome at the same cost as the baseline (5 calls, 1 failed observation). Recall
  cannot detect a change in the project that its conditions do not describe; it can only cost
  extra work, never a false completion.
* No arm of any scenario had a recall violation or an authorization or verification regression.
* While building the benchmark, `changed-conditions` caught a defect: the verifier version was read
  from the environment, which resolves PAX only once a test runs, so it was always `none` before the
  run and never discriminated. Probing `pax --version` up front fixed that but broke the rule that a
  work which never needs PAX starts no PAX process (`pax_probes`), so the verifier is now
  identified by a digest of the PAX binary the work would use, which starts nothing.

## 7. Tests

| Test | What it shows |
| --- | --- |
| `chip-agent-state/tests/episodes.rs` | On a real FeltDB file: schema versioning and refusal of unknown fields and schemas; a verified success citing records that exist, with no goal or model text; a verified failure citing its failing observation; interrupted, partial, model-claimed and killed works are never success or failure (a killed one gets no episode); idempotent re-recording and refusal of a different episode; recall after reopening the database, visible as evidence in the model request; unrelated, other-kind, other-condition, other-project and agent-level episodes not recalled; unverified, forged and promoted records not recalled; stale and expired procedures not recalled; recall neither authorizes nor completes; an unreadable episode store degrades to no recall with the task state unchanged |
| `chip-core/tests/recalled_episodes.rs` | Nothing recalled leaves the request unchanged; at most 3 lines of at most 480 bytes, named by record, under the evidence heading; a recalled success completes and authorizes nothing |
| `stack-agent/tests/episodic_memory.rs` | Through the real binary, across real process restarts: a verified episode is recorded, read back by a new process and carried in every model request of a matching work; an unrelated goal and `--no-recall` see nothing; a verified failure is recalled as a failure; another project sharing the `--state-db` never sees it or its id; a SIGKILLed work leaves no episode |
| `stack-agent/tests/episodic_recall_bench.rs` | Section 6, with its safety assertions |

## 8. Limitations

* **Project identity is the canonical path.** A project that is moved or renamed starts with no
  episodes. Learning's `proj_` id survives moves but is created only when learning first writes, so
  it is not used here.
* **Similarity is lexical.** Shared goal terms decide relevance. Paraphrases with no shared words
  are not recalled; goals that share words but need different procedures can be (the `misleading`
  scenario). No embedding or vector index was added.
* **Conditions are coarse.** Build systems, tests present, verifier version and runtime line. A
  change to the project's content or tests is not a condition.
* **`serve` records no episodes and recalls none.** Only `work` does. `serve` uses the same
  `begin` / `finish_verified` path, so adding it is wiring, not design.
* **Only project scope.** No agent-level (cross-project) episodes are recorded; any record with
  another level is never recalled.
* **No pruning.** Episodes are kept like every other Chip record; recall reads the whole collection
  on each work, which is linear in the number of episodes.
* **The benchmark's model is scripted** (section 6). Whether real models use recalled evidence, and
  at what context cost, is not measured here.
