# Workflow candidates: procedures proposed from verified experience

An [episode](episodic-memory.md) describes one attempt. A **workflow candidate** describes the
procedure one or more verified attempts followed, in terms of the capabilities they executed, so it
can be inspected and checked against the runtime as it is now.

A candidate is a proposal. It is never:
* trusted because it exists;
* executable authority;
* a replacement for the agent loop.

Nothing executes a candidate. Whether a procedure is safe and succeeds now is established only by a
work that performs it under the current authorization, validation and verification.

Past work can suggest a procedure. Only current conditions and current verification can establish
whether it is safe and successful now.

## 1. What was reused, and the one gap

| Concern | Existing contract | How candidates use it |
| --- | --- | --- |
| Durable state | FeltDB through `chip-agent-state` | `chip.WorkflowCandidate` is one more write-once Chip record collection |
| Experience | `chip.Episode` (outcome, procedure, conditions, contract digests) | Only `verified_success` episodes that their records back |
| Steps and inputs | `chip.Observation` v2 records, which hold each step's validated request | Referenced per step; each step's input names come from the request |
| Capability contracts | `work_state::contract_digest` over each `CapabilityDescriptor` | Required capabilities carry the digest they ran under; validation compares it with what the binary declares now |
| Verification | The host's verdict in `chip.WorkResult` | Every supporting episode must have been verified (or, for an inspection, grounded) |
| Conditions | The episode's conditions, including `test_definitions` | The candidate's preconditions |

**The gap.** The episode's procedure keeps each step's capability, target path, contract and
status, but not every input. Each step's full validated request is in the `chip.Observation`
record the episode cites, so extraction reads it from there by reference. No schema field was added.

Steps are matched to their observation records by position: the episode builds both lists in the
same order under the same bound. An episode where the two differ in length, or whose executions
outnumber its recorded steps, is rejected as `procedure_incomplete`.

## 2. The candidate (`chip.WorkflowCandidate`, schema `chip.workflow_candidate.v1`)

