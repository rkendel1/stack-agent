# P4: capability effect declarations and safe re-observation, measured

What P4 changes is described in
[`docs/product/agent-state.md`](../../../../../product/agent-state.md#capability-effects-and-safe-re-observation).
A capability declares `observation`, `consequential` or `unknown` (the default). An interrupted
request with no recorded observation is performed again, through the normal validated and durable
path, only if its capability declares `observation`. Anything else still stops with
`OutcomeUncertain` (exit 3), gives its lease back and waits for `agent-state settle`.

## Setup, and what is different from P1 to P3

* **Host:** macOS arm64. P1 to P3 ran on Linux with `gdb`. **There is no `gdb` here**, so the
  commit-boundary kills use `lldb_kill.py`, which counts `fcntl(F_FULLFSYNC)` on the state-db file
  (FeltDB's sync on macOS). `W1a`, `W1a-model`, `W1b`, `W2` kill through the fake `pax` and the fake
  model as before. The four `torn` windows (`W4c`, `W4-torn-obs`, `W4-torn-outcome`,
  `W4-torn-final`) need `write()` counting and **were not run** (`not_run` in the raw logs).
  These numbers are not comparable with the Linux numbers in the P1 to P3 reports; the comparison
  below is P3 and P4 on this host.
* **Binaries:** before = release build of `main` at `d90c097` (P3); after = release build of this
  branch at `40ecf74`. The same harness (this directory's `../harness.py`) drove both.
* **Both harness runs were concurrent on one machine**, so wall times are indicative only.
* Kills are real `SIGKILL`s of the real release binary. The ledger (fake `pax`) is the oracle for
  `pax.test`. `project.read` is Chip's own executor and has no ledger; the state-db is the oracle
  there (the intent is committed and the observation is not, shown by `history_at_kill`).

## 1. Crash harness (E2)

Two windows are new. `W-read`: SIGKILL after `record_intent(project.read)` (commit 4) is durable
and before its observation. `W-read-unsynced`: SIGKILL at entry of that commit's sync.

| Window | Kill inside | P3: resume | P3: operator steps | P4: resume | P4: operator steps | Charges (both) | Final |
| --- | --- | --- | --- | --- | --- | --- | --- |
| W-read | project.read, intent durable | exit 3 `OutcomeUncertain` | 1 | **exit 0**, read observed again | **0** | 1 | completed |
| W-read-unsynced | project.read, intent unsynced | exit 3 | 1 | **exit 0** | **0** | 1 | completed |
| W2 | pax.test, after the effect | exit 3 | 1 | exit 3 | 1 | 1 | blocked (settled `executed`) |
| W1b | pax.test, before the effect | exit 3 | 1 | exit 3 | 1 | 1 | completed |
| W4d | pax.test intent unsynced | exit 3 | 1 | exit 3 | 1 | 1 | completed |
| W1a, W1a-model, W3, W4a, W4b | (no operator needed) | identical | 0 | identical | 0 | 1 | completed |
| W2-default-lease, W2-refusal-default-lease | | identical | | identical | | 1 | exit 3 as before |

* **Observation-only recovery success:** 2 of 2 windows. Operator settlements for them: 1 each
  before, **0** after.
* **Operator settlements elsewhere:** unchanged (`W2`, `W1b`, `W4d`: 1 each).
* **Duplicate consequential effects:** 0 before, 0 after. The ledger holds 1 `pax.test` line in
  every window.
* **Refusals for unknown or consequential declarations:** the same 3 windows refuse, before and
  after (`W2`, `W1b`, `W4d`, all exit 3 with the lease released).
* **`W-read` after (from `after-p4/logs/e2_raw.json`):** the resumed process is attempt 3, exits 0,
  makes 1 model call (for `pax.test`) and 1 execution (`pax.test`). `agent_state.reobserved`
  names `project.read`, the settled intent `a2.e2-task-exec-1` and the new execution
  `e2-task-reobs-3-0`. FeltDB holds: the old intent resolved with no observation of its own, the
  new intent `project.read … reobserves=a2.e2-task-exec-1 …`, and no settlement.
* **Recovery latency of the resume process:** `W-read` 182 ms and `W-read-unsynced` 187 ms after,
  against 48 ms and 39 ms for the refused resume before. They are not comparable: the refusal does
  nothing, and the P4 process does the rest of the work (the test run and completion). Before, the
  operator step and a second resume followed.
* **Model calls and executions:** `W-read` before: 2 model calls and 2 executions
  (`project.read`, `pax.test`) in the resume after settlement; after: 1 and 1 in the resume (the
  fresh read is the re-observation, not a model decision). That is not evidence of anything about
  effect safety.

### E1 (unchanged)

Both: 1 charge, 2 model calls, 2 executions; both invalid-argument runs exit 1 with 0 ledger lines.

## 2. Durability bench (`stack-agent --durability-bench --repeats 20`)

Every cell is identical before and after, except the median wall time. The bench declares no
effects, so every interrupted action stays uncertain, as before. This is what "retains its expected
behavior for consequential operations" means here; it says nothing about observation capabilities,
which the bench does not exercise. `bench/before-p3` and `bench/after-p4`.

## 3. Tests

Run on this host (macOS arm64, `pax` 0.4.1 on `PATH`), at the code of `40ecf74`:

* `cargo fmt --all -- --check`: clean.
* `cargo test --workspace --locked --no-fail-fast`: **1843 passed, 4 failed.**
* The 4 failures are **identical on the P3 baseline `d90c097`** on this host, so they are not caused
  by P4. They are `chip-pax pax_boundary::a_file_held_open_for_writing_cannot_be_executed`,
  `fx-native-provider tls_boundaries::a_certificate_for_another_host_is_refused_even_when_its_ca_is_trusted`,
  `stack-agent --lib service::tests::malformed_requests_are_refused_and_start_nothing`, and
  `stack-agent verify::the_standalone_path_needs_no_compute_and_no_ambient_tooling_beyond_pax_and_cargo`.
  I did not investigate their cause.
* New: `chip-agent-state/tests/effects.rs` (13, real FeltDB), `agent_state_cli.rs` (+3, the real
  binary), `capability_surface.rs` (+1), `chip-remote-env/tests/protocol.rs` (+3),
  `chip-core/tests/invocation_boundary.rs` (+1). Existing P1, P2 and P3 tests pass.
* The unit-level crashes in `effects.rs` and `agent_state_cli.rs` are an execution that never
  returns, followed by a real fresh process in the CLI tests. The real `SIGKILL` evidence is the
  harness above.

### Mapping to the required tests

| # | Where |
| --- | --- |
| 1, 2, 3, 4, 5 | `protocol.rs`: `a_capability_declares_its_effect…`, `a_missing_or_unrecognized_effect…`, `a_descriptor_built_without_a_declaration_is_unknown` |
| 6 | `invocation_boundary.rs`: `a_model_cannot_declare_what_a_capability_does_to_the_world` |
| 7, 20 | `effects.rs` read recovery; `agent_state_cli.rs` read; harness `W-read` |
| 8, 9, 20 | `effects.rs` consequential; `agent_state_cli.rs` `pax.test`; harness `W2` |
| 10 | `effects.rs` no declaration or unknown; `agent_state_cli.rs` `project.list` |
| 11, 12 | `effects.rs` `a_recorded_request_that_cannot_be_reconstructed_exactly…` (8 cases), `…no_longer_accepts…`, `recovery_re_validates…` |
| 13 | `effects.rs` `a_changed_read_result_is_the_fresh_observation…` |
| 14 | `effects.rs` `a_consequential_capability_that_deduplicates…`, `an_observation_that_carries_the_key…` |
| 15 | `effects.rs` `an_interrupted_consequential_action…` (settlement not restored) |
| 16 to 19 | existing suites, unchanged and passing; the bench above |

## 4. Findings and limits

* **The contract cannot say "re-observed".** `reconcile` accepts only `executed` or `not_executed`.
  The old intent is reconciled `not_executed`. That choice neither credits the lost execution with
  an effect nor claims it ran, but it is not a fact about the lost execution. The records carry the
  distinction in the new intent's `reobserves=` marker and the old intent's citation of a different
  execution's observation. Please confirm you accept this; the alternative is `executed`, which
  claims more.
* **chip-core's event changed.** `ObservationRestored` carries `reobserved`, and
  `RestoredObservation` has a `reobserved` field, because the existing event would otherwise have
  called a freshly executed observation "restored". This is the only chip-core change beyond the
  descriptor field and the extracted `validate_request`.
* **The re-observation runs outside the work's cancellation and deadline control**, once per open
  observation intent (in practice one).
* **The declaration is trusted, not checked**, like `deduplicated`. A wrong `observation` would make
  recovery repeat a write. Only `project.read` declares it, after reading its implementation.
  `project.list`, `project.search` and `project.git.*` read only in their descriptions and declare
  nothing, so they still stop. Declaring them is a separate, evidence-based step (for example,
  `git status` can refresh the index).
* **A failed re-observation is not retried.** The next attempt tries once more.
* **Not run:** the torn-write windows, any Linux/gdb run, and the E5 overhead measurement.
