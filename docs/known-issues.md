# Known issues and explicit limitations

These gaps were carried forward into stack-agent when Chip and FX were consolidated. The
consolidation moved code; it did not close any of them. The "harden runtime contracts" change
(2026-10-17) then closed or reduced A, B, D, E, F and G. Each section says what is now supported,
what proves it, and what remains. Each item was checked against the code. Its status is one of **unresolved**, **partially addressed** or **verified fixed**,
with source references, test evidence and a follow-up. Paths are relative to the repository root.
The tests named here run in CI unless the entry says otherwise.

External references (in `rkendel1/fx` at `b2d11d7`, not copied here):
`docs/rebuild/ZIG_RETIREMENT.md`, `docs/rebuild/ASK_CHIP_ROUTING.md`,
`docs/rebuild/parity/agent-loop.v1.json`. Chip's own register of the same gaps, with roadmap IDs, is
[`product/technical-debt-register.md`](product/technical-debt-register.md) (cited below as `D-nn`).

| | Area | Status |
| --- | --- | --- |
| A | Streaming and CLI contract | One tested exit contract for every command; typed timeout classification; `serve` streams events by polling. The CLI does not stream |
| B | Cancellation | Partially addressed: one stop path (service, Ctrl-C/SIGTERM, whole-work deadline) exercised end to end through the binary. Executions in flight are not interrupted; remote stops are reported as unconfirmed |
| C | Persistence, replay, idempotency | Partially addressed: every `work` and `serve` work is persisted in FeltDB by default, events with provenance and the host's verdict included, and a restart reports interrupted work. Nothing resumes on its own; no exactly-once in general |
| D | Typed delivery certainty | Fixed and aligned with FX's contract (an HTTP response is `confirmed_response`); exposed in `work`, `verify` and `serve` output |
| E | Completion and verification | Strengthened: replaced test files, changes after the passing run, and contradictory or skipped-only PAX results do not verify. Feature acceptance deferred |
| F | Credentials and transport | Deterministic proxy, CA, TLS and redaction gaps closed. Opt-in live provider and live proxy smoke tests passed once (no authenticating corporate proxy tried) |
| G | WASM | The `fx-host` and `chip-wasm-host` session modules and the decision module are executed under `wasmi` in tests (host-played provider only); no WASI, Wasmtime, Node/V8 or in-Wasm provider bridge |
| H | Performance | One recorded real-model run (Vercel AI Gateway, 20 samples, reports committed); not a budget; time to first token unmeasured |
| I | Legacy (Zig) retirement | Separate task; not done; no parity is claimed |
| J | Project files: what a model may read, write and be told | Partially addressed: default credential deny-list, protected execution and instruction surfaces, descriptor-based confinement on Unix, a cross-process project lock, and a prompt-injection corpus. Verifying a repository still runs its own code (HA-01) |

---

## A. Streaming and CLI contract (exit contract fixed; serve streams events; the CLI does not)

**One exit contract.** `crates/stack-agent/src/verify.rs` `exit_code` is the single mapping. It is
used by `work` (`SoftwareWork::exit_status`) and `verify` (`Verification::exit_status`). It also
produces the per-work `exit_status` that `serve` reports. The one-shot prompt uses it through
`exit_for_model_failure`.

| Code | Meaning |
| --- | --- |
| 0 | Completed, and an independent predicate verified the outcome. A one-shot prompt turn that completed is also 0; that path makes no verification claim |
| 1 | The runtime ran correctly but nothing was verified: blocked, a limit, escalated, cancelled (by request or by the deadline), a grounded inspection, or completed on evidence that was then not accepted (E) |
| 2 | Usage, from each command's own argument parser |
| 3 | A required dependency was unavailable and nothing was done |
| 4 | Any other failure, including a safety-invariant violation, a model that answered but broke the protocol, and any model failure after work had begun |

Exit 3 is decided before the loop: no model selected, no PAX, no environment. After that it applies
only when the first model call failed before anything executed. Even then it is decided from the
typed FX failure kind (`ModelFailure::dependency_unavailable`), never from text.

- These kinds count as unavailable: `capability_unavailable`, `model_unavailable`,
  `provider_unavailable`, `unauthenticated`, `forbidden`, `rate_limited` and `configuration`.
- So does `transport` with no HTTP status, as when a connection is refused.
- `protocol_violation`, `timeout` and `cancelled` are runtime failures (4), not "unavailable".

**Behavior changes:**

- The one-shot prompt used to exit 1 for every error. It now exits 3 for an unavailable dependency
  and 4 otherwise.
- `work` used to exit 3 for any failed first call. A protocol violation (for example a
  non-streaming stand-in) now exits 4, with the report printed.
- `serve` process codes (2, 3, 4) are unchanged.

Test evidence:

- `crates/stack-agent/tests/exit_codes.rs`: tables for `work` (11 cases), `verify` (6), the
  one-shot prompt (5) and `serve` process codes (3), all through the binary.
- `work.rs` and `verify.rs` (unchanged expectations).