| Field | Meaning |
| --- | --- |
| `schema`, `id` | `chip.workflow_candidate.v1`; `<procedure key>.<support digest>` |
| `procedure_key` | `wfp1.` + a digest of the project, kind, goal signature, conditions and procedure (each step's capability, contract, fixed inputs and status). The same for every candidate of this procedure |
| `scope`, `kind` | The project (its `path:` digest) and the kind of work |
| `goal_signature`, `goal_terms` | A digest of the supporting episodes' goal-term digests, and those digests. **Not readable text**; inspection only counts them |
| `preconditions` | The conditions every supporting episode ran under |
| `required_capabilities` | Each capability the procedure executes, to its contract digest |
| `steps` | In order: `capability`, `contract`, `role` (`change`, `verify` or `observe`), `inputs`, `expected_status`, and `evidence` (the step's observation record in each supporting episode) |
| `verification` | `test_run_after_last_change` (with the verifying step) or `grounded_answer`, with the verdict the host must reach. `null` when the records do not establish one |
| `support` | Each supporting episode, its record and result record, and its outcome |
| `strength` | `single_observation` (one episode) or `repeated` |
| `unresolved` | What the records do not establish, each with a code and detail |
| `created_at_ms`, `extracted_by` | The latest supporting episode's time (a fact of the records, so extraction repeats exactly), and the runtime that extracted it |

**Inputs.** Each input is named as the recorded request named it:
* **`fixed`**: only a project-relative `path`, which every supporting episode used;
* **`parameter`**: everything else, notably a write's `content`, which a model wrote. A work that
  performs the step supplies it then, under the capability's own validation.

No content is ever copied into a candidate (`a_verified_episode_yields_a_single_observation_candidate_with_references_not_content`).
A candidate is a procedure skeleton in capability terms, never stored code.

## 3. Extraction

`chip_agent_state::extract` is pure and deterministic. `AgentStore::extract_workflows` runs it over
one project's episodes and writes each candidate once. `work` and local `serve` call it after a
work's episode and feedback are written. It reads episodes and observations and writes only
candidates (`extraction_writes_only_candidates_and_never_changes_episodes_or_evidence`).

**Which episodes count.** An episode is rejected, with a code, when:

| Code | When |
| --- | --- |
| `not_verified_success` | its outcome is anything but `verified_success` |
| `unsupported` | its record does not carry the verdict its outcome claims, or it is not a project episode |
| `evidence_missing` | the records it cites no longer back it |
| `no_procedure` | it executed nothing |
| `procedure_incomplete` | its procedure cannot be reconstructed whole (cut short, or steps and records disagree) |
| `inputs_unrecorded` | a step's observation holds no validated request (a `v1` record, an operator's settlement), names another capability, or has a path that is not project-relative |

The procedure is never completed by guesswork. A rejected episode yields nothing.

**Grouping.** Episodes are grouped by project, kind of work, goal signature (the *exact* set of
goal-term digests), conditions, and procedure. Each group is one candidate. Two different
procedures for the same task are **never merged**: they become separate candidates, and validation
reports each as conflicting with the others.

**Unresolved, not invented.**
* `failed_steps`: the procedure includes failed steps (a wrong write, a failing test) before its
  verified result. Which earlier steps that result depends on is not established, so the candidate
  records each step's observed status and stays unresolved.
* `verification_undefined`: no passing test run follows the last change, or (for an inspection)
  not every supporting answer was accepted as grounded.

**Write-once.** A candidate's id names its procedure and the exact set of episodes that support it.
Extracting again writes nothing new. A new supporting episode writes a *new* candidate for the same
procedure (`strength: repeated`); the old record is unchanged and validation reports it
`superseded`. More support strengthens provenance and changes no check. Writing a different record
under an existing id is refused and the stored one is kept.

**Failure.** If candidates cannot be written, `work` says so on stderr and in
`episodes.workflows.error`, and nothing else changes: the outcome, the exit status, the task and
its episode stay exactly as they were (`a_failed_candidate_write_changes_nothing_else`).

## 4. Validation

`chip_agent_state::validate` checks a candidate against the runtime as it is now. It reads records
and declarations only, and never executes a step. Every check passes or fails with a stable code:

| Check | Fails with | When |
| --- | --- | --- |
| `supporting_evidence` | `evidence_missing` | a supporting episode is gone, unreadable, no longer a verified success, or no longer backed by its records |
| `project` | `other_project` | the candidate is another project's |
| `preconditions` | `precondition_unmet` | a condition differs (build systems, tests present, test definitions, verifier binary, runtime line) |
| `capabilities` | `capability_unavailable`, `contract_changed` | a required capability is not offered to this kind of work now, or declares another contract |
| `inputs` | `input_unsatisfiable` | a step carries an input its capability no longer accepts, or the capability now requires one the step does not carry |
| `verification` | `verification_undefined` | the candidate carries no verification requirement |
| `resolution` | `unresolved_step` | anything about the procedure is unresolved |
| `conflicts` | `conflicting_procedure` | another procedure was verified for the same task under the same conditions |
| `current` | `superseded` | a later candidate supports the same procedure with more episodes |
| `task` (only with a task) | `task_mismatch` | the task is another kind of work, or its goal shares fewer than 50% of terms |

A candidate is **eligible for consideration** when no check fails. That is advisory. It executes
nothing, grants no authority and promises no success; a work that follows the procedure is
validated, authorized and verified like any other. Nothing in the runtime consults validation to
decide anything.

## 5. Inspecting candidates

```
stack-agent agent-state workflows [--state-db FILE] [--project-dir DIR] [--goal TEXT [--kind change|verify|inspect]] [--json]
```

It follows the other `agent-state` commands (the default database, `--state-db`, `--json`). For
each candidate it shows:
* the id, schema and procedure key;
* the project digest and kind, and the number of goal terms;
* supporting episodes with their outcomes, the number of supporting observations, and strength;
* the steps with their capabilities, roles, contracts, inputs (fixed paths and parameters),
  expected statuses and evidence;
* the required capabilities, preconditions and verification requirement;
* anything unresolved;
* `known_incompatibilities`: later feedback on supporting episodes. It is observational and never
  used to decide anything;
* the full validation against the project in `--project-dir` (default: the current directory).

The current capability declarations come from the binary's own capability set for the project,
built without taking the project lock or starting anything. `--goal` adds the task check.

It reads and changes nothing: the database file is byte for byte the same afterwards
(`a_verified_work_proposes_a_workflow_that_a_new_process_inspects_and_validates_read_only`). It
shows no absolute path, no goal words and no file content.

## 6. Tests

| Test | What it shows |
| --- | --- |
| `chip-agent-state/tests/workflows.rs` | On a real FeltDB file: a verified episode yields a single-observation candidate with fixed paths, parameter content, evidence references and a verification step, and no content or goal words; unverified, unbacked, unrecorded and cut-short procedures are rejected with codes; extraction is deterministic (any read order), idempotent and survives reopening; more support writes a new candidate and supersedes the old one; different procedures stay separate and conflict; each current incompatibility (contract changed, capability gone, inputs changed, precondition unmet, evidence missing, other project, task mismatch) fails with its code, the same every time; failed steps and missing verification keep a candidate ineligible; a failed candidate write changes nothing else; extraction changes no episode or evidence |
| `stack-agent/tests/episodic_memory.rs` | Through the real binary: a verified work proposes a candidate; a new process inspects and validates it against the binary's real capability contracts, eligible; inspection leaves the database file byte for byte unchanged and leaks no goal words, content or path; an unrelated `--goal` gives `task_mismatch`; a changed verifier binary gives `precondition_unmet` |
| Existing episode, feedback, `serve` and benchmark tests | Unchanged and passing: recall, feedback and the deterministic benchmark's counts are the same as before |

## 7. Limitations

* **Goal signatures are exact.** Episodes group only when their goal-term digest sets are equal,
  so similar but differently worded goals give separate candidates. No similarity engine was
  added.
* **Procedures with failed steps stay unresolved.** Extraction does not decide which earlier steps
  a verified result depends on, so a verified correction after a failure yields an ineligible
  candidate.
* **Candidates are skeletons.** Every input but a path is a parameter, so a candidate says *what
  to do with which capabilities*, not the content to write.
* **Conflicts are permanent until the evidence changes.** Two verified procedures for one task
  make both ineligible: neither can be preferred without new evidence, and none is chosen
  automatically.
* **Extraction runs after a work.** Episodes recorded before this release get candidates the next
  time a work in their project finishes.
* **Not used by the runtime.** Nothing offers, executes or promotes a candidate. Using candidates
  in the control loop is a later decision.
