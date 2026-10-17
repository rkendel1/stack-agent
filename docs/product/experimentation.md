# Experimentation seam

`crates/chip-experiments` lets us try alternative agent behaviors (and capability implementations)
on controlled tasks without touching production execution. An experiment:

1. runs a baseline and a candidate strategy on the same tasks;
2. judges both by the same host-defined criteria over host-observed evidence;
3. reports the comparison with its provenance.

The seam is opt-in (`stack-agent experiment ...`) and off unless invoked. When it is not invoked,
nothing about `work`, `verify` or `serve` changes.

It composes existing contracts and is not an authority of its own. It has no agent loop, no
inference interface, no capability registry, no authorization, no persistence, no verification
engine and no plugin loader.

## 1. Architecture and integration points

The seam reuses existing contracts throughout. For each concern below, the first line is the
existing contract and the second is how the experiment uses it.

**Orchestration, authorization, execution, limits and completion**
- *Contract:* `chip_core::Agent::run_work_with_context_policy`, one ordinary Chip work per run.
- *Use:* every proposal is validated, every request routed through the work's capability set, and
  every limit enforced by Chip. Completion is refused while required observations are unmet.

**Model interpretation**
- *Contract:* `chip_core::ModelDecisionBoundary`, the same strict `chip.work-decision.v1` reader that
  `work` uses.
- *Use:* not pluggable. A strategy cannot change what model output means.

**Inference**
- *Contract:* the `chip_inference::ModelProvider` interface (FX's boundary for Chip).
- *Use:* experiments use either no model (any escalation fails with a typed error, and nothing is
  sent) or a task's recorded replies. Real providers are not wired in yet (section 10).

**Decision behavior: planning and capability selection**
- *Contract:* `chip_core::LocalWorkPolicy`.
- *Use:* a strategy's proposals. Chip still decides whether a proposal is executed (see the
  evidence-reuse rule in section 7).

**Context selection**
- *Contract:* `chip_core::EscalationContextPolicy`.
- *Use:* a strategy's choice of what an escalation tells the model.

**Capabilities and authorization**
- *Contract:* `chip_core::CapabilitySet` and `CapabilityBackend`; Chip refuses any capability that
  is not declared to the work.
- *Use:* each run gets a set built from capability fixtures, filtered to the definition's
  allowlist by an `Allowlisted` wrapper (a filter, not a catalog). The owning backend's own input
  validation (for example the project path rules) runs unchanged.

**Environment**
- *Contract:* `chip_core::WorkEnvironment`, plus `chip-project`'s path rules and observation
  invariants.
- *Use:* a disposable directory per run (isolated and observe modes), audited by the same
  invariants the local environment uses.

**Cancellation and deadlines**
- *Contract:* `chip_core::WorkControl`, which Chip checks before every model call and every
  execution.
- *Use:* each run's control carries the experiment's `CancelHandle` and the run's own deadline.

**Safety audit and measurement**
- *Contract:* `audit_safety`, `verify_trajectory` and `measure_utility`.
- *Use:* applied to every run, exactly as `work` applies them.

**Evidence**
- *Contract:* recorded `Observation`s and their `ObservationOrigin`s.
- *Use:* the report references each observation by execution id, capability and the `sha256` of its
  output.

**Secret handling**
- *Contract:* the native provider scrubs exact secret values (its scrubber is private to that crate).
- *Use:* the same exact-value rule, applied by `Redactor` to every report and summary, with values
  the host supplies.

**What does not exist here.** There is no AppPort code in this workspace (it appears only in
architecture tests that forbid depending on it). AppPort capabilities can be integrated only once a
supported interface exists. No AppPort API is assumed or invented.

**Dependency direction.** `chip-experiments` depends on `chip-core`, `chip-inference` and
`chip-project`. No product crate depends on it; `scripts/audit-dependencies.sh` checks this.
In stack-agent, only `src/experiment.rs` (reached from the `experiment` subcommand in `main.rs`) uses
it. `tests/experiment_cli.rs` pins this, and `tests/product_path.rs` still forbids the crate in the
product modules.

## 2. Behavior plugins versus capability plugins

**`Strategy`** is a behavior plugin (`plugin.rs`). It has an id, a version and a description,
declares its extension points (`decision` and/or `context`), and supplies:
- a `LocalWorkPolicy` (`decisions()`);
- optionally an `EscalationContextPolicy` (`context()`; the default is `FullEscalationContext`).

It can only propose. It never gets an executor, the capability set, the model boundary, the spec's
completion predicates or the evidence store. A `Complete` it proposes is a claim:
- Chip refuses it if a required observation is unmet;
- the evaluator rejects it if a host criterion is not met (section 6).