**Typed provider errors (fixed).**

- A body-read failure counts as a timeout only from its type: `ureq::Error::Timeout`, or an I/O
  error of kind `TimedOut` (`crates/fx-native-provider/src/provider.rs` `read_failure_kind`). The
  connect/send path matches the same types.
- The error text is no longer inspected. Redaction and delivery certainty are applied
  independently of the category.
- Tests:
  - `provider.rs` unit tests `a_typed_ureq_timeout_is_a_timeout` and
    `a_failure_whose_text_mentions_timeout_is_not_a_timeout`
  - `conformance.rs`: `a_stalled_stream_times_out_with_its_partial_output_and_a_confirmed_response`
    (a genuine timeout), `http_errors_keep_status_and_retry_after_and_are_confirmed_responses`,
    `a_stream_cut_before_its_terminal_marker_keeps_the_partial_and_is_not_a_completion` and
    `cancellation_in_flight_returns_promptly_with_partial_output_and_is_unconfirmed`
- No retry or fallback exists anywhere.

**Event streaming for `serve` (fixed, by polling).** `GET /v1/work/{id}/events` now returns the
events the loop has recorded so far while the work runs.

- **Mechanism.** chip-core's `WorkControl::on_event` hands each event to the run's `RunControl` as
  it is recorded. This is the same sequence the finished report keeps.
- **Cursor.** `?after=N` returns only events after the first N; `next` is the cursor for the next
  poll, and `complete` becomes true with the terminal event. This is an additive, compatible
  extension: without `after`, the reply is the full list as before.
