# Native-skill experiment: architecture

**Status: experimental infrastructure. No model trial has been run.** This is the seam, the reviewed
artifacts, the tests and the runner for the experiment pre-registered in
[`native-skills-experiment/PREREGISTRATION.md`](native-skills-experiment/PREREGISTRATION.md). It
does not ship native skills. `stack-agent work`, `verify` and `serve` behave exactly as before, and
[`release-limitations.md`](release-limitations.md) still lists skills as unsupported, because they
are. The design this implements is [`native-skills.md`](native-skills.md). Deviations from it are in
§6.

## 1. What exists, verified against the code

| Question | Answer | Where |
| --- | --- | --- |
| How a real-model benchmark run is launched | `stack-agent --agent-bench` runs each trial as a **child process**, `stack-agent work <goal> --kind … --max-turns … --json --print-reply --no-state`. The child gets a cleared environment (only `PATH`, `HOME`, the toolchain variables and `CHIP_PROVIDER`/`CHIP_MODEL`/`CHIP_ENDPOINT`), in a fresh fixture, with no retries. The executable is `current_exe()` | `agent_bench.rs`: `work_args` (531), `run_work` (584), `run_trial` (1104), `Config.exe` from `current_exe` (1626) |
| What decides a task-E trial | `judge` (854): completed, `verified`, `src/lib.rs` changed, **the harness's own `cargo test --offline`** passes (`run_project_tests`, 994), no other file changed, and a clean audit. The model's claim is never an input | `agent_bench.rs` |
| What one request carries | `EscalationContextPolicy::build` is a pure function of `WorkState` and `WorkTrajectory`. `render_instructions()` (434) sends `Goal: … \nQuestion: …` first, unchanged across turns. `ContextMetrics` (401) counts the bytes actually sent | `chip-core/src/work.rs` (trait at 539) |
| Which policy `work` uses | `run_software_work_durable` (1251) picks `RunOverrides.context` when `--auto` selected a configuration, else `InspectCoveredContext`, else `default_context_policy(kind)` (1161). For `change` that is `DeduplicatedEscalationContext` | `software_work.rs` |
| `GuidedContext` | Appends a guidance text to `context.goal` under a label naming the capability source. It is reachable only through `chip-experiments`, whose runs have **no real model**: `ModelSpec` is `None` or `Recorded` | `chip-experiments/src/plugin.rs` (209), `definition.rs` (82) |
| Experimental selection | Autoconfig refuses a configuration with `sources` unless it is `Experimental`, and `Policy::work()` provides none. So `work` can never select an experimental context | `autoconfig/catalog.rs`, `autoconfig/resolve.rs` (48) |
| Recording | Child `work --json` report (model calls, per-call tokens including cache reads, executions by capability, verified, audit); trial record by `run_trial`; FeltDB only when the durable arm is used (`--state-db`); learning writes to the project and to `STACK_AGENT_DATA_DIR`, and is **not** injected into prompts (`learning.md`) | `agent_bench.rs`, `software_work.rs` `render_json` (1806) |

**What this means.** No existing path could run all three arms on the same real-model path:

- the agent bench always launches `work`, and `work` has no context hook;
- `--auto` cannot select an experimental configuration;
- the experiment layer has no real model.

A mechanism that works in replay (`GuidedContext`) is not available to real-model runs.

## 2. The seam

**Injection point:** the escalation-context policy, wrapped in `run_software_work_durable`, after
the existing policy choice (`software_work.rs` 1315):

```text
context_policy = <the policy work already chose>
if instructions is Some:  context_policy = WithInstructions { inner: context_policy, instructions }
```

`WithInstructions` (`software_work.rs` 1141) calls `inner.build` and appends
`ADDED_INSTRUCTIONS_LABEL` (1099) and the text to `context.goal`. Nothing else changes: state,
evidence, observations, prior decisions, ruled-out, omitted, condensed, the question, and the
policy `id()`. In the rendered request, the text sits after Chip's own goal suffix (the capability
and completion rules `goal_text_for_contract` adds through `spec_for_contract`) and immediately
before `\nQuestion: `, in the first system message. That message is the stable cached prefix.

