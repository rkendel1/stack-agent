# Project and runtime learning

Every `stack-agent work` run records what it learned, in two separate scopes:

- **Project learning** lives in the project.
- **Runtime learning** lives with this installation of stack-agent.

Later runs retrieve applicable, re-validated behaviors from both scopes before they decide.

**A run document is a historical record.** Writing one never promotes its conclusion to trusted
behavior. A behavior becomes trusted only when its referenced run documents meet the promotion
rules below, and that is checked again every time it is read.

Learning reuses what already exists and adds no new execution path:
- the automatic-configuration outcome record (`autoconfig::evidence::OutcomeRecord`);
- its host classification;
- the existing resolver (`autoconfig::resolve::decide`) and its evidence seam.

Learning adds no model call, no second loop, no new registry, verifier or crate, and no network
access.

## 1. The two scopes

| | Project scope | Runtime scope |
| --- | --- | --- |
| **Location** | `<project>/.stack-agent/learning/runs/` and `.../behaviors/` | `<data>/runtimes/<installation>/learning/runs/` and `.../behaviors/` |
| **Identity** | `<project>/.stack-agent/project.json`: a random `proj_…` id, created on the first write. It survives renaming or moving the directory and is not derived from the directory name. | `<installation>`: the first 16 hex characters of the `sha256` of the executable's canonical path. `installation.json` holds a private salt. |
| **What a run document holds** | The host-classified outcome, starting context (including the profile digest), the actions taken, the outcome record with references to the test-result observations, and lessons, including project facts (the last test result, refused evidence, failed executions) | Generic facts only: task kind, build system, configuration, outcome, capability counts and runtime version. The outcome record has no verification references and no selection id. There is no project id; a salted project token counts distinct projects without naming any. |
| **Who can read it** | stack-agent only. `.stack-agent` is reserved by the project path rules, so no capability can list, search, read or write it, and a model cannot forge records. | stack-agent only |
| **Promotion** | Validated: at least 2 verified successes, no contradicting outcome, every reference valid, support no older than 180 days, and the same catalog version | As project, and the successes must come from **at least 2 distinct projects** on the same runtime `major.minor` line |
| **Effect on `--auto`** | Records of validated, contested and rejected behaviors (failures matter as much as successes) | Records of validated behaviors only |

**Where `<data>` is:**
1. `$STACK_AGENT_DATA_DIR`, the documented override for tests and isolated environments;
2. else `$XDG_DATA_HOME/stack-agent`;
3. else `$HOME/.local/share/stack-agent`.

It must be an absolute path. It is never the executable's directory and never the project. With
none of these set, runtime learning is reported as unavailable.

**Isolation by construction:**
- The two stores have different roots.
- Each document and behavior carries its scope, and a store refuses a document of the other scope.
- A runtime document carrying a project id, a profile digest or verification references is
  invalid.
- A project document must carry this project's id; documents of another project are reported and
  not counted.

Tests check that project B inherits none of project A's lessons, and that no runtime file appears
in the project or project file in the runtime store.

## 2. Formats

**Run document** (`stack-agent.learning.run.v1`, at most 16 KiB). Fields:
- `id`: `sha256:` of the document with this field empty. The file is named after it.
- `scope`, `run`, `runtime_version`, `task_kind`, `project` / `project_token`.
- `starting_context`: build system, tests present, Git repository, verification, profile version,
  and the profile digest (project scope only).
- `actions`: capabilities invoked, model calls, decisions, failed executions.
- `outcome` and `terminal_state`.
- `outcome_record` (`null` for a refusal that never ran).
- `lessons`, each a `fact` or a `hypothesis` with evidence ids.
- `evidence`.
- `applicability`: task kind, build system, configuration (or `baseline`), catalog version, runtime
  line, `recorded_at`, `valid_days`.
- `limitations`.
- `behaviors`: the ids this run updated.

**Behavior** (`stack-agent.learning.behavior.v1`). Fields:
- `id`: `sha256:` of the scope and key, so it is stable.
- `key`: task kind, build system, configuration.
- `proposal`: advisory text.
- `catalog_version`, `runtime_line`.
- `references`: up to 32 `{document, path: "runs/<hex>.json"}`.
- `recorded_status`, `recorded_support`, `recorded_contradiction`: informational only.
- `updated_at`, `limitations`.

**Never stored:** the goal's text, file contents, command output, a host path or a credential.
Tests scan every learning file for a sentinel key, the goal text, written source and the project's
absolute path.

## 3. Lifecycle of a run

1. **Locate both stores.** Profile the project; this is the same file-only profile `--auto` uses.
2. **Retrieve.** Load at most 64 behavior files per scope and keep those that apply (same task kind
   and build system). Re-validate each against the run documents it references. Order them
   deterministically (validated, contested, candidate, stale, rejected, then id) and keep at most
   16.
3. **Under `--auto` only:** the outcome records of qualifying behaviors join the `--selection-evidence`
   snapshot as input to `decide`. Duplicate records are counted once: the scorer dedupes by record
   id, and a runtime record of a run the project already contributed is dropped. Without `--auto`,
   behaviors are reported and select nothing.
4. **Run** through the same `WorkRuntime::run_with`, Chip loop, FX boundary and verification as
   before. Learning adds no model request; tests check the request counts.