- **Not provided.** Nothing is pushed, and nothing is replayed after the process exits.
- **Retention.** It is bounded as before: by the work's own limits, then `--max-retained-work`.
- Tests:
  - `crates/stack-agent/tests/cancellation.rs::serve_shows_events_while_the_work_runs_then_its_terminal_result`
    sees an intermediate event (the read's observation) before completion, then the terminal
    result. It also checks that the finished trajectory starts with exactly what was shown.
  - `service.rs::post_is_prompt_the_id_is_chips_and_the_loop_runs_in_the_background`

**Streaming of the provider call.**

- The native FX provider always requests and normalizes an event stream.
- A 2xx reply that is not a stream is refused as `protocol_violation`.
- Regression for the old stand-in mismatch (chip-rs 2c22fd8):
  `product.rs::a_non_streaming_stand_in_is_refused_explicitly_not_accepted`, which now expects
  exit 4. CI also runs the packaged smoke test with an event-stream stand-in.

**Remaining:**

- The CLI (`work`, `verify`, the one-shot prompt) still prints one report at the end. It does not
  stream to its caller.
- A request for a nonexistent capability still exits 4, like a safety-invariant violation (D-17).
- Time to first token is not measured (H).

## B. Cancellation (partially addressed: one stop path, exercised end to end; executions are not interrupted)

**One stop path.** The service's cancel request, Ctrl-C or SIGTERM to `stack-agent work`, and the
whole-work deadline all set the run's `RunControl` (`crates/stack-agent/src/software_work.rs`). It
is the run's chip-core `WorkControl`.

- **Before every model call and before every execution** (`crates/chip-core/src/work.rs`
  `Run::stopped`), the loop asks it whether to stop. So neither a cancellation nor the deadline can
  be extended by starting another call or another tool.
- **A model call in flight** gets the same stop through `chip_fx::with_call_cancel`. FX abandons it
  at its next poll and reports what it knows:
  - its delivery certainty, which is never `definitely_unsent` once the call was made;
  - its remote state (`not_started`, `unconfirmed` or `confirmed`), now carried as
    `remote_cancellation`.
- **The outcome** is `WorkOutcome::Cancelled { cause: cancelled | deadline, delivery, remote }`,
  with terminal state `cancelled`, a `WorkCancelled` event, and exit 1. It is never reported as a
  completion.

**Request is not termination.**

- `POST /v1/work/{id}/cancel` answers `cancellation_requested` with `termination: "unconfirmed"`.
  The work keeps running until it reaches its next boundary.
- The native provider cannot interrupt a blocking read, so the remote stop of an abandoned call is
  `unconfirmed`. Its worker thread ends at its next read or at its own deadline.
- Nothing undoes a provider request that may already have been sent, or a tool effect that already
  happened.

**Deadline.** `stack-agent work --deadline-secs N` (from start) and
`stack-agent serve --work-deadline-secs N` (from admission) accept 1 to 86400 seconds. When it
passes, the loop stops at the next boundary with cause `deadline`.

**Signals (Unix).**

- The first SIGINT or SIGTERM requests cancellation and lets the work report what it established.
- A second one exits at once with 130 and reports nothing; it says so on stderr.
- On other platforms only Ctrl-C is handled.

**Executions are not interrupted (intentional).**

- An execution in flight finishes, is observed and reported, and its effect stays done. There is no
  thread abort.
- PAX has its own timeout. A capability with no timeout of its own can run past the deadline; the
  work ends right after it.

**Tests through the binary** (`crates/stack-agent/tests/cancellation.rs`), using gated stand-ins
rather than sleeps:

- **Before admission:** `queued_work_cancelled_before_admission_never_reaches_the_model`.
- **During an active stream:**
  `cancelling_during_an_active_stream_abandons_the_call_with_its_delivery_and_retries_nothing`. It
  checks `confirmed_response`, `remote_cancellation: unconfirmed`, one provider call before and
  after the stream is released, a repeated cancel, and 409 once finished.
- **While a tool is pending:**
  `cancelling_while_a_tool_runs_lets_it_finish_reports_it_and_starts_nothing_else`.
- **Deadline:** `the_service_deadline_stops_a_stalled_work_and_says_why` and
  `the_cli_deadline_bounds_the_whole_work`.
- **Ctrl-C:** `ctrl_c_during_inference_stops_the_work_and_reports_what_happened` and
  `ctrl_c_while_a_tool_is_pending_lets_it_finish_and_reports_it`.
- **Usage:** `a_bad_deadline_is_a_usage_error`.

**Before dispatch, and the loop's rules**, in `crates/chip-core/tests/work_control.rs`:

- a stop requested before the work starts calls nothing;
- a stop that arrives while the model answers starts no execution;
- the deadline cannot be extended;
- an execution in flight finishes and is observed;
- an abandoned call keeps its delivery and remote state;
- the owner sees every event in order.

**After completion:** a cancel is refused with 409 (the service tests and `cancellation.rs`).

**Remaining:**

- No interruption of an execution in flight, and no confirmed termination of a blocked provider
  read.
- On Windows only Ctrl-C is handled.
- The deadline is checked at boundaries, so a single long execution can exceed it.

## C. Persistence, replay and idempotency (partially addressed, 2026-10-20)

- **Every work is persisted in FeltDB by default** (`docs/product/agent-state.md`), in `work` and
  in `serve`, unless `--no-state`:
  - its intents, observations and result (as `--state-db` did before, opt-in);
  - every event of its trajectory as it happens (`chip.WorkEvent`), each marked `host_observed`,
    `model_inferred` or `runtime_decided`;
  - the host's verification verdict beside Chip's outcome (`chip.work_result.v2`).
  - Each work has its own id, `work_<16 hex>`, in `work` and `serve` alike. Before, every CLI work
    was `work`, so its execution ids repeated from run to run.
  - The database is the project's own under the runtime data directory, or `--state-db FILE`. The
    default one is a record: if it cannot hold the work, the work runs and says so, and its exit
    status is unchanged. A database that was asked for fails closed (exit 3).
  - Evidence: `crates/chip-agent-state/tests/work_events.rs`; through the binary,
    `crates/stack-agent/tests/durable_events.rs`, which SIGKILLs `work` and `serve` mid-run and
    restarts them over the same database.
- **`serve` still keeps live work in memory**, bounded by `--max-retained-work` (default 256), and
  evicted work answers 410 while it runs. A work it does not hold (evicted, or run by an earlier
  process) is answered from FeltDB: `persisted: true`, its status (`interrupted` for a task still
  `running`), result, verdict and events (audit HA-18).
- `crates/stack-agent/tests/credential_redaction.rs` checks that no file left under a fresh
  `HOME`/`XDG_*` or the project during `work`, `serve` and the prompt path contains the key.
- **Learning documents are persisted** (`work`, 2026-10-19; `docs/product/learning.md`).
  - Each `work` run writes a run document under `<project>/.stack-agent/learning/` and another
    under the runtime data directory (`$STACK_AGENT_DATA_DIR`, else `$XDG_DATA_HOME/stack-agent`,
    else `$HOME/.local/share/stack-agent`), with the behaviors they update.
  - These are historical records and advisory evidence for `work --auto` ranking. They are not a
    session, turn or event log, and nothing is resumed or replayed from them.
  - A failed write is reported as not persisted and does not change the run's outcome.
- **Automatic-configuration outcome records** are kept inside those learning documents. The
  selection and outcome records are also returned in the `--json` report, and records kept from it
  can still be passed back with `--selection-evidence`
  (`docs/product/automatic-configuration.md`, section 4).
- **Durable writes that do happen are project state, not agent state:**
  - project files, written by temp file plus rename (`crates/chip-project/src/lib.rs`). A crash
    can leave `.chip-write-*.tmp` behind; the next work to take the project lock removes them and
    reports their paths (D-05).
  - the exclusive OS lock a work holds on its project for its whole run
    (`chip_project::ProjectLock`): `flock` on the project's root directory on Unix, which creates
    nothing; `<project>/.stack-agent/work.lock` elsewhere. A second `stack-agent` process on the same directory
    exits 3 before anything runs (audit HA-04). The OS releases the lock when the process exits,
    however it exits.
  - `.chip/graph/` from `init`/`graph`.
  - build artefacts produced by the project's own tests.
- **Durable authority for work state: FeltDB** (`chip-agent-state`). The owner decision is made:
  FeltDB holds work state; `chip-session` stays the reader and writer of Zig fx conversation
  sessions (`docs/product/session-format.md`) and is not wired into the binary. Learning documents
  are not work state and are not used to resume anything.
- **Nothing is resumed or replayed on its own.** A new run is a new task, even after an
  interrupted one: it reports the interrupted work (`note: work <id> was interrupted ...`, and
  `agent_state.interrupted`) so it does not start blind (audit HA-16), and starts from the goal.
  Only `--task-id <id>` resumes a task, through FeltDB's recovery (an execution whose outcome is
  unknown blocks it until an operator settles it), and only once the dead process's lease (900 s)
  has expired. Effects may still repeat. This is the "RETRYING A TURN MAY REPEAT EXTERNAL EFFECTS"
  warning that the README, `--help`, `work`'s usage text and `docs/product/release-limitations.md`
  all carry. `crates/stack-agent/tests/release_notices.rs` keeps the warning in place.