**`CapabilityFixture`** is a capability plugin. It contributes one `CapabilityBackend` per run,
declares which capability ids it provides, and states what confines each one:
- `Pure`: no effect outside the call;
- `Root`: every effect is confined to the run's directory by the executor's own rules;
- `Unconfined`: runs a process, a network call or project code with the operator's permissions.

Chip's routing, input validation and loop authorization apply to it unchanged. Two fixtures that
provide the same id are refused when they are registered.

**Deferred extension points:**

- **Task decomposition.** Chip has no decomposition contract; planning exists only as next-step
  proposals.
- **Model selection.** The provider is chosen by `stack-agent`'s `provider_selection` from explicit
  configuration. Letting a plugin propose a model would mean changing that boundary, and
  experiments do not use real models yet.

## 3. Execution modes

Modes are typed (`definition::Mode`). Only modes with implemented semantics exist. `shadow` and
`canary` are reserved names; parsing refuses them explicitly, and every other unknown mode is
refused too.

**`observe`**
- *Runs:* the baseline only. It must be a strategy the host registered as production
  (`Registry::with_production_strategy`). Naming a candidate is refused.
- *Guarantees:* the strategy's decisions are unchanged, and nothing is compared. The run uses a
  disposable directory with only confined capabilities, as in isolated mode.
- *Does not do:* run `stack-agent work` itself. The run uses the experiment's criteria, not the
  production goal kind's completion predicate (which needs `pax.test`, an unconfined capability).

**`replay`**
- *Runs:* both sides, against the task's recorded capability results and recorded model replies.
- *Guarantees:* nothing executes. A request not in the recording fails, and nothing is substituted
  for it. No directory is created. A task without a recording, or a recording holding a capability
  outside the allowlist, is refused.
- *Does not do:* reproduce external effects, timing or a live model. Criteria that need a final
  project (`tree.*`) are refused in replay.

**`isolated`**
- *Runs:* each side in its own disposable directory under `--work-dir`, offering only `Pure` and
  `Root` capabilities. An `Unconfined` capability on the allowlist (for example `project.git.*`
  or `pax.test`) makes the definition refused (exit 3) instead of running.
- *Guarantees:* the project path rules refuse paths outside the directory. The observation
  invariants (path escape, out-of-root write, host-path leak) audit every observation. The
  directory is removed afterwards, and any failure to remove it is reported.
- *Does not do:* provide an operating-system sandbox. The process runs with the operator's
  permissions, and isolation rests on the offered capabilities' own confinement.

## 4. Experiment definition (`chip.experiment.v1`)

A definition is immutable data:

```json
{
  "schema": "chip.experiment.v1",
  "id": "reference.retries-isolated", "version": 1, "description": "…",
  "mode": "isolated",
  "baseline": {"id": "reference.read-then-write", "version": 1},
  "candidate": {"id": "reference.search-then-write", "version": 1},
  "tasks": [{"id": "retries.explicit", "version": 1}, {"id": "retries.worded", "version": 1}],
  "capabilities": ["project.read", "project.search", "project.write"],
  "limits": {"max_turns": 8, "max_executions": 6, "deadline_ms": 30000},
  "criteria": ["observed.expected-writes", "tree.expected-files", "tree.no-unexpected-changes"],
  "model": {"kind": "none"},
  "evidence": ["outcome", "criteria", "decisions", "executions", "model_calls", "duration", "tokens"]
}
```

**What parsing refuses** (`ExperimentDefinition::from_json`):
- the wrong schema, unknown fields, a reserved or unknown mode;
- limits outside 1–64 turns, 1–64 executions or 1–600000 ms;
- no tasks, duplicate tasks, an empty allowlist, no criteria;
- a missing or duplicate candidate where the mode needs one, and a candidate in observe;
- `cost` as required evidence (no provider here reports cost), and evidence that omits `outcome` or
  `criteria`.

**What resolution refuses** (`Registry::resolve`):
- unknown strategies, tasks, capabilities and criteria;
- version mismatches;
- an unconfined capability in observe or isolated mode;
- criteria that cannot be decided in the mode;
- fixtures that do not meet the input contract: recorded model replies the task does not have, a
  replay without a recording, unsafe or oversized project files.

Nothing invalid is replaced by a default. Every refusal has a stable `code()`, which the CLI prints
with `--json`.

## 5. Report format (`chip.experiment-report.v1`)

`report_json` produces the versioned report and `summary` a human-readable text; both are redacted.
The report contains:

- **`experiment`**: the id, the version, the full canonical definition and its `sha256` digest.
- **`provenance`**: the host program and version, `source_revision` (null; the binary does not
  embed one), and the `chip-experiments` version.