5. **Classify** with `OutcomeClass::of`, from the work's verified result, terminal state and audit.
   A model's claim of success is never a verified outcome: a test where the model claims
   completion records `blocked`.
6. **Write** one run document per scope, after the outcome and exit status are fixed.
7. **Update behaviors** only when the outcome bears on success (`verified_success`,
   `verified_failure`, `unverified`).
   - Before each update, the behavior is re-read, so a concurrent writer's reference is kept.
   - A behavior file that fails validation is reported and left as it is, never overwritten.
   - Blocked, cancelled and runtime-failure runs produce documents but change no behavior.
8. **Report.** `--json` gains `learning`: per scope, what was retrieved (status, support,
   contradiction, distinct projects, references, problems), diagnostics, and what was written
   (`persisted`, path or error). Human output gains a "Learning" section.

`--auto` refusals are runs too: they get a `blocked` document with no outcome record.

## 4. Validation, staleness and conflict

The status is recomputed on every read; the status recorded in the file is never trusted. A test
writes `validated` into a file backed by one success and gets `candidate`.

| Status | When |
| --- | --- |
| **stale** | Another catalog version or (runtime) runtime line, or the newest support is older than 180 days |
| **contested** | At least one verified success and at least one verified failure or unverified outcome. Conflict is visible, never overwritten. |
| **rejected** | No support and at least 3 contradictions |
| **validated** | The promotion rule of its scope (section 1) holds and no reference is missing |
| **candidate** | Anything else, including a single success, missing references or foreign documents |

**References** are document ids that map only to `runs/<64 hex>.json`. Symbolic links, oversized
files, wrong names and other scopes' documents are refused, so a record can never redirect a read
outside its store.

A tampered document no longer matches its id: it is ignored with a diagnostic ("altered or
forged"), and the behavior falls back to `candidate`. Invalid files are ignored with a visible
diagnostic; nothing is quarantined or deleted.

Failed and unverified outcomes are diagnostic lessons and contradiction counts. They never count as
success. Hypotheses are labelled and never count as evidence.

## 5. What learning can and cannot do

A learned behavior can only add outcome records to the existing `--auto` ranking, which orders
configurations that are already eligible. It cannot:

- change which capabilities are offered, or required and prohibited rules;
- override a hard constraint (applicability, stage, isolation, requirements, policy);
- bypass authorization, cancellation, deadlines, delivery certainty or verification;
- turn experiment-origin evidence into production evidence (learning records only `work` runs,
  which are production-origin);
- turn an unverified completion into a success;
- select a configuration without `--auto`.

Lessons are **not** injected into model prompts in this version. The evidence the decision used is
named in the selection record (`evidence_used`).

## 6. Persistence, failures and version control

- **What is persisted:**
  - project run documents, behaviors and `project.json`, inside the project;
  - runtime run documents, behaviors and `installation.json`, in the runtime data directory.
- **What is not:** sessions, turns, event logs and work state. Known issue C still holds for those.
- **Writes** are atomic (temporary file, then rename). If any step fails, the scope's
  `written.persisted` is `false` and its error says "not persisted".
  - Causes include a read-only project, `.stack-agent` being a file, a full disk, a missing or
    unwritable data directory, or an invalid `project.json` (which is never overwritten).
  - The task's outcome and exit status are unaffected; a test checks both scopes failing on a
    verified run, which still exits 0.
- **Version control:** stack-agent never stages, commits or ignores anything, and it never edits
  `.gitignore` or project configuration.
  - `.stack-agent/` appears as an untracked directory.
  - Commit it to share project learning with collaborators (records hold no secrets or source), or
    add it to `.gitignore` to keep it local. Either is the project's decision.
- **Retention and cleanup:** nothing is deleted automatically.
  - A behavior keeps its 32 most recent references; older run documents stay as history.
  - Deleting `.stack-agent/learning/`, or `<data>/runtimes/<installation>/`, resets that scope.
  - Deleting `.stack-agent/project.json` gives the project a new identity, and its old documents
    are then reported as foreign.

## 7. Coverage and limitations

- **Commands:** only `work` records and retrieves learning. `verify`, `serve`, the one-shot prompt
  and `experiment` do not (experiments keep their own report and evidence format).
- **Lessons** are deterministic summaries of host evidence; no model writes or synthesizes them. A
  behavior's proposal concerns a configuration (or the baseline) for a task kind and build system.
- **Generalization.** Runtime behaviors generalize only to configurations the catalog has, and only
  after successes in two distinct projects. No cross-project evaluation beyond that rule has been
  run, so no claim is made that any runtime lesson generalizes.
- **Concurrency.** Two concurrent runs in one project merge behavior references on write, but there
  is no lock: a race can still drop one reference. The run documents themselves are never lost.
- **Size.** Retrieval is bounded (64 behavior files and 16 retrieved per scope; 16 KiB per
  document), so beyond that, behaviors are not examined, and the diagnostic says so.
- **Tests** that do not set `STACK_AGENT_DATA_DIR` write runtime learning under the test machine's
  `HOME`. The shared test helper sets a private directory per command.

**Future extensions:**
- prompt-side use of validated lessons, behind the same advisory rules;
- `serve` integration;
- an experiment-backed revalidation command for stale runtime behaviors (through the existing
  experiment seam);
- an explicit retention policy.