**The hook is inert and lives in the product module; the experiment depends on it, never the
reverse.** `tests/product_path.rs` forbids `software_work.rs` from naming an experiment module. The
first implementation did name one, and the guard caught it. The guard was kept, and the dependency
was inverted:

- **`AddedInstructions`** (1106): a text plus its provenance. It can be constructed only inside the
  crate (`pub(crate) fn new`). `skill_experiment` makes one only after a pin check.
- **`InstructionSource`** (1127): `select(kind, build_systems, offered) -> Result<AddedInstructions,
  String>`. It can only refuse; it cannot add to `offered`. `skill_experiment::ArmInstructions`
  implements it.

**How an arm reaches it.** This is the minimum change to production code:

- `work(args)` is now `work_with_experiment(args, None)` (2647). The body is unchanged apart from
  one block (3107). That block runs only when the argument is `Some`: it calls `select` after the
  capability set is final and before durable state and the first model call. On refusal it prints
  `… nothing was run` and exits 3.
- `WorkRuntime` has a private `instructions` field. It is always `None` from `prepare`.
- `run_software_work_durable` takes one more `Option`. Its only callers pass `None`
  (`run_software_work_controlled`) or the runtime's field.
- `declared()` is `pub(crate)`, so the skill validator checks names against the real capability
  list.

No flag, environment variable, configuration or catalog entry can produce `Some`. The only code that
does is `examples/skill_experiment.rs`, an example, which the product binary does not contain.

**Why it cannot do more:**

| It cannot… | Because |
| --- | --- |
| add or remove capabilities | It runs after `work_capabilities` has built the set, and it receives no handle to the set. `select` reads the offered ids and can only **refuse** the work when a named capability is missing |
| change capability descriptions, inputs, forms or effects | Those are in `question`, built by `ModelDecisionBoundary::question` from descriptors. The wrapper returns `inner`'s `question` unchanged. A test asserts this byte for byte |
| bypass decision validation or authorization | `ModelDecisionBoundary::interpret`, the capability checks and `Run::perform` are untouched. The policy only produces text |
| execute commands or scripts | `EscalationContextPolicy` has no executor. The loader treats every file as bytes. Any undeclared file (a script, a binary) refuses the whole skill |
| modify FeltDB or evidence | The policy has no store handle. Trials run with `--no-state` in every arm |
| change `GoalKind` completion | Completion reads `spec`, observations and `pax.test`, never `context.goal`. A test shows a skill-worded `complete` is refused in every arm |
| suppress failures or alter evaluator results | The evaluator is `agent_bench::judge` and `run_project_tests`, unchanged, run by the parent after the child exits |

## 3. The artifacts and their integrity

| Artifact | Path | Pinned in |
| --- | --- | --- |
| Skill `repo.diagnose-failing-test@1` | [`native-skills/examples/diagnose-failing-test/`](native-skills/examples/diagnose-failing-test/) | `REVIEWED_SKILLS` in `skill_experiment.rs` |
| Length control `control.length-v1` | [`native-skills/controls/length-control-v1.md`](native-skills/controls/length-control-v1.md) | `LENGTH_CONTROL` in `skill_experiment.rs` |

**Digest construction (defined before coding).** The digest is
`SHA-256( domain ‖ for each file in ascending byte order of its relative path: path ‖ 0x00 ‖
u64-be length ‖ bytes )`, written `sha256:<hex>`:

- A skill hashes `skill.json`, its instruction file and **every** declared reference, under the
  domain `chip.skill.v1\n`.
- The control hashes its one file, by file name, under `chip.skill.v1.control\n`.
- Paths are `/`-separated, relative and UTF-8.
- Nothing else is hashed: no modes, no timestamps, no directory names.

The pins were computed by the Rust implementation and independently by a Python reimplementation
of the same construction, and the two agree. A test recomputes the pin from the specification.

**Trust.** The expected digest lives in reviewed source. A skill cannot supply its own. Changing one
byte of any file, or the version, makes the skill unusable until review updates the pin (tested
per file).