- **`mode`**: its guarantees, its limitations, and each allowlisted capability with its confinement.
- **`model`**: the kind only; no credentials are used.
- **`strategies`**: id, version, description, the `production` flag and extension points.
- **`tasks`**: id, version and fixture digest; **`criteria`**: each criterion's description.
- **`runs[]`**, one per task and side:
  - the verdict (section 6), terminal state, outcome reason and typed delivery;
  - per-criterion results;
  - decisions (`local: request project.read`, …), executions by capability, model calls;
  - tokens, only when the provider reported them (otherwise `available: false` with the reason);
  - cost, always `available: false`;
  - observed wall-clock duration, never compared;
  - the safety audit, evidence references, whether any observation named the host path, and the
    cleanup outcome.
- **`comparison`**: the conclusion, the winner (or null), verified counts, observed differences, and
  the basis ("verified task outcomes only").
- **`cleanup`** and **`limitations`**.

The report is an evidence artifact, not an authoritative record of external reality. This crate
does not store it; `--report FILE` writes one file, and that file is as durable as wherever it was
written.

## 6. Host-side verification

Criteria are host code (`criteria.rs`). They read the run's recorded observations and, in modes that
have one, the disposable project as the run left it. They never read a model's or a strategy's
description of itself.

- **`observed.expected-writes`**: the last successful `project.write` of each expected file wrote
  exactly the expected content (by `sha256`). It is also handed to Chip as a required observation,
  so Chip refuses a completion before it holds. It works in every mode; in replay the observations
  are recorded ones.
- **`tree.expected-files`**: the host reads each expected file after the run and compares it to
  the expected content.
- **`tree.no-unexpected-changes`**: no other project file was changed, added or removed.

The expected content lives in the task fixture (`expected_files`). Only criteria read it; a strategy
or model never sees it.

**Verdicts** (`runner::Verdict`), decided from the runtime's outcome, the criteria and the audit
only:

- **`verified`**: completed, every criterion met, the audit clean, and every required measurement
  present.
- **`completed_unverified`** (completed, grounded but not verified): the strategy claimed success and
  Chip accepted the completion, but a host criterion failed. The claim is rejected.
- **`incomplete`**: blocked, a limit was reached, or escalated.
- **`runtime_failure`**: a typed failure, with its delivery.
- **`cancelled`**: by request or by deadline, with termination `known`. It is `uncertain` when a
  model call was abandoned with an unconfirmed remote stop.
- **`evidence_invalid`**: a criterion could not be decided, the audit found violations, or a
  required measurement such as tokens is missing.
- **`not_run`**: for example, the experiment was cancelled first, or the environment could not be
  created. A missing result is never success.

**Comparison:**
- If any run of either side is `cancelled`, `evidence_invalid` or `not_run`, or is missing, the
  conclusion is `incomplete` and no winner is named.
- Otherwise the side that verified more tasks is named.
- With equal verified counts, the conclusion is `same_verified_outcomes`, or
  `different_tasks_verified` when the verified tasks differ.
- Decisions, execution counts and durations are reported as differences; they are never ranked.

## 7. Security, authorization, isolation and secrets

- **Authorization.** Chip's own loop authorizes every request. A capability off the allowlist is
  undeclared to the work, so Chip refuses it, and `Allowlisted` refuses it again before any backend.
  Tested: `a_capability_off_the_allowlist_is_never_executed` (0 executions, project unchanged).
- **Evidence-reuse rule.** Chip executes a locally proposed request on its own say-so only for a
  capability whose results may not be reused (`reuse_evidence: false`: state-changing or
  state-dependent, like every `project.*` capability). For any other capability, the model is
  asked. Strategies cannot override this.
- **Isolation.** `paths_outside_the_disposable_directory_are_refused_by_the_executor` tries
  `../outside.txt`, an absolute path and `../../etc/hostname`. Nothing is written outside the
  directory, the audit stays clean, no observation names the host path, and the directory is
  removed. Every run also records whether any observation contains the host root.