- **Evidence for the library-level contracts:**
  - `crates/chip-turns/tests/turns.rs` (`a_crash_before_the_watermark_leaves_no_turn_and_nothing_is_run_again`,
    `a_persistence_failure_is_reported_as_not_committed_and_the_work_is_not_repeated`, and others)
  - `crates/chip-session/tests/writer.rs`

**Not claimed:** automatic resumption after a crash, automatic replay, exactly-once side effects in
general (only deduplicating capabilities, `agent-state.md`), pruning of the default database.

**Follow-up:** a forced takeover of an interrupted task (contract v1 has none, so a resume waits for
the lease), and retention for the default database.

## D. Typed delivery certainty (fixed and aligned with the FX contract)

FX's contract defines `ConfirmedResponse` as "a response from the provider was observed"
(`crates/fx-core/src/contract.rs`). The single-attempt port defines `OutputObserved` as "a response
or stream chunk arrived" (`crates/fx-single-attempt/src/port.rs`).

The native provider now follows both: a non-2xx response, or a 2xx that is not an event stream, is
recorded as observed output, so it reports `confirmed_response` (`provider.rs` `exchange`). These
remain failures, never completions: an observed HTTP status and body are not model output.

Before this change they were `possibly_sent`. The tests that pinned that were updated together with
the contract wording:

- `conformance.rs::http_errors_keep_status_and_retry_after_and_are_confirmed_responses`
- `anthropic.rs::http_errors_keep_status_and_retry_after_are_confirmed_responses_and_not_retried`
- `conformance.rs::a_two_hundred_that_is_not_an_event_stream_is_a_protocol_violation`
- `chip-fx-native native.rs::an_http_error_keeps_status_retry_after_and_delivery_and_is_not_retried`
- `stack-agent fx_executor.rs::a_failure_carries_its_delivery_certainty`
- `http_provider_path.rs::provider_failure_surfaces_as_agent_error_without_secret`

**Still true:**

- **Never regresses.** `fx-core` `delivery_certainty_never_regresses` and chip-inference
  `delivery_only_moves_toward_sent`.
- **Never read from text.** It is always a typed field, mapped in `chip-fx/src/mapping.rs`.
- **No retry or fallback.** For example
  `chip-fx integrated.rs::an_ambiguous_delivery_does_not_cause_a_duplicate_provider_call`.
- **Cancellation after dispatch is never unsent.** See
  `integrated.rs::cancellation_after_invocation_is_never_reported_as_unsent` and
  `work_control.rs::a_model_call_abandoned_by_the_stop_keeps_its_delivery_certainty`.
- **Refused connections stay `possibly_sent`.** A connection refused after the call started is
  conservatively `possibly_sent` (`a_refused_connection_is_conservatively_possibly_sent`). The
  single-attempt adapter treats a started call as possibly sent, and proving "nothing left" would
  need a different adapter design. Only a refusal before the call
  (`cancellation_before_dispatch_never_opens_a_connection`, unsupported requests) is
  `definitely_unsent`.
- **Model delivery is not tool certainty.** Model-request delivery is never used as tool-side
  certainty. Tool effects are reported by their own execution events and observations.

**Where it appears:**

- `work --json` and the service result: `delivery`.
- `verify --json`: `delivery`, new in this change.
- The `WorkFailed` and `WorkCancelled` events, which also carry `remote_cancellation`.
- A failure unrelated to inference carries `null`; no value is invented.

Serialization tests:

- `product.rs::a_failed_model_call_reports_typed_delivery_and_is_not_retried` (CLI, HTTP 500, then
  `confirmed_response`)