**Validation** (`load_skill_dir`, fail-closed, first problem reported):

- **Manifest.** Strict JSON (no comments, no trailing commas, no duplicate members), at most
  4,096 bytes. Unknown members are refused at every level. `inputs`, `outputs`, `permissions`,
  `effects`, `scripts`, `commands`, `executable`, `tools` and `hooks` are refused by name, even
  when empty.
- **Fields.** Schema `chip.skill.v1`; an id of the form `seg.seg…`; version ≥ 1; a one-line
  description; known kinds and build systems; capability ids that a real capability declares, no
  duplicates.
- **Paths.** Relative `.md` paths with no `.` or `..`, no absolute, drive or backslash forms, no
  duplicates.
- **Files.** Exactly the declared files: no undeclared file, no symbolic link (file, directory or
  root), regular files only. UTF-8 with no control characters except newline and tab.
- **Content.** At most 4,096 bytes of instructions and references together, and a
  `## Verification` section.

**Measured context cost** (`skill_experiment check`, task E: `change`, Cargo):

| Arm | Added text | Label | Total added to every request |
| --- | --- | --- | --- |
| baseline | 0 | 0 | 0 |
| skill | 2,416 bytes (instructions plus `refs/rust.md`) | 151 | 2,567 bytes |
| length control | 2,413 bytes | 151 | 2,564 bytes |

For scale: [`small-model-reliability`](small-model-reliability/README.md) measured a first task-A
request at 9.9 KB, and on its 3B model 73–76% of input tokens were served from llama.cpp's prefix
cache. Token cost per arm is a recorded outcome, not an assumption.

## 4. Tests

[`crates/stack-agent/tests/skill_experiment.rs`](../../crates/stack-agent/tests/skill_experiment.rs)
has 18 tests. None needs PAX or a model.

| Requirement | Test |
| --- | --- |
| Invalid or mismatched skills refused before any model call | `every_file_change_invalidates_the_pin`, `malformed_or_oversized_manifests_fail_closed`. The loader runs before `work_with_experiment` is called, and `select` runs before durable state and the first model call (smoke-checked: the refusal prints `nothing was run`) |
| A skill cannot introduce an absent capability | `a_skill_cannot_require_or_add_a_capability_the_work_does_not_offer`, plus `capability` in the malformed cases |
| A skill cannot change inputs, forms or authority | `the_seam_appends_to_the_goal_and_changes_nothing_else`, `every_arm_runs_the_same_work_and_differs_only_in_the_added_text` (every request byte-identical except the inserted text) |
| Text is inert, never an observation or evidence | the same two tests: executions, observations and rejections are identical across arms |
| No skill-provided command, script or executable runs | `undeclared_files_and_scripts_are_refused`. In the end-to-end test a scripted `shell.exec` and an invented `command` input are refused in every arm, and the project is unchanged |
| Digest mismatch prevents use | `every_file_change_invalidates_the_pin`, `a_changed_length_control_is_refused`, `the_digest_is_the_documented_construction` |
| Invalid paths and symbolic links | `paths_outside_the_skill_or_of_the_wrong_kind_are_refused`, `symbolic_links_are_refused` |
| Oversized or malformed manifests | `malformed_or_oversized_manifests_fail_closed` (23 cases), `members_that_would_carry_authority_are_refused_even_when_empty` |
| Instructions cannot satisfy completion | end-to-end: a `complete` quoting the skill is `completion refused` in every arm; nothing completes and nothing is verified |
| Baseline byte-equivalence | `the_baseline_arm_is_the_existing_context_construction`: the existing entry point and the experiment path with `None` send identical requests. The only normalized field is the temporary project path, which differs per directory |
| Same model configuration, capabilities, limits and evaluator | `every_arm_gets_the_same_command_line_limits_and_model_settings` (the arm is not an input to the command line), `the_schedule_interleaves_the_arms_evenly`. The evaluator is one function called by one runner |

## 5. The runner

