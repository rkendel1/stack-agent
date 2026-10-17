# Automatic configuration (`work --auto`)

With `stack-agent work --auto "<goal>"`, the user still describes only the work. stack-agent then:

1. profiles the project;
2. selects one of its built-in configurations under policy and hard constraints, ranked by
   recorded verified outcomes;
3. runs the work through the same Chip loop, FX boundary and verification as always;
4. returns a selection record and an outcome record.

The feature is opt-in. Without `--auto`, nothing in this document runs and `work` behaves exactly
as before. There are no recipe commands: users never name, select or manage a configuration.

## 1. What a configuration can change, and what it cannot

A configuration (`src/autoconfig/catalog.rs`, `catalog.v1`) chooses among parts the binary already
has:

- **Decision policy:** the goal kind's production decision policy (unchanged: `--kind` is still
  the user's).
- **Context policy:** one of Chip's existing escalation-context policies: deduplicated (what
  `work` uses for change and verify), full (chip-core's default), or inspection progress (what
  `work` uses for inspect).
- **Capabilities:** which of the local environment's existing capabilities are offered. The
  environment's own set is filtered through `chip_core::AllowlistedBackend`, so a configuration
  can only narrow it, never widen it.
- **Limits:** its turn and execution limits, used only when the user gave none (today they equal
  `work`'s defaults).

It cannot change the loop, the model boundary, authorization, cancellation, the deadline, the
completion predicates or verification. There is no second execution path: a selected configuration
becomes a `RunOverrides` value passed to the same `WorkRuntime::run_with` that plain `work` calls
with `None`.

**The built-in catalog:**

| Configuration | Stage | Applies to | Context | Capability rules |
| --- | --- | --- | --- | --- |
| `change.cargo-verified@1` (rank 0) | production | change, Cargo | deduplicated | **required:** read, write, `pax.test`; **preferred:** list, search, `project.observe`, Git |
| `change.cargo-verified-full-context@1` (rank 1) | production | change, Cargo | full | as above |
| `verify.cargo@1` | production | verify, Cargo | deduplicated | **required:** read, `pax.test`; **prohibited:** write |
| `inspect.read-only@1` | production | inspect, any | inspection progress | **required:** read; **prohibited:** write, `pax.test` |
| `change.cargo-verified-full-context-isolated@1` | experimental, isolated | change, Cargo | full | as the change rows; `work` never selects it |

**Capability states.**
- *Required:* without it the configuration is ineligible.
- *Preferred:* offered when available.
- *Prohibited:* withheld even when available.
- *Unavailable* is a fact about the environment, not part of the catalog. `pax.test` and
  `project.observe` need PAX; `project.observe` covers Cargo only; Git capabilities need a Git
  repository and `git` on `PATH`.

`catalog::validate` refuses an invalid definition (and a test keeps the built-in catalog valid):
- a duplicate id, an unknown capability, or a capability stated twice;
- no required capability, or limits outside 1–64;
- the inspection-progress context with a kind other than inspect;
- verify or inspect without prohibiting writing, or change without requiring `pax.test`;
- Cargo test verification declared for anything other than Cargo.

## 2. Profiling

`ProjectProfile::collect` (`profile.v1`) records only these sources:
- the build manifests at the project root;
- the names of files and directories under it (bounded to 2,000 entries; contents are never read);
- `.git`;
- whether `pax` and `git` resolve on `PATH` (lookups, never runs);
- whether a model is selected.

It runs no project code, no build tool (not even `cargo metadata`) and no network request.

**Attributes:**
- Every attribute is `known` (`observed`, or `inferred` with the rule named), `ambiguous` (with the
  candidates) or `unknown` (with the reason).
- Every attribute cites the observations that support it.
- `network` is always `unknown`: it is never probed.

The facts that matter most:

- **PAX establishes test results only for Cargo.** For npm, pnpm, yarn, bun and Python it runs the
  tool and reports `unsupported`. So verification is `pax_cargo` for a Cargo project with PAX,
  `pax_uninterpreted` for other build systems, and `none` without PAX.
- **Conflicting manifests** (for example `Cargo.toml` and `package.json`) make the build system
  `ambiguous` and verification `unknown`. PAX's own selection, at run time, decides which tool
  runs; the profile does not guess.

The profile authorizes nothing. It is an input to selection, and its `sha256` digest is recorded.
The same inputs and version give the same profile, byte for byte.

## 3. Policy, selection and learning: three separate things

**Policy** (`resolve::Policy`) is what the command may use, whatever the project:
- `work` allows production configurations only;
- the local environment is the user's shared directory, so isolated configurations are refused;
- a caller-forbidden capability list (empty for `work`).

**Selection** (`resolve::decide`, `score.v1`) is a fixed pipeline:

1. **Inputs:** the profile, the goal kind (the task), the environment facts and the policy.
2. **Hard constraints.** Applicability (kind, build system), stage, isolation, required
   capabilities and their availability, policy prohibitions, and requirements (a model; Cargo test
   verification). Any failure makes a configuration ineligible. A fact the profile cannot decide
   makes it *undecidable*, not eligible.
3. **Compatibility findings** are recorded for each configuration.
4. **Evidence** is gathered for eligible configurations only.
5. **Score**, from production outcome records for the same configuration and task kind:
   - weighted by relevance: 1 for the same build system, ½ when unknown, and not used for a
     different one;
   - weighted by freshness: the weight halves every 90 days, and records older than 365 days are
     stale;
   - `score = (successes + 1) / (successes + failures + unverified/2 + 2)`, applied only with at
     least 3 weighted records; below that the configuration is ranked at the neutral 0.5.
6. **Choice.** Order by effective score, then the catalog's documented `default_rank`, then the
   configuration key (byte order). The first is selected.
7. **Record.** The decision records every assessment, the evidence ids used, the day, and the
   catalog, scorer and profile versions. Its id is the digest of all of that.

**What counts as evidence:**

| Outcome | Effect on the score |
| --- | --- |
| `verified_success` | counts for |
| `verified_failure` (tests did not pass after the change, or evidence refused as tampered or stale) | counts against |
| `unverified` (completed or stopped with no verification evidence) | counts half against: missing verification is not success |
| `blocked`, `cancelled`, `runtime_failure`, `insufficient_evidence` | not counted; listed as ignored, with the reason |

- **No evidence is not success.** It is neutral, and the decision is marked `evidence_limited`.
- **Duplicates count once.** A record supplied twice is counted once (by id).
- **Snapshot order does not matter.** Records are sorted before summing, so any order of the same
  snapshot gives the same decision.

**Outcomes of a decision:**

- **Selected:** run with that configuration. Confidence is `high` when the choice rests on
  enough evidence, and `evidence_limited` when the default order decided.
- **Blocked:** no applicable configuration is eligible. Nothing runs (exit 3, no model call), and
  the reasons are printed. Examples: `change` on an npm project (PAX could never verify it), or
  `change` without PAX. A missing required capability is never worked around.
- **Fallback:** eligibility is undecidable (ambiguous build system) or the catalog is invalid. The
  existing, unconfigured behavior runs unchanged, and the reason is printed.

**Hard constraints cannot be bought.** An ineligible configuration is not even scored. Tests check
this both ways: 10 recorded successes for the experimental configuration do not select it, and 10
recorded successes on an npm project do not unblock `change` there.

**Learning** is nothing more than the next selection reading outcome records. There is no model
training and no remote optimization.

## 4. Evidence records, privacy and persistence

**Selection record.** This is the decision itself, `record: "selection"`.

**Outcome record** (`record: "outcome"`, produced after the run). It contains:
- the selection id, the configuration, `origin: production`, a unique `run` id, and `recorded_at`;
- coarse features: the task kind and the build system;
- the outcome class, computed by the host from the work's verified result, its terminal state and
  the audit (never by the configuration that ran);
- the terminal state, delivery and cancellation (including an unconfirmed remote stop);
- the capabilities actually invoked;
- `sha256` references to the observations that carried test results;
- duration, and tokens only when reported; cost is always `null`, because no provider here reports
  it;
- limitations.

A record's id is the digest of its content, and a snapshot whose records do not match their ids is
refused.

**Privacy.** Records carry no goal text, no file content, no path, no prompt and no credential.
Tests check this with a sentinel key and a sentinel inside the goal.

**Persistence.** stack-agent stores none of these records. There is no durable store for agent
state in this binary (known issue C), and this PR adds none: no database, file or cache.
- `work --auto --json` returns both records in its report (`automatic_configuration`).
- An operator who keeps the outcome records can pass a snapshot back with
  `--selection-evidence FILE` (a JSON array or JSON lines). It is read, validated exactly, and
  never written.
- `resolve::decide` takes an `EvidenceSource`, the seam a durable authority would implement later.

**Why two installations with the same binary can choose differently.** The catalog, scorer and
profiler are versioned and identical. But selection also depends on:
- the project (its profile);
- the environment (PAX, Git, the model);
- the snapshot an operator supplies;
- the day, which sets freshness.

Every one of these is recorded in the decision, so a decision can be explained, and reproduced by
feeding the same profile and snapshot on the same day.

## 5. Experiments inform people, not the production policy

Each change configuration is registered as an experiment strategy, `config.<id>`: the production
change policy with the configuration's context policy. `stack-agent experiment run
autoconfig.change-context` compares the two production change configurations:
- on the same task, with the same acceptance criteria and recorded model replies;
- isolated, with host verification;
- the reference result is `same_verified_outcomes` (both verified).

The experiment report includes `selection_evidence`: outcome records in the same format, with
`origin: experiment`. The production scorer never counts experiment-origin records; they are listed
as ignored. A test feeds ten experiment successes for the full-context configuration into
`work --auto` and checks that the decision does not move.

To act on an experiment, change the catalog (for example a `default_rank`) in a reviewed pull
request. Nothing promotes a configuration automatically, and `work` never selects an experimental
one.

## 6. Diagnostics

- **Human output** adds an "Automatic configuration" section: the detected project facts with their
  sources; the selected configuration with its confidence and rationale; what is offered, withheld
  and preferred but unavailable; each ineligible configuration and why; the evidence used; and the
  outcome and its record id.
- **`--json`** adds `automatic_configuration`: `profile`, `profile_digest`, `decision`,
  `outcome_record` and `persistence`. A blocked run prints the same section with `"ran": false`.

## 7. Limitations

- **Commands.** Only `work` supports `--auto`; `serve` and `verify` do not yet.
- **Selection inputs.** Selection uses the goal kind, never the goal's words, so it cannot tell a
  feature from a bug fix.
- **Profiling** reads root manifests only. Nested projects, monorepos and tests inline in source
  files are not seen. An ambiguous build system falls back to the existing behavior.
- **Scoring** counts verified outcomes only. Latency is recorded but not scored, and cost is never
  available.
- **Evidence** is as durable as wherever the operator keeps the JSON. stack-agent persists nothing.
- **Isolation.** The only isolated environment is the experiment runner's disposable directory, so
  the isolated configuration exists only for experiments.
- **Inspect wording.** The inspect goal text still lists `project.write` among the project
  capabilities, but under `inspect.read-only` it is not declared, so a request for it is refused by
  Chip.