- `product.rs::a_non_streaming_stand_in_is_refused_explicitly_not_accepted`
- `service.rs::a_failed_model_call_reports_its_delivery_as_a_field_not_only_text`
- `cancellation.rs` (the service's `WorkCancelled` event)

## E. Completion and verification (strengthened; feature acceptance deferred)

**Unchanged rule.** A model's claim never establishes completion; host-observed evidence does
(`chip-core` `completion_refused` / `answer_refused`; `software_work.rs` `GoalKind::verified`).

**Test tampering (partially addressed).** Both checks run when the result is accepted
(`WorkRuntime::run`, where the project root is known: CLI `work`, and `serve` over the local
environment). They do not run for an embedder's environments or for the library entry points. A
work refused here ends not verified, with `evidence.invalidated` saying why (exit 1).

- **Replaced baseline tests.** Verification is refused if the work replaced a test file that the
  repository tracks (`software_work.rs` `tampered_test_files`; one `git ls-files` call).
  - Outside a Git work tree, or without `git`, every replaced test file counts, conservatively.
  - A test file the task itself added and later refines, untracked, is allowed. The coding-agent
    ladder relies on this when a later tier refines the test an earlier tier wrote.
  - Test files are recognized by path convention (`is_test_path`): `tests/`, `test/`, `benches/`,
    `__tests__/` and `spec/` directories, `*_test.*`, `*_tests.*`, `*_spec.*`, `test_*.py`,
    `*.test.*` and `*.spec.*`.
  - The report lists every replaced test file in `evidence.test_files_replaced`.
- **Changes after the passing run.** chip-pax records the project's fingerprint as the test run
  left it (`project_fingerprint`). It covers every file except `.git` and `target`. At acceptance
  it is recomputed (`stale_test_evidence`); a change made outside any observed write after the
  passing run is refused.
- **Not detected:**
  - tests inline in source files (`#[cfg(test)]`), and weakened test configuration outside test
    paths;
  - a change made and undone between run and acceptance;
  - anything when the root is unknown (embedder environments, library entry points);
  - anything outside the observation boundary that leaves the tree unchanged.

  Nothing here prevents arbitrary malicious behavior.

**PAX evidence accounting (fixed).**

- A `passed` result whose counts show a failed test, or no passed test (skipped or ignored only),
  is not passing evidence (`PaxExecutionResult::is_passing_evidence`, used by `PaxTestPassed`).
- The report's `pax.last_tests` carries PAX's counts, with `complete: false` unless the run passed.
  A red `cargo test` stops at the first failing target, so its counts are partial.

**Inspection.** A grounded inspection stays unverified (exit 1). No host-defined verification
procedure for answers exists (D-12).

**Feature acceptance (deferred, D-11 / D-38).** A host-supplied acceptance predicate would need a
new request field, its validation, and a product decision on who may supply it. It was not invented
here. Work therefore completes on the first verified test pass.

**Tests:**

- `crates/stack-agent/tests/completion_evidence.rs`:
  - `a_passing_run_after_replacing_an_existing_test_file_verifies_nothing` (a committed test file)
  - `adding_a_new_test_file_is_not_tampering`
  - the coding-agent harness: `weakening_deleting_or_ignoring_a_failing_test_makes_the_suite_green_but_is_not_verified`
    (the acceptance check finds the weakened baseline test) and the ladder's task-created test being
    refined without being flagged
  - `failed_and_skipped_only_results_are_not_passing_evidence`
  - `a_completion_claim_without_test_evidence_is_refused`
  - `a_project_changed_after_the_passing_run_invalidates_the_evidence` (the real PAX executor and
    the acceptance check)
- `crates/chip-pax/tests/evidence.rs` (5 tests)
- Model claims and grounded inspection: `product.rs`.

## F. Credentials and transport (partially addressed)

**Credentials**

- Keys come from `CHIP_API_KEY` or `OPENAI_API_KEY` / `ANTHROPIC_API_KEY`
  (`crates/stack-agent/src/lib.rs`, `crates/chip-fx-native/src/credentials.rs`).
- The native worker scrubs the exact key, and the proxy password, from every message it produces.
  It also truncates messages (`crates/fx-native-provider/src/provider.rs:251-271`).
- `crates/fx-core/src/redact.rs` adds pattern masking, before diagnostic text is truncated. Since
  2026-10-20 it follows Beacon's telemetry redaction rules: any label ending in a secret word
  (`*_KEY=`, `*_TOKEN:`, `"password": ...`, `client_secret = ...`, the generic rule the oracle port
  lacked), credential flags (`--password x`), `Authorization` schemes, the password in any URL's
  userinfo, PEM private-key blocks, the shapes of common provider tokens (OpenAI/Anthropic, Stripe,
  GitHub, GitLab, Slack, Google, npm, Hugging Face, AWS, JWT), and every other copy of a value it
  masked. The corpus is `crates/fx-core/tests/fixtures/redaction.json`, including look-alikes that
  must stay readable (`max_tokens=4096`, `token == x`).

**Redaction regression**

`crates/stack-agent/tests/credential_redaction.rs` is new and runs as its own CI step. The sentinel
`sk-SENTINEL-…` is set as every key variable. The tests assert two things:

1. The provider received the sentinel as its credential.
2. The sentinel is absent from:
   - stdout and stderr of `work` (text, `--json`, `--print-reply`)
   - the one-shot prompt path
   - `serve`'s work state, event log and its own stdout/stderr
   - every file left in the project or in a fresh `HOME`

These checks cover success and also providers that echo the key back in a 401 or 500 error body.
Observed output for that case is `HTTP 401: Incorrect API key provided: [redacted]`.

Existing tests:

- the `sk-*-never-printed` keys in `work.rs`, `verify.rs`, `serve.rs`, `pax_work.rs`,
  `native_consumers.rs`
- `crates/fx-native-provider/tests/conformance.rs::a_credential_echoed_by_the_provider_never_reaches_errors_events_or_diagnostics`
- `provider_selection.rs::the_api_key_never_appears_in_the_resolved_configuration_text`
- transport diagnostics: `crates/fx-native-provider/tests/transport_boundaries.rs` (an endpoint answering
  garbage that quotes the key; a proxy echoing its credentials in a 407/502/garbage reply) and the TLS
  refusals in `tests/tls_boundaries.rs`; through the binary, `crates/stack-agent/tests/transport_boundaries.rs`
  (key and proxy password absent from output and files)
- real provider, opt-in: `crates/stack-agent/tests/live_smoke.rs` (`#[ignore]`, `STACK_AGENT_LIVE_PROVIDER=1`);
  run once on 2026-10-17 against Vercel AI Gateway (`nvidia/nemotron-3.5-lightning-free`): passed

Not covered: crash output, anything an embedder logs itself.

**Custom root CA**

- `SSL_CERT_FILE` replaces the bundled roots (`crates/stack-agent/src/provider_selection.rs:212`).
- An unreadable, empty or certificate-less bundle is an error naming `SSL_CERT_FILE`, never a silent
  fallback.
- Tests:
  - `provider_selection.rs::an_unreadable_ca_bundle_is_an_error_naming_the_variable_never_a_silent_fallback`
  - `crates/fx-native-provider/tests/private_ca.rs`. These need `openssl` and print `SKIPPED`
    without it, unless `FX_REQUIRE_TLS_TESTS=1`. CI sets that variable, so the tests must run.
  - `crates/fx-native-provider/tests/tls_boundaries.rs`: self-signed, wrong-CA, bundled-roots and wrong-host
    servers refused directly and through a proxy; success through an authenticating proxy (same `openssl` rule).
  - `crates/stack-agent/tests/transport_boundaries.rs`: the real binary with `SSL_CERT_FILE`, directly and via
    `CHIP_PROXY`, and refusals with other tools' "insecure" variables set (same `openssl` rule).

**Proxy**

- Only an explicit CONNECT proxy is supported: `CHIP_PROXY`, `CHIP_PROXY_USERNAME` /
  `CHIP_PROXY_PASSWORD` (basic auth), and `CHIP_NO_PROXY` (`provider_selection.rs:235-290`).
- Ambient `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY` are deliberately ignored.
- Tests:
  - `provider_selection.rs::chip_proxy_is_read_with_its_companions_and_ambient_proxy_variables_are_not`
  - `crates/chip-fx-native/tests/proxy.rs`
  - `crates/fx-native-provider/tests/proxy.rs`
  - `private_ca.rs::a_private_ca_endpoint_is_reached_through_a_proxy_end_to_end`
  - `crates/fx-native-provider/tests/transport_boundaries.rs` (407 with no retry, `no_proxy` matching)
  - `crates/stack-agent/tests/transport_boundaries.rs` (the real binary: ambient proxy variables route nothing,
    407 fails once with no direct fallback, invalid settings fail before any connection naming the variable,
    `CHIP_NO_PROXY` direct versus proxied)
- Invalid proxy settings are refused before any request with an error naming the variable
  (`provider_selection.rs::an_invalid_proxy_setting_names_its_variable_and_never_prints_a_secret`).
- There is no native support for the system proxy configuration.

**Insecure options.** There is no TLS verification bypass (`check-boundaries.sh` forbids
`disable_verification` and `dangerous` in `fx-native-provider/src`).

- Plain HTTP is allowed to loopback.
- Plain HTTP is allowed to a private-IP literal only with `CHIP_ALLOW_INSECURE_LAN_HTTP=1`, and
  then keyless (`crates/chip-fx-native/src/credentials.rs:162`).

**Gaps**

- The opt-in proxy smoke test (`live_smoke.rs`, `STACK_AGENT_LIVE_PROXY`) was run once on
  2026-10-17. It made one request to Vercel AI Gateway through this development environment's
  egress proxy, an unauthenticated, TLS-intercepting HTTP CONNECT proxy on loopback, trusted
  through `SSL_CERT_FILE`, and it passed with no secret in any output.
  - It is not a corporate proxy, and it has no proxy authentication.
  - Direct connections also work in that environment, so the run does not independently prove the
    request took the proxy route.
  - The deterministic tests are what prove routing: `transport_boundaries.rs` checks connection
    counts at the proxy and at the endpoint.
- No run against an authenticating corporate proxy.
- No mTLS. Proxy authentication is basic only.

**Docs carried from chip-rs:** `docs/product/authoritative-fx.md`, `docs/product/production-boundary.md`
and `docs/product/native-provider.md` were updated for the current transport on 2026-10-17
(`production-boundary.md` carries a status note; its other `chip-cli` paths are not rewritten).
`docs/product/release-limitations.md` was corrected during migration (see provenance).

## G. WASM (session modules executed under `wasmi`; host-played provider only)

- **CI builds these for `wasm32-unknown-unknown`:**
  - `fx-core`, `fx-single-attempt`, `fx-wasm-abi`, `fx-host`
  - `chip-core` (check only)
  - `chip-wasm-host`
  - `chip-wasm-decision`
- CI also asserts that `chip_wasm_host.wasm` has no import section (carried from chip-rs CI).
- **Executed in this repository (all under `wasmi` 0.51, `wasm32-unknown-unknown`):**
  - `chip-wasm-decision`, in `crates/chip-wasm-decision-host` tests (which build the module and
    fail without the wasm32 target)
  - `fx-host` and `chip-wasm-host`, in `crates/chip-wasm-host/tests/wasmi_conformance.rs`. The test
    builds both release modules into `target/wasm-session-conformance` (failing, never skipping, if
    the build or the target is missing), instantiates them with no imports, asserts that neither has
    an import section and that each exports exactly `memory` plus the nine `fx-wasm-abi` functions,
    and drives them only through that ABI. It runs every fx-host conformance fixture against the
    module, an fx-host round trip and a `possibly_sent` provider failure, a multi-step Chip work
    whose inference is answered by the fx-host module (and, separately, by the native FX executor),
    an FX failure that ends the Chip work with its delivery and no second inference, malformed input
    and bad handles. Every exchange is replayed against the native `Session` and must be
    byte-identical. The provider and the capabilities are always played by the test.
  - a fixture module in `chip-wasm-reasoner`
- **Elsewhere:** fx's Node/V8 harnesses (`unified-runtime.yml`, `host-bridge.yml`,
  `native-provider.yml` in rkendel1/fx) ran them. That npm tooling was not migrated.