`cargo run --release -p stack-agent --example skill_experiment -- <check | plan | run-schedule>`.

- `check` and `plan` make no model call.
- `run-schedule` spends inference: **not authorized by this change**.

What `run-schedule` does:

- It refuses to start when the checkout has tracked changes, when `--out` is not new, or when any
  pin fails.
- It writes `plan.json`: commit, fixture hashes, the exact `work` arguments, provider and model,
  arm provenance, the schedule, and the PAX, cargo and git versions.
- It runs each scheduled trial through the **unchanged** `agent_bench::run_trial`. Only `exe` is
  set, to `DIR/bin/skill-arm-<arm>`.

That launcher runs the work child with a per-trial `STACK_AGENT_DATA_DIR` and keeps these under
`DIR/artifacts/<trial>/`:

- `report.json` (the complete `work --json` report)
- `stderr.log` (every model reply)
- `exit.txt`
- `agent.diff`
- `before.json` and `after.json` (file hashes)

Artifacts are never overwritten. After each trial, the runner adds the arm, round, position,
provenance and infrastructure classification to the bench record and writes it to `DIR/trials/`.
It **stops** on any unauthorized execution, attempted unauthorized action, or completion or
verification that the oracle does not confirm. The trace is kept. No trial is retried.

**No-inference smoke check (run here).** The endpoint was a closed local port, so no model could
answer.

| Run | Result |
| --- | --- |
| Skill arm on an `inspect` work | `does not apply … nothing was run` (exit 3) |
| Length-control arm | printed its provenance, then failed at the connection, as expected |
| Skill arm on `change` without PAX | `PAX unavailable … nothing was run` |

All artifact files were written.

## 6. Deviations from the design proposal

1. **The label names no id or digest.** `GuidedContext`'s label names the source. Here the skill and
   control arms share one label, so the text is the only difference. Provenance is recorded by the
   runner and on stderr, not sent to the model.
2. **`WithInstructions` keeps `inner`'s policy id.** Nothing in a report distinguishes arms by id;
   the runner records the arm.
3. **Guidance is attached through `work_with_experiment`, not a `Strategy`.** Strategies run only in
   `chip-experiments`, which has no real model (mismatch 2 of the design, resolved differently).
4. **The control is a pinned text file, not a skill.** It has no applicability or capabilities. It
   goes through the same path checks, size and UTF-8 checks, digest and label.
5. **Selected text is the instruction file plus each matching reference, in manifest order, joined
   by a newline.** The 4,096-byte bound covers all of a skill's texts, loaded or not.
6. **`project.git.diff_stat` was added to the skill's capabilities** for step 6 ("inspect the
   relevant diff"). It is a declared read-only capability, so the skill remains eligible only where
   it is offered.
7. **A production hook** (§2), which the design deferred to "the implementation". It is inert, it
   changes no behavior when `None`, and the tests above check that. It is placed so that
   `product_path`'s rule (no experiment module on the product path) still holds.

## 7. Unresolved questions

- **Time to first useful edit is not measurable from existing records.** `work --json` has no
  per-turn timestamps, and `--print-reply` prints replies at the end. The turn index is
  recoverable from the replies (PREREGISTRATION §6). Elapsed time would need either the durable
  arm (FeltDB records) in every arm, or new instrumentation. Both are separate decisions.
- **Near-determinism.** At temperature 0, repeated trials of the 3B model are close to identical
  (small-model-reliability, "Limitations"). Repetitions may then measure server nondeterminism, not
  sampling variance. The plan reports distinct trajectories per arm. A nonzero temperature would be
  a separately pre-registered experiment.
- **The llama.cpp prefix cache persists across trials** on one server. It affects latency and
  cached-token counts. Whether it affects outputs at temperature 0 is unverified. The plan records
  it rather than restarting the server per trial; restarting is an alternative to decide before
  running.
- **The full SHA-256 of the model file** used in the earlier runs is recorded only as `724fb256…`.
  It must be recorded in full before inference.
- **PAX and the model server are not installed here,** so none of this was exercised end to end
  with real PAX or a model.