- **Secrets.** Experiments need no credentials. `stack-agent experiment` reads the credential
  variables (`CHIP_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `CHIP_PROXY_PASSWORD`) only to
  scrub their values from every output: errors, summary, JSON and the report file. Tested with
  sentinels in `secrets_never_reach_the_report_or_the_summary` (a strategy that leaks a sentinel
  into its block reason) and
  `experiment_cli.rs::a_secret_in_the_definition_never_reaches_any_output`.
- **No hidden switches.** `chip-experiments` reads no environment variable (pinned by a test).
  `work_is_unchanged_whatever_the_environment_says_about_experiments` runs `work` with and
  without experiment-looking variables and requires byte-identical results, timing fields aside.

## 8. Adding a plugin or an experiment

**A behavior strategy.** Implement `Strategy`, usually a `LocalWorkPolicy` wrapped in
`reference::DecisionStrategy`. Register it in the host's registry with `with_strategy`. In
stack-agent, that registry is `experiment::registry()`. Give it a new version whenever its behavior
changes: definitions pin versions.

**A capability fixture.** Implement `CapabilityFixture`. Be honest about confinement: anything that
runs a process or reaches the network is `Unconfined`. Its declarations must not touch the root,
because replay reads them without executing anything.

**A criterion.** Implement `Criterion` from observations and/or the final tree. Declare the modes in
which it can be decided. Return `Unavailable` rather than guessing.

**A task.** A `TaskFixture` holds the goal, the starting files, the host's `expected_files`, and
optionally recorded model replies and a recording. Generate a recording from real execution, as
`tests/recordings.rs` does. That test fails if a committed recording drifts from what
`ProjectExecutor` observes; regenerate with `--ignored`.

**An experiment.** Write a `chip.experiment.v1` document and run it with `--definition FILE`, or
register it with `with_experiment`. Validate it first with `stack-agent experiment validate`.

## 9. Reproducing the deterministic comparison

```sh
cargo build --release --bin stack-agent
./target/release/stack-agent experiment run reference.retries-isolated --report report.json
./target/release/stack-agent experiment run reference.retries-replay
./target/release/stack-agent experiment run stack-agent.observe-change
```

No key and no network are needed. Two runs of the same definition give the same verdicts, decisions,
executions and evidence digests; only durations differ. The isolated and replay runs give the same
verdicts and the same evidence digests (`replay_reaches_the_same_verdicts_from_the_recording_and_executes_nothing`).
Real-model runs are not deterministic, and the seam makes no such assumption.

**Recorded result**, against this commit's reference experiment:

| Task | Baseline (read-then-write) | Candidate (search-then-write) |
| --- | --- | --- |
| `retries.explicit` (notes say "retries must be 3") | verified, 3 executions | verified, 3 executions (different decisions: search, read, write) |
| `retries.worded` (notes say "retry count must be 4") | verified, 3 executions | incomplete: blocked, "searching for `retries` found no stated requirement", 1 execution |

The conclusion is `baseline_verifies_more` (2 of 2 against 1 of 2). The candidate's search-first
approach is just as cheap here and brittle to wording. It blocks rather than guess, so it is
incomplete, not wrong. `stack-agent.observe-change` (the production change policy with recorded
replies) is verified with 4 model calls and 3 executions.

## 10. Known limitations and unsupported modes

- **Not implemented:** `shadow` and `canary`. A future shadow mode must never execute real tools
  merely because it shares a context with the baseline.
- **No real models.** `ModelSpec` is `none` or `recorded`. A configured provider would bring
  credentials and nondeterminism into experiments and is the next step (below); recorded replies
  report no token usage.
- **Isolation** is capability confinement inside a directory, not an operating-system sandbox, so
  `pax.test` (project code) and Git cannot be offered in isolated or observe mode.
- **Observe** runs the production decision and context policies with the experiment's criteria,
  not the production goal kind's predicate.
- **Provenance** has no source revision: the binary embeds none.
- **Cancellation:** cancelling a run that is executing waits for that execution to finish (Chip's
  cooperative stop). Under `stack-agent experiment`, Ctrl-C cancels the remaining runs.
- **No persistence:** results exist in the report the caller writes, and nowhere else.

## 11. Evaluating and promoting a candidate

The seam never promotes anything. No registration, report or exit code changes a production
default, and there is no traffic shifting or canary. Promotion is a separate, reviewed change:

1. Write a definition that names the candidate and the current production strategy as the baseline,
   with criteria that express what the change is for. Run it in replay and isolated mode.
2. Require a conclusion that is not `incomplete`, and no verified task lost relative to the baseline.
   Read the differences and every `completed_unverified` run.
3. Record the report (`--report`) and the definition digest in the pull request.
4. Change the production code path explicitly (for example, `GoalKind::policy`) in its own pull
   request, with the product tests as evidence. The experiment registry keeps the old strategy, so
   the comparison can be re-run.

**The next experiment this enables:** compare `stack-agent.change@1` with a candidate change
policy, or a context policy such as `DeduplicatedEscalationContext` against
`FullEscalationContext`. Run it against recorded model replies captured from real `work` runs.
After that, add an explicitly configured real-model `ModelSpec`, gated like `live_smoke.rs`.