- **Unsupported:** no WASI or Wasmtime integration, no Node/V8 run here, no in-Wasm provider
  bridge. The only host combination with a conformance test is the Rust/`wasmi` host above with a
  host-played (scripted) provider; every other host/provider combination for the Wasm sessions is
  unsupported until it has one.

## H. Performance measurement (one recorded real-model run; not a budget)

**Recorded in this repository.** On 2026-10-17, `fxlive` ran against Vercel AI Gateway with
`nvidia/nemotron-3.5-lightning-free`, from `main` at `5a7a7f1`. The raw reports are committed in
[`product/perf-fxlive-vercel-2026-10-17/`](product/perf-fxlive-vercel-2026-10-17/); each passes
`scripts/check-perf-report.py --live`. Method and full tables:
[`product/performance.md`](product/performance.md) section 8.

| | n | min | p50 | p95 | max |
| --- | ---: | ---: | ---: | ---: | ---: |
| Inference = turn latency (3-iteration run) | 3 | 3.30 s | 3.71 s | not reported | 5.63 s |
| Inference = turn latency (5 batches of 4) | 20 | 2.80 s | 3.60 s | 4.56 s | 4.64 s |
| Chip overhead, per-batch p50 | 5 batches | 57.1 us | | | 77.2 us |
| Chip overhead, all samples | 20 | 46.7 us | | | 703.3 us (one outlier; 19 of 20 at or below 113 us) |
| Time to first response event | | not measured (`unavailable`) | | | |

- **Rate limit.** The free tier allows 5 requests per minute. A single 20-iteration run got 16
  HTTP 429s, recorded as typed `rate_limited` / `possibly_sent` errors with no retry. That run is
  kept and excluded from the figures.
- **Earlier external reference.** It was taken with the same gateway and model, 3 samples:
  3.63 / 4.12 / 4.42 s, and Chip overhead of 58.5 / 98.2 / 107.3 us. Its report,
  `/tmp/chip-perf-vercel.json`, was a temporary local file. It is not in this repository or in
  chip-rs or fx, and is not reproducible.

**What the numbers support.** For this workload, Chip's orchestration overhead (tens of
microseconds) is more than four orders of magnitude below the model call. The model call is
dominated by output length: about 1,150 completion tokens, mostly reasoning, at about 3.2 ms per
token.

**What they do not support:**

- FX overhead is not isolated from the network, gateway and model.
- Time to first token is not measured.
- One model, region and machine say nothing about other providers, other models, `serve`, or
  multi-turn work.
- The 20-sample p95 is a single run's tail, not a stable percentile.

**Reproducible without a model.** The deterministic harness
`crates/stack-agent/benches/baseline.rs` covers sections `parts`, `loop`, `real`, `process`,
`serve` and `fx` (a scripted provider under real FX). It asserts counts (one model call per
escalation, no retries) and reports timings. It compiles in CI but does not run there.

**Follow-up:**

- Expose time to first response event without weakening the `chip-fx-native` boundary.
- Optionally, add pacing to `fxlive` for rate-limited endpoints. Today the harness never waits or
  retries by design.

## I. Remaining legacy retirement work (separate task)

stack-agent contains no Zig source, Zig build definition, Zig toolchain, Zig-generated artifact or
retirement tooling. CI fails if a `*.zig`, `build.zig` or `build.zig.zon` file is tracked.

What remains:

- Zig appears only in:
  - docs
  - comments
  - `chip-session`'s fixture data for reading the Zig session layout
  - `scripts/audit-dependencies.sh` and `crates/chip-remote-env/tests/architecture.rs`, which
    forbid a dependency on it
- The Zig retirement in `rkendel1/fx` is a separate migration and decommissioning task. This
  repository does **not** prove behavioral parity with the Zig application.
- Per `ZIG_RETIREMENT.md` (external), these Zig surfaces have no Rust equivalent or no parity
  evidence:
  - the TUI (`src/ui`)
  - ACP and the gateway (`src/acp`, `src/gateway`)
  - the SDK entry points (N-API, WASM, terminal)
  - most of `src/core`: permissions, MCP, hooks, configuration, auth, subagents, tasks
  - tools such as shell, web, skills and memory
  - reverse session resume, including interrupted turns
- `zig-agent-loop` is still **blocked** there, and `zig-surfaces` has not started.
- The ten Zig agent behaviors the Rust release refuses or fails closed on are listed in
  `docs/product/release-limitations.md`.
- `ASK_CHIP_ROUTING.md` proposes, but does not implement, an opt-in `fx ask --executor chip`
  route, and lists the owner decisions it needs.

**Not claimed:** that the legacy product has been replaced. Each surface needs an identified Rust
equivalent and parity evidence first. The original repositories were not modified by this
consolidation.

---

## J. Project files: what a model may read, write and be told (partially addressed, 2026-10-20)

Closes or narrows the hostile audit's HA-04, HA-13, HA-14 and HA-15 and debts D-05, D-06, D-08 and
D-09. The patterns follow Beacon's endpoint agent (asymptote-labs/agent-beacon, MIT): its
credential-access and sensitive-edit threat rules, its `sessionfs` confinement and its redaction.

- **Withheld names** (`crates/chip-project/src/policy.rs`, `withheld`): Chip's own state (`.git`,
  `.stack-agent`, temporary files), `.env*`, credential directories (`.aws`, `.ssh`, `.gnupg`,
  `.docker`, `.kube`, `.azure`, `.password-store`), credential files (`.netrc`, `.pgpass`,
  `.git-credentials`, `.npmrc`, `.pypirc`, `.htpasswd`, `.vault-token`, `kubeconfig`, ...), private
  keys and key stores (`id_rsa`, `*.pem`, `*.key`, `*.p12`, `*.jks`, ...), data files named for
  secrets or credentials (`secrets.toml`, `credentials.json`; `credentials.rs` stays readable code)
  and Terraform state. No capability reads, writes, lists or searches them. Comparisons fold ASCII
  case and ignore trailing dots (HA-14), here and in `chip-pax`'s scope check.
- **Protected paths** (`protected`, `manifest_build_change`): readable, never written. A Cargo build
  script, `.cargo/` configuration, the toolchain file, CI pipelines, Git hook managers, agent
  instruction files (`AGENTS.md`, `CLAUDE.md`, `.cursorrules`, ...), editor and dev-container
  settings, shell start-up files, and a `Cargo.toml` write that changes its `build` setting. These
  run code or steer agents outside the work's verification (an editor's language server runs a
  build script when the project is opened). A refusal is an invalid invocation that names the rule.
- **Confinement** (`crates/chip-project/src/confine.rs`): on Unix every operation is relative to
  directory descriptors opened with `O_NOFOLLOW`, so no symlink is followed at any component even if
  it appears after the check; files are opened non-blocking and checked on the descriptor (a FIFO or
  device is `not_a_file`); a file with more than one hard link is not read (`hard_linked`) or
  searched; temporary files are created with their final mode.
- **Project lock** (`crates/chip-project/src/lock.rs`): see section C.
- **Threat model for repository content**: content reaches the model verbatim and is not filtered;
  obeying it gains nothing beyond the declared capabilities, the withheld and protected paths, and
  evidence-based completion. `crates/stack-agent/tests/prompt_injection.rs` runs the 18 cases of
  `tests/fixtures/prompt-injection.json` through the product loop with a scripted model that obeys
  each injection.

Evidence: `cargo test -p chip-project --test path_policy --test confinement --test lock`,
`cargo test -p fx-core --test redaction`, `cargo test -p stack-agent --test prompt_injection`.

**Not claimed:**
- That verifying a repository is safe: `pax.test` still runs the repository's existing build script,
  compiler wrapper and tests with your permissions (HA-01). The protected paths stop a model from
  *adding* such code, not the repository from already containing it.
- A configurable path policy, or detection of secrets inside ordinary source files.
- Any measurement of a real model's susceptibility to injection.
- Descriptor-based confinement on platforms other than Unix, where the check and the operation are
  still separate steps.
